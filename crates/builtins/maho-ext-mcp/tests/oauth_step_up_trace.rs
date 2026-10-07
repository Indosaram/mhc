//! SC-U2 401/403 `WWW-Authenticate` transport trace — diagnostic history, not the step-up
//! acceptance (which lives in `tests/oauth_step_up.rs`).
//!
//! It drives the real HTTP consumer (`maho_ext_mcp::transport_sdk::McpClient`) against a loopback
//! MCP server that answers the first two tool calls with the pinned challenge shapes and records
//! every request it receives. With no refresh manager set the native issues one request per call,
//! so the trace observes the exact `WWW-Authenticate` contract without asserting step-up behavior;
//! the earlier gap-asserting revision is superseded.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::header;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};

use maho_ext_mcp::auth::step_up::{is_step_up_required, parse_www_authenticate};
use maho_ext_mcp::errors::McpErrorKind;
use maho_ext_mcp::log::McpLogger;
use maho_ext_mcp::transport_sdk::{McpClient, McpTransportSpec};

const CHALLENGE_401: &str =
    "Bearer error=\"invalid_token\", error_description=\"access token expired\", scope=\"mcp:read\"";
const CHALLENGE_403: &str =
    "Bearer error=\"insufficient_scope\", scope=\"mcp:write mcp:admin\"";

#[derive(Debug, Clone, PartialEq, Eq)]
struct RecordedCall {
    method: String,
    authorization: Option<String>,
    emitted_challenge: Option<String>,
}

#[derive(Clone, Default)]
struct TraceState {
    calls: Arc<Mutex<Vec<RecordedCall>>>,
    next: Arc<AtomicUsize>,
}

impl TraceState {
    fn record(&self, method: String, authorization: Option<String>, challenge: Option<&str>) {
        self.calls.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(RecordedCall { method, authorization, emitted_challenge: challenge.map(str::to_owned) });
    }
    fn tool_calls(&self) -> Vec<RecordedCall> {
        self.calls.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter().filter(|call| call.method == "tools/call").cloned().collect()
    }
}

async fn mcp(State(state): State<TraceState>, headers: HeaderMap, body: String) -> Response {
    let value: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let method = value.get("method").and_then(Value::as_str).unwrap_or("").to_owned();
    let authorization = headers.get("authorization").and_then(|value| value.to_str().ok()).map(str::to_owned);
    if value.get("id").is_none() {
        state.record(method, authorization, None);
        return StatusCode::ACCEPTED.into_response();
    }
    if method == "initialize" {
        state.record(method, authorization, None);
        return Json(json!({"jsonrpc":"2.0","id":value["id"],"result":{"protocolVersion":"2025-11-25","capabilities":{},"serverInfo":{"name":"step-up-trace","version":"1"}}})).into_response();
    }
    let (status, challenge) = match state.next.fetch_add(1, Ordering::SeqCst) {
        0 => (StatusCode::UNAUTHORIZED, Some(CHALLENGE_401)),
        1 => (StatusCode::FORBIDDEN, Some(CHALLENGE_403)),
        _ => (StatusCode::OK, None),
    };
    state.record(method, authorization, challenge);
    match challenge {
        Some(challenge) => Response::builder().status(status).header(header::WWW_AUTHENTICATE, challenge).header(header::CONTENT_TYPE, "application/json").body(Body::from(json!({"error":"challenge"}).to_string())).unwrap(),
        None => Json(json!({"jsonrpc":"2.0","id":value["id"],"result":{"content":[{"type":"text","text":"ok"}],"isError":false}})).into_response(),
    }
}

async fn spawn_trace_server() -> (std::net::SocketAddr, tokio::sync::oneshot::Sender<()>, tokio::task::JoinHandle<()>, TraceState) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let state = TraceState::default();
    let app = Router::new().route("/mcp", post(mcp)).with_state(state.clone());
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move { axum::serve(listener, app).with_graceful_shutdown(async { let _ = stopped.await; }).await.unwrap(); });
    (address, stop, server, state)
}

async fn shutdown(stop: tokio::sync::oneshot::Sender<()>, server: tokio::task::JoinHandle<()>) {
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), server).await.unwrap().unwrap();
}

#[tokio::test]
async fn transport_trace_records_the_pinned_401_and_403_challenges() {
    let (address, stop, server, state) = spawn_trace_server().await;
    let root = tempfile::tempdir().unwrap();
    let logger = Arc::new(Mutex::new(McpLogger::new("trace", root.path(), None).unwrap()));
    let spec = McpTransportSpec::Http { url: format!("http://{address}/mcp").parse().unwrap(), headers: Default::default() };
    let client = McpClient::materialize("trace", &spec, logger).await.unwrap();
    client.initialize(Duration::from_secs(2)).await.unwrap();

    let unauthorized = client.request("tools/call", json!({"name":"tool","arguments":{}}), Duration::from_secs(2)).await.unwrap_err();
    let _forbidden = client.request("tools/call", json!({"name":"tool","arguments":{}}), Duration::from_secs(2)).await.unwrap_err();
    assert_eq!(unauthorized.kind, McpErrorKind::Auth);

    let calls = state.tool_calls();
    assert_eq!(calls.len(), 2, "no refresh manager is set, so each call reaches the server exactly once");
    assert_eq!(calls[0].emitted_challenge.as_deref(), Some(CHALLENGE_401));
    assert_eq!(calls[1].emitted_challenge.as_deref(), Some(CHALLENGE_403));
    assert!(calls.iter().all(|call| call.authorization.is_none()));

    assert_eq!(parse_www_authenticate(CHALLENGE_401).unwrap().required_scopes, vec!["mcp:read".to_owned()]);
    assert_eq!(parse_www_authenticate(CHALLENGE_403).unwrap().required_scopes, vec!["mcp:write".to_owned(), "mcp:admin".to_owned()]);
    assert!(is_step_up_required(403, Some(CHALLENGE_403)).is_some());
    assert!(is_step_up_required(401, Some(CHALLENGE_401)).is_none());

    client.close().await.unwrap();
    drop(client);
    shutdown(stop, server).await;
}
