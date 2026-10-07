use std::{collections::BTreeMap, io::BufRead, sync::{Arc, Mutex, mpsc}, time::Duration};
use senpi_task::{manager::{ManagedChildHandle, types::{ManagedRunners, ManagerStartSpec, StartResult}}, runners::{rpc::{process::{RpcChildProcess, RpcSpawnDescriptor}, model_admission::{RpcModelAdmissionOptions, create_rpc_model_admission}, terminate::terminate_rpc_child}, rpc_process::RpcProcessRunnerOptions, types::TerminateOptions}};

struct Actions;
impl maho_ext_api::ExtensionActions for Actions {
    fn send_message(&self,message:maho_ext_api::CustomMessage,_:maho_ext_api::SendMessageOptions)->Result<(),maho_ext_api::ExtensionFailure> { println!("MESSAGE {}",serde_json::to_string(&serde_json::json!({"customType":message.custom_type,"content":message.content,"display":message.display,"details":message.details})).map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?); Ok(()) }
    fn send_user_message(&self,_:maho_ext_api::UserMessageContent,_:maho_ext_api::SendUserMessageOptions)->Result<(),maho_ext_api::ExtensionFailure> { Err(maho_ext_api::ExtensionFailure::new("unexpected user message")) }
    fn append_entry(&self,_:&str,_:Option<maho_ext_api::JsonValue>)->Result<(),maho_ext_api::ExtensionFailure> { Err(maho_ext_api::ExtensionFailure::new("unexpected entry")) }
    fn get_all_tools(&self)->Result<Vec<maho_ext_api::ToolInfo>,maho_ext_api::ExtensionFailure> { Ok(vec![]) }
}

struct Processes(Arc<Mutex<Vec<Arc<RpcChildProcess>>>>);
struct ManagerCleanup(Arc<senpi_task::manager::TaskManager>);
impl Drop for ManagerCleanup {
    fn drop(&mut self) {
        for entry in self.0.list(&senpi_task::manager::types::ListScope::All) {
            if !entry.record.status.is_terminal() && let Err(error)=self.0.cancel_task(&entry.record.task_id,Some("native runner proof teardown"),Default::default()) { eprintln!("native manager cleanup failed: {error}"); }
            self.0.forget(&entry.record.task_id);
        }
    }
}
struct HandleCleanup(Arc<dyn ManagedChildHandle>);
impl Drop for HandleCleanup {
    fn drop(&mut self) {
        if let Err(error)=senpi_task::manager::child_handle::discard_managed_handle(self.0.as_ref()) { eprintln!("native handle cleanup failed: {error}"); }
    }
}
impl Drop for Processes {
    fn drop(&mut self) {
        for child in self.0.lock().expect("processes").iter() {
            if let Err(error)=terminate_rpc_child(child,TerminateOptions { sigkill_delay_ms:Some(100) }) { eprintln!("native proof cleanup failed: {error}"); }
        }
    }
}
fn command<T:Send+'static>(handle:Arc<dyn ManagedChildHandle>, operation:impl FnOnce(Arc<dyn ManagedChildHandle>)->Result<T,String>+Send+'static)->Result<T,Box<dyn std::error::Error>> {
    let (sender,receiver)=mpsc::channel(); let active=handle.clone();
    let worker=std::thread::spawn(move || { let _=sender.send(operation(active)); });
    let result=receiver.recv_timeout(Duration::from_secs(15));
    let cleanup=if result.is_err() { senpi_task::manager::child_handle::discard_managed_handle(handle.as_ref()) } else { Ok(()) };
    worker.join().map_err(|_| "native command worker panicked")?;
    cleanup?;
    Ok(result?.map_err(std::io::Error::other)?)
}
fn main()->Result<(),Box<dyn std::error::Error>> {
    let mut args=std::env::args().skip(1);
    let executable=args.next().ok_or("native mhc path required")?;
    let home=args.next().ok_or("isolated HOME with loopback models.json required")?;
    let session=args.next().ok_or("actual persisted native session path required")?;
    let receipt_path=args.next().ok_or("provider receipt Unix socket required")?;
    let receipt=std::os::unix::net::UnixStream::connect(receipt_path)?;
    receipt.set_read_timeout(Some(Duration::from_secs(15)))?;
    let mut receipts=std::io::BufReader::new(receipt);
    let mut ready=String::new(); receipts.read_line(&mut ready)?;
    assert_eq!(ready,"TASK44_PROVIDER_OBSERVER_READY\n","provider must register its observer before any request");
    let env=BTreeMap::from([("HOME".into(),home.clone()),("MAHO_CODING_AGENT_DIR".into(),std::path::Path::new(&home).join("agent").to_string_lossy().into_owned()),("PATH".into(),"/usr/bin:/bin".into())]);
    let catalog=RpcSpawnDescriptor { command:executable.clone(),args:["--offline","--no-session","--no-tools","--no-skills","--no-prompt-templates","--list-models","task44"].map(str::to_owned).into(),cwd:home.clone(),env:env.clone() };
    let admission=create_rpc_model_admission(RpcModelAdmissionOptions { build_spawn:Some(Arc::new(move |_| catalog.clone())),..Default::default() });
    let descriptor=RpcSpawnDescriptor { command:executable,args:vec!["--mode".into(),"rpc".into(),"--offline".into(),"--no-tools".into(),"--no-skills".into(),"--no-prompt-templates".into(),"--model".into(),"task44/native".into(),"--session".into(),session.clone()],cwd:home.clone(),env };
    let processes=Arc::new(Mutex::new(Vec::new())); let captured=processes.clone(); let cleanup=Processes(processes.clone());
    let options=RpcProcessRunnerOptions {
        build_spawn:Some(Arc::new(move |_| descriptor.clone())),model_admission:Some(admission),
        spawn_child:Some(Arc::new(move |descriptor| {
            println!("NATIVE_SPAWN {}",serde_json::json!({"command":descriptor.command,"args":descriptor.args,"cwd":descriptor.cwd}));
            let child=Arc::new(RpcChildProcess::spawn(descriptor)); captured.lock().expect("processes").push(child.clone()); child
        })),..Default::default()
    };
    let runner=maho_omo_task::engine_runners::build_process_runner(options.clone());
    let engine=maho_omo_task::engine::compose_task_engine_with_rpc_respawn(maho_omo_task::engine::ComposeTaskEngineDeps {
        cwd:home.clone().into(), config:serde_json::json!({"task":{"default_execution_mode":"process"},"agents":{"native-proof":{"execution_mode":"process","model":"task44/native"}}}),
        runners:ManagedRunners { in_process:runner.clone(),process:runner },actions:Arc::new(Actions),coordinator:None,resolve_registry:Arc::new(|| None),host_transport:None,
    },Some(maho_omo_task::engine_runners::build_rpc_respawn_runner(options)));
    let manager_cleanup=ManagerCleanup(engine.manager.clone());
    let (sender,receiver)=mpsc::channel(); let launching=engine.manager.clone();
    let worker=std::thread::spawn(move || { let _=sender.send(launching.start(&ManagerStartSpec {
        prompt:"task44-launch".into(),subagent_type:Some("native-proof".into()),model:Some("task44/native".into()),
        parent_session_id:"task44-native-parent".into(),root_session_id:Some("task44-native-parent".into()),run_in_background:true,
        execution_mode:Some(senpi_task::manager::execution_mode::ExecutionMode::Process),..Default::default()
    })); });
    let result=receiver.recv_timeout(Duration::from_secs(15));
    if result.is_err() { drop(cleanup); worker.join().map_err(|_| "native launch worker panicked")?; return Err("native launch exceeded bounded deadline".into()); }
    worker.join().map_err(|_| "native launch worker panicked")?;
    let StartResult::Started(record)=result? else { return Err("native manager launch was not admitted".into()); };
    let completed=engine.manager.wait_for(&record.task_id,None,Some(Duration::from_secs(15)))?;
    assert_eq!(completed.status,senpi_task::state::TaskStatus::Completed);
    assert_eq!(completed.final_response.as_deref(),Some("task44-native-provider"));
    assert!(completed.spawn_spec.is_some(),"reconstruction must use actual manager-persisted spawn facts");
    assert!(std::fs::metadata(&session)?.len()>0,"initial native launch must persist its session before reconstruction");
    let handle=engine.manager.get_resident_handle(&record.task_id).ok_or("completed native resident handle unavailable")?;
    let handle_cleanup=HandleCleanup(handle.clone());
    println!("RECEIPT provider launch output observed through actual manager scheduling and persisted record {}",record.task_id);
    drop(handle_cleanup);
    for child in processes.lock().expect("processes").iter() { assert!(child.wait_exit_timeout(Duration::from_secs(5)).is_some(),"initial child must exit before reconstruction"); }
    drop(handle);
    engine.manager.forget(&record.task_id);
    let (resumed_sender,resumed_receiver)=mpsc::channel();
    let resuming=engine.manager.clone();
    let persisted=completed.clone();
    let resumed_worker=std::thread::spawn(move || {
        let _=resumed_sender.send(resuming.respawn(&persisted,Some(std::path::Path::new(&session))));
    });
    let resumed=resumed_receiver.recv_timeout(Duration::from_secs(15));
    if resumed.is_err() { drop(cleanup); resumed_worker.join().map_err(|_| "native resume worker panicked")?; return Err("native resume exceeded bounded deadline".into()); }
    resumed_worker.join().map_err(|_| "native resume worker panicked")?;
    let handle=match resumed? {
        senpi_task::lifecycle::port::RespawnResult::Ok(handle)=>handle,
        senpi_task::lifecycle::port::RespawnResult::Failed { disposition,code,reason }=>return Err(format!("native manager reconstruction failed: {disposition:?} {code:?} {reason}").into()),
    };
    let handle_cleanup=HandleCleanup(handle.clone());
    assert_eq!(processes.lock().expect("processes").len(),2,"resume must construct a second native process");
    match engine.manager.reattach(&completed,handle.clone()) {
        senpi_task::lifecycle::port::ReattachResult::Ok=>{},
        senpi_task::lifecycle::port::ReattachResult::Failed { kind,reason }=>return Err(format!("native manager reattachment failed: {kind:?} {reason}").into()),
    }
    let continuing=engine.manager.clone(); let task_id=record.task_id.clone();
    command(handle.clone(),move |_| continuing.continue_task(&task_id,"task44-resumed",None).map_err(|error| error.to_string()))?;
    let resumed_record=engine.manager.wait_for(&record.task_id,None,Some(Duration::from_secs(15)))?;
    assert_eq!(resumed_record.status,senpi_task::state::TaskStatus::Completed);
    assert_eq!(resumed_record.final_response.as_deref(),Some("task44-native-resumed"));
    assert!(resumed_record.notification.run_epoch>completed.notification.run_epoch,"manager continuation must advance the persisted run epoch");
    println!("RECEIPT actual manager reattachment, continuation and persisted resumed output observed");
    let continuing=engine.manager.clone(); let task_id=record.task_id.clone();
    command(handle.clone(),move |_| continuing.continue_task(&task_id,"task44-error",None).map_err(|error| error.to_string()))?;
    let failed=engine.manager.wait_for(&record.task_id,None,Some(Duration::from_secs(15)))?;
    assert_eq!(failed.status,senpi_task::state::TaskStatus::Error,"real provider rejection must persist as a manager error");
    println!("RECEIPT deterministic provider rejection settled through persisted manager outcome");
    let continuing=engine.manager.clone(); let task_id=record.task_id.clone();
    command(handle.clone(),move |_| continuing.continue_task(&task_id,"task44-cancel",None).map_err(|error| error.to_string()))?;
    let mut line=String::new(); receipts.read_line(&mut line)?;
    assert_eq!(line,"TASK44_PROVIDER_CANCEL_HELD\n","abort must follow the actual provider-held request");
    let cancelling=engine.manager.clone(); let task_id=record.task_id.clone();
    command(handle.clone(),move |_| cancelling.cancel_task(&task_id,Some("native provider cancellation proof"),Default::default()).map_err(|error| error.to_string()))?;
    let cancelled=engine.manager.wait_for(&record.task_id,None,Some(Duration::from_secs(15)))?;
    assert_eq!(cancelled.status,senpi_task::state::TaskStatus::Cancelled);
    for child in processes.lock().expect("processes").iter() { assert!(child.wait_exit_timeout(Duration::from_secs(5)).is_some(),"manager cancellation must reap native child before explicit driver cleanup"); }
    line.clear(); receipts.read_line(&mut line)?;
    assert_eq!(line,"TASK44_PROVIDER_CANCEL_CLOSED\n","manager cancellation must close the actual held provider transport");
    println!("RECEIPT manager cancellation persisted and native child reaped before driver cleanup");
    drop(handle_cleanup);
    for child in processes.lock().expect("processes").iter() { assert!(child.wait_exit_timeout(Duration::from_secs(5)).is_some(),"discard must reap native child"); }
    drop(cleanup);
    drop(manager_cleanup);
    println!("RECEIPT cancellation outcome, native process exit and worker joins observed");
    Ok(())
}
