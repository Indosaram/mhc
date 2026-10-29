use std::{
    sync::{Arc, Mutex, atomic::{AtomicUsize, Ordering}},
    time::Duration,
};
use axum::{Json, Router, extract::State, http::{HeaderMap, StatusCode}, response::{IntoResponse, Response}, routing::{get, post}};
use maho_ext_mcp::{
    auth::{
        oauth_provider::McpOAuthProvider,
        oauth_refresh::McpRefreshManager,
        token_store::{McpStoredAuth, McpTokenStore},
    },
    errors::McpErrorKind,
    log::McpLogger,
    transport_sdk::{McpClient, McpTransportSpec},
};
use serde_json::{Value, json};

#[derive(Clone, Copy, PartialEq)]
enum Reject {
    Never,
    Always401,
    Always403Scope,
}
#[derive(Clone)]
struct Shared {
    mode: Reject,
    token_hits: Arc<AtomicUsize>,
    bearers: Arc<Mutex<Vec<String>>>,
    base: Arc<Mutex<String>>,
}
async fn mcp(State(state): State<Shared>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    let token = headers.get("authorization").and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).unwrap_or("").to_owned();
    state.bearers.lock().expect("test fixture").push(token.clone());
    match state.mode {
        Reject::Never if token == "fresh" => Json(json!({"jsonrpc":"2.0","id":body["id"],"result":{"ok":true}})).into_response(),
        Reject::Always403Scope => (StatusCode::FORBIDDEN, [("www-authenticate", r#"Bearer error="insufficient_scope", scope="x y""#)], "no").into_response(),
        _ => (StatusCode::UNAUTHORIZED, "no").into_response(),
    }
}
async fn token(State(state): State<Shared>) -> Json<Value> {
    state.token_hits.fetch_add(1, Ordering::SeqCst);
    Json(json!({"access_token":"fresh","refresh_token":"rt-next","token_type":"Bearer","expires_in":3600}))
}
async fn metadata(State(state): State<Shared>) -> Json<Value> {
    let base = state.base.lock().expect("test fixture").clone();
    Json(json!({"issuer":base,"authorization_endpoint":format!("{base}/authorize"),"token_endpoint":format!("{base}/token"),"code_challenge_methods_supported":["S256"]}))
}
struct Rig {
    client: Arc<McpClient>,
    state: Shared,
    task: tokio::task::JoinHandle<()>,
    _root: tempfile::TempDir,
}
impl Drop for Rig {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn rig(mode: Reject) -> Rig {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("test fixture");
    let base = format!("http://{}", listener.local_addr().expect("test fixture"));
    let state = Shared { mode, token_hits: Arc::default(), bearers: Arc::default(), base: Arc::new(Mutex::new(base.clone())) };
    let app = Router::new()
        .route("/mcp", post(mcp))
        .route("/token", post(token))
        .route("/.well-known/oauth-authorization-server", get(metadata))
        .route("/.well-known/oauth-authorization-server/mcp", get(metadata))
        .with_state(state.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("test fixture");
    });
    let root = tempfile::tempdir().expect("test fixture");
    let url = format!("{base}/mcp");
    let store = McpTokenStore::new(root.path(), "reject", &url);
    store.write(McpStoredAuth { access_token: Some("stale".into()), refresh_token: Some("rt".into()), ..Default::default() }).expect("test fixture");
    let mut provider = McpOAuthProvider::new(store);
    provider.require_https = false;
    provider.client_id = Some("fixture-client".into());
    let manager = Arc::new(McpRefreshManager::new(Arc::new(provider), reqwest::Client::new()));
    let logger = Arc::new(Mutex::new(McpLogger::new("reject", root.path(), None).expect("test fixture")));
    let spec = McpTransportSpec::Http { url: url::Url::parse(&url).expect("test fixture"), headers: Default::default() };
    let client = McpClient::materialize("reject", &spec, logger).await.expect("test fixture");
    client.set_auth(manager).await;
    Rig { client, state, task, _root: root }
}
async fn call(rig: &Rig) -> Result<Value, maho_ext_mcp::errors::McpError> {
    tokio::time::timeout(Duration::from_secs(10), rig.client.request("tools/list", json!({}), Duration::from_secs(10))).await.expect("test fixture")
}

#[tokio::test]
async fn a_401_forces_one_refresh_then_retries() {
    let rig = rig(Reject::Never).await;
    assert_eq!(call(&rig).await.expect("test fixture")["ok"], true);
    assert_eq!(rig.state.token_hits.load(Ordering::SeqCst), 1);
    assert_eq!(*rig.state.bearers.lock().expect("test fixture"), vec!["stale".to_owned(), "fresh".into()]);
}

#[tokio::test]
async fn a_second_401_fails_with_the_status_in_the_cause() {
    let rig = rig(Reject::Always401).await;
    let error = call(&rig).await.expect_err("expected an error");
    assert_eq!(error.kind, McpErrorKind::Auth);
    assert_eq!(error.cause.as_ref().expect("test fixture")["status"], 401);
    assert_eq!(rig.state.token_hits.load(Ordering::SeqCst), 1);
    assert_eq!(rig.state.bearers.lock().expect("test fixture").len(), 2);
}

#[tokio::test]
async fn a_403_with_challenge_reports_header_and_required_scopes() {
    let rig = rig(Reject::Always403Scope).await;
    let error = call(&rig).await.expect_err("expected an error");
    assert_eq!(error.kind, McpErrorKind::Protocol);
    let cause = error.cause.expect("test fixture");
    assert_eq!(cause["status"], 403);
    assert_eq!(cause["wwwAuthenticate"], r#"Bearer error="insufficient_scope", scope="x y""#);
    assert_eq!(cause["requiredScopes"], json!(["x", "y"]));
}
