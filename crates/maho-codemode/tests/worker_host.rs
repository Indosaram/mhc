use maho_ai::utils::abort::AbortController;
use maho_codemode::{bridge::protocol::BridgeConnectionConfig, kernels::{js::{worker_host::{WorkerHost, JavaScriptKernelMode}, inline_worker::create_inline_worker}, shared::runtime_asset::CodemodeRuntimeAssetEnvironment}};

#[tokio::test]
async fn external_worker_and_inline_worker_initialize_and_close() {
    let cwd = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let executable = std::env::current_exe().unwrap();
    for mode in [JavaScriptKernelMode::Worker, JavaScriptKernelMode::Inline] {
        let mut worker = if mode == JavaScriptKernelMode::Worker {
            WorkerHost::spawn(&cwd.join("assets/kernels/js/worker-entry.js"), cwd, 4, mode).unwrap()
        } else {
            create_inline_worker(cwd, 4, &CodemodeRuntimeAssetEnvironment {bun_version:None,executable_path:&executable}).unwrap()
        };
        let pid = worker.pid().unwrap();
        let controller = AbortController::new();
        let connection=BridgeConnectionConfig {port:1,token:"local".into(),local_roots:None,artifacts_dir:None,parallel_pool_width:Some(4)};
        let session_env=maho_codemode::kernels::session_env::SessionEnvironment::from([("PI_SESSION_ID".into(),"context-session".into())]);
        let options=maho_codemode::kernels::js::worker_startup::WorkerStartupOptions {cwd,session_id:"worker-test",parallel_pool_width:4,connection:&connection,generation:1,host_tool_names:&[],foreign_language_names:&[],session_env:Some(&session_env),worker_entry:None,environment:CodemodeRuntimeAssetEnvironment {bun_version:None,executable_path:&executable}};
        tokio::time::timeout(std::time::Duration::from_secs(5), worker.initialize(&options, &controller.signal())).await.unwrap().unwrap();
        worker.post_message(&serde_json::json!({"type":"run","cellId":"cell","code":"process.env.PI_SESSION_ID"})).await.unwrap();
        loop {
            let message = tokio::time::timeout(std::time::Duration::from_secs(5), worker.next_message()).await.unwrap().unwrap();
            if message["type"] == "result" { assert_eq!(message["ok"],true); assert!(message["valueRepr"].as_str().unwrap().contains("context-session")); break; }
        }
        worker.post_message(&serde_json::json!({"type":"run","cellId":"arithmetic","code":"1+1"})).await.unwrap();
        loop {
            let message=tokio::time::timeout(std::time::Duration::from_secs(5),worker.next_message()).await.unwrap().unwrap();
            if message["type"]=="result" {assert_eq!(message["ok"],true);assert_eq!(message["valueRepr"],"2");break;}
        }
        worker.terminate().await.unwrap();
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    }
}

#[tokio::test]
async fn worker_slot_falls_back_and_fences_retired_generation() {
    use maho_codemode::kernels::js::{worker_startup::WorkerStartupOptions, worker_slot::WorkerSlot};
    let cwd = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let executable = std::env::current_exe().unwrap();
    let connection = BridgeConnectionConfig {port:1,token:"local".into(),local_roots:None,artifacts_dir:None,parallel_pool_width:Some(4)};
    let controller = AbortController::new();
    let mut slot = WorkerSlot::default();
    tokio::time::timeout(std::time::Duration::from_secs(5), slot.ensure_ready(WorkerStartupOptions {
        cwd, session_id:"fallback",parallel_pool_width:4,connection:&connection,generation:0,
        host_tool_names:&[],foreign_language_names:&[],session_env:None,worker_entry:Some(std::path::Path::new("/missing/worker.js")),
        environment:CodemodeRuntimeAssetEnvironment {bun_version:None,executable_path:&executable},
    }, &controller.signal())).await.unwrap().unwrap();
    assert_eq!(slot.mode(), JavaScriptKernelMode::Inline);
    assert!(slot.present());
    assert_eq!(slot.generation(), 1);
    slot.retire().await.unwrap();
    assert!(!slot.present());
    assert_eq!(slot.generation(), 2);
}
