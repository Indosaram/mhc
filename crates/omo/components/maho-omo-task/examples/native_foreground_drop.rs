#[path = "../tests/support/mod.rs"]
mod support;
use std::{collections::BTreeMap, io::BufRead, sync::{Arc, Mutex}, time::Duration};
use maho_ext_api::*;
use maho_omo_task::{component::TaskComponent, engine::{compose_task_engine_with_rpc_respawn, ComposeTaskEngineDeps}, engine_runners::{build_process_runner, build_rpc_respawn_runner}};
use senpi_task::{manager::types::{ManagedRunners, ListScope}, runners::{rpc::{process::{RpcSpawnDescriptor, RpcChildProcess}, model_admission::{RpcModelAdmissionOptions, create_rpc_model_admission}, terminate::terminate_rpc_child}, rpc_process::RpcProcessRunnerOptions, types::TerminateOptions}};
struct Actions;
struct ReceiptReader { socket:std::os::unix::net::UnixStream, thread:Option<std::thread::JoinHandle<()>> }
impl Drop for ReceiptReader {
    fn drop(&mut self) {
        if let Err(error)=self.socket.shutdown(std::net::Shutdown::Both) { eprintln!("provider receipt shutdown failed: {error}"); }
        if let Some(thread)=self.thread.take() && thread.join().is_err() { eprintln!("provider receipt reader panicked"); }
    }
}
impl ExtensionActions for Actions {
    fn send_message(&self,message:CustomMessage,_:SendMessageOptions)->Result<(),ExtensionFailure> { println!("MESSAGE {}",serde_json::to_string(&message).map_err(|error| ExtensionFailure::new(error.to_string()))?); Ok(()) }
    fn send_user_message(&self,_:UserMessageContent,_:SendUserMessageOptions)->Result<(),ExtensionFailure> { Err(ExtensionFailure::new("unexpected user message")) }
    fn append_entry(&self,_:&str,_:Option<JsonValue>)->Result<(),ExtensionFailure> { Err(ExtensionFailure::new("unexpected entry")) }
    fn get_all_tools(&self)->Result<Vec<ToolInfo>,ExtensionFailure> { Ok(vec![]) }
}
struct Cleanup { component:Arc<TaskComponent>,processes:Arc<Mutex<Vec<Arc<RpcChildProcess>>>> }
impl Drop for Cleanup {
    fn drop(&mut self) {
        for entry in self.component.engine.manager.list(&ListScope::All) {
            if !entry.record.status.is_terminal() && let Err(error)=self.component.engine.manager.cancel_task(&entry.record.task_id,Some("native drop proof teardown"),Default::default()) { eprintln!("native cancel cleanup failed: {error}"); }
        }
        for child in self.processes.lock().expect("processes").iter() { if let Err(error)=terminate_rpc_child(child,TerminateOptions { sigkill_delay_ms:Some(100) }) { eprintln!("native process cleanup failed: {error}"); } }
        self.component.dispose();
        for entry in self.component.engine.manager.list(&ListScope::All) { self.component.engine.manager.forget(&entry.record.task_id); }
    }
}
#[tokio::main(flavor="current_thread")]
async fn main()->Result<(),Box<dyn std::error::Error>> {
    let mut args=std::env::args().skip(1);
    let executable=args.next().ok_or("native mhc required")?; let home=args.next().ok_or("isolated HOME required")?;
    let receipt_socket=args.next().ok_or("provider receipt Unix socket required")?;
    let receipt=std::os::unix::net::UnixStream::connect(receipt_socket)?;
    receipt.set_read_timeout(Some(Duration::from_secs(15)))?;
    let mut receipt=std::io::BufReader::new(receipt);
    let receipt_control=receipt.get_ref().try_clone()?;
    let mut ready=String::new(); receipt.read_line(&mut ready)?;
    assert_eq!(ready,"TASK44_PROVIDER_OBSERVER_READY\n","provider must register its observer before foreground launch");
    let env=BTreeMap::from([("HOME".into(),home.clone()),("MAHO_CODING_AGENT_DIR".into(),std::path::Path::new(&home).join("agent").to_string_lossy().into_owned()),("PATH".into(),"/usr/bin:/bin".into())]);
    let catalog=RpcSpawnDescriptor { command:executable.clone(),args:["--offline","--no-session","--no-tools","--no-skills","--no-prompt-templates","--list-models","task44"].map(str::to_owned).into(),cwd:home.clone(),env:env.clone() };
    let admission=create_rpc_model_admission(RpcModelAdmissionOptions { build_spawn:Some(Arc::new(move |_| catalog.clone())),..Default::default() });
    let descriptor=RpcSpawnDescriptor { command:executable,args:["--mode","rpc","--offline","--no-session","--no-tools","--no-skills","--no-prompt-templates","--model","task44/native"].map(str::to_owned).into(),cwd:home.clone(),env };
    let processes=Arc::new(Mutex::new(Vec::new())); let captured=processes.clone();
    let options=RpcProcessRunnerOptions { build_spawn:Some(Arc::new(move |_| descriptor.clone())),model_admission:Some(admission),spawn_child:Some(Arc::new(move |descriptor| {
        println!("NATIVE_SPAWN {}",serde_json::json!({"command":descriptor.command,"args":descriptor.args,"cwd":descriptor.cwd}));
        let child=Arc::new(RpcChildProcess::spawn(descriptor)); captured.lock().expect("processes").push(child.clone()); child
    })),..Default::default() };
    let process=build_process_runner(options.clone()); let actions=Arc::new(Actions);
    let engine=compose_task_engine_with_rpc_respawn(ComposeTaskEngineDeps { cwd:home.clone().into(),config:serde_json::json!({"task":{"default_execution_mode":"process"},"background_task":{"defaultConcurrency":1},"agents":{"native-proof":{"execution_mode":"process","model":"task44/native"}}}),runners:ManagedRunners { in_process:process.clone(),process },actions:actions.clone(),coordinator:None,resolve_registry:Arc::new(|| None) },Some(build_rpc_respawn_runner(options)));
    let mut api=support::api(); api.runtime.bind(actions);
    let component=TaskComponent::register(&mut api,engine,Default::default(),senpi_task::team::liveness_ownership::TeamMemberOwnershipDeps { state_dir:senpi_task::store::StateDirConfig { project_dir:home.clone().into(),task_state_dir:None },team_bounds:senpi_task::team::runtime_config::TeamTaskBounds { max_members:4,max_parallel_members:2,max_wall_clock_minutes:10 },load_runtime_state:None },false)?.ok_or("component disabled")?;
    let cleanup=Cleanup { component:component.clone(),processes:processes.clone() };
    let mut context=support::context(); context.cwd=home.into();
    let mut start=ExtensionEvent::SessionStart(SessionStartEvent { reason:SessionReason::New,initial_model_provenance:None,previous_session_file:None });
    for handler in &api.registered.handlers[&EventKind::SessionStart] { handler(&mut start,&context).await?; }
    let task=api.registered.tools.iter().find(|tool| tool.definition.name=="task").ok_or("task missing")?;
    let (ready,received)=tokio::sync::oneshot::channel();
    let (closed,closure)=std::sync::mpsc::channel();
    let provider_receipt=std::thread::spawn(move || {
        let mut lines=receipt.lines();
        let line=lines.next().transpose();
        let accepted=matches!(&line,Ok(Some(line)) if line=="TASK44_PROVIDER_HELD");
        let _=ready.send(accepted);
        let _=closed.send(lines.next().transpose());
    });
    let mut provider_receipt=ReceiptReader { socket:receipt_control,thread:Some(provider_receipt) };
    let mut invocation=(task.definition.execute)(ToolCall { id:"native-foreground-drop",params:serde_json::json!({"prompt":"task44-drop","subagent_type":"native-proof","model":"task44/native","run_in_background":false}),signal:Default::default(),on_update:None,context:Some(&context) });
    let (stop,stopped)=std::sync::mpsc::channel(); let (timeout,deadline)=tokio::sync::oneshot::channel();
    let watchdog=std::thread::spawn(move || { if matches!(stopped.recv_timeout(Duration::from_secs(15)),Err(std::sync::mpsc::RecvTimeoutError::Timeout)) { let _=timeout.send(()); } });
    let observation=tokio::select! {
        result=&mut invocation=>Err(format!("foreground settled before provider-held signal: {result:?}")),
        result=received=>result.map_err(|error| error.to_string()).and_then(|held| if held { Ok(()) } else { Err("missing exact provider-held receipt".into()) }),
        _=deadline=>Err("native foreground provider-held progress deadline exceeded".into()),
    };
    let _=stop.send(()); watchdog.join().map_err(|_| "progress watchdog panicked")?;
    observation.map_err(std::io::Error::other)?;
    let pending=component.engine.manager.start(&senpi_task::manager::types::ManagerStartSpec {
        prompt:"task44-launch".into(),subagent_type:Some("native-proof".into()),model:Some("task44/native".into()),
        parent_session_id:"session".into(),root_session_id:Some("session".into()),run_in_background:true,
        execution_mode:Some(senpi_task::manager::execution_mode::ExecutionMode::Process),..Default::default()
    });
    let senpi_task::manager::types::StartResult::Started(queued)=pending else { return Err("native queued task was not admitted".into()); };
    assert_eq!(queued.status,senpi_task::state::TaskStatus::Pending,"one live foreground task must occupy the configured concurrency slot");
    assert_eq!(processes.lock().expect("processes").len(),1,"pending task must not spawn a native process before slot release");
    let began=std::time::Instant::now(); drop(invocation);
    assert!(began.elapsed()<Duration::from_secs(5),"actual registered foreground Drop must settle its executor within bound");
    let records=component.engine.manager.list(&ListScope::All); assert_eq!(records.len(),2);
    let dropped=records.iter().find(|entry| entry.record.task_id!=queued.task_id).ok_or("dropped task missing")?;
    assert_eq!(dropped.record.status,senpi_task::state::TaskStatus::Cancelled,"run_spawn must propagate the owned executor abort to manager cancellation");
    assert!(processes.lock().expect("processes")[0].wait_exit_timeout(Duration::from_secs(5)).is_some(),"actual foreground Drop must reap its child before later explicit cleanup");
    let closed=closure.recv_timeout(Duration::from_secs(15))??;
    assert_eq!(closed.as_deref(),Some("TASK44_PROVIDER_DROP_CLOSED"),"foreground Drop must close its actual held provider transport");
    provider_receipt.thread.take().ok_or("provider reader missing")?.join().map_err(|_| "provider receipt reader panicked")?;
    let completed=component.engine.manager.wait_for(&queued.task_id,None,Some(Duration::from_secs(15)))?;
    assert_eq!(completed.status,senpi_task::state::TaskStatus::Completed);
    assert_eq!(completed.final_response.as_deref(),Some("task44-native-provider"));
    assert_eq!(processes.lock().expect("processes").len(),2,"slot release must launch exactly one queued native child");
    println!("RECEIPT registered foreground Drop settled executor and cancelled/reaped native child before explicit teardown");
    println!("RECEIPT actual manager concurrency slot release promoted queued native task and observed provider output");
    drop(cleanup);
    for child in processes.lock().expect("processes").iter() { assert!(child.wait_exit_timeout(Duration::from_secs(5)).is_some()); }
    println!("RECEIPT explicit lifecycle teardown terminated and reaped native child after Drop observation");
    Ok(())
}
