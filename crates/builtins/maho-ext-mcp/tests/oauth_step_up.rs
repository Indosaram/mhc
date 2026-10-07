//! SC-U2 step-up regression tests: the pinned 401/403 `WWW-Authenticate` handling
//! (`mcp-oauth/step-up.ts` plus `skill-mcp-manager/oauth-handler.ts`) driven through the real HTTP
//! consumer (`maho_ext_mcp::transport_sdk::McpClient`). Each fails if the challenge read, the
//! scope escalation or the post-request refresh/retry is absent.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::header;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use serde_json::{json, Value};

use maho_ext_mcp::auth::oauth_provider::McpOAuthProvider;
use maho_ext_mcp::auth::oauth_refresh::McpRefreshManager;
use maho_ext_mcp::auth::step_up::{is_step_up_required, merge_scopes, parse_www_authenticate};
use maho_ext_mcp::auth::token_store::{McpStoredAuth, McpTokenStore};
use maho_ext_mcp::log::McpLogger;
use maho_ext_mcp::errors::{McpError, McpErrorKind};
use maho_ext_mcp::transport_sdk::{AuthStepUpLoginFuture, AuthStepUpReconnectFuture, McpAuthStepUp, McpClient, McpTransportSpec};

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

#[derive(Clone)]
struct ChallengeState {
    calls: Arc<Mutex<Vec<RecordedCall>>>,
    next: Arc<AtomicUsize>,
    status: StatusCode,
    challenge: &'static str,
}

impl ChallengeState {
    fn new(status: StatusCode, challenge: &'static str) -> Self {
        Self { calls: Arc::new(Mutex::new(Vec::new())), next: Arc::new(AtomicUsize::new(0)), status, challenge }
    }
    fn record(&self, method: String, authorization: Option<String>, challenge: Option<&str>) {
        self.calls.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(RecordedCall { method, authorization, emitted_challenge: challenge.map(str::to_owned) });
    }
    fn tool_calls(&self) -> Vec<RecordedCall> {
        self.calls.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter().filter(|call| call.method == "tools/call").cloned().collect()
    }
}

async fn challenge_mcp(State(state): State<ChallengeState>, headers: HeaderMap, body: String) -> Response {
    let value: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let method = value.get("method").and_then(Value::as_str).unwrap_or("").to_owned();
    let authorization = headers.get("authorization").and_then(|value| value.to_str().ok()).map(str::to_owned);
    if value.get("id").is_none() {
        state.record(method, authorization, None);
        return StatusCode::ACCEPTED.into_response();
    }
    if method == "initialize" {
        state.record(method, authorization, None);
        return Json(json!({"jsonrpc":"2.0","id":value["id"],"result":{"protocolVersion":"2025-11-25","capabilities":{},"serverInfo":{"name":"step-up","version":"1"}}})).into_response();
    }
    if state.next.fetch_add(1, Ordering::SeqCst) == 0 {
        state.record(method, authorization, Some(state.challenge));
        return Response::builder()
            .status(state.status)
            .header(header::WWW_AUTHENTICATE, state.challenge)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json!({"error":"challenge"}).to_string()))
            .unwrap();
    }
    state.record(method, authorization, None);
    Json(json!({"jsonrpc":"2.0","id":value["id"],"result":{"content":[{"type":"text","text":"ok"}],"isError":false}})).into_response()
}

fn stored_auth() -> McpStoredAuth {
    McpStoredAuth {
        access_token: Some("fixture-access".into()),
        refresh_token: Some("fixture-refresh".into()),
        expires_at: Some(chrono::Utc::now().timestamp_millis() as f64 + 3_600_000.0),
        client_info: Some(json!({"client_id": "fixture-client"})),
        ..Default::default()
    }
}

fn provider(root: &Path, name: &str, server: &str) -> Arc<McpOAuthProvider> {
    let store = McpTokenStore::new(root, name, server);
    store.write(stored_auth()).unwrap();
    let mut provider = McpOAuthProvider::new(store);
    provider.require_https = false;
    Arc::new(provider)
}

async fn step_up_client(server: &str, provider: Arc<McpOAuthProvider>, root: &Path) -> Arc<McpClient> {
    let logger = Arc::new(Mutex::new(McpLogger::new("step-up", root, None).unwrap()));
    let spec = McpTransportSpec::Http { url: server.parse().unwrap(), headers: Default::default() };
    let client = McpClient::materialize("step-up", &spec, logger).await.unwrap();
    client.set_auth(Arc::new(McpRefreshManager::new(provider, reqwest::Client::new()))).await;
    client.initialize(Duration::from_secs(2)).await.unwrap();
    client
}

async fn spawn_challenge_server(state: ChallengeState) -> (String, tokio::sync::oneshot::Sender<()>, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new().route("/mcp", post(challenge_mcp)).with_state(state);
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move { axum::serve(listener, app).with_graceful_shutdown(async { let _ = stopped.await; }).await.unwrap(); });
    (format!("http://{address}/mcp"), stop, server)
}

async fn shutdown(stop: tokio::sync::oneshot::Sender<()>, server: tokio::task::JoinHandle<()>) {
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), server).await.unwrap().unwrap();
}

#[test]
fn www_authenticate_challenges_parse_to_the_pinned_step_up_info() {
    let unauthorized = parse_www_authenticate(CHALLENGE_401).unwrap();
    assert_eq!(unauthorized.required_scopes, vec!["mcp:read".to_owned()]);
    assert_eq!(unauthorized.error.as_deref(), Some("invalid_token"));
    assert_eq!(unauthorized.error_description.as_deref(), Some("access token expired"));

    let forbidden = parse_www_authenticate(CHALLENGE_403).unwrap();
    assert_eq!(forbidden.required_scopes, vec!["mcp:write".to_owned(), "mcp:admin".to_owned()]);
    assert_eq!(forbidden.error.as_deref(), Some("insufficient_scope"));
    assert!(forbidden.error_description.is_none());
}

#[test]
fn www_authenticate_requires_bearer_and_a_non_empty_scope() {
    for header in ["Basic realm=\"x\"", "Bearer error=\"invalid_token\"", "Bearer scope=\"\"", "Bearer", ""] {
        assert!(parse_www_authenticate(header).is_none(), "{header}");
    }
    let bare = parse_www_authenticate("Bearer scope=mcp:read error=invalid_token").unwrap();
    assert_eq!(bare.required_scopes, vec!["mcp:read".to_owned()]);
    assert_eq!(bare.error.as_deref(), Some("invalid_token"));
}

#[test]
fn step_up_is_required_only_for_403_with_a_parseable_challenge() {
    assert!(is_step_up_required(403, Some(CHALLENGE_403)).is_some());
    assert!(is_step_up_required(401, Some(CHALLENGE_401)).is_none());
    assert!(is_step_up_required(403, None).is_none());
    assert!(is_step_up_required(403, Some("Basic realm=\"x\"")).is_none());
}

#[test]
fn merged_scopes_keep_the_existing_order_and_deduplicate() {
    assert_eq!(merge_scopes(&["a".into(), "b".into()], &["b".into(), "c".into()]), vec!["a", "b", "c"]);
    assert_eq!(merge_scopes(&[], &["x".into()]), vec!["x"]);
    assert_eq!(merge_scopes(&["x".into()], &[]), vec!["x"]);
}

#[tokio::test]
async fn forbidden_challenge_escalates_the_scopes_and_retries() {
    let state = ChallengeState::new(StatusCode::FORBIDDEN, CHALLENGE_403);
    let (server, stop, task) = spawn_challenge_server(state.clone()).await;
    let root = tempfile::tempdir().unwrap();
    let provider = provider(root.path(), "step-up-403", &server);
    let client = step_up_client(&server, provider.clone(), root.path()).await;

    let result = client.request("tools/call", json!({"name":"tool","arguments":{}}), Duration::from_secs(2)).await;
    assert!(result.is_ok(), "the escalated retry must reach the server again: {result:?}");
    assert_eq!(provider.effective_scopes(), vec!["mcp:write".to_owned(), "mcp:admin".to_owned()]);

    let calls = state.tool_calls();
    assert_eq!(calls.len(), 2, "a 403 challenge must escalate the scopes and retry exactly once");
    assert_eq!(calls[0].emitted_challenge.as_deref(), Some(CHALLENGE_403));
    assert_eq!(calls[1].emitted_challenge, None);
    assert_eq!(calls[1].authorization.as_deref(), Some("Bearer fixture-access"));

    client.close().await.unwrap();
    drop(client);
    shutdown(stop, task).await;
}

#[tokio::test]
async fn post_request_401_refreshes_once_and_retries() {
    let state = ChallengeState::new(StatusCode::UNAUTHORIZED, CHALLENGE_401);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let base = format!("http://{address}");
    let (sender, mut token_requests) = tokio::sync::mpsc::unbounded_channel();
    let protected = json!({"resource": format!("{base}/mcp"), "authorization_servers": [base.clone()]});
    let metadata = json!({"issuer": base, "authorization_endpoint": format!("{base}/authorize"), "token_endpoint": format!("{base}/token")});
    let app = Router::new()
        .route("/mcp", post(challenge_mcp))
        .route("/.well-known/oauth-protected-resource", get(move || { let protected = protected.clone(); async move { Json(protected) } }))
        .route("/.well-known/oauth-authorization-server", get(move || { let metadata = metadata.clone(); async move { Json(metadata) } }))
        .route("/token", post(move |Form(form): Form<BTreeMap<String, String>>| { let sender = sender.clone(); async move { sender.send(form).unwrap(); Json(json!({"access_token":"refreshed-access","token_type":"Bearer","expires_in":3600})) } }))
        .with_state(state.clone());
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move { axum::serve(listener, app).with_graceful_shutdown(async { let _ = stopped.await; }).await.unwrap(); });

    let mcp_url = format!("{base}/mcp");
    let root = tempfile::tempdir().unwrap();
    let provider = provider(root.path(), "step-up-401", &mcp_url);
    let client = step_up_client(&mcp_url, provider.clone(), root.path()).await;

    let result = client.request("tools/call", json!({"name":"tool","arguments":{}}), Duration::from_secs(5)).await;
    assert!(result.is_ok(), "the refreshed retry must succeed: {result:?}");
    let form = tokio::time::timeout(Duration::from_secs(2), token_requests.recv()).await.unwrap().unwrap();
    assert_eq!(form["grant_type"], "refresh_token");
    assert_eq!(form["refresh_token"], "fixture-refresh");

    let calls = state.tool_calls();
    assert_eq!(calls.len(), 2, "a 401 must refresh once and retry");
    assert_eq!(calls[0].authorization.as_deref(), Some("Bearer fixture-access"));
    assert_eq!(calls[1].authorization.as_deref(), Some("Bearer refreshed-access"));
    assert_eq!(provider.store.read().unwrap().unwrap().access_token.as_deref(), Some("refreshed-access"));

    client.close().await.unwrap();
    drop(client);
    shutdown(stop, server).await;
}

struct FakeStepUp {
    logins: Arc<Mutex<Vec<Vec<String>>>>,
    reconnects: Arc<AtomicUsize>,
    renewed: Arc<tokio::sync::Mutex<Option<Arc<McpClient>>>>,
}

impl McpAuthStepUp for FakeStepUp {
    fn login(&self, required_scopes: Vec<String>) -> AuthStepUpLoginFuture {
        let logins = self.logins.clone();
        Box::pin(async move {
            logins.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(required_scopes);
            Ok::<_, McpError>(())
        })
    }
    fn reconnect(&self) -> AuthStepUpReconnectFuture {
        let reconnects = self.reconnects.clone();
        let renewed = self.renewed.clone();
        Box::pin(async move {
            reconnects.fetch_add(1, Ordering::SeqCst);
            renewed.lock().await.clone().ok_or_else(|| McpError::new(McpErrorKind::Connect, "no renewed client"))
        })
    }
}

struct FailingStepUp;

impl McpAuthStepUp for FailingStepUp {
    fn login(&self, _required_scopes: Vec<String>) -> AuthStepUpLoginFuture {
        Box::pin(async move { Err::<(), McpError>(McpError::new(McpErrorKind::Auth, "login refused")) })
    }
    fn reconnect(&self) -> AuthStepUpReconnectFuture {
        Box::pin(async move { Err::<Arc<McpClient>, McpError>(McpError::new(McpErrorKind::Connect, "reconnect refused")) })
    }
}

#[tokio::test]
async fn bound_step_up_logs_in_reconnects_and_retries_on_the_renewed_client() {
    let state = ChallengeState::new(StatusCode::FORBIDDEN, CHALLENGE_403);
    let (server, stop, task) = spawn_challenge_server(state.clone()).await;
    let root = tempfile::tempdir().unwrap();
    let provider = provider(root.path(), "step-up-bound", &server);
    let client = step_up_client(&server, provider.clone(), root.path()).await;

    let renewed_root = tempfile::tempdir().unwrap();
    let renewed_provider = provider(renewed_root.path(), "step-up-renewed", &server);
    renewed_provider.store.write(McpStoredAuth { access_token: Some("renewed-access".into()), ..Default::default() }).unwrap();
    let logger = Arc::new(Mutex::new(McpLogger::new("step-up-renewed", renewed_root.path(), None).unwrap()));
    let spec = McpTransportSpec::Http { url: server.parse().unwrap(), headers: Default::default() };
    let renewed = McpClient::materialize("step-up-renewed", &spec, logger).await.unwrap();
    renewed.set_auth(Arc::new(McpRefreshManager::new(renewed_provider, reqwest::Client::new()))).await;
    renewed.initialize(Duration::from_secs(2)).await.unwrap();

    let logins: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let reconnects = Arc::new(AtomicUsize::new(0));
    client.set_step_up(Arc::new(FakeStepUp { logins: logins.clone(), reconnects: reconnects.clone(), renewed: Arc::new(tokio::sync::Mutex::new(Some(renewed.clone()))) })).await;

    let result = client.request("tools/call", json!({"name":"tool","arguments":{}}), Duration::from_secs(2)).await;
    assert!(result.is_ok(), "the retry on the renewed client must succeed: {result:?}");
    let recorded = logins.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    assert_eq!(recorded, vec![vec!["mcp:write".to_owned(), "mcp:admin".to_owned()]]);
    assert_eq!(reconnects.load(Ordering::SeqCst), 1);
    assert_eq!(provider.effective_scopes(), vec!["mcp:write".to_owned(), "mcp:admin".to_owned()]);

    let calls = state.tool_calls();
    assert_eq!(calls.len(), 2, "a bound step-up logs in, reconnects and retries exactly once");
    assert_eq!(calls[0].authorization.as_deref(), Some("Bearer fixture-access"));
    assert_eq!(calls[1].authorization.as_deref(), Some("Bearer renewed-access"), "the retry must run on the client the reconnect produced");

    client.close().await.unwrap();
    renewed.close().await.unwrap();
    drop(client);
    drop(renewed);
    shutdown(stop, task).await;
}

#[tokio::test]
async fn a_failed_step_up_login_never_retries_with_stale_auth() {
    let state = ChallengeState::new(StatusCode::FORBIDDEN, CHALLENGE_403);
    let (server, stop, task) = spawn_challenge_server(state.clone()).await;
    let root = tempfile::tempdir().unwrap();
    let store = McpTokenStore::new(root.path(), "step-up-fail", &server);
    store.write(McpStoredAuth { access_token: Some("fixture-access".into()), expires_at: Some(chrono::Utc::now().timestamp_millis() as f64 + 3_600_000.0), ..Default::default() }).unwrap();
    let mut provider = McpOAuthProvider::new(store);
    provider.require_https = false;
    let client = step_up_client(&server, Arc::new(provider), root.path()).await;
    client.set_step_up(Arc::new(FailingStepUp)).await;

    let result = client.request("tools/call", json!({"name":"tool","arguments":{}}), Duration::from_secs(2)).await;
    assert!(result.is_err(), "a refused re-login with no refresh token must fail");
    assert_eq!(state.tool_calls().len(), 1, "the transport must not retry with the stale token");

    client.close().await.unwrap();
    drop(client);
    shutdown(stop, task).await;
}
