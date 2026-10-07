//! SC-U2 process-local discovery cache regressions: the pinned `mcp-oauth/discovery.ts`
//! `discoveryCache` + `pendingDiscovery` in-flight coalescing + `resetDiscoveryCache()`, plus the
//! record migration that drops the never-persisted `discoveryState` field.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;

use maho_ext_mcp::auth::oauth::{discover, reset_discovery_cache};
use maho_ext_mcp::auth::oauth_provider::McpOAuthProvider;
use maho_ext_mcp::auth::token_store::McpTokenStore;

/// The pinned cache is process-global, so the cache tests in this binary run serially: a
/// concurrent `reset_discovery_cache()` must never clear another test's entry. A tokio mutex is
/// used because its guard is held across `.await`.
static CACHE_TESTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Fixture {
    address: std::net::SocketAddr,
    hits: Arc<AtomicUsize>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    server: Option<tokio::task::JoinHandle<()>>,
}

impl Fixture {
    async fn start(flaky: bool) -> Self {
        let hits = Arc::new(AtomicUsize::new(0));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("fixture operation must succeed");
        let address = listener.local_addr().expect("fixture operation must succeed");
        let protected = json!({"resource": format!("http://{address}/mcp"), "authorization_servers": [format!("http://{address}")]});
        let metadata = json!({"issuer": format!("http://{address}"), "authorization_endpoint": format!("http://{address}/authorize"), "token_endpoint": format!("http://{address}/token")});
        let observed = hits.clone();
        let protected_route = get(move || { let protected = protected.clone(); let observed = observed.clone(); async move {
            let hit = observed.fetch_add(1, Ordering::SeqCst);
            if flaky && hit == 0 { (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": "boom"}))) } else { (StatusCode::OK, Json(protected)) }
        }});
        let metadata_route = get(move || { let metadata = metadata.clone(); async move { Json(metadata) } });
        let app = Router::new().route("/.well-known/oauth-protected-resource", protected_route).route("/.well-known/oauth-authorization-server", metadata_route);
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move { axum::serve(listener, app).with_graceful_shutdown(async { let _ = stopped.await; }).await.expect("fixture operation must succeed"); });
        Self { address, hits, stop: Some(stop), server: Some(server) }
    }
    fn provider(&self, name: &str, root: &std::path::Path, resource_path: &str) -> McpOAuthProvider {
        let mut provider = McpOAuthProvider::new(McpTokenStore::new(root, name, &format!("http://{}{resource_path}", self.address)));
        provider.require_https = false;
        provider
    }
    fn hits(&self) -> usize { self.hits.load(Ordering::SeqCst) }
    async fn shutdown(&mut self) {
        if let Some(stop) = self.stop.take() { let _ = stop.send(()); }
        let Some(server) = self.server.take() else { return; };
        match tokio::time::timeout(Duration::from_secs(2), server).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => panic!("the discovery fixture server task failed: {error}"),
            Err(_) => panic!("the discovery fixture server did not terminate within the bound"),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() { let _ = stop.send(()); }
        if let Some(server) = self.server.take() { server.abort(); }
    }
}

#[tokio::test]
async fn the_cache_serves_a_repeat_without_a_second_request() {
    let _serial = CACHE_TESTS.lock().await;
    reset_discovery_cache();
    let mut fixture = Fixture::start(false).await;
    let root = tempfile::tempdir().expect("fixture operation must succeed");
    let provider = fixture.provider("hit", root.path(), "/mcp");
    let client = reqwest::Client::new();
    let first = discover(&provider, &client).await.expect("fixture operation must succeed");
    let second = discover(&provider, &client).await.expect("fixture operation must succeed");
    assert_eq!(first.authorization_server_metadata, second.authorization_server_metadata);
    assert_eq!(fixture.hits(), 1, "the pinned discoveryCache must serve the repeat");
    drop(client);
    fixture.shutdown().await;
    reset_discovery_cache();
}

#[tokio::test]
async fn concurrent_misses_coalesce_onto_one_discovery() {
    let _serial = CACHE_TESTS.lock().await;
    reset_discovery_cache();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("fixture operation must succeed");
    let address = listener.local_addr().expect("fixture operation must succeed");
    let hits = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let (entered, mut requests) = tokio::sync::mpsc::unbounded_channel();
    let protected = json!({"resource": format!("http://{address}/mcp"), "authorization_servers": [format!("http://{address}")]});
    let metadata = json!({"issuer": format!("http://{address}"), "authorization_endpoint": format!("http://{address}/authorize"), "token_endpoint": format!("http://{address}/token")});
    let observed = hits.clone();
    let gate = release.clone();
    let app = Router::new()
        .route("/.well-known/oauth-protected-resource", get(move || { let protected = protected.clone(); let observed = observed.clone(); let gate = gate.clone(); let entered = entered.clone(); async move {
            observed.fetch_add(1, Ordering::SeqCst); entered.send(()).expect("fixture operation must succeed"); gate.acquire().await.expect("fixture operation must succeed").forget(); Json(protected)
        }}))
        .route("/.well-known/oauth-authorization-server", get(move || { let metadata = metadata.clone(); async move { Json(metadata) } }));
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move { axum::serve(listener, app).with_graceful_shutdown(async { let _ = stopped.await; }).await.expect("fixture operation must succeed"); });
    let root = tempfile::tempdir().expect("fixture operation must succeed");
    let mut provider = McpOAuthProvider::new(McpTokenStore::new(root.path(), "coalesce", &format!("http://{address}/mcp")));
    provider.require_https = false;
    let provider = Arc::new(provider);
    let client = reqwest::Client::new();

    // Drive the first caller as a task so the runtime advances its request, and wait for the exact
    // "PRM handler entered" signal (subscription was created before the server started).
    let first = tokio::spawn({ let provider = provider.clone(); let client = client.clone(); async move { discover(&provider, &client).await } });
    tokio::time::timeout(Duration::from_secs(2), requests.recv()).await.expect("the first discovery must reach the PRM handler").expect("fixture operation must succeed");
    assert_eq!(hits.load(Ordering::SeqCst), 1, "the first caller must have issued the only PRM request");

    // The first caller is parked inside the handler with the gate held, so its pendingDiscovery entry
    // is registered; one poll of the second caller joins it synchronously (before its first await).
    let mut second = Box::pin(discover(&provider, &client));
    assert!(futures::poll!(&mut second).is_pending(), "the second caller must park on the in-flight discovery");
    assert_eq!(hits.load(Ordering::SeqCst), 1, "a coalesced caller must not issue its own PRM request");

    release.add_permits(2);
    let (first, second) = tokio::time::timeout(Duration::from_secs(2), async { tokio::join!(first, second) }).await.expect("the coalesced discovery must complete once released");
    let first = first.expect("fixture operation must succeed").expect("fixture operation must succeed");
    let second = second.expect("fixture operation must succeed");
    assert_eq!(first.authorization_server_url, second.authorization_server_url);
    assert_eq!(hits.load(Ordering::SeqCst), 1, "the pinned pendingDiscovery must coalesce both callers onto one request");
    assert!(requests.try_recv().is_err(), "exactly one PRM request must ever enter the handler");
    drop(client);
    stop.send(()).expect("fixture operation must succeed");
    tokio::time::timeout(Duration::from_secs(2), server).await.expect("fixture operation must succeed").expect("fixture operation must succeed");
    reset_discovery_cache();
}

#[tokio::test]
async fn reset_discovery_cache_forces_a_refetch() {
    let _serial = CACHE_TESTS.lock().await;
    reset_discovery_cache();
    let mut fixture = Fixture::start(false).await;
    let root = tempfile::tempdir().expect("fixture operation must succeed");
    let provider = fixture.provider("reset", root.path(), "/mcp");
    let client = reqwest::Client::new();
    discover(&provider, &client).await.expect("fixture operation must succeed");
    reset_discovery_cache();
    discover(&provider, &client).await.expect("fixture operation must succeed");
    assert_eq!(fixture.hits(), 2, "the pinned resetDiscoveryCache must clear discoveryCache");
    drop(client);
    fixture.shutdown().await;
    reset_discovery_cache();
}

#[tokio::test]
async fn a_failed_discovery_is_not_cached() {
    let _serial = CACHE_TESTS.lock().await;
    reset_discovery_cache();
    let mut fixture = Fixture::start(true).await;
    let root = tempfile::tempdir().expect("fixture operation must succeed");
    let provider = fixture.provider("failed", root.path(), "/mcp");
    let client = reqwest::Client::new();
    let error = discover(&provider, &client).await.err().expect("discovery must fail");
    assert_eq!(error.to_string(), "OAuth protected resource metadata fetch failed (500)");
    let recovered = discover(&provider, &client).await.expect("fixture operation must succeed");
    assert_eq!(recovered.authorization_server_url, format!("http://{}/", fixture.address));
    assert_eq!(fixture.hits(), 2, "a failed discovery must not be cached and must clear pendingDiscovery");
    drop(client);
    fixture.shutdown().await;
    reset_discovery_cache();
}

#[tokio::test]
async fn the_cache_key_is_the_normalized_resource_url() {
    let _serial = CACHE_TESTS.lock().await;
    reset_discovery_cache();
    let mut fixture = Fixture::start(false).await;
    let root = tempfile::tempdir().expect("fixture operation must succeed");
    let bare = fixture.provider("bare", root.path(), "");
    let slashed = fixture.provider("slashed", root.path(), "/");
    let client = reqwest::Client::new();
    let first = discover(&bare, &client).await.expect("fixture operation must succeed");
    let second = discover(&slashed, &client).await.expect("fixture operation must succeed");
    assert_eq!(first.authorization_server_url, second.authorization_server_url);
    assert_eq!(fixture.hits(), 1, "the pinned key is new URL(resource).toString()");
    drop(client);
    fixture.shutdown().await;
    reset_discovery_cache();
}

#[test]
fn a_legacy_discovery_state_record_is_migrated_out() {
    let root = tempfile::tempdir().expect("fixture operation must succeed");
    let store = McpTokenStore::new(root.path(), "legacy", "https://mcp.invalid/sse");
    std::fs::create_dir_all(store.dir()).expect("fixture operation must succeed");
    std::fs::write(store.tokens_path(), serde_json::to_string(&json!({
        "accessToken": "fixture-access",
        "refreshToken": "fixture-refresh",
        "resource": "https://mcp.invalid/sse",
        "discoveryState": {"authorizationServerUrl": "https://auth.invalid", "authorizationServerMetadata": {"token_endpoint": "https://auth.invalid/token"}, "resourceMetadata": null},
        "futureField": {"kept": true}
    })).expect("fixture operation must succeed")).expect("fixture operation must succeed");
    let record = store.read().expect("fixture operation must succeed").expect("fixture operation must succeed");
    assert_eq!(record.access_token.as_deref(), Some("fixture-access"));
    assert_eq!(record.refresh_token.as_deref(), Some("fixture-refresh"));
    assert_eq!(record.resource.as_deref(), Some("https://mcp.invalid/sse"));
    assert_eq!(record.extra.get("futureField"), Some(&json!({"kept": true})));
    assert!(!record.extra.contains_key("discoveryState"), "the migration must drop the persisted discovery field");
    store.write(record).expect("fixture operation must succeed");
    let persisted: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(store.tokens_path()).expect("fixture operation must succeed")).expect("fixture operation must succeed");
    assert!(persisted.get("discoveryState").is_none());
    assert_eq!(persisted["accessToken"], json!("fixture-access"));
    assert_eq!(persisted["refreshToken"], json!("fixture-refresh"));
    assert_eq!(persisted["futureField"], json!({"kept": true}));
}
