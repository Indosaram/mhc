use maho_codemode::kernels::js::context_manager::JavaScriptKernel;
use maho_codemode::kernels::shared::subprocess_contract::KernelRunInput;
use std::path::Path;

#[tokio::test]
async fn real_bun_persistent_kernel() {
    let mut kernel = JavaScriptKernel::start(Path::new(env!("CARGO_MANIFEST_DIR")), "bun-test", 4, None).await.unwrap();
    for (code, expected) in [("1+1", "2"), ("var x=7; x", "7"), ("x+1", "8")] {
        let result = kernel.run(KernelRunInput { cell_id: "c".into(), code: code.into(), timeout_ms: Some(5_000) }, |_| {}).await.unwrap();
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(result["valueRepr"], expected);
    }
    kernel.close().await.unwrap();
}

#[tokio::test]
async fn real_bun_timeout_kills_worker_and_recovers() {
    let mut kernel = JavaScriptKernel::start(Path::new(env!("CARGO_MANIFEST_DIR")), "bun-timeout", 4, None).await.unwrap();
    let old = kernel.pid().unwrap();
    let result = kernel.run(KernelRunInput { cell_id: "runaway".into(), code: "while(true) {}".into(), timeout_ms: Some(20) }, |_| {}).await.unwrap();
    assert_eq!(result["ok"], false);
    assert!(!Path::new(&format!("/proc/{old}")).exists());
    let result = kernel.run(KernelRunInput { cell_id: "after".into(), code: "1+1".into(), timeout_ms: Some(5_000) }, |_| {}).await.unwrap();
    assert_eq!(result["valueRepr"], "2");
    kernel.close().await.unwrap();
}
