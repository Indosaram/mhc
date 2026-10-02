use maho_codemode::kernels::{py::transport::*, shared::runtime_asset::CodemodeRuntimeAssetEnvironment};
use std::path::Path;

#[test]
fn shipped_prelude_resolves_without_interpreter_rewrite() {
    let path=resolve_python_prelude_path(PythonPreludePathOptions{local_path:None,environment:CodemodeRuntimeAssetEnvironment{bun_version:None,executable_path:Path::new("/tmp/mhc")}}).unwrap();
    assert!(path.ends_with("assets/kernels/py/prelude.py"));
    assert!(path.is_file());
}

#[test]
fn failed_result_omits_empty_stack_and_preserves_cell_identity() {
    let result=failed_python_result("cell","failed",Some(""));
    assert_eq!(result["cellId"],"cell");
    assert_eq!(result["ok"],false);
    assert!(result["error"].get("stack").is_none());
    assert_eq!(failed_python_result("cell","failed",Some("trace"))["error"]["stack"],"trace");
}

#[tokio::test]
async fn real_python_transport_runs_and_reaps_on_close() {
    use maho_codemode::bridge::protocol::BridgeConnectionConfig;
    use std::time::Duration;
    let options=PythonTransportOptions { interpreter_path:"python3".into(),session_id:"transport".into(),cwd:env!("CARGO_MANIFEST_DIR").into(),connection:BridgeConnectionConfig{port:1,token:"test".into(),local_roots:None,artifacts_dir:None,parallel_pool_width:None},env:None,session_env:None,startup_timeout:Duration::from_secs(5) };
    let mut transport=PythonKernelTransport::start(&options,Path::new(concat!(env!("CARGO_MANIFEST_DIR"),"/assets/kernels/py/prelude.py")),||true).await.unwrap();
    let pid=transport.pid().unwrap();
    transport.run("cell","1+1",None).await.unwrap();
    let result=tokio::time::timeout(Duration::from_secs(5),transport.next_message()).await.unwrap().unwrap();
    assert_eq!(result["cellId"],"cell");
    assert_eq!(result["valueRepr"],"2");
    transport.close().await.unwrap();
    assert!(!Path::new(&format!("/proc/{pid}")).exists());
}
