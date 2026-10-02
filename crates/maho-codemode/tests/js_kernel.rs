use maho_codemode::kernels::js::context_manager::JavaScriptKernel;
use maho_codemode::kernels::shared::subprocess_contract::KernelRunInput;
use std::path::Path;

#[tokio::test]
async fn real_bun_persistent_kernel() {
    let kernel = JavaScriptKernel::start(Path::new(env!("CARGO_MANIFEST_DIR")), "bun-test", 4, None).await.unwrap();
    for (code, expected) in [("1+1", "2"), ("var x=7; x", "7"), ("x+1", "8")] {
        let result = kernel.run(KernelRunInput { cell_id: "c".into(), code: code.into(), timeout_ms: Some(5_000) }, |_| {}).await.unwrap();
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(result["valueRepr"], expected);
    }
    kernel.close().await.unwrap();
}

#[tokio::test]
async fn real_bun_timeout_kills_worker_and_recovers() {
    let kernel = JavaScriptKernel::start(Path::new(env!("CARGO_MANIFEST_DIR")), "bun-timeout", 4, None).await.unwrap();
    let old = kernel.pid().unwrap();
    let result = kernel.run(KernelRunInput { cell_id: "runaway".into(), code: "while(true) {}".into(), timeout_ms: Some(20) }, |_| {}).await.unwrap();
    assert_eq!(result["ok"], false);
    assert!(!Path::new(&format!("/proc/{old}")).exists());
    let result = kernel.run(KernelRunInput { cell_id: "after".into(), code: "1+1".into(), timeout_ms: Some(5_000) }, |_| {}).await.unwrap();
    assert_eq!(result["valueRepr"], "2");
    kernel.close().await.unwrap();
}

#[tokio::test]
async fn callback_run_admission_preserves_worker_output_and_queue_snapshot() {
    use std::sync::{Arc,Mutex};
    let kernel=JavaScriptKernel::start(Path::new(env!("CARGO_MANIFEST_DIR")),"bun-callback",4,None).await.unwrap();
    let output=Arc::new(Mutex::new(String::new()));
    let observed=output.clone();
    let (sender,mut started)=tokio::sync::mpsc::unbounded_channel();
    let result=kernel.run_with_callbacks(KernelRunInput {cell_id:"callback".into(),code:"print('visible'); 42".into(),timeout_ms:Some(5000)},Some(Arc::new(move |message| {if let Some(text)=message["data"].as_str() {observed.lock().expect("output lock").push_str(text);}})),Some(Arc::new(move || {sender.send(()).expect("start receiver");}))).await.unwrap();
    started.recv().await.unwrap();
    assert_eq!(result["valueRepr"],"42");
    assert!(output.lock().unwrap().contains("visible"));
    assert!(!kernel.cancel_queued("missing","cancel").await);
    assert_eq!(kernel.queue_snapshot(),(None,vec![]));
    kernel.close().await.unwrap();
}

#[tokio::test]
async fn pulled_host_call_reply_resumes_real_worker() {
    let kernel=JavaScriptKernel::start(Path::new(env!("CARGO_MANIFEST_DIR")),"bun-pull",4,None).await.unwrap();
    let run=kernel.run_with_callbacks(KernelRunInput {cell_id:"pull".into(),code:"await tool.echo({value:42})".into(),timeout_ms:Some(5000)},None,None);
    let reply=async {
        let call=tokio::time::timeout(std::time::Duration::from_secs(3),kernel.next_tool_call()).await.unwrap().unwrap();
        assert_eq!(call["toolName"],"echo");
        assert_eq!(call["args"]["value"],42);
        kernel.deliver_tool_reply(serde_json::json!({"type":"tool-reply","callId":call["callId"],"ok":true,"value":42})).unwrap();
    };
    let (result,())=tokio::join!(run,reply);
    assert_eq!(result.unwrap()["valueRepr"],"42");
    kernel.close().await.unwrap();
}

#[tokio::test]
async fn cooperative_interrupt_preserves_live_worker_globals() {
    let kernel=JavaScriptKernel::start(Path::new(env!("CARGO_MANIFEST_DIR")),"bun-retained",4,None).await.unwrap();
    let pid=kernel.pid().unwrap();
    let run=kernel.run_with_callbacks(KernelRunInput {cell_id:"parked".into(),code:"var retained=41; await tool.park({})".into(),timeout_ms:None},None,None);
    let stop=async {
        tokio::time::timeout(std::time::Duration::from_secs(3),kernel.next_tool_call()).await.unwrap().unwrap();
        kernel.interrupt("test stop",Some("parked")).await.unwrap()
    };
    let (result,retained)=tokio::join!(run,stop);
    let after=kernel.run(KernelRunInput {cell_id:"retained".into(),code:"retained+1".into(),timeout_ms:Some(5000)},|_|{}).await;
    let same_pid=kernel.pid()==Some(pid);
    kernel.close().await.unwrap();
    assert!(!result.unwrap()["ok"].as_bool().unwrap());
    assert!(retained,"bridge wait should settle cooperatively without losing globals");
    assert!(same_pid);
    assert_eq!(after.unwrap()["valueRepr"],"42");
}
