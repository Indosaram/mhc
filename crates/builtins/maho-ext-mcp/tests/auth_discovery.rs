use std::{
    collections::HashMap,
    sync::{Arc, Mutex, atomic::{AtomicUsize, Ordering}},
    time::Duration,
};
use axum::{Json, Router, extract::{Request, State}, http::StatusCode, response::{IntoResponse, Response}};
use maho_ext_mcp::auth::{
    oauth::{OAuthRequestError, OAuthServerInfo, discover, reset_discovery_cache},
    oauth_provider::{CredentialScope, McpOAuthProvider},
    oauth_refresh::{DiscoverFn, McpRefreshManager, RefreshManagerOptions},
    token_store::{McpStoredAuth, McpTokenStore},
};
use serde_json::{Value, json};

type Routes = HashMap<String, (u16, Option<Value>)>;

struct Server {
    base: String,
    hits: Arc<Mutex<Vec<String>>>,
    routes: Arc<Mutex<Routes>>,
    gate: Arc<tokio::sync::Semaphore>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Server {
    fn count(&self, path: &str) -> usize {
        self.hits.lock().expect("test fixture").iter().filter(|hit| *hit == path).count()
    }
    fn set(&self, path: &str, status: u16, body: Option<Value>) {
        self.routes.lock().expect("test fixture").insert(path.into(), (status, body));
    }
}

#[derive(Clone)]
struct AppState {
    hits: Arc<Mutex<Vec<String>>>,
    routes: Arc<Mutex<Routes>>,
    gate: Arc<tokio::sync::Semaphore>,
}

async fn handle(State(state): State<AppState>, request: Request) -> Response {
    let path = request.uri().path().to_owned();
    state.hits.lock().expect("test fixture").push(path.clone());
    // Waits only when the test has closed the gate (zero permits); open gates are cheap.
    if let Ok(permit) = state.gate.acquire().await {
        drop(permit);
    }
    let route = state.routes.lock().expect("test fixture").get(&path).cloned();
    match route {
        Some((status, Some(body))) => (StatusCode::from_u16(status).expect("test fixture"), Json(body)).into_response(),
        Some((status, None)) => StatusCode::from_u16(status).expect("test fixture").into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn server() -> Server {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("test fixture");
    let base = format!("http://{}", listener.local_addr().expect("test fixture"));
    let state = AppState {
        hits: Arc::default(),
        routes: Arc::default(),
        // Starts open: a semaphore with many permits.
        gate: Arc::new(tokio::sync::Semaphore::new(1 << 20)),
    };
    let app = Router::new().fallback(handle).with_state(state.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("test fixture");
    });
    Server { base, hits: state.hits, routes: state.routes, gate: state.gate, task }
}

fn metadata(base: &str) -> Value {
    json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/authorize"),
        "token_endpoint": format!("{base}/token"),
    })
}

fn provider(root: &std::path::Path, url: &str) -> McpOAuthProvider {
    let mut provider = McpOAuthProvider::new(McpTokenStore::new(root, "disc", url));
    provider.require_https = false;
    provider
}

fn message(error: OAuthRequestError) -> String {
    error.to_string()
}

const PRM: &str = "/.well-known/oauth-protected-resource";
const ASM: &str = "/.well-known/oauth-authorization-server";

#[tokio::test]
async fn protected_resource_is_fetched_at_the_origin_and_issuer_origin_is_retried() {
    let server = server().await;
    let base = &server.base;
    server.set(PRM, 200, Some(json!({"authorization_servers": ["", format!("{base}/tenant/")]})));
    server.set(ASM, 200, Some(metadata(base)));
    let root = tempfile::tempdir().expect("test fixture");
    let provider = provider(root.path(), &format!("{base}/deep/mcp?x=1#f"));
    let info = discover(&provider, &reqwest::Client::new()).await.expect("test fixture");
    assert_eq!(info.authorization_server_url, format!("{base}/tenant/"));
    assert!(info.resource_metadata.get("authorization_servers").is_some());
    assert_eq!(server.count(PRM), 1);
    assert_eq!(server.count("/deep/mcp"), 0);
    assert_eq!(server.count(&format!("{ASM}/tenant")), 1);
    assert_eq!(server.count(ASM), 1);
    assert_eq!(info.authorization_server_metadata["token_endpoint"], format!("{base}/token"));
}

#[tokio::test]
async fn missing_authorization_servers_and_bad_statuses_are_reported() {
    let server = server().await;
    let root = tempfile::tempdir().expect("test fixture");
    let client = reqwest::Client::new();
    let provider = provider(root.path(), &format!("{}/mcp", server.base));

    server.set(PRM, 200, Some(json!({"authorization_servers": []})));
    assert!(message(discover(&provider, &client).await.expect_err("expected an error")).contains("authorization_servers"));

    server.set(PRM, 200, Some(Value::Null));
    assert!(message(discover(&provider, &client).await.expect_err("expected an error")).contains("not valid JSON"));

    server.set(PRM, 500, None);
    assert!(message(discover(&provider, &client).await.expect_err("expected an error")).contains("500"));
}

#[tokio::test]
async fn missing_token_endpoint_and_missing_metadata_are_reported() {
    let server = server().await;
    let base = &server.base;
    let root = tempfile::tempdir().expect("test fixture");
    let client = reqwest::Client::new();
    let provider = provider(root.path(), &format!("{base}/mcp"));
    server.set(ASM, 200, Some(json!({"authorization_endpoint": format!("{base}/authorize")})));
    assert!(message(discover(&provider, &client).await.expect_err("expected an error")).contains("token_endpoint"));
    server.set(ASM, 404, None);
    assert!(message(discover(&provider, &client).await.expect_err("expected an error")).contains("not found"));
    server.set(ASM, 503, None);
    assert!(message(discover(&provider, &client).await.expect_err("expected an error")).contains("503"));
}

#[tokio::test]
async fn missing_protected_resource_falls_back_to_the_server_url_as_issuer() {
    let server = server().await;
    let base = &server.base;
    server.set(ASM, 200, Some(metadata(base)));
    let root = tempfile::tempdir().expect("test fixture");
    let provider = provider(root.path(), base);
    let info = discover(&provider, &reqwest::Client::new()).await.expect("test fixture");
    assert_eq!(info.authorization_server_url, *base);
    assert!(info.resource_metadata.is_null());
}

#[tokio::test]
async fn cache_serves_sequential_and_shares_concurrent_lookups() {
    let server = server().await;
    let base = &server.base;
    server.set(ASM, 200, Some(metadata(base)));
    let root = tempfile::tempdir().expect("test fixture");
    let client = reqwest::Client::new();
    let provider = Arc::new(provider(root.path(), &format!("{base}/mcp")));

    // Concurrent lookups while the server is gated share one fetch per endpoint.
    let permits = server.gate.available_permits();
    server.gate.forget_permits(permits);
    let lookups: Vec<_> = (0..5)
        .map(|_| {
            let (provider, client) = (provider.clone(), client.clone());
            tokio::spawn(async move { discover(&provider, &client).await })
        })
        .collect();
    tokio::time::timeout(Duration::from_secs(5), async {
        while server.count(PRM) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("test fixture");
    server.gate.add_permits(1 << 20);
    for lookup in lookups {
        tokio::time::timeout(Duration::from_secs(5), lookup).await.expect("test fixture").expect("test fixture").expect("test fixture");
    }
    assert_eq!((server.count(PRM), server.count(ASM)), (1, 1));

    discover(&provider, &client).await.expect("test fixture");
    assert_eq!((server.count(PRM), server.count(ASM)), (1, 1));

    reset_discovery_cache();
    discover(&provider, &client).await.expect("test fixture");
    assert_eq!((server.count(PRM), server.count(ASM)), (2, 2));

    provider.invalidate_credentials(CredentialScope::Discovery).expect("test fixture");
    discover(&provider, &client).await.expect("test fixture");
    assert_eq!(server.count(PRM), 3);
}

#[tokio::test]
async fn failed_fetch_is_not_cached() {
    let server = server().await;
    let base = &server.base;
    let root = tempfile::tempdir().expect("test fixture");
    let client = reqwest::Client::new();
    let provider = provider(root.path(), &format!("{base}/mcp"));
    server.set(ASM, 500, None);
    assert!(discover(&provider, &client).await.is_err());
    server.set(ASM, 200, Some(metadata(base)));
    discover(&provider, &client).await.expect("test fixture");
    assert_eq!(server.count(PRM), 2);
}

#[tokio::test]
async fn injected_discover_wins_over_a_warm_cache() {
    let server = server().await;
    let base = &server.base;
    server.set(ASM, 200, Some(metadata(base)));
    let root = tempfile::tempdir().expect("test fixture");
    let client = reqwest::Client::new();
    let url = format!("{base}/mcp");
    let warm = provider(root.path(), &url);
    discover(&warm, &client).await.expect("test fixture");

    let store = McpTokenStore::new(root.path(), "disc", &url);
    store.write(McpStoredAuth { access_token: Some("a".into()), refresh_token: Some("r".into()), expires_at: Some(0.0), ..Default::default() }).expect("test fixture");
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let injected: DiscoverFn = Arc::new(move |_| {
        seen.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Ok(OAuthServerInfo {
                authorization_server_url: "http://injected.invalid".into(),
                authorization_server_metadata: json!({"token_endpoint": "http://127.0.0.1:1/token"}),
                resource_metadata: Value::Null,
            })
        })
    });
    let mut refresh_provider = McpOAuthProvider::new(store);
    refresh_provider.require_https = false;
    refresh_provider.client_id = Some("c".into());
    let manager = McpRefreshManager::new_with_options(
        Arc::new(refresh_provider),
        client,
        RefreshManagerOptions { discover: Some(injected), max_retries: Some(0), ..Default::default() },
    );
    assert!(manager.refresh().await.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn legacy_discovery_state_is_dropped_on_read_and_not_rewritten() {
    let root = tempfile::tempdir().expect("test fixture");
    let store = McpTokenStore::new(root.path(), "legacy", "https://legacy.test/mcp");
    store.write(McpStoredAuth { access_token: Some("a".into()), ..Default::default() }).expect("test fixture");
    let mut raw: Value = serde_json::from_str(&std::fs::read_to_string(store.tokens_path()).expect("test fixture")).expect("test fixture");
    raw["discoveryState"] = json!({"authorizationServerUrl": "https://old.test"});
    raw["customKey"] = json!({"keep": true});
    std::fs::write(store.tokens_path(), serde_json::to_string(&raw).expect("test fixture")).expect("test fixture");

    let record = store.read().expect("test fixture").expect("test fixture");
    assert!(!record.extra.contains_key("discoveryState"));
    assert_eq!(record.extra["customKey"], json!({"keep": true}));

    store.update(|record| record).expect("test fixture");
    let rewritten: Value = serde_json::from_str(&std::fs::read_to_string(store.tokens_path()).expect("test fixture")).expect("test fixture");
    assert!(rewritten.get("discoveryState").is_none());
    assert_eq!(rewritten["customKey"], json!({"keep": true}));
    assert_eq!(rewritten["accessToken"], "a");
}
