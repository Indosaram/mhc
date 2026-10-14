use crate::session_worker_protocol::{WorkerDisplay,SESSION_WORKER_LIMITS};
pub fn apply_display(display:&mut WorkerDisplay,next:WorkerDisplay)->bool{if next.revision<display.revision{return false;}*display=next;true}
pub fn output_needs_snapshot(record:&serde_json::Value)->bool{matches!(record.get("type").and_then(serde_json::Value::as_str),Some("session_replaced"|"question_resolved"|"question_updated"))||(record.get("type").and_then(serde_json::Value::as_str)==Some("extension_ui_request")&&record.get("method").and_then(serde_json::Value::as_str)==Some("question"))}
pub fn check_output_size(record:&serde_json::Value,snapshot:bool)->Result<(),&'static str>{if record.to_string().len()>SESSION_WORKER_LIMITS.output_bytes{Err(if snapshot{"session_worker_snapshot_limit"}else{"session_worker_output_limit"})}else{Ok(())}}
/// What a worker builds from the host's opaque launch configuration (senpi \`createCliRuntimeFactory(prepared.configuration)\`).
pub struct SessionWorkerConfiguration{pub create_runtime:crate::multi_session_host::HostRuntimeFactory,pub agent_dir:String,pub session_dir:Option<String>}
pub type ConfigureSessionWorker=Box<dyn Fn(&serde_json::Value)->Result<SessionWorkerConfiguration,String>+Send+Sync>;
fn lock<T>(mutex:&std::sync::Mutex<T>)->std::sync::MutexGuard<'_,T>{mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)}
/// The worker's half of the credit protocol: one stdout frame per message, and blocking exchanges
/// whose grants the stdin thread delivers (senpi \`createWorkerCredit\` over \`Atomics.wait\`).
struct WorkerPort{out:std::sync::Mutex<std::io::Stdout>,pending:std::sync::Mutex<std::collections::HashMap<u64,crate::session_worker_signals::WorkerSignal>>,next:std::sync::atomic::AtomicU64}
impl WorkerPort{
    fn send(&self,message:&crate::session_worker_protocol::SessionWorkerWire){
        use std::io::Write;
        let Ok(line)=serde_json::to_string(message)else{return};
        let mut out=lock(&self.out);
        if out.write_all(line.as_bytes()).and_then(|()|out.write_all(b"\n")).and_then(|()|out.flush()).is_err(){std::process::exit(1);}
    }
    /// senpi \`failWorker\`: report and leave; the host releases ownership on the actual exit.
    fn fail(&self,error:&str)->!{self.send(&crate::session_worker_protocol::SessionWorkerWire::Failure{error:error.to_owned()});std::process::exit(1)}
    fn request(&self,build:impl FnOnce(u64)->crate::session_worker_protocol::SessionWorkerWire)->(i32,crate::session_worker_signals::WorkerSignal){
        let id=self.next.fetch_add(1,std::sync::atomic::Ordering::SeqCst);let signal=crate::session_worker_signals::WorkerSignal::default();
        lock(&self.pending).insert(id,signal.clone());
        self.send(&build(id));
        let code=signal.wait(std::time::Duration::from_millis(crate::session_worker_protocol::SESSION_WORKER_LIMITS.control_ms));
        lock(&self.pending).remove(&id);
        (code,signal)
    }
    fn exchange(&self,build:impl FnOnce(u64)->crate::session_worker_protocol::SessionWorkerWire,denied:&str)->crate::session_worker_signals::WorkerSignal{
        let(code,signal)=self.request(build);
        if code==crate::session_worker_protocol::WORKER_CREDIT_CODES.conflict{self.fail(denied);}
        if code!=crate::session_worker_protocol::WORKER_CREDIT_CODES.granted{self.fail("session_worker_credit_timeout");}
        signal
    }
    fn credit(&self,signal:u64,code:i32,display:Option<crate::session_worker_protocol::WorkerDisplay>){
        let Some(waiter)=lock(&self.pending).get(&signal).cloned()else{return};
        match(display,crate::session_worker_protocol::grant_from_credit_code(code)){
            (Some(display),Some(crate::session_worker_protocol::SessionWriteGrant::Granted))=>waiter.respond_display(&display),
            (_,Some(grant))=>waiter.acknowledge_grant(grant),
            (_,None)=>{},
        }
    }
}
fn canonical(path:&str)->String{crate::session_registry::canonical_path(std::path::Path::new(path)).map_or_else(|_|path.to_owned(),|path|path.to_string_lossy().into_owned())}
struct WorkerSession{runtime:std::sync::Arc<maho_core::agent_session_runtime::AgentSessionRuntime>,scope:maho_ai::node::provider_scope::ProviderScope}
struct WorkerState{
    port:std::sync::Arc<WorkerPort>,configure:ConfigureSessionWorker,
    prepared:std::sync::Mutex<Option<(SessionWorkerConfigurationHandle,crate::session_registry::RpcSessionLaunchProfile)>>,
    session:std::sync::Mutex<Option<std::sync::Arc<WorkerSession>>>,
    binding:std::sync::Mutex<Option<std::sync::Arc<crate::session_binding::RpcSessionBinding>>>,
    subscription:std::sync::Mutex<Option<maho_core::agent_session::AgentSessionSubscription>>,
    closing:std::sync::atomic::AtomicBool,
}
type SessionWorkerConfigurationHandle=std::sync::Arc<SessionWorkerConfiguration>;
tokio::task_local!{static WORKER_CONNECTION:Option<String>;}
/// senpi \`snapshot()\`: the routing identity, activity and every path a writer in this process still owns.
fn snapshot(session:&maho_core::agent_session::AgentSession)->crate::session_worker_protocol::WorkerSnapshot{
    let session_path=session.session_file().map(|path|canonical(&path));
    let mut live=maho_core::session_write_reservation::live_session_write_paths().iter().map(|path|canonical(path)).collect::<Vec<_>>();
    if let Some(path)=&session_path&&!live.contains(path){live.push(path.clone());}
    let activity=session.activity_snapshot();
    crate::session_worker_protocol::WorkerSnapshot{state:crate::connection_handler::build_rpc_session_state(session,None),session_path,live_session_paths:live,busy:session.is_session_busy(),handoff_busy:Some(crate::handoff_activity::is_handoff_busy(activity)),streaming:session.is_streaming()}
}
fn publish_snapshot(port:&WorkerPort,session:&maho_core::agent_session::AgentSession,settled:bool){
    let value=snapshot(session);
    if serde_json::to_vec(&value).map_or(true,|bytes|bytes.len()>crate::session_worker_protocol::SESSION_WORKER_LIMITS.output_bytes){port.fail("session_worker_snapshot_limit");}
    port.exchange(|signal|crate::session_worker_protocol::SessionWorkerWire::Snapshot{snapshot:value,signal,settled:settled.then_some(true)},"session_worker_output_denied");
}
/// senpi \`WorkerEventWriter.enqueue\`: every record crosses to the host under its own credit.
fn publish_output(port:&WorkerPort,session:&maho_core::agent_session::AgentSession,record:serde_json::Value){
    let replacement=output_needs_snapshot(&record).then(||snapshot(session));
    if check_output_size(&record,false).is_err(){port.fail("session_worker_output_limit");}
    if let Some(replacement)=&replacement&&serde_json::to_value(replacement).map_or(true,|value|check_output_size(&value,true).is_err()){port.fail("session_worker_snapshot_limit");}
    let activity=crate::session_worker_protocol::WorkerActivity{busy:session.is_session_busy(),handoff_busy:Some(crate::handoff_activity::is_handoff_busy(session.activity_snapshot())),streaming:session.is_streaming()};
    let connection=WORKER_CONNECTION.try_with(Clone::clone).ok().flatten();
    port.exchange(|signal|crate::session_worker_protocol::SessionWorkerWire::Output{record,connection,signal,activity,snapshot:replacement},"session_worker_output_denied");
}
impl WorkerState{
    fn current(&self)->Option<std::sync::Arc<WorkerSession>>{lock(&self.session).clone()}
    async fn handle(self:&std::sync::Arc<Self>,message:crate::session_worker_protocol::HostToSessionWorker)->Result<Option<crate::session_worker_protocol::SessionWorkerWire>,String>{
        use crate::session_worker_protocol::{HostToSessionWorker as H,SessionWorkerWire as W};
        match message{
            H::Prepare{request,configuration,profile}=>{
                if lock(&self.prepared).is_some(){return Err("Session worker already prepared".into());}
                let configuration=std::sync::Arc::new((self.configure)(&configuration)?);
                let mut profile=crate::session_worker_protocol::profile_from_wire(&profile)?;
                let path=match &profile.session_path{Some(path)=>path.clone(),None=>{
                    let dir=configuration.session_dir.clone().unwrap_or_else(||maho_core::session_manager::get_default_session_dir_with(&profile.runtime.cwd,&configuration.agent_dir));
                    let _=std::fs::create_dir_all(&dir);
                    std::path::Path::new(&dir).join(format!("{}_{}.jsonl",maho_core::session_manager::now_iso_millis().replace([':','.'],"-"),uuid::Uuid::new_v4())).to_string_lossy().into_owned()
                }};
                let session_path=canonical(&path);profile.session_path=Some(session_path.clone());
                *lock(&self.prepared)=Some((configuration,profile));
                Ok(Some(W::Prepared{request,session_path}))
            },
            H::Commit{request}=>{
                let Some((configuration,profile))=lock(&self.prepared).clone()else{return Err("Invalid session commit".into())};
                if self.current().is_some(){return Err("Invalid session commit".into());}
                let mut registry=crate::session_registry::RpcSessionRegistry::default();
                let crate::session_registry::OpenAdmission::Create(handle)=registry.admit_open(profile.clone(),false,crate::connection_handler::now_ms()).map_err(|error|error.to_string())?else{return Err("Invalid session commit".into())};
                registry.create_admitted_runtime(&handle,(configuration.create_runtime)(profile)).await.map_err(|error|error.to_string())?;
                let entry=registry.get(&handle).ok_or("Invalid session commit")?;
                let runtime=entry.runtime.clone().ok_or("Invalid session commit")?;
                let session=std::sync::Arc::new(WorkerSession{runtime,scope:entry.scope.clone()});
                let value=snapshot(session.runtime.session());
                *lock(&self.session)=Some(session);
                Ok(Some(W::Ready{request,snapshot:value}))
            },
            H::Bind{request,session_id,display,connection}=>{
                let session=self.current().ok_or("Invalid session binding")?;
                if lock(&self.binding).is_some(){return Err("Invalid session binding".into());}
                let port=std::sync::Arc::clone(&self.port);let output_session=session.runtime.session().weak_accessor();
                let sink:std::sync::Arc<dyn Fn(serde_json::Value)+Send+Sync>=std::sync::Arc::new(move|record|{if let Some(session)=output_session(){publish_output(&port,&session,record);}});
                let binding=std::sync::Arc::new(WORKER_CONNECTION.scope(connection,crate::session_binding::create_rpc_session_binding(session_id,session.runtime.session().clone(),sink,display.capabilities)).await);
                *lock(&self.binding)=Some(binding);
                let port=std::sync::Arc::clone(&self.port);let events=session.runtime.session().weak_accessor();
                let subscription=session.runtime.session().subscribe(std::sync::Arc::new(move|event:&maho_ext_api::AgentSessionEvent|{
                    if let Some(session)=events(){publish_snapshot(&port,&session,matches!(event,maho_ext_api::AgentSessionEvent::AgentSettled|maho_ext_api::AgentSessionEvent::AgentIdle));}
                }));
                *lock(&self.subscription)=Some(subscription);
                publish_snapshot(&self.port,session.runtime.session(),false);
                Ok(Some(W::Result{request,error:None}))
            },
            H::Command{request,command,connection,display:_}=>{
                let privileged=command.get("type").and_then(serde_json::Value::as_str).is_some_and(|kind|matches!(kind,"abort"|"abort_bash"|"extension_ui_response"|"extension_ui_progress"));
                let binding=lock(&self.binding).clone();
                let Some(binding)=binding.filter(|_|privileged||!self.closing.load(std::sync::atomic::Ordering::SeqCst))else{return Err("session_closing".into())};
                WORKER_CONNECTION.scope(connection,binding.handle(&command)).await?;
                if let Some(session)=self.current(){publish_snapshot(&self.port,session.runtime.session(),false);}
                Ok(Some(W::Result{request,error:None}))
            },
            H::Display{..}=>Ok(Some(W::ControlDone{control:crate::session_worker_protocol::WorkerControlKind::Display})),
            H::CancelUi=>{
                let binding=lock(&self.binding).clone();
                if let Some(binding)=binding{binding.cancel_pending_extension_ui_requests();}
                Ok(Some(W::ControlDone{control:crate::session_worker_protocol::WorkerControlKind::CancelUi}))
            },
            H::Close=>{
                self.closing.store(true,std::sync::atomic::Ordering::SeqCst);
                drop(lock(&self.subscription).take());
                let binding=lock(&self.binding).take();
                if let Some(binding)=binding{binding.dispose().await;}
                if let Some(session)=self.current(){let _=crate::session_teardown::dispose_runtime(&session.runtime,&session.scope).await;}
                std::process::exit(0)
            },
            H::Credit{..}=>Ok(None),
        }
    }
}
/// The session-worker process entry (senpi \`modes/rpc/session-worker.ts\`): one session, driven by the
/// host over stdin/stdout JSONL, whose writes and output each wait on a host grant.
pub async fn run_session_worker(configure:ConfigureSessionWorker)->Result<(),String>{
    let port=std::sync::Arc::new(WorkerPort{out:std::sync::Mutex::new(std::io::stdout()),pending:Default::default(),next:std::sync::atomic::AtomicU64::new(1)});
    // senpi \`installWriteReservation(canonicalPath)\`: every session writer asks the host first.
    let reserve=std::sync::Arc::clone(&port);
    maho_core::session_write_reservation::install_session_write_reservation(move|path|{
        let(code,_)=reserve.request(|signal|crate::session_worker_protocol::SessionWorkerWire::Reserve{path:canonical(path),signal});
        match crate::session_worker_protocol::grant_from_credit_code(code){
            Some(crate::session_worker_protocol::SessionWriteGrant::Granted)=>{},
            Some(crate::session_worker_protocol::SessionWriteGrant::Conflict)=>reserve.fail("session_path_in_use"),
            Some(crate::session_worker_protocol::SessionWriteGrant::Limit)=>reserve.fail("session_reservation_limit"),
            None=>reserve.fail("session_worker_credit_timeout"),
        }
    })?;
    let state=std::sync::Arc::new(WorkerState{port:std::sync::Arc::clone(&port),configure,prepared:Default::default(),session:Default::default(),binding:Default::default(),subscription:Default::default(),closing:std::sync::atomic::AtomicBool::new(false)});
    let(messages,mut inbox)=tokio::sync::mpsc::unbounded_channel::<crate::session_worker_protocol::HostToSessionWorker>();
    // Credits are answered on this thread so a worker blocked in an exchange is always released.
    let credits=std::sync::Arc::clone(&port);
    std::thread::spawn(move||{
        use std::io::BufRead;
        for line in std::io::stdin().lock().lines(){
            let Ok(line)=line else{break};
            match serde_json::from_str::<crate::session_worker_protocol::HostToSessionWorker>(&line){
                Ok(crate::session_worker_protocol::HostToSessionWorker::Credit{signal,code,display})=>credits.credit(signal,code,display),
                Ok(message)=>{if messages.send(message).is_err(){break;}},
                Err(error)=>credits.fail(&format!("session_worker_protocol_error: {error}")),
            }
        }
    });
    while let Some(message)=inbox.recv().await{
        let state=std::sync::Arc::clone(&state);
        tokio::spawn(async move{
            let request=match &message{crate::session_worker_protocol::HostToSessionWorker::Prepare{request,..}|crate::session_worker_protocol::HostToSessionWorker::Commit{request}|crate::session_worker_protocol::HostToSessionWorker::Bind{request,..}|crate::session_worker_protocol::HostToSessionWorker::Command{request,..}=>Some(*request),_=>None};
            match state.handle(message).await{
                Ok(Some(reply))=>state.port.send(&reply),
                Ok(None)=>{},
                Err(error)=>{
                    if error.starts_with("session_worker_"){state.port.fail(&error);}
                    match request{Some(request)=>state.port.send(&crate::session_worker_protocol::SessionWorkerWire::Result{request,error:Some(error)}),None=>state.port.fail(&error)}
                }
            }
        });
    }
    // The host closed the pipe: nobody can grant another write, so the worker leaves.
    Ok(())
}
#[cfg(test)]mod tests{use super::*;#[test]fn display_rejects_stale_revision_but_accepts_equal(){let mut display=WorkerDisplay{revision:2.,width:80.,rendered:false,capabilities:vec![]};let mut next=display.clone();next.revision=1.;next.width=40.;assert!(!apply_display(&mut display,next.clone()));assert_eq!(display.width,80.);next.revision=2.;assert!(apply_display(&mut display,next));assert_eq!(display.width,40.);}#[test]fn replacement_and_question_records_carry_snapshot_not_regular_events(){for kind in ["session_replaced","question_resolved","question_updated"]{assert!(output_needs_snapshot(&serde_json::json!({"type":kind})));}assert!(output_needs_snapshot(&serde_json::json!({"type":"extension_ui_request","method":"question"})));assert!(!output_needs_snapshot(&serde_json::json!({"type":"extension_ui_request","method":"select"})));assert!(!output_needs_snapshot(&serde_json::json!({"type":"message_update"})));}#[test]fn output_limit_counts_utf8_json_bytes(){let large=serde_json::json!("한".repeat(SESSION_WORKER_LIMITS.output_bytes/3));assert_eq!(check_output_size(&large,false),Err("session_worker_output_limit"));assert_eq!(check_output_size(&large,true),Err("session_worker_snapshot_limit"));assert!(check_output_size(&serde_json::json!({}),false).is_ok());}}
