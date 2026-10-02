use std::sync::Arc;
use maho_ai::utils::abort::AbortController;
use maho_codemode::bridge::http_server::*;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn open_call(server: &BridgeServerHandle) -> tokio::net::TcpStream {
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", server.port)).await.expect("bridge connection");
    let body = r#"{"callId":"call","toolName":"echo","args":{}}"#;
    stream.write_all(format!("POST /call HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", server.token, body.len()).as_bytes()).await.expect("write call request");
    stream
}

#[tokio::test]
async fn live_listener_returns_reply_and_releases_port_idempotently() {
    let server = start_bridge_server(options()).await.unwrap();
    let mut stream = open_call(&server).await;
    let mut reply = String::new();
    tokio::time::timeout(std::time::Duration::from_secs(3), stream.read_to_string(&mut reply)).await.unwrap().unwrap();
    assert!(reply.starts_with("HTTP/1.1 200"));
    let body: serde_json::Value = serde_json::from_str(reply.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body["value"]["toolName"], "echo");
    server.close().await;
    server.close().await;
    assert!(tokio::net::TcpStream::connect(("127.0.0.1", server.port)).await.is_err());
}

#[tokio::test]
async fn disconnect_aborts_inflight_call() {
    let (signal_tx, mut signals) = tokio::sync::mpsc::unbounded_channel();
    let mut options = options();
    options.on_call = Arc::new(move |request| {
        signal_tx.send(request.signal.clone()).unwrap();
        Box::pin(async move { request.signal.cancelled().await; Ok(json!(null)) })
    });
    let server = start_bridge_server(options).await.unwrap();
    let stream = open_call(&server).await;
    let signal = tokio::time::timeout(std::time::Duration::from_secs(3), signals.recv()).await.unwrap().unwrap();
    assert!(!signal.aborted());
    drop(stream);
    tokio::time::timeout(std::time::Duration::from_secs(3), signal.cancelled()).await.unwrap();
    server.close().await;
}

#[tokio::test]
async fn close_aborts_active_calls_and_closes_idle_sockets() {
    let (signal_tx, mut signals) = tokio::sync::mpsc::unbounded_channel();
    let mut options = options();
    options.on_call = Arc::new(move |request| {
        signal_tx.send(request.signal.clone()).unwrap();
        Box::pin(async move { request.signal.cancelled().await; Ok(json!(null)) })
    });
    let server = start_bridge_server(options).await.unwrap();
    let _active = open_call(&server).await;
    let signal = tokio::time::timeout(std::time::Duration::from_secs(3), signals.recv()).await.unwrap().unwrap();
    let mut idle = tokio::net::TcpStream::connect(("127.0.0.1", server.port)).await.unwrap();
    server.close().await;
    assert!(signal.aborted());
    let mut byte = [0];
    let closed = tokio::time::timeout(std::time::Duration::from_secs(3), idle.read(&mut byte)).await.unwrap();
    assert!(matches!(closed, Ok(0)) || closed.is_err_and(|error| error.kind() == std::io::ErrorKind::ConnectionReset));
}

fn options()->BridgeServerOptions {
    BridgeServerOptions{token:None,body_limit_bytes:None,on_call:Arc::new(|request|Box::pin(async move {assert!(!request.signal.aborted());Ok(json!({"toolName":request.tool_name,"args":request.args}))})),on_emit:Arc::new(|_,signal|Box::pin(async move {assert!(!signal.aborted());Ok(())})),on_completion:Arc::new(|request|Box::pin(async move {Ok(json!({"prompt":request.prompt,"opts":request.opts}))}))}
}

async fn post(server: &BridgeServerHandle, route: &str, token: &str, body: &str) -> (u16, Option<serde_json::Value>) {
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", server.port)).await.expect("bridge connection");
    stream.write_all(format!("POST {route} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.expect("write request");
    let mut reply = String::new();
    tokio::time::timeout(std::time::Duration::from_secs(3), stream.read_to_string(&mut reply)).await.expect("response deadline").expect("read response");
    let (headers, body) = reply.split_once("\r\n\r\n").expect("response headers");
    let status = headers.split_whitespace().nth(1).expect("status code").parse().expect("numeric status");
    (status, if body.is_empty() { None } else { Some(serde_json::from_str(body).expect("JSON response")) })
}

#[tokio::test]
async fn live_transport_validation_and_auth_do_not_leak_token() {
    let mut options = options();
    options.body_limit_bytes = Some(16);
    let server = start_bridge_server(options).await.unwrap();
    for (route, token, body, status, code) in [
        ("/call", "wrong", "{}", 401, "unauthorized"),
        ("/missing", server.token.as_str(), "{}", 404, "not_found"),
        ("/call", server.token.as_str(), "{", 400, "invalid_json"),
        ("/call", server.token.as_str(), "{\"payload\":\"1234567890\"}", 413, "body_too_large"),
    ] {
        let (actual, reply) = post(&server, route, token, body).await;
        assert_eq!(actual, status);
        let reply = reply.unwrap();
        assert_eq!(reply["error"]["code"], code);
        assert!(!reply.to_string().contains(&server.token));
    }
    server.close().await;
}

#[tokio::test]
async fn handler_failure_remains_a_protocol_reply() {
    let mut options = options();
    options.on_call = Arc::new(|_| Box::pin(async { Err(json!({"name":"Error","message":"tool failed","stack":"trace"})) }));
    let server = start_bridge_server(options).await.unwrap();
    let (status, reply) = post(&server, "/call", &server.token, r#"{"callId":"call","toolName":"echo","args":{}}"#).await;
    assert_eq!(status, 200);
    assert_eq!(reply.unwrap(), json!({"ok":false,"error":{"name":"Error","message":"tool failed","stack":"trace"}}));
    server.close().await;
}

#[tokio::test]
async fn completion_signal_stays_live_on_normal_response() {
    let (signal_tx, mut signals) = tokio::sync::mpsc::unbounded_channel();
    let mut options = options();
    options.on_completion = Arc::new(move |request| {
        signal_tx.send(request.signal.clone()).unwrap();
        Box::pin(async move { assert!(!request.signal.aborted()); Ok(json!("alive")) })
    });
    let server = start_bridge_server(options).await.unwrap();
    let (_, reply) = post(&server, "/completion", &server.token, r#"{"prompt":"hello"}"#).await;
    assert_eq!(reply.unwrap()["value"], "alive");
    assert!(!signals.recv().await.unwrap().aborted());
    server.close().await;
}

#[tokio::test]
async fn concurrent_calls_settle_and_port_can_be_rebound() {
    let server = start_bridge_server(options()).await.unwrap();
    let mut calls = Vec::new();
    for index in 0..10 {
        let port = server.port;
        let token = server.token.clone();
        calls.push(tokio::spawn(async move {
            let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            let body = json!({"callId":format!("call-{index}"),"toolName":"echo","args":{"index":index}}).to_string();
            stream.write_all(format!("POST /call HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            let mut reply = String::new();
            tokio::time::timeout(std::time::Duration::from_secs(3), stream.read_to_string(&mut reply)).await.unwrap().unwrap();
            let value: serde_json::Value = serde_json::from_str(reply.split_once("\r\n\r\n").unwrap().1).unwrap();
            assert_eq!(value["value"]["args"]["index"], index);
        }));
    }
    for call in calls { call.await.unwrap(); }
    server.close().await;
    let _probe = tokio::net::TcpListener::bind(("127.0.0.1", server.port)).await.unwrap();
}

#[tokio::test]
async fn authenticated_call_marshals_handler_reply() {
    let (status,reply)=dispatch_bridge_http_request("POST","/call",Some("Bearer test"),"test",br#"{"callId":"call","toolName":"echo","args":{"q":1}}"#,&options(),AbortController::new().signal()).await;
    assert_eq!(status,200);
    assert_eq!(reply.unwrap(),json!({"ok":true,"value":{"toolName":"echo","args":{"q":1}}}));
}

#[tokio::test]
async fn routes_use_whatwg_path_normalization() {
    for route in ["/unused/../call?query=1", "/unused/%2e%2e/call", "http://127.0.0.1/call", "/call#fragment"] {
        let (status,reply)=dispatch_bridge_http_request("POST",route,Some("Bearer test"),"test",br#"{"callId":"normalized","toolName":"echo","args":{}}"#,&options(),AbortController::new().signal()).await;
        assert_eq!(status,200,"{route}");
        assert_eq!(reply.unwrap()["ok"],true);
    }
    let server=start_bridge_server(options()).await.unwrap();
    let response=post(&server,"/unused/%2e%2e/call",&server.token,r#"{"callId":"live-normalized","toolName":"echo","args":{}}"#).await;
    server.close().await;
    assert_eq!(response.0,200);
    assert_eq!(response.1.unwrap()["ok"],true);
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

#[tokio::test]
async fn emit_handler_receives_only_protocol_fields() {
    let mut options = options();
    options.on_emit = Arc::new(|event, _| Box::pin(async move {
        assert_eq!(event, json!({"kind":"phase","title":"working"}));
        Ok(())
    }));
    let (status, reply) = dispatch_bridge_http_request("POST", "/emit", Some("Bearer test"), "test", br#"{"kind":"phase","title":"working","extra":"discard"}"#, &options, AbortController::new().signal()).await;
    assert_eq!(status, 204);
    assert!(reply.is_none());
}
