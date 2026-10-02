use maho_codemode::bridge::protocol::BridgeConnectionConfig;
use maho_codemode::kernels::shared::subprocess_contract::*;
use maho_codemode::kernels::shared::subprocess_kernel::SubprocessKernel;

fn options() -> SubprocessKernelOptions {
    SubprocessKernelOptions {
        command: "python3".into(), args: vec!["-u".into(), concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/py/prelude.py").into()],
        cwd: env!("CARGO_MANIFEST_DIR").into(), env: None, session_env: None, session_id: "kernel-test".into(),
        connection: BridgeConnectionConfig { port: 1, token: "test".into(), local_roots: None, artifacts_dir: None, parallel_pool_width: None },
    }
}

#[tokio::test]
async fn real_kernel_persistence_and_reset() {
    let kernel = SubprocessKernel::start(options()).await.unwrap();
    for (code, expected) in [("x = 7\nx", "7"), ("x + 1", "8")] {
        let result = kernel.run(KernelRunInput { cell_id: "c".into(), code: code.into(), timeout_ms: Some(5_000) }, |_| {}).await.unwrap();
        assert_eq!(result["valueRepr"], expected);
    }
    kernel.reset().await.unwrap();
    let result = kernel.run(KernelRunInput { cell_id: "reset".into(), code: "x".into(), timeout_ms: Some(5_000) }, |_| {}).await.unwrap();
    assert_eq!(result["ok"], false);
    kernel.close().await.unwrap();
    assert!(kernel.pid().is_none());
}

#[tokio::test]
async fn real_kernel_timeout_reaps_and_restarts() {
    let kernel = SubprocessKernel::start(options()).await.unwrap();
    let pid = kernel.pid().unwrap();
    let result = kernel.run(KernelRunInput { cell_id: "timeout".into(), code: "while True: pass".into(), timeout_ms: Some(20) }, |_| {}).await.unwrap();
    assert_eq!(result["ok"], false);
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    let result = kernel.run(KernelRunInput { cell_id: "after".into(), code: "1+1".into(), timeout_ms: Some(5_000) }, |_| {}).await.unwrap();
    assert_eq!(result["valueRepr"], "2");
    kernel.close().await.unwrap();
}

#[tokio::test]
async fn queued_cancellation_preserves_active_and_interrupt_restarts_state() {
    use std::sync::Arc;
    let kernel=Arc::new(SubprocessKernel::start(options()).await.unwrap());
    let (started,mut starts)=tokio::sync::mpsc::unbounded_channel();
    let first_kernel=kernel.clone();
    let first=tokio::spawn(async move {first_kernel.run_with_callbacks(KernelRunInput {cell_id:"active".into(),code:"x=41\nwhile True: pass".into(),timeout_ms:None},None,Some(Arc::new(move || {started.send(()).expect("start receiver");}))).await});
    starts.recv().await.unwrap();
    let second=kernel.run_with_callbacks(KernelRunInput {cell_id:"queued".into(),code:"42".into(),timeout_ms:None},None,None);
    tokio::pin!(second);
    let cancel=async {kernel.cancel_queued("queued","test cancellation").await};
    let (result,cancelled)=tokio::join!(&mut second,cancel);
    assert!(cancelled);
    assert_eq!(result.unwrap()["durationMs"],0.0);
    assert_eq!(kernel.queue_snapshot().0.as_deref(),Some("active"));
    let old=kernel.pid().unwrap();
    assert!(!kernel.interrupt("test interruption",Some("active")).await.unwrap());
    assert_eq!(first.await.unwrap().unwrap()["ok"],false);
    assert!(!std::path::Path::new(&format!("/proc/{old}")).exists());
    let result=kernel.run(KernelRunInput {cell_id:"after".into(),code:"x".into(),timeout_ms:Some(5000)},|_|{}).await.unwrap();
    assert_eq!(result["ok"],false);
    kernel.close().await.unwrap();
}
