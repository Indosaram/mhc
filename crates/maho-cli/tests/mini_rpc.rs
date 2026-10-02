use std::{collections::HashMap, sync::Arc, time::Duration};
use maho_cli::experimental::mini::shared::{rpc::*, protocol::define_service};
use serde_json::json;
fn peers() -> (RpcPeer, RpcPeer) {
    let (left, right) = tokio::io::duplex(4096);
    let (lr, lw) = tokio::io::split(left); let (rr, rw) = tokio::io::split(right);
    (create_peer(lr, lw, PeerOptions { dead_ms: 0, ..Default::default() }), create_peer(rr, rw, PeerOptions { dead_ms: 0, ..Default::default() }))
}
#[tokio::test]
async fn remote_service_calls_and_unknown_methods_use_real_frames() {
    let (left, right) = peers();
    let echo: Handler = Arc::new(|args, _| Box::pin(async move { Ok(args[0].clone()) }));
    right.provide(define_service("echo"), HashMap::from([("value".to_owned(), echo)]));
    assert_eq!(tokio::time::timeout(Duration::from_secs(1), left.call("echo.value", vec![json!(42)])).await.unwrap().unwrap(), json!(42));
    assert_eq!(left.call("echo.missing", vec![]).await.unwrap_err(), "Unknown method: echo.missing");
    assert_eq!(left.call("absent.method", vec![]).await.unwrap_err(), "No service provides absent.method");
    assert!(left.announced().contains("echo"));
}
#[tokio::test]
async fn addressed_events_preserve_destination() {
    let (left, right) = peers();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let sender = Arc::new(std::sync::Mutex::new(Some(sender)));
    right.on_event(move |service, payload, to| { if let Some(sender) = sender.lock().unwrap().take() { sender.send((service.to_owned(), payload.clone(), to.map(str::to_owned))).unwrap(); } });
    left.emit_to(define_service("lane"), json!({"update":1}), "presentation");
    assert_eq!(tokio::time::timeout(Duration::from_secs(1), receiver).await.unwrap().unwrap(), ("lane".to_owned(), json!({"update":1}), Some("presentation".to_owned())));
}
#[tokio::test]
async fn caller_abort_reaches_inflight_service() {
    let (left, right) = peers();
    let (started, start) = tokio::sync::oneshot::channel();
    let (cancelled, cancellation) = tokio::sync::oneshot::channel();
    let signals = Arc::new(std::sync::Mutex::new(Some((started, cancelled))));
    let handler: Handler = Arc::new(move |_, signal| { let (started, cancelled) = signals.lock().unwrap().take().unwrap(); Box::pin(async move { started.send(()).unwrap(); signal.cancelled().await; cancelled.send(()).unwrap(); Ok(json!(null)) }) });
    right.provide(define_service("lane"), HashMap::from([("wait".to_owned(), handler)]));
    let controller = maho_ai::utils::abort::AbortController::new();
    let call = left.call_with(CallOptions { signal: Some(controller.signal()), timeout_ms: None }, "lane.wait", vec![]);
    let action = async { start.await.unwrap(); controller.abort(None); };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(1), async { tokio::join!(call, action) }).await.unwrap();
    assert_eq!(result.unwrap_err(), "Call cancelled");
    tokio::time::timeout(Duration::from_secs(1), cancellation).await.unwrap().unwrap();
}
