//! SC-U2 interactive step-up integration: the real `McpInteractiveStepUp` handler driving the
//! pinned re-login (`commands_auth::run_interactive_login`) and reconnect
//! (`ServerConnection::renew`) through the production `ServerConnection` -> transport -> `McpClient`
//! path. Each assertion fails if the transport merely escalates and retries stale credentials.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::header;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

use maho_ext_mcp::auth::oauth_provider::McpOAuthProvider;
use maho_ext_mcp::auth::oauth_refresh::McpRefreshManager;
use maho_ext_mcp::auth::step_up_login::McpInteractiveStepUp;
use maho_ext_mcp::auth::token_store::{McpStoredAuth, McpTokenStore};
use maho_ext_mcp::config_schema::{McpServerConfig, OAuthConfig, Transport};
use maho_ext_mcp::connection::ServerConnection;
use maho_ext_mcp::log::McpLogger;

const CHALLENGE_403: &str = "Bearer error=\"insufficient_scope\", scope=\"mcp:write\"";

#[derive(Clone, Default)]
struct InteractiveState {
    base: String,
    scopes: Arc<Mutex<Vec<String>>>,
    mcp_calls: Arc<Mutex<Vec<Option<String>>>>,
}

async fn protected_resource(State(state): State<InteractiveState>) -> Json<Value> {
    let base = state.base.clone();
    Json(json!({"resource": format!("{base}/mcp"), "authorization_servers": [base]}))
}

async fn authorization_server(State(state): State<InteractiveState>) -> Json<Value> {
    let base = state.base.clone();
    Json(json!({
        "issuer": base.clone(),
        "authorization_endpoint": format!("{base}/authorize"),
        "token_endpoint": format!("{base}/token"),
        "code_challenge_methods_supported": ["S256"],
    }))
}

async fn authorize(State(state): State<InteractiveState>, Query(params): Query<BTreeMap<String, String>>) -> Redirect {
    state.scopes.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(params.get("scope").cloned().unwrap_or_default());
    let redirect = params.get("redirect_uri").cloned().unwrap_or_default();
    let state_value = params.get("state").cloned().unwrap_or_default();
    Redirect::to(&format!("{redirect}?code=authorization-code-1&state={state_value}"))
}

async fn token() -> Json<Value> {
    Json(json!({"access_token": "fresh-access", "refresh_token": "fresh-refresh", "token_type": "Bearer", "expires_in": 3600}))
}

async fn mcp(State(state): State<InteractiveState>, headers: HeaderMap, body: String) -> Response {
    let value: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let method = value.get("method").and_then(Value::as_str).unwrap_or("").to_owned();
    let authorization = headers.get("authorization").and_then(|value| value.to_str().ok()).map(str::to_owned);
    if value.get("id").is_none() {
        return StatusCode::ACCEPTED.into_response();
    }
    if method == "initialize" {
        return Json(json!({"jsonrpc":"2.0","id":value["id"],"result":{"protocolVersion":"2025-11-25","capabilities":{},"serverInfo":{"name":"interactive","version":"1"}}})).into_response();
    }
    state.mcp_calls.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(authorization.clone());
    if authorization.as_deref() != Some("Bearer fresh-access") {
        return Response::builder()
            .status(StatusCode::FORBIDDEN)
            .header(header::WWW_AUTHENTICATE, CHALLENGE_403)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json!({"error":"challenge"}).to_string()))
            .unwrap();
    }
    Json(json!({"jsonrpc":"2.0","id":value["id"],"result":{"content":[{"type":"text","text":"ok"}],"isError":false}})).into_response()
}

#[tokio::test]
async fn interactive_step_up_logs_in_reconnects_and_retries_on_the_renewed_connection() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let base = format!("http://{address}");
    let state = InteractiveState { base: base.clone(), ..Default::default() };
    let app = Router::new()
        .route("/.well-known/oauth-protected-resource", get(protected_resource))
        .route("/.well-known/oauth-authorization-server", get(authorization_server))
        .route("/authorize", get(authorize))
        .route("/token", post(token))
        .route("/mcp", post(mcp))
        .with_state(state.clone());
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move { axum::serve(listener, app).with_graceful_shutdown(async { let _ = stopped.await; }).await.unwrap(); });

    let mcp_url = format!("{base}/mcp");
    let root = tempfile::tempdir().unwrap();
    let store = McpTokenStore::new(root.path(), "interactive", &mcp_url);
    store.write(McpStoredAuth { access_token: Some("fixture-access".into()), expires_at: Some(chrono::Utc::now().timestamp_millis() as f64 + 3_600_000.0), ..Default::default() }).unwrap();
    let mut provider = McpOAuthProvider::new(store);
    provider.require_https = false;
    let provider = Arc::new(provider);

    let config = McpServerConfig {
        enabled: Some(true),
        transport: Some(Transport::Http),
        url: Some(mcp_url.clone()),
        oauth: Some(OAuthConfig { client_id: Some("fixture-client".into()), scopes: Some(vec!["mcp:read".into()]), ..Default::default() }),
        ..Default::default()
    };
    let logger = Arc::new(Mutex::new(McpLogger::new("interactive", root.path(), None).unwrap()));
    let connection = ServerConnection::new("interactive", config.clone(), None, logger);
    connection.set_auth(Arc::new(McpRefreshManager::new(provider, reqwest::Client::new())));
    let mut step_up = McpInteractiveStepUp::new("interactive", &config, root.path(), None, Arc::downgrade(&connection));
    step_up.require_https = false;
    let browser = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
    step_up.set_browser(Arc::new(move |url: &str| {
        let browser = browser.clone();
        let url = url.to_owned();
        let _ = tokio::spawn(async move {
            if let Ok(response) = browser.get(&url).send().await
                && let Some(location) = response.headers().get("location").and_then(|value| value.to_str().ok())
            {
                let _ = browser.get(location).send().await;
            }
        });
    }));
    connection.set_step_up(Arc::new(step_up));

    let client = connection.connect().await.unwrap();
    let result = client.request("tools/call", json!({"name":"tool","arguments":{}}), Duration::from_secs(5)).await;
    assert!(result.is_ok(), "the interactive step-up must re-login, reconnect and retry: {result:?}");

    let calls = state.mcp_calls.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    assert_eq!(calls.len(), 2, "the 403 must re-login + reconnect once and retry exactly once");
    assert_eq!(calls[0].as_deref(), Some("Bearer fixture-access"));
    assert_eq!(calls[1].as_deref(), Some("Bearer fresh-access"), "the retry must carry the freshly authorized token");

    let scopes = state.scopes.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    assert_eq!(scopes.len(), 1, "exactly one authorization redirect must run");
    assert_eq!(scopes[0], "mcp:read mcp:write", "the challenge scopes must merge into the configured scopes");

    assert_eq!(connection.generation(), 1, "the step-up must perform a real reconnect");
    assert_eq!(McpTokenStore::new(root.path(), "interactive", &mcp_url).read().unwrap().unwrap().access_token.as_deref(), Some("fresh-access"));

    connection.dispose().await.unwrap();
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), server).await.unwrap().unwrap();
}
