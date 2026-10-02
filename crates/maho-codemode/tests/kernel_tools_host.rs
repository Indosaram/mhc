use std::sync::Arc;
use serde_json::json;
use maho_ext_api::AbortSignal;
use maho_codemode::kernels::js::{kernel_tools_host::KernelToolHostPump, kernel_tools_types::*, kernel_tools_errors::KernelToolErrorCode};

#[tokio::test]
async fn describe_reply_and_invoke_scope_roundtrip() {
    let (post, mut frames) = tokio::sync::mpsc::unbounded_channel();
    let pump = Arc::new(KernelToolHostPump::new(Arc::new(move |frame| { post.send(frame).expect("posted frame"); }), Arc::new(||true)));
    let describe_pump = pump.clone();
    let describe = tokio::spawn(async move { describe_pump.describe(&["echo".into()]).await });
    let frame = frames.recv().await.unwrap();
    assert_eq!(frame["names"], json!(["echo"]));
    assert!(pump.consume(json!({"type":"kernel-tool-describe-reply","requestId":frame["requestId"],"ok":true,"results":[]})));
    assert_eq!(describe.await.unwrap().unwrap(), json!({"results":[]}));
    let mut events = pump.events();
    let invoke_pump = pump.clone();
    let invoke = tokio::spawn(async move { invoke_pump.invoke(KernelToolsInvokeRequest {name:"echo".into(),kernel_generation:1,definition_revision:2,args:json!({}),call_id:"call".into()}, KernelToolsInvokeOptions {signal:None,scope:Some(KernelToolsInvokeScope {allow:Some(vec!["read".into()]),deny:None})}).await });
    let frame = frames.recv().await.unwrap();
    assert_eq!(events.recv().await.unwrap(), "nestedInvoke");
    assert_eq!(frame["scope"], json!({"tools":{"allow":["read"]}}));
    pump.consume(json!({"type":"kernel-tool-invoke-reply","requestId":frame["requestId"],"ok":true,"value":42}));
    assert_eq!(invoke.await.unwrap().unwrap(), 42);
}

#[tokio::test]
async fn cancellation_posts_cancel_and_unknown_codes_normalize() {
    let (post, mut frames) = tokio::sync::mpsc::unbounded_channel();
    let pump = Arc::new(KernelToolHostPump::new(Arc::new(move |frame| { post.send(frame).expect("posted frame"); }), Arc::new(||true)));
    let controller = AbortSignal::default();
    let signal = controller.clone();
    let invoke_pump = pump.clone();
    let invoke = tokio::spawn(async move { invoke_pump.invoke(KernelToolsInvokeRequest {name:"echo".into(),kernel_generation:1,definition_revision:2,args:json!({}),call_id:"call".into()}, KernelToolsInvokeOptions {signal:Some(signal),scope:None}).await });
    let request = frames.recv().await.unwrap();
    assert!(request.get("scope").is_none());
    controller.abort();
    assert_eq!(frames.recv().await.unwrap(), json!({"type":"kernel-tool-cancel","requestId":request["requestId"]}));
    assert_eq!(invoke.await.unwrap().unwrap_err().code, KernelToolErrorCode::KernelToolStale);
    let describe_pump = pump.clone();
    let describe = tokio::spawn(async move { describe_pump.describe(&[]).await });
    let request = frames.recv().await.unwrap();
    pump.consume(json!({"type":"kernel-tool-describe-reply","requestId":request["requestId"],"ok":false,"error":{"code":"future_error","message":"failure"}}));
    assert_eq!(describe.await.unwrap().unwrap_err().code, KernelToolErrorCode::KernelToolFailed);
}
