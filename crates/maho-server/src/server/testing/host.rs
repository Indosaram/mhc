use crate::server::{errors::ServerError,types::*};
use maho_agent::harness::{context::BACKGROUND_CONTEXT,session::{MemorySessionRepo,MemorySessionRepoOptions,Session,SessionRepo,SessionMetadata,SessionCreateOptions}};
use serde_json::{Value,json};
use std::{collections::BTreeMap,sync::Arc};
use tokio::sync::{Mutex,watch};

#[derive(Clone)]
pub struct Deferred<T:Clone> {sender:watch::Sender<Option<T>>}
impl<T:Clone> Default for Deferred<T> {fn default()->Self {Self {sender:watch::channel(None).0}}}
impl<T:Clone> Deferred<T> {
    pub fn resolve(&self,value:T) {self.sender.send_replace(Some(value));}
    pub async fn wait(&self)->T {
        let mut receiver=self.sender.subscribe();
        loop {if let Some(value)=receiver.borrow_and_update().clone() {return value;}
            if receiver.changed().await.is_err() {unreachable!("deferred sender lives with waiter");}}
    }
}
#[derive(Clone,Default)]
pub struct OpenGate {pub entered:Deferred<()>,pub release:Deferred<()>}
#[derive(Default)]
pub struct HarnessState {
    pub attached_clients:usize,pub attachment_release_count:usize,pub close_count:usize,
    pub service_calls:Vec<Value>,pub fail_attachment_release:Option<ServerError>,pub fail_close:Option<ServerError>,
    pub next_service_error:Option<ServerError>,pub next_service_result:Option<Value>,
    next_close_gate:Option<OpenGate>,next_service_gate:Option<OpenGate>,
}
pub struct TestHarness {
    pub session:Box<dyn Session>,pub closed:Deferred<()>,pub terminated:Deferred<Option<ServerError>>,
    pub state:Arc<Mutex<HarnessState>>,
}
impl TestHarness {
    pub fn new(session:Box<dyn Session>)->Self {
        Self {session,closed:Deferred::default(),terminated:Deferred::default(),state:Arc::new(Mutex::new(HarnessState {next_service_result:Some(json!({"ok":true})),..Default::default()}))}
    }
    pub async fn gate_next_close(&self)->OpenGate {let gate=OpenGate::default();self.state.lock().await.next_close_gate=Some(gate.clone());gate}
    pub async fn gate_next_service_call(&self)->OpenGate {let gate=OpenGate::default();self.state.lock().await.next_service_gate=Some(gate.clone());gate}
    pub async fn terminate(&self,error:ServerError) {self.session.close(&BACKGROUND_CONTEXT).await;self.terminated.resolve(Some(error));}
}
struct TestAttachment {state:Arc<Mutex<HarnessState>>,released:Mutex<bool>}
impl RoutedSessionAttachment for TestAttachment {
    fn invoke_service<'a>(&'a self,call:Value,_publish:Publisher,_context:Context)->ServerFuture<'a,Option<Value>> {
        Box::pin(async move {
            let gate={let mut state=self.state.lock().await;state.service_calls.push(call);
                if let Some(error)=state.next_service_error.take() {return Err(error);}
                state.next_service_gate.take()};
            if let Some(gate)=gate {gate.entered.resolve(());gate.release.wait().await;}
            let mut state=self.state.lock().await;let result=state.next_service_result.take();state.next_service_result=Some(json!({"ok":true}));Ok(result)
        })
    }
    fn release(&self)->ServerFuture<'_,()> {Box::pin(async move {
        let mut released=self.released.lock().await;if *released {return Ok(());}
        let mut state=self.state.lock().await;state.attachment_release_count+=1;
        if let Some(error)=&state.fail_attachment_release {return Err(error.clone());}
        *released=true;state.attached_clients-=1;Ok(())
    })}
}
impl RoutedSessionHandle for TestHarness {
    fn attach_client(&self)->ServerFuture<'_,Arc<dyn RoutedSessionAttachment>> {Box::pin(async move {
        self.state.lock().await.attached_clients+=1;
        Ok(Arc::new(TestAttachment {state:self.state.clone(),released:Mutex::new(false)}) as Arc<dyn RoutedSessionAttachment>)
    })}
    fn close(&self)->ServerFuture<'_,()> {Box::pin(async move {
        let gate={let mut state=self.state.lock().await;state.close_count+=1;state.next_close_gate.take()};
        if let Some(gate)=gate {gate.entered.resolve(());gate.release.wait().await;}
        if let Some(error)=self.state.lock().await.fail_close.take() {return Err(error);}
        self.session.close(&BACKGROUND_CONTEXT).await;self.closed.resolve(());self.terminated.resolve(None);Ok(())
    })}
}
pub struct TestServerServices;
struct TestServicesAttachment {presentation:Arc<dyn RoutedServerPresentation>}
pub fn create_test_server_services()->TestServerServices {TestServerServices}
impl RoutedServerServiceHost for TestServerServices {
    fn attach_client(&self,presentation:Arc<dyn RoutedServerPresentation>)->ServerFuture<'_,Arc<dyn RoutedServerServiceAttachment>> {
        Box::pin(async move {Ok(Arc::new(TestServicesAttachment {presentation}) as Arc<dyn RoutedServerServiceAttachment>)})
    }
}
impl RoutedServerServiceAttachment for TestServicesAttachment {
    fn invoke_service<'a>(&'a self,call:Value,_publish:Publisher,_context:Context)->ServerFuture<'a,Option<Value>> {Box::pin(async move {
        if call.get("instance").is_none() && call["serviceId"]=="pi.session-management" {
            if call["member"]=="attach" && let Some(args)=call["args"].as_array() && args.len()==1 && let Some(id)=args[0].as_str() {
                self.presentation.attach_session(id).await?;return Ok(Some(Value::Null));
            }
            if call["member"]=="detach" && call["args"].as_array().is_some_and(Vec::is_empty) {
                self.presentation.detach_session().await?;return Ok(Some(Value::Null));
            }
        }
        Err(ServerError::new("internal_error",&format!("Unsupported test server service {}.{}",call["serviceId"].as_str().unwrap_or_default(),call["member"].as_str().unwrap_or_default())))
    })}
    fn release(&self)->ServerFuture<'_,()> {Box::pin(async {Ok(())})}
}
#[derive(Default)]
pub struct TestHostState {pub harnesses:BTreeMap<String,Vec<Arc<TestHarness>>>,pub open_session_count:usize,pub next_open_session_error:Option<ServerError>,pub next_harness_close_error:Option<ServerError>,next_open_session_gate:Option<OpenGate>}
pub struct TestServerHost {pub server_services:TestServerServices,pub repo:MemorySessionRepo,pub state:Mutex<TestHostState>}
impl Default for TestServerHost {
    fn default()->Self {Self {server_services:create_test_server_services(),repo:MemorySessionRepo::new(MemorySessionRepoOptions {now:Some(Arc::new(||1))}),state:Mutex::new(TestHostState::default())}}
}
impl TestServerHost {
    pub async fn seed(&self,id:Option<String>,parent_session_id:Option<String>)->Result<SessionMetadata,ServerError> {
        let session=self.repo.create(SessionCreateOptions {id:Some(id.unwrap_or_else(||"session-1".into())),parent_session_id},&BACKGROUND_CONTEXT).await.map_err(session_error)?;
        let metadata=session.metadata().clone();session.close(&BACKGROUND_CONTEXT).await;Ok(metadata)
    }
    pub async fn gate_next_open_session(&self)->OpenGate {let gate=OpenGate::default();self.state.lock().await.next_open_session_gate=Some(gate.clone());gate}
    pub async fn latest_harness(&self,id:&str)->Result<Arc<TestHarness>,ServerError> {
        self.state.lock().await.harnesses.get(id).and_then(|harnesses|harnesses.last()).cloned().ok_or_else(||ServerError::new("internal_error",&format!("No harness for {id}")))
    }
}
fn session_error(error:maho_agent::harness::session::SessionError)->ServerError {ServerError::new("internal_error",&error.to_string())}
impl ServerHost for TestServerHost {
    fn server_services(&self)->&dyn RoutedServerServiceHost {&self.server_services}
    fn resolve_session<'a>(&'a self,id:&'a str)->ServerFuture<'a,Value> {Box::pin(async move {
        let matches=self.repo.list(None,&BACKGROUND_CONTEXT).await.map_err(session_error)?.into_iter().filter(|metadata|metadata.id==id).collect::<Vec<_>>();
        match matches.as_slice() {[]=>Err(ServerError::session_not_found(Some(&format!("Unknown session: {id}")))),[metadata]=>serde_json::to_value(metadata).map_err(|error|ServerError::new("internal_error",&error.to_string())),_=>Err(ServerError::session_ambiguous())}
    })}
    fn open_session(&self,metadata:Value)->ServerFuture<'_,Arc<dyn RoutedSessionHandle>> {Box::pin(async move {
        let gate={let mut state=self.state.lock().await;state.open_session_count+=1;state.next_open_session_gate.take()};
        if let Some(gate)=gate {gate.entered.resolve(());gate.release.wait().await;}
        let metadata:SessionMetadata=serde_json::from_value(metadata).map_err(|error|ServerError::new("internal_error",&error.to_string()))?;
        let session=self.repo.open(metadata.clone(),&BACKGROUND_CONTEXT).await.map_err(session_error)?;
        let mut state=self.state.lock().await;
        if let Some(error)=state.next_open_session_error.take() {drop(state);session.close(&BACKGROUND_CONTEXT).await;return Err(error);}
        let harness=Arc::new(TestHarness::new(session));
        harness.state.lock().await.fail_close=state.next_harness_close_error.take();
        state.harnesses.entry(metadata.id).or_default().push(harness.clone());Ok(harness as Arc<dyn RoutedSessionHandle>)
    })}
}
