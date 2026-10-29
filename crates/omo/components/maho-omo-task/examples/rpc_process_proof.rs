use std::{collections::BTreeMap, sync::{Arc, Mutex, mpsc}, time::Duration};
use senpi_task::runners::rpc::{process::{RpcChildProcess, RpcSpawnDescriptor}, protocol_client::{RpcClientPort, RpcProtocolClient, RpcProtocolClientOptions}, terminate::terminate_rpc_child};
use senpi_task::runners::types::TerminateOptions;

struct ProcessCleanup(Arc<RpcChildProcess>);
impl Drop for ProcessCleanup {
    fn drop(&mut self) {
        if let Err(error) = terminate_rpc_child(&self.0, TerminateOptions { sigkill_delay_ms: Some(100) }) {
            eprintln!("RPC proof cleanup failed: {error}");
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let executable = std::env::args().nth(1).ok_or("native mhc path required")?;
    let root = tempfile::tempdir()?;
    let descriptor = RpcSpawnDescriptor {
        command: executable,
        args: ["--mode", "rpc", "--offline", "--no-session", "--model", "openai/gpt-4o"].map(str::to_owned).into(),
        cwd: root.path().to_string_lossy().into_owned(),
        env: BTreeMap::from([
            ("HOME".into(), root.path().to_string_lossy().into_owned()),
            ("MAHO_CODING_AGENT_DIR".into(), root.path().join("agent").to_string_lossy().into_owned()),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ]),
    };
    let child = Arc::new(RpcChildProcess::spawn(&descriptor));
    let cleanup = ProcessCleanup(child.clone());
    let client = Arc::new(RpcProtocolClient::new(child.clone(), RpcProtocolClientOptions::default()));
    let (sender, receiver) = mpsc::channel();
    let query = client.clone();
    let worker = std::thread::spawn(move || {
        if let Err(error) = sender.send(query.send(serde_json::json!({"type":"get_messages"}))) {
            eprintln!("RPC proof query delivery failed: {error}");
        }
    });
    let response = receiver.recv_timeout(Duration::from_secs(10));
    drop(cleanup);
    worker.join().map_err(|_| "RPC query worker panicked")?;
    let response = response??;
    assert_eq!(response["command"], "get_messages");
    assert_eq!(response["success"], true);
    assert_eq!(response["data"]["messages"], serde_json::json!([]));
    assert!(child.exit_status().is_some());
    client.detach();
    println!("PASS real native RPC executable query through production task protocol client");

    let spawned = Arc::new(Mutex::new(None::<Arc<RpcChildProcess>>));
    let captured = spawned.clone();
    let runner = maho_omo_task::engine_runners::build_process_runner(senpi_task::runners::rpc_process::RpcProcessRunnerOptions {
        build_spawn: Some(Arc::new(move |_| descriptor.clone())),
        spawn_child: Some(Arc::new(move |descriptor| {
            let child = Arc::new(RpcChildProcess::spawn(descriptor));
            *captured.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(child.clone());
            child
        })),
        model_admission: Some(Arc::new(|_| Ok(()))),
        ..Default::default()
    });
    let (sender, receiver) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let result = runner.start(&senpi_task::manager::types::ManagedStartSpec { task_id:"st_rpc_proof".into(), prompt:"offline admission proof".into(), ..Default::default() });
        if let Err(error) = sender.send(result) { eprintln!("RPC runner proof delivery failed: {error}"); }
    });
    let result = receiver.recv_timeout(Duration::from_secs(10));
    let child = spawned.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone().ok_or("RPC runner did not spawn")?;
    let cleanup = ProcessCleanup(child.clone());
    drop(cleanup);
    worker.join().map_err(|_| "RPC runner worker panicked")?;
    let result = result?;
    if let Ok(handle) = &result { senpi_task::manager::child_handle::discard_managed_handle(handle.as_ref())?; }
    assert!(matches!(result, Err(senpi_task::manager::types::ManagedRunnerError::Runner(failure)) if failure.kind == senpi_task::runners::RunnerFailureKind::ChildPromptFailed));
    assert!(child.exit_status().is_some());
    drop(client); drop(child); drop(spawned);
    root.close()?;
    println!("PASS production process runner native offline prompt rejection and process cleanup");
    println!("cleanup: both owned process groups terminated and reaped, workers joined, temporary HOME removed");
    println!("OPEN: successful provider-backed task execution and exact-candidate SDK host integration");
    Ok(())
}
