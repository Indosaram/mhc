use std::{
    sync::{Arc, Mutex, atomic::{AtomicUsize, Ordering}},
    time::Duration,
};
use axum::{Json, Router, extract::State, http::{HeaderMap, StatusCode}, response::{IntoResponse, Response}, routing::post};
use futures::future::BoxFuture;
use maho_ext_mcp::{
    auth::{
        challenge::McpAuthChallengeHandler,
        oauth_provider::McpOAuthProvider,
        oauth_refresh::McpRefreshManager,
        token_store::{McpStoredAuth, McpTokenStore},
    },
    errors::{McpError, McpErrorKind},
    log::McpLogger,
    transport_sdk::{McpClient, McpTransportSpec},
};
use serde_json::{Value, json};

struct Server {
    url: url::Url,
    tokens: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
#[derive(Clone)]
struct ServerState {
    tokens: Arc<Mutex<Vec<String>>>,
    accept: Option<&'static str>,
}
async fn mcp(State(state): State<ServerState>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    let token = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or("")
        .to_owned();
    state.tokens.lock().expect("test fixture").push(token.clone());
    if state.accept == Some(token.as_str()) {
        return Json(json!({"jsonrpc":"2.0","id":body["id"],"result":{"ok":true}})).into_response();
    }
    (
        StatusCode::FORBIDDEN,
        [("www-authenticate", r#"Bearer error="insufficient_scope", scope="extra other""#)],
        "denied",
    )
        .into_response()
}
/// A server that answers 200 only to `accept`, and 403 + scope challenge otherwise.
async fn server(accept: Option<&'static str>) -> Server {
    let tokens = Arc::new(Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("test fixture");
    let url = url::Url::parse(&format!("http://{}/mcp", listener.local_addr().expect("test fixture"))).expect("test fixture");
    let app = Router::new().route("/mcp", post(mcp)).with_state(ServerState { tokens: tokens.clone(), accept });
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("test fixture");
    });
    Server { url, tokens, task }
}

struct FakeHandler {
    me: std::sync::Weak<FakeHandler>,
    manager: Arc<McpRefreshManager>,
    spec: McpTransportSpec,
    logger: Arc<Mutex<McpLogger>>,
    scopes: Mutex<Vec<Vec<String>>>,
    renews: AtomicUsize,
    new_token: Option<&'static str>,
}
impl McpAuthChallengeHandler for FakeHandler {
    fn reauthorize(&self, scopes: Vec<String>) -> BoxFuture<'_, Result<(), McpError>> {
        Box::pin(async move {
            self.scopes.lock().expect("test fixture").push(scopes);
            match self.new_token {
                Some(token) => {
                    self.manager
                        .provider()
                        .store
                        .update(|record| {
                            let mut record = record.unwrap_or_default();
                            record.access_token = Some(token.into());
                            Some(record)
                        })
                        .expect("test fixture");
                    Ok(())
                }
                None => Err(McpError::new(McpErrorKind::Auth, "login refused")),
            }
        })
    }
    fn renew(&self) -> BoxFuture<'_, Result<Arc<McpClient>, McpError>> {
        Box::pin(async move {
            self.renews.fetch_add(1, Ordering::SeqCst);
            let client = McpClient::materialize("step-up", &self.spec, self.logger.clone()).await?;
            client.set_auth(self.manager.clone()).await;
            client.set_auth_challenge_handler(self.me.upgrade().expect("test fixture")).await;
            Ok(client)
        })
    }
}

struct Rig {
    server: Server,
    handler: Arc<FakeHandler>,
    client: Arc<McpClient>,
    provider: Arc<McpOAuthProvider>,
    _root: tempfile::TempDir,
}
async fn rig(accept: Option<&'static str>, new_token: Option<&'static str>) -> Rig {
    let server = server(accept).await;
    let root = tempfile::tempdir().expect("test fixture");
    let store = McpTokenStore::new(root.path(), "step-up", server.url.as_str());
    store.write(McpStoredAuth { access_token: Some("stale".into()), ..Default::default() }).expect("test fixture");
    let mut provider = McpOAuthProvider::new(store);
    provider.require_https = false;
    provider.scopes = Some(vec!["base".into()]);
    let provider = Arc::new(provider);
    let manager = Arc::new(McpRefreshManager::new(provider.clone(), reqwest::Client::new()));
    let logger = Arc::new(Mutex::new(McpLogger::new("step-up", root.path(), None).expect("test fixture")));
    let spec = McpTransportSpec::Http { url: server.url.clone(), headers: Default::default() };
    let handler = Arc::new_cyclic(|me| FakeHandler {
        me: me.clone(),
        manager: manager.clone(),
        spec: spec.clone(),
        logger: logger.clone(),
        scopes: Mutex::new(Vec::new()),
        renews: AtomicUsize::new(0),
        new_token,
    });
    let client = McpClient::materialize("step-up", &spec, logger).await.expect("test fixture");
    client.set_auth(manager).await;
    client.set_auth_challenge_handler(handler.clone()).await;
    Rig { server, handler, client, provider, _root: root }
}
async fn call(client: &McpClient) -> Result<Value, McpError> {
    tokio::time::timeout(Duration::from_secs(10), client.request("tools/list", json!({}), Duration::from_secs(10)))
        .await
        .expect("test fixture")
}

#[tokio::test]
async fn step_up_reauthorizes_once_renews_and_retries_on_the_renewed_client() {
    let rig = rig(Some("fresh"), Some("fresh")).await;
    assert_eq!(call(&rig.client).await.expect("test fixture")["ok"], true);
    assert_eq!(*rig.handler.scopes.lock().expect("test fixture"), vec![vec!["base".to_owned(), "extra".into(), "other".into()]]);
    assert_eq!(rig.handler.renews.load(Ordering::SeqCst), 1);
    assert_eq!(*rig.server.tokens.lock().expect("test fixture"), vec!["stale".to_owned(), "fresh".into()]);
    assert_eq!(rig.provider.effective_scopes().expect("test fixture"), vec!["base".to_owned(), "extra".into(), "other".into()]);
}

#[tokio::test]
async fn failed_reauthorization_never_retries_with_the_stale_token() {
    let rig = rig(Some("fresh"), None).await;
    let error = call(&rig.client).await.expect_err("expected an error");
    assert_eq!(error.kind, McpErrorKind::Protocol);
    assert_eq!(error.cause.as_ref().expect("test fixture")["status"], 403);
    assert_eq!(rig.handler.renews.load(Ordering::SeqCst), 0);
    assert_eq!(*rig.server.tokens.lock().expect("test fixture"), vec!["stale".to_owned()]);
}

#[tokio::test]
async fn attempt_budget_of_three_bounds_a_server_that_always_answers_403() {
    let rig = rig(None, Some("still-rejected")).await;
    let error = call(&rig.client).await.expect_err("expected an error");
    assert_eq!(error.cause.as_ref().expect("test fixture")["status"], 403);
    assert_eq!(rig.handler.scopes.lock().expect("test fixture").len(), 3);
    assert_eq!(rig.handler.renews.load(Ordering::SeqCst), 3);
    assert_eq!(rig.server.tokens.lock().expect("test fixture").len(), 4);
}
