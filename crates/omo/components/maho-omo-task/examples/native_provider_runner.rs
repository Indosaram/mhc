use std::{collections::BTreeMap, io::BufRead, sync::{Arc, Mutex, mpsc}, time::Duration};
use senpi_task::{manager::{ManagedChildHandle, types::ManagedStartSpec}, runners::{RunnerOutcome, rpc::{process::{RpcChildProcess, RpcSpawnDescriptor}, model_admission::{RpcModelAdmissionOptions, create_rpc_model_admission}, terminate::terminate_rpc_child}, rpc_process::RpcProcessRunnerOptions, types::TerminateOptions}};

struct Processes(Arc<Mutex<Vec<Arc<RpcChildProcess>>>>);
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
fn outcome(handle:Arc<dyn ManagedChildHandle>)->Result<RunnerOutcome,Box<dyn std::error::Error>> {
    let (sender,receiver)=mpsc::channel(); let waiting=handle.clone();
    let worker=std::thread::spawn(move || { let _=sender.send(waiting.wait_for_outcome()); });
    let result=receiver.recv_timeout(Duration::from_secs(15));
    let cleanup=if result.is_err() { senpi_task::manager::child_handle::discard_managed_handle(handle.as_ref()) } else { Ok(()) };
    worker.join().map_err(|_| "native outcome worker panicked")?;
    cleanup?;
    Ok(result?)
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
    let env=BTreeMap::from([("HOME".into(),home.clone()),("MAHO_CODING_AGENT_DIR".into(),std::path::Path::new(&home).join("agent").to_string_lossy().into_owned()),("PATH".into(),"/usr/bin:/bin".into())]);
    let catalog=RpcSpawnDescriptor { command:executable.clone(),args:["--offline","--no-session","--no-tools","--no-skills","--no-prompt-templates","--list-models","task44"].map(str::to_owned).into(),cwd:home.clone(),env:env.clone() };
    let admission=create_rpc_model_admission(RpcModelAdmissionOptions { build_spawn:Some(Arc::new(move |_| catalog.clone())),..Default::default() });
    let descriptor=RpcSpawnDescriptor { command:executable,args:vec!["--mode".into(),"rpc".into(),"--offline".into(),"--no-tools".into(),"--no-skills".into(),"--no-prompt-templates".into(),"--model".into(),"task44/native".into(),"--session".into(),session.clone()],cwd:home.clone(),env };
    let processes=Arc::new(Mutex::new(Vec::new())); let captured=processes.clone(); let cleanup=Processes(processes.clone());
    let options=RpcProcessRunnerOptions {
        build_spawn:Some(Arc::new(move |_| descriptor.clone())),model_admission:Some(admission),
        spawn_child:Some(Arc::new(move |descriptor| { let child=Arc::new(RpcChildProcess::spawn(descriptor)); captured.lock().expect("processes").push(child.clone()); child })),..Default::default()
    };
    let runner=maho_omo_task::engine_runners::build_process_runner(options.clone());
    let (sender,receiver)=mpsc::channel(); let launching=runner.clone();
    let worker=std::thread::spawn(move || { let _=sender.send(launching.start(&ManagedStartSpec { task_id:"st_task44_native_provider".into(),prompt:"task44-launch".into(),model:Some("task44/native".into()),cwd:home,..Default::default() })); });
    let result=receiver.recv_timeout(Duration::from_secs(15));
    if result.is_err() { drop(cleanup); worker.join().map_err(|_| "native launch worker panicked")?; return Err("native launch exceeded bounded deadline".into()); }
    worker.join().map_err(|_| "native launch worker panicked")?;
    let handle=result?.map_err(|error| std::io::Error::other(error.to_string()))?;
    let handle_cleanup=HandleCleanup(handle.clone());
    assert_eq!(outcome(handle.clone())?,RunnerOutcome::completed("task44-native-provider"));
    println!("RECEIPT provider launch output observed through production ManagedRunner");
    drop(handle_cleanup);
    for child in processes.lock().expect("processes").iter() { assert!(child.wait_exit_timeout(Duration::from_secs(5)).is_some(),"initial child must exit before reconstruction"); }
    drop(handle);
    let (resumed_sender,resumed_receiver)=mpsc::channel();
    let resumed_worker=std::thread::spawn(move || {
        let runner=maho_omo_task::engine_runners::build_rpc_respawn_runner(options);
        let _=resumed_sender.send(runner.start(&senpi_task::runners::types::RpcRunnerSpec {
            task_id:"st_task44_native_resumed".into(),model:Some("task44/native".into()),resume_session_path:Some(session),..Default::default()
        }));
    });
    let resumed=resumed_receiver.recv_timeout(Duration::from_secs(15));
    if resumed.is_err() { drop(cleanup); resumed_worker.join().map_err(|_| "native resume worker panicked")?; return Err("native resume exceeded bounded deadline".into()); }
    resumed_worker.join().map_err(|_| "native resume worker panicked")?;
    let handle=resumed?.map_err(|error| std::io::Error::other(error.to_string()))?;
    let handle_cleanup=HandleCleanup(handle.clone());
    assert_eq!(processes.lock().expect("processes").len(),2,"resume must construct a second native process");
    command(handle.clone(),|handle| handle.follow_up("task44-resumed").map_err(|error| error.to_string()))?;
    assert_eq!(outcome(handle.clone())?,RunnerOutcome::completed("task44-native-resumed"));
    println!("RECEIPT actual persisted-session switch and resumed output observed");
    command(handle.clone(),|handle| handle.follow_up("task44-error").map_err(|error| error.to_string()))?;
    assert!(matches!(outcome(handle.clone())?,RunnerOutcome::Error { .. }),"real provider rejection must settle as an error");
    println!("RECEIPT deterministic provider rejection settled through production outcome");
    command(handle.clone(),|handle| handle.follow_up("task44-cancel").map_err(|error| error.to_string()))?;
    let mut line=String::new(); receipts.read_line(&mut line)?;
    assert_eq!(line,"TASK44_PROVIDER_CANCEL_HELD\n","abort must follow the actual provider-held request");
    command(handle.clone(),|handle| handle.abort().map_err(|error| error.to_string()))?;
    assert_eq!(outcome(handle.clone())?,RunnerOutcome::Cancelled);
    command(handle.clone(),|handle| handle.follow_up("task44-drop").map_err(|error| error.to_string()))?;
    line.clear(); receipts.read_line(&mut line)?;
    assert_eq!(line,"TASK44_PROVIDER_HELD\n","cleanup must follow the actual provider-held request");
    drop(handle_cleanup);
    for child in processes.lock().expect("processes").iter() { assert!(child.wait_exit_timeout(Duration::from_secs(5)).is_some(),"discard must reap native child"); }
    drop(cleanup);
    println!("RECEIPT cancellation outcome, native process exit and worker joins observed");
    Ok(())
}
