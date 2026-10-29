use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use axum::{
    Form, Json, Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use maho_ext_mcp::{
    auth::{
        challenge::McpAuthChallengeHandler,
        challenge_login::{BrowserOpener, InteractiveChallengeHandler, ProviderFactory},
        oauth_provider::McpOAuthProvider,
        oauth_refresh::McpRefreshManager,
        token_store::{McpStoredAuth, McpTokenStore},
    },
    config_schema::{McpServerConfig, Transport},
    connection::ServerConnection,
    log::McpLogger,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Clone, Default)]
struct Idp {
    base: Arc<Mutex<String>>,
    authorize_queries: Arc<Mutex<Vec<BTreeMap<String, String>>>>,
    bearers: Arc<Mutex<Vec<String>>>,
    token_hits: Arc<Mutex<usize>>,
}
async fn protected_resource() -> StatusCode {
    StatusCode::NOT_FOUND
}
async fn metadata(State(idp): State<Idp>) -> Json<Value> {
    let base = idp.base.lock().expect("test fixture").clone();
    Json(json!({"issuer":base,"authorization_endpoint":format!("{base}/authorize"),"token_endpoint":format!("{base}/token"),"code_challenge_methods_supported":["S256"]}))
}
async fn authorize(State(idp): State<Idp>, Query(query): Query<BTreeMap<String, String>>) -> Redirect {
    let redirect = query["redirect_uri"].clone();
    let state = query["state"].clone();
    idp.authorize_queries.lock().expect("test fixture").push(query);
    Redirect::to(&format!("{redirect}?code=login-code&state={state}"))
}
async fn token(State(idp): State<Idp>, Form(_form): Form<Vec<(String, String)>>) -> Json<Value> {
    *idp.token_hits.lock().expect("test fixture") += 1;
    Json(json!({"access_token":"new-token","refresh_token":"rt-new","token_type":"Bearer","expires_in":3600}))
}
async fn mcp(State(idp): State<Idp>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    let bearer = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("").to_owned();
    idp.bearers.lock().expect("test fixture").push(bearer);
    if body.get("id").is_none() {
        return StatusCode::ACCEPTED.into_response();
    }
    Json(json!({"jsonrpc":"2.0","id":body["id"],"result":{"protocolVersion":"2025-11-25","capabilities":{},"serverInfo":{"name":"login","version":"1"}}})).into_response()
}

#[tokio::test]
async fn forced_interactive_login_replaces_a_fresh_token_and_renews_the_connection() {
    let idp = Idp::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("test fixture");
    let base = format!("http://{}", listener.local_addr().expect("test fixture"));
    *idp.base.lock().expect("test fixture") = base.clone();
    let app = Router::new()
        .route("/.well-known/oauth-protected-resource", get(protected_resource))
        .route("/.well-known/oauth-authorization-server", get(metadata))
        .route("/authorize", get(authorize))
        .route("/token", post(token))
        .route("/mcp", post(mcp))
        .with_state(idp.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("test fixture");
    });
    let url = format!("{base}/mcp");
    let root = tempfile::tempdir().expect("test fixture");
    let store = McpTokenStore::new(root.path(), "login", &url);
    store
        .write(McpStoredAuth { access_token: Some("fresh-old".into()), expires_at: Some(chrono::Utc::now().timestamp_millis() as f64 + 3_600_000.0), ..Default::default() })
        .expect("test fixture");
    let make_provider = {
        let store = store.clone();
        move || {
            let mut provider = McpOAuthProvider::new(store.clone());
            provider.require_https = false;
            provider.client_id = Some("fixture-client".into());
            provider.scopes = Some(vec!["base".into()]);
            provider
        }
    };
    let manager = Arc::new(McpRefreshManager::new(Arc::new(make_provider()), reqwest::Client::new()));
    let logger = Arc::new(Mutex::new(McpLogger::new("login", root.path(), None).expect("test fixture")));
    let connection = ServerConnection::new(
        "login",
        McpServerConfig { enabled: Some(true), transport: Some(Transport::Http), url: Some(url.clone()), connect_timeout_ms: Some(10_000.0), ..Default::default() },
        None,
        logger,
    );
    connection.set_auth(manager);

    // The injected opener follows the authorization redirect like a browser would.
    let opened = Arc::new(Mutex::new(Vec::<url::Url>::new()));
    let (done_sender, mut done) = tokio::sync::mpsc::unbounded_channel::<Result<(), String>>();
    let opener: BrowserOpener = {
        let opened = opened.clone();
        Arc::new(move |target: &url::Url| {
            opened.lock().expect("test fixture").push(target.clone());
            let target = target.clone();
            let done = done_sender.clone();
            tokio::spawn(async move {
                let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().expect("test fixture");
                let result = async {
                    let response = client.get(target).send().await.map_err(|e| e.to_string())?;
                    let location = response.headers().get("location").and_then(|v| v.to_str().ok()).ok_or("no redirect")?.to_owned();
                    client.get(location).send().await.map_err(|e| e.to_string())?.error_for_status().map_err(|e| e.to_string())?;
                    Ok::<(), String>(())
                }
                .await;
                let _ = done.send(result);
            });
            Ok(())
        })
    };
    let factory: ProviderFactory = Arc::new(move || Ok(make_provider()));
    let handler = InteractiveChallengeHandler::new(factory, Arc::downgrade(&connection), opener, None);

    tokio::time::timeout(Duration::from_secs(20), handler.reauthorize(vec!["base".into(), "extra".into()])).await.expect("test fixture").expect("test fixture");
    tokio::time::timeout(Duration::from_secs(5), done.recv()).await.expect("test fixture").expect("test fixture").expect("test fixture");

    assert_eq!(opened.lock().expect("test fixture").len(), 1);
    assert_eq!(*idp.token_hits.lock().expect("test fixture"), 1, "login ran although a fresh token was stored");
    assert_eq!(store.read().expect("test fixture").expect("test fixture").access_token.as_deref(), Some("new-token"));
    let scope = idp.authorize_queries.lock().expect("test fixture")[0].get("scope").cloned().expect("test fixture");
    assert_eq!(scope.split(' ').collect::<Vec<_>>(), ["base", "extra"]);

    let client = tokio::time::timeout(Duration::from_secs(20), handler.renew()).await.expect("test fixture").expect("test fixture");
    assert!(idp.bearers.lock().expect("test fixture").iter().any(|bearer| bearer == "Bearer new-token"));
    assert!(Arc::ptr_eq(&client, &connection.client().expect("test fixture")));
    connection.dispose().await.expect("test fixture");
    server.abort();
}
