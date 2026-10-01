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
    let mut kernel = SubprocessKernel::start(options()).await.unwrap();
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
    let mut kernel = SubprocessKernel::start(options()).await.unwrap();
    let pid = kernel.pid().unwrap();
    let result = kernel.run(KernelRunInput { cell_id: "timeout".into(), code: "while True: pass".into(), timeout_ms: Some(20) }, |_| {}).await.unwrap();
    assert_eq!(result["ok"], false);
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    let result = kernel.run(KernelRunInput { cell_id: "after".into(), code: "1+1".into(), timeout_ms: Some(5_000) }, |_| {}).await.unwrap();
    assert_eq!(result["valueRepr"], "2");
    kernel.close().await.unwrap();
}
