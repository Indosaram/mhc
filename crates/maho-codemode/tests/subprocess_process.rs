use maho_codemode::kernels::shared::subprocess_process::*;
use std::path::Path;
use std::time::Duration;
use serde_json::json;

#[tokio::test]
async fn real_python_framed_transport() {
    let env = std::env::vars().collect();
    let mut process = SubprocessProcess::spawn("python3", &["-u".into(), concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/py/prelude.py").into()], Path::new(env!("CARGO_MANIFEST_DIR")), &env).unwrap();
    process.send(&json!({"type":"init","sessionId":"transport","connection":{"port":1,"token":"test"}})).await.unwrap();
    let ready = tokio::time::timeout(Duration::from_secs(5), process.next_message()).await.unwrap().unwrap();
    assert_eq!(ready["type"], "ready");
    process.send(&json!({"type":"run","cellId":"c","code":"1+1"})).await.unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), process.next_message()).await.unwrap().unwrap();
    assert_eq!(result["ok"], true);
    assert_eq!(result["valueRepr"], "2");
    process.shutdown(Some(&json!({"type":"close"}))).await.unwrap();
    assert!(process.is_retiring());
    assert!(!process.send(&json!({"type":"close"})).await.unwrap());
}

#[tokio::test]
async fn missing_executable_reports_spawn_error() {
    let env = std::env::vars().collect();
    assert!(matches!(SubprocessProcess::spawn("/missing/maho-kernel", &[], Path::new("/tmp"), &env), Err(ProcessError::Io(_))));
}

#[tokio::test]
async fn stderr_preserves_unterminated_chunk_while_process_is_live() {
    let env=std::env::vars().collect();
    let mut process=SubprocessProcess::spawn("python3", &["-u".into(),"-c".into(),"import sys; sys.stderr.write('progress\\rnext'); sys.stderr.flush(); sys.stdin.readline()".into()],Path::new("/tmp"),&env).unwrap();
    let observed=tokio::time::timeout(Duration::from_secs(2),process.next_message()).await;
    process.terminate("TERM",Duration::from_millis(1500)).await.unwrap();
    let message=observed.expect("unterminated stderr must arrive before exit").unwrap();
    assert_eq!(message["type"],"text");
    assert_eq!(message["stream"],"stderr");
    assert_eq!(message["data"],"progress\rnext");
}

#[tokio::test]
async fn termination_retires_process_group() {
    let env = std::env::vars().collect();
    let mut process = SubprocessProcess::spawn("python3", &["-u".into(), "-c".into(), "import signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); print('{\"type\":\"ready\"}', flush=True); time.sleep(1000)".into()], Path::new("/tmp"), &env).unwrap();
    let pid = process.pid().unwrap();
    assert_eq!(tokio::time::timeout(Duration::from_secs(5), process.next_message()).await.unwrap().unwrap()["type"], "ready");
    process.terminate("TERM", Duration::from_millis(10)).await.unwrap();
    assert!(!Path::new(&format!("/proc/{pid}")).exists());
}
