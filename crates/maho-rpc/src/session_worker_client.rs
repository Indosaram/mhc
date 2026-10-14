use crate::session_worker_protocol::{WorkerDisplay,WorkerSnapshot};
#[derive(Debug,Clone,PartialEq)]pub enum WorkerControl{Display(WorkerDisplay),CancelUi}
#[derive(Default)]pub struct WorkerControls{display_pending:bool,cancel_pending:bool,latest_display:Option<WorkerDisplay>,stopped:bool}
impl WorkerControls{
    pub fn post(&mut self,control:WorkerControl)->Option<WorkerControl>{
        if self.stopped{return None;}
        match &control{WorkerControl::Display(display)=>{if self.display_pending{self.latest_display=Some(display.clone());return None;}self.display_pending=true;},WorkerControl::CancelUi=>{if self.cancel_pending{return None;}self.cancel_pending=true;}}
        Some(control)
    }
    pub fn control_done(&mut self,display:bool)->Option<WorkerControl>{
        if display{self.display_pending=false;if let Some(latest)=self.latest_display.take(){return self.post(WorkerControl::Display(latest));}}else{self.cancel_pending=false;}None
    }
    pub fn stop(&mut self){self.stopped=true;}
    pub fn post_envelope(&mut self,control:WorkerControl)->Option<crate::session_worker_protocol::HostToSessionWorker>{self.post(control).map(|control|match control{WorkerControl::Display(display)=>crate::session_worker_protocol::HostToSessionWorker::Display{display},WorkerControl::CancelUi=>crate::session_worker_protocol::HostToSessionWorker::CancelUi})}
    pub fn receive_control_done(&mut self,control:crate::session_worker_protocol::WorkerControlKind)->Option<crate::session_worker_protocol::HostToSessionWorker>{self.control_done(control==crate::session_worker_protocol::WorkerControlKind::Display).map(|control|match control{WorkerControl::Display(display)=>crate::session_worker_protocol::HostToSessionWorker::Display{display},WorkerControl::CancelUi=>crate::session_worker_protocol::HostToSessionWorker::CancelUi})}
}
pub fn worker_busy(active_requests:usize,snapshot:Option<&WorkerSnapshot>)->bool{active_requests>0||snapshot.is_some_and(|snapshot|snapshot.busy)}
pub fn worker_handoff_busy(active_requests:usize,snapshot:Option<&WorkerSnapshot>)->bool{active_requests>0||snapshot.is_some_and(|snapshot|snapshot.handoff_busy.unwrap_or(snapshot.busy))}
pub fn receive_reservation(message:crate::session_worker_protocol::SessionWorkerToHost,stopped:bool,reserve:impl FnOnce(&str)->crate::session_worker_protocol::SessionWriteGrant)->Option<crate::session_worker_protocol::SessionWorkerToHost>{
    if stopped{match &message{
        crate::session_worker_protocol::SessionWorkerToHost::Reserve{signal,..}|crate::session_worker_protocol::SessionWorkerToHost::Snapshot{signal,..}|crate::session_worker_protocol::SessionWorkerToHost::Output{signal,..}|crate::session_worker_protocol::SessionWorkerToHost::Width{signal,..}|crate::session_worker_protocol::SessionWorkerToHost::Capabilities{signal,..}=>signal.acknowledge(false),
        _=>{},
    }return None;}
    match message{crate::session_worker_protocol::SessionWorkerToHost::Reserve{path,signal}=>{signal.acknowledge_grant(reserve(&path));None},message=>Some(message)}
}
pub fn commit_output_activity(snapshot:&mut Option<WorkerSnapshot>,replacement:Option<WorkerSnapshot>,busy:bool,handoff_busy:Option<bool>,streaming:bool){
    if let Some(replacement)=replacement{*snapshot=Some(replacement);}else if let Some(snapshot)=snapshot{snapshot.busy=busy;snapshot.handoff_busy=handoff_busy;snapshot.streaming=streaming;snapshot.state["isStreaming"]=streaming.into();}
}
pub fn receive_snapshot(snapshot:&mut Option<WorkerSnapshot>,replacement:WorkerSnapshot,signal:&crate::session_worker_signals::WorkerSignal,settled:bool,reconcile:impl FnOnce(&[String]),notify_settled:impl FnOnce()){
    *snapshot=Some(replacement);
    reconcile(&snapshot.as_ref().expect("committed snapshot").live_session_paths);
    signal.acknowledge(true);
    if settled{notify_settled();}
}
pub async fn settle_output_credit(signal:&crate::session_worker_signals::WorkerSignal,consumed:impl std::future::Future<Output=Result<(),String>>,fail:impl FnOnce(&str)){
    match consumed.await{Ok(())=>signal.acknowledge(true),Err(error)=>{signal.acknowledge(false);fail(&error);}}
}
/// Spawns one isolated session worker process with piped stdin/stdout (senpi constructs a
/// \`node:worker_threads\` Worker; the native worker is a child process of the same binary).
pub type SpawnSessionWorker=std::sync::Arc<dyn Fn()->std::io::Result<tokio::process::Child>+Send+Sync>;
/// Lifecycle hooks the owning registry installs on every worker it allocates (senpi \`SessionWorkerCallbacks\`),
/// plus the output/close routes senpi reaches through the bound \`SessionEventWriter\`.
pub struct SessionWorkerCallbacks{
    pub reserve:Box<dyn Fn(&str)->crate::session_worker_protocol::SessionWriteGrant+Send+Sync>,
    /// Every snapshot republishes which paths this worker still writes.
    pub reconcile:Box<dyn Fn(&[String],Option<&str>)+Send+Sync>,
    pub output:Box<dyn Fn(serde_json::Value,Option<String>)+Send+Sync>,
    pub request_close:Box<dyn Fn()+Send+Sync>,
    pub failure:Box<dyn Fn(&str)+Send+Sync>,
    pub exit:Box<dyn Fn()+Send+Sync>,
}
struct ClientState{snapshot:Option<WorkerSnapshot>,stopped:bool,closing:bool,exited:bool,binding_ready:bool,display_revision:f64,capabilities:Vec<String>,controls:WorkerControls}
struct ClientInner{
    state:std::sync::Mutex<ClientState>,
    requests:std::sync::Mutex<crate::session_worker_requests::SessionWorkerRequests>,
    outbox:tokio::sync::mpsc::UnboundedSender<String>,
    kill:std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    exited:tokio::sync::watch::Receiver<bool>,
    settled:tokio::sync::watch::Sender<u64>,
    callbacks:SessionWorkerCallbacks,
    started:std::time::Instant,
}
/// Host-side handle of one session worker process (senpi \`SessionWorkerClient\`).
pub struct SessionWorkerClient{inner:std::sync::Arc<ClientInner>}
fn lock<T>(mutex:&std::sync::Mutex<T>)->std::sync::MutexGuard<'_,T>{mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)}
fn wire_signal(message:&crate::session_worker_protocol::SessionWorkerWire)->Option<u64>{
    use crate::session_worker_protocol::SessionWorkerWire as W;
    match message{W::Reserve{signal,..}|W::Snapshot{signal,..}|W::Output{signal,..}|W::Width{signal,..}|W::Capabilities{signal,..}=>Some(*signal),_=>None}
}
impl ClientInner{
    fn now(&self)->u64{u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)}
    fn send(&self,message:&crate::session_worker_protocol::HostToSessionWorker){if let Ok(line)=serde_json::to_string(message){let _=self.outbox.send(format!("{line}\n"));}}
    fn credit(&self,signal:u64,code:i32,display:Option<WorkerDisplay>){self.send(&crate::session_worker_protocol::HostToSessionWorker::Credit{signal,code,display});}
    fn display(&self)->WorkerDisplay{let mut state=lock(&self.state);state.display_revision+=1.;WorkerDisplay{revision:state.display_revision,width:80.,rendered:false,capabilities:state.capabilities.clone()}}
    fn post(&self,control:WorkerControl){let envelope=lock(&self.state).controls.post_envelope(control);if let Some(envelope)=envelope{self.send(&envelope);}}
    fn quarantine(&self){
        lock(&self.state).stopped=true;
        lock(&self.requests).close_with_error("session_closing");
        if let Some(kill)=lock(&self.kill).take(){let _=kill.send(());}
    }
    fn fail(&self,error:&str){
        {let state=lock(&self.state);if state.stopped{return;}}
        (self.callbacks.failure)(error);
        self.quarantine();
    }
    fn exited_now(&self){
        let unexpected={let mut state=lock(&self.state);let unexpected=!state.stopped&&!state.closing;state.exited=true;unexpected};
        if unexpected{self.fail("session_worker_exited");}
        {let mut state=lock(&self.state);state.stopped=true;state.controls.stop();}
        lock(&self.requests).close_with_error("session_worker_exited");
        self.settled.send_modify(|count|*count+=1);
        (self.callbacks.exit)();
    }
    fn receive(&self,message:crate::session_worker_protocol::SessionWorkerWire){
        use crate::session_worker_protocol::SessionWorkerWire as W;
        if lock(&self.state).stopped{if let Some(signal)=wire_signal(&message){self.credit(signal,crate::session_worker_protocol::WORKER_CREDIT_CODES.conflict,None);}return;}
        match message{
            W::Prepared{..}|W::Ready{..}|W::Result{..}=>{if let Ok(value)=serde_json::to_value(&message){lock(&self.requests).receive(&value);}},
            W::Reserve{path,signal}=>{let grant=(self.callbacks.reserve)(&path);self.credit(signal,crate::session_worker_protocol::worker_credit_code(grant),None);},
            W::Snapshot{snapshot,signal,settled}=>{
                (self.callbacks.reconcile)(snapshot.live_session_paths.as_slice(),snapshot.session_path.as_deref());
                lock(&self.state).snapshot=Some(snapshot);
                self.credit(signal,crate::session_worker_protocol::WORKER_CREDIT_CODES.granted,None);
                if settled==Some(true){self.settled.send_modify(|count|*count+=1);}
            },
            W::ControlDone{control}=>{let next=lock(&self.state).controls.receive_control_done(control);if let Some(next)=next{self.send(&next);}},
            W::Output{record,connection,signal,activity,snapshot}=>{
                if !lock(&self.state).binding_ready{self.credit(signal,crate::session_worker_protocol::WORKER_CREDIT_CODES.conflict,None);return;}
                // Identity and activity commit before publication; clients may attach or disconnect on that event.
                if let Some(replacement)=&snapshot{(self.callbacks.reconcile)(replacement.live_session_paths.as_slice(),replacement.session_path.as_deref());}
                {let mut state=lock(&self.state);commit_output_activity(&mut state.snapshot,snapshot,activity.busy,activity.handoff_busy,activity.streaming);}
                (self.callbacks.output)(record,connection);
                self.credit(signal,crate::session_worker_protocol::WORKER_CREDIT_CODES.granted,None);
            },
            W::Width{signal,..}|W::Capabilities{signal,..}=>{let display=self.display();self.credit(signal,crate::session_worker_protocol::WORKER_CREDIT_CODES.granted,Some(display));},
            W::RequestClose=>(self.callbacks.request_close)(),
            W::Failure{error}=>self.fail(&error),
        }
    }
}
impl SessionWorkerClient{
    /// Starts the worker process and its pipe pumps; only the process's actual exit releases ownership.
    pub fn spawn(spawn:&SpawnSessionWorker,callbacks:SessionWorkerCallbacks)->std::io::Result<Self>{
        use tokio::io::{AsyncBufReadExt,AsyncWriteExt};
        let mut child=spawn()?;
        let mut stdin=child.stdin.take().ok_or_else(||std::io::Error::other("session worker stdin is not piped"))?;
        let stdout=child.stdout.take().ok_or_else(||std::io::Error::other("session worker stdout is not piped"))?;
        let(outbox,mut lines)=tokio::sync::mpsc::unbounded_channel::<String>();
        let(kill,killed)=tokio::sync::oneshot::channel::<()>();
        let(exited_tx,exited)=tokio::sync::watch::channel(false);
        let inner=std::sync::Arc::new(ClientInner{
            state:std::sync::Mutex::new(ClientState{snapshot:None,stopped:false,closing:false,exited:false,binding_ready:false,display_revision:0.,capabilities:Vec::new(),controls:WorkerControls::default()}),
            requests:Default::default(),outbox,kill:std::sync::Mutex::new(Some(kill)),exited,settled:tokio::sync::watch::channel(0).0,callbacks,started:std::time::Instant::now(),
        });
        tokio::spawn(async move{while let Some(line)=lines.recv().await{if stdin.write_all(line.as_bytes()).await.is_err()||stdin.flush().await.is_err(){break;}}});
        let reader=std::sync::Arc::downgrade(&inner);
        let pump=tokio::spawn(async move{
            let mut lines=tokio::io::BufReader::new(stdout).lines();
            while let Ok(Some(line))=lines.next_line().await{
                let Some(inner)=reader.upgrade()else{break};
                match serde_json::from_str::<crate::session_worker_protocol::SessionWorkerWire>(&line){Ok(message)=>inner.receive(message),Err(error)=>inner.fail(&format!("session_worker_protocol_error: {error}"))}
            }
        });
        let watcher=std::sync::Arc::downgrade(&inner);
        tokio::spawn(async move{
            tokio::select!{_=child.wait()=>{},_=killed=>{let _=child.start_kill();let _=child.wait().await;}}
            let _=pump.await;
            if let Some(inner)=watcher.upgrade(){inner.exited_now();}
            let _=exited_tx.send(true);
        });
        Ok(Self{inner})
    }
    async fn request(&self,message:serde_json::Value)->Result<serde_json::Value,String>{
        let now=self.inner.now();
        let outbox=self.inner.outbox.clone();let weak=std::sync::Arc::downgrade(&self.inner);
        let receiver=lock(&self.inner.requests).submit_with_timeout(&message,now,move|value|outbox.send(format!("{value}\n")).map_err(|error|error.to_string()),move||{if let Some(inner)=weak.upgrade(){inner.fail("session_worker_request_timeout");}}).map_err(|error|error.to_string())?;
        receiver.await.map_err(|_|"session_closing".to_owned())?
    }
    pub async fn prepare(&self,configuration:&serde_json::Value,profile:&crate::session_registry::RpcSessionLaunchProfile)->Result<String,String>{
        let result=self.request(serde_json::json!({"type":"prepare","configuration":configuration,"profile":crate::session_worker_protocol::profile_to_wire(profile)})).await?;
        match(result["type"].as_str(),result["sessionPath"].as_str()){(Some("prepared"),Some(path))=>Ok(path.to_owned()),_=>Err("Invalid worker prepare response".into())}
    }
    pub async fn commit(&self)->Result<WorkerSnapshot,String>{
        let result=self.request(serde_json::json!({"type":"commit"})).await?;
        if result["type"]!="ready"{return Err("Invalid worker commit response".into());}
        let snapshot:WorkerSnapshot=serde_json::from_value(result["snapshot"].clone()).map_err(|error|error.to_string())?;
        lock(&self.inner.state).snapshot=Some(snapshot.clone());
        Ok(snapshot)
    }
    /// Installs the worker's routing binding; output is accepted from this point on.
    pub async fn bind(&self,session_id:&str,capabilities:Vec<String>,connection:Option<String>)->Result<(),String>{
        {let mut state=lock(&self.inner.state);state.capabilities=capabilities;state.binding_ready=true;}
        let mut message=serde_json::json!({"type":"bind","sessionId":session_id,"display":self.inner.display()});
        if let Some(connection)=connection{message["connection"]=connection.into();}
        self.request(message).await.map(|_|())
    }
    pub async fn command(&self,command:serde_json::Value,connection:Option<String>)->Result<(),String>{
        let mut message=serde_json::json!({"type":"command","command":command,"display":self.inner.display()});
        if let Some(connection)=connection{message["connection"]=connection.into();}
        self.request(message).await.map(|_|())
    }
    pub fn cancel_pending_extension_ui_requests(&self){self.inner.post(WorkerControl::CancelUi);}
    pub fn rerender_components(&self){let display=self.inner.display();self.inner.post(WorkerControl::Display(display));}
    pub fn dispose(&self){self.inner.post(WorkerControl::CancelUi);}
    pub fn snapshot(&self)->Option<WorkerSnapshot>{lock(&self.inner.state).snapshot.clone()}
    pub fn busy(&self)->bool{worker_busy(lock(&self.inner.requests).active_count(),lock(&self.inner.state).snapshot.as_ref())}
    pub fn handoff_busy(&self)->bool{worker_handoff_busy(lock(&self.inner.requests).active_count(),lock(&self.inner.state).snapshot.as_ref())}
    pub fn is_stopped(&self)->bool{lock(&self.inner.state).stopped}
    /// Bumped whenever the worker reports a settled turn or exits.
    pub fn settled(&self)->tokio::sync::watch::Receiver<u64>{self.inner.settled.subscribe()}
    /// Deadline requests termination, but only the actual exit releases ownership.
    pub async fn close(&self,grace_ms:u64){
        let mut exited=self.inner.exited.clone();
        let already={let mut state=lock(&self.inner.state);let already=state.exited||state.closing||state.stopped;state.closing=true;already};
        if !already{self.inner.send(&crate::session_worker_protocol::HostToSessionWorker::Close);}
        if tokio::time::timeout(std::time::Duration::from_millis(grace_ms),exited.wait_for(|exited|*exited)).await.is_err(){
            self.inner.quarantine();
            let _=exited.wait_for(|exited|*exited).await;
        }
    }
    pub fn quarantine(&self){self.inner.quarantine();}
}
#[cfg(test)]mod tests{
    use super::*;fn display(revision:f64)->WorkerControl{WorkerControl::Display(WorkerDisplay{revision,width:80.,rendered:false,capabilities:vec![]})}
    #[test]fn output_activity_updates_identity_before_publication_without_creating_runtime(){let mut snapshot=None;commit_output_activity(&mut snapshot,None,true,Some(true),true);assert!(snapshot.is_none());let replacement=WorkerSnapshot{state:serde_json::json!({"sessionId":"durable","isStreaming":false}),session_path:Some("/session".into()),live_session_paths:vec!["/session".into()],busy:false,handoff_busy:Some(false),streaming:false};commit_output_activity(&mut snapshot,Some(replacement),true,Some(true),true);assert!(!snapshot.as_ref().unwrap().busy);commit_output_activity(&mut snapshot,None,true,Some(true),true);let snapshot=snapshot.unwrap();assert_eq!(snapshot.state["sessionId"],"durable");assert_eq!(snapshot.state["isStreaming"],true);assert!(snapshot.busy);assert_eq!(snapshot.session_path.as_deref(),Some("/session"));}
    #[test]fn control_debt_coalesces_latest_display_and_cancel_independently(){let mut controls=WorkerControls::default();assert_eq!(controls.post(display(1.)),Some(display(1.)));assert!(controls.post(display(2.)).is_none());assert!(controls.post(display(3.)).is_none());assert_eq!(controls.post(WorkerControl::CancelUi),Some(WorkerControl::CancelUi));assert!(controls.post(WorkerControl::CancelUi).is_none());assert_eq!(controls.control_done(true),Some(display(3.)));assert!(controls.post(display(4.)).is_none());controls.control_done(false);assert_eq!(controls.post(WorkerControl::CancelUi),Some(WorkerControl::CancelUi));controls.stop();assert!(controls.control_done(true).is_none());assert!(controls.post(display(5.)).is_none());}
    #[test]fn handoff_uses_specific_busy_field_but_request_debt_always_holds(){let mut snapshot=WorkerSnapshot{state:serde_json::json!({}),session_path:None,live_session_paths:vec![],busy:true,handoff_busy:Some(false),streaming:false};assert!(worker_busy(0,Some(&snapshot)));assert!(!worker_handoff_busy(0,Some(&snapshot)));assert!(worker_handoff_busy(1,Some(&snapshot)));snapshot.handoff_busy=None;assert!(worker_handoff_busy(0,Some(&snapshot)));assert!(!worker_busy(0,None));}
}
