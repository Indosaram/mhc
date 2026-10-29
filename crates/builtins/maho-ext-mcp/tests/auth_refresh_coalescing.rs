use std::{sync::Arc, time::Duration};
use axum::{Form, Json, Router, extract::State, routing::post};
use maho_ext_mcp::auth::{
    oauth::OAuthServerInfo,
    oauth_provider::McpOAuthProvider,
    oauth_refresh::{DiscoverFn, McpRefreshManager, RefreshManagerOptions, active_refresh_count, is_refresh_in_progress},
    token_store::{McpStoredAuth, McpTokenStore},
};
use serde_json::{Value, json};
use tokio::sync::{Semaphore, mpsc};

#[derive(Clone)]
struct Idp {
    entered: mpsc::UnboundedSender<()>,
    gate: Arc<Semaphore>,
}

async fn token(State(idp): State<Idp>, Form(_form): Form<Vec<(String, String)>>) -> Json<Value> {
    idp.entered.send(()).expect("test fixture");
    idp.gate.acquire().await.expect("test fixture").forget();
    Json(json!({"access_token": "fixture-new", "refresh_token": "fixture-next", "token_type": "Bearer", "expires_in": 3600}))
}

struct Fixture {
    base: String,
    entered: mpsc::UnboundedReceiver<()>,
    gate: Arc<Semaphore>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn fixture() -> Fixture {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("test fixture");
    let base = format!("http://{}", listener.local_addr().expect("test fixture"));
    let (entered, receiver) = mpsc::unbounded_channel();
    let gate = Arc::new(Semaphore::new(0));
    let app = Router::new().route("/token", post(token)).with_state(Idp { entered, gate: gate.clone() });
    let task = tokio::spawn(async move { axum::serve(listener, app).await.expect("test fixture") });
    Fixture { base, entered: receiver, gate, task }
}

fn manager(root: &std::path::Path, name: &str, server_url: &str, token_url: &str) -> McpRefreshManager {
    let store = McpTokenStore::new(root, name, server_url);
    store
        .write(McpStoredAuth { access_token: Some("old".into()), refresh_token: Some("r".into()), expires_at: Some(0.0), ..Default::default() })
        .expect("test fixture");
    let mut provider = McpOAuthProvider::new(store);
    provider.require_https = false;
    provider.client_id = Some("fixture-client".into());
    let info = OAuthServerInfo {
        authorization_server_url: "http://idp.invalid".into(),
        authorization_server_metadata: json!({"token_endpoint": token_url}),
        resource_metadata: Value::Null,
    };
    let discover: DiscoverFn = Arc::new(move |_| {
        let info = info.clone();
        Box::pin(async move { Ok(info) })
    });
    McpRefreshManager::new_with_options(Arc::new(provider), reqwest::Client::new(), RefreshManagerOptions { discover: Some(discover), ..Default::default() })
}

async fn wait_entered(fixture: &mut Fixture) {
    tokio::time::timeout(Duration::from_secs(5), fixture.entered.recv()).await.expect("test fixture").expect("test fixture");
}

#[tokio::test]
async fn two_managers_for_one_url_share_a_single_token_request() {
    let mut fixture = fixture().await;
    let root = tempfile::tempdir().expect("test fixture");
    let url = format!("{}/coalesce-a", fixture.base);
    let token_url = format!("{}/token", fixture.base);
    let first = Arc::new(manager(root.path(), "a", &url, &token_url));
    let second = Arc::new(manager(root.path(), "a", &url, &token_url));
    assert!(!is_refresh_in_progress(&url));

    let (f, s) = (first.clone(), second.clone());
    let one = tokio::spawn(async move { f.force_refresh().await });
    wait_entered(&mut fixture).await;
    assert!(is_refresh_in_progress(&url));
    let two = tokio::spawn(async move { s.force_refresh().await });

    fixture.gate.add_permits(1);
    let (one, two) = tokio::time::timeout(Duration::from_secs(5), async { tokio::join!(one, two) }).await.expect("test fixture");
    assert_eq!(one.expect("test fixture").expect("test fixture").access_token, "fixture-new");
    assert_eq!(two.expect("test fixture").expect("test fixture").access_token, "fixture-new");
    assert!(fixture.entered.try_recv().is_err(), "exactly one token request");
    assert!(!is_refresh_in_progress(&url));
}

#[tokio::test]
async fn different_urls_refresh_independently() {
    let mut fixture = fixture().await;
    let root = tempfile::tempdir().expect("test fixture");
    let token_url = format!("{}/token", fixture.base);
    let (url_a, url_b) = (format!("{}/indep-a", fixture.base), format!("{}/indep-b", fixture.base));
    let a = Arc::new(manager(root.path(), "a", &url_a, &token_url));
    let b = Arc::new(manager(root.path(), "b", &url_b, &token_url));
    let (ta, tb) = (tokio::spawn({ let a = a.clone(); async move { a.force_refresh().await } }), tokio::spawn({ let b = b.clone(); async move { b.force_refresh().await } }));
    wait_entered(&mut fixture).await;
    wait_entered(&mut fixture).await;
    assert!(is_refresh_in_progress(&url_a) && is_refresh_in_progress(&url_b));
    assert!(active_refresh_count() >= 2);
    fixture.gate.add_permits(2);
    tokio::time::timeout(Duration::from_secs(5), async { ta.await.expect("test fixture").expect("test fixture"); tb.await.expect("test fixture").expect("test fixture"); }).await.expect("test fixture");
    assert!(!is_refresh_in_progress(&url_a) && !is_refresh_in_progress(&url_b));
}

#[tokio::test]
async fn dropping_the_first_caller_does_not_cancel_the_shared_refresh() {
    let mut fixture = fixture().await;
    let root = tempfile::tempdir().expect("test fixture");
    let url = format!("{}/drop", fixture.base);
    let token_url = format!("{}/token", fixture.base);
    let first = Arc::new(manager(root.path(), "d", &url, &token_url));
    let second = Arc::new(manager(root.path(), "d", &url, &token_url));
    let one = tokio::spawn({ let first = first.clone(); async move { first.force_refresh().await } });
    wait_entered(&mut fixture).await;
    one.abort();
    let _ = one.await;
    assert!(is_refresh_in_progress(&url));
    let two = tokio::spawn({ let second = second.clone(); async move { second.force_refresh().await } });
    fixture.gate.add_permits(1);
    let tokens = tokio::time::timeout(Duration::from_secs(5), two).await.expect("test fixture").expect("test fixture").expect("test fixture");
    assert_eq!(tokens.access_token, "fixture-new");
    assert!(fixture.entered.try_recv().is_err(), "no second token request");
    assert!(!is_refresh_in_progress(&url));
}
