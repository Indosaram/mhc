use maho_codemode::kernels::{py::transport::*, shared::runtime_asset::CodemodeRuntimeAssetEnvironment};
use std::path::Path;

#[test]
fn shipped_prelude_resolves_without_interpreter_rewrite() {
    let path=resolve_python_prelude_path(PythonPreludePathOptions{local_path:None,environment:CodemodeRuntimeAssetEnvironment{bun_version:None,executable_path:Path::new("/tmp/mhc")}}).unwrap();
    assert!(path.ends_with("assets/kernels/py/prelude.py"));
    assert!(path.is_file());
}

#[test]
fn python_prelude_wrapper_resolves_compiled_sidecar() {
    let root=tempfile::tempdir().unwrap();
    let executable=root.path().join("pi/pi");
    let sidecar=root.path().join("pi/node_modules/@code-yeongyu/senpi-codemode/src/kernels/py/prelude.py");
    std::fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
    std::fs::write(&sidecar,"runner").unwrap();
    let local=root.path().join("$bunfs/prelude.py");
    assert_eq!(resolve_python_prelude_path(PythonPreludePathOptions{local_path:Some(&local),environment:CodemodeRuntimeAssetEnvironment{bun_version:Some("1.4.0"),executable_path:&executable}}).unwrap(),sidecar);
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

#[tokio::test]
async fn cooperative_interrupt_preserves_python_globals() {
    use maho_codemode::bridge::protocol::BridgeConnectionConfig;
    use std::time::Duration;
    let options=PythonTransportOptions { interpreter_path:"python3".into(),session_id:"interrupt".into(),cwd:env!("CARGO_MANIFEST_DIR").into(),connection:BridgeConnectionConfig{port:1,token:"test".into(),local_roots:None,artifacts_dir:None,parallel_pool_width:None},env:None,session_env:None,startup_timeout:Duration::from_secs(5) };
    let mut transport=PythonKernelTransport::start(&options,Path::new(concat!(env!("CARGO_MANIFEST_DIR"),"/assets/kernels/py/prelude.py")),||true).await.unwrap();
    transport.run("busy","import sys,json\nretained = 41\nsys.__stdout__.write(json.dumps({'type':'text','stream':'stdout','data':'started'}) + '\\n')\nsys.__stdout__.flush()\nwhile True: pass",None).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5),async {
        loop {
            let message=transport.next_message().await.unwrap();
            if message["type"]=="text" && message["data"].as_str().is_some_and(|text|text.contains("started")) { break; }
        }
    }).await.unwrap();
    transport.interrupt("test interrupt").await.unwrap();
    let result=tokio::time::timeout(Duration::from_secs(5),async {
        loop {
            let message=transport.next_message().await.unwrap();
            if message["type"]=="result" { break message; }
        }
    }).await.unwrap();
    assert_eq!(result["ok"],false);
    transport.run("after","retained + 1",None).await.unwrap();
    let result=tokio::time::timeout(Duration::from_secs(5),transport.next_message()).await.unwrap().unwrap();
    assert_eq!(result["valueRepr"],"42");
    transport.close().await.unwrap();
}
