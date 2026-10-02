use std::sync::Arc;
use maho_ai::utils::abort::AbortController;
use maho_codemode::bridge::http_server::*;
use serde_json::json;

fn options()->BridgeServerOptions {
    BridgeServerOptions{token:None,body_limit_bytes:None,on_call:Arc::new(|request|Box::pin(async move {assert!(!request.signal.aborted());Ok(json!({"toolName":request.tool_name,"args":request.args}))})),on_emit:Arc::new(|_,signal|Box::pin(async move {assert!(!signal.aborted());Ok(())})),on_completion:Arc::new(|request|Box::pin(async move {Ok(json!({"prompt":request.prompt,"opts":request.opts}))}))}
}

#[tokio::test]
async fn authenticated_call_marshals_handler_reply() {
    let (status,reply)=dispatch_bridge_http_request("POST","/call",Some("Bearer test"),"test",br#"{"callId":"call","toolName":"echo","args":{"q":1}}"#,&options(),AbortController::new().signal()).await;
    assert_eq!(status,200);
    assert_eq!(reply.unwrap(),json!({"ok":true,"value":{"toolName":"echo","args":{"q":1}}}));
}

#[tokio::test]
async fn validates_auth_route_json_and_body_limit() {
    for (method,url,auth,body,status,code) in [
        ("GET","/call",Some("Bearer test"),"{}",404,"not_found"),
        ("POST","/missing",Some("Bearer test"),"{}",404,"not_found"),
        ("POST","/call",Some("Bearer wrong"),"{}",401,"unauthorized"),
        ("POST","/call",None,"{}",401,"unauthorized"),
        ("POST","/call",Some("Bearer test"),"{",400,"invalid_json"),
    ] {
        let (actual,reply)=dispatch_bridge_http_request(method,url,auth,"test",body.as_bytes(),&options(),AbortController::new().signal()).await;
        assert_eq!(actual,status);assert_eq!(reply.unwrap()["error"]["code"],code);
    }
    let mut options=options();options.body_limit_bytes=Some(1);
    let (status,reply)=dispatch_bridge_http_request("POST","/call",Some("Bearer test"),"test",b"{}",&options,AbortController::new().signal()).await;
    assert_eq!(status,413);assert_eq!(reply.unwrap()["error"]["code"],"body_too_large");
}

#[tokio::test]
async fn emit_has_empty_success_body_and_completion_preserves_options() {
    let (status,reply)=dispatch_bridge_http_request("POST","/emit",Some("Bearer test"),"test",br#"{"kind":"text","stream":"stdout","data":"hello"}"#,&options(),AbortController::new().signal()).await;
    assert_eq!(status,204);assert!(reply.is_none());
    let (_,reply)=dispatch_bridge_http_request("POST","/completion",Some("Bearer test"),"test",br#"{"prompt":"hello","opts":{"model":"default"}}"#,&options(),AbortController::new().signal()).await;
    assert_eq!(reply.unwrap()["value"]["opts"]["model"],"default");
}
