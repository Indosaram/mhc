//! Tests for senpi#6327 / #6328: dynamic client registration reuse scoped to redirect URI.
use axum::{
    Form, Json, Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use maho_ext_mcp::{
    auth::{
        catalog_identity::mcp_credential_identity,
        commands_auth::{
            AuthCommandDeps, run_auth, run_auth_complete_command, run_auth_start_command,
        },
        context::{ServerAuthDeps, resolve_server_auth_with},
        oauth::{begin_authorization, begin_authorization_forced},
        oauth_provider::McpOAuthProvider,
        oauth_refresh::McpRefreshManager,
        token_store::McpTokenStore,
    },
    config_schema::{McpServerConfig, OAuthConfig, OAuthFlow, Transport},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

#[derive(Default)]
struct StrictIdpState {
    base: String,
    clients: BTreeMap<String, Vec<String>>,
    registrations: Vec<(String, Vec<String>)>,
    codes: BTreeMap<String, (String, String)>,
    token_seq: u64,
}

async fn strict_metadata(State(state): State<Arc<Mutex<StrictIdpState>>>) -> Json<Value> {
    let base = state.lock().unwrap().base.clone();
    Json(json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/authorize"),
        "token_endpoint": format!("{base}/token"),
        "registration_endpoint": format!("{base}/register"),
        "code_challenge_methods_supported": ["S256"],
    }))
}

async fn strict_register(
    State(state): State<Arc<Mutex<StrictIdpState>>>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let mut guard = state.lock().unwrap();
    let seq = guard.registrations.len() + 1;
    let client_id = format!("dcr-{seq}");
    let redirect_uris: Vec<String> = body
        .get("redirect_uris")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    guard
        .clients
        .insert(client_id.clone(), redirect_uris.clone());
    guard
        .registrations
        .push((client_id.clone(), redirect_uris.clone()));
    (
        StatusCode::CREATED,
        Json(json!({
            "client_id": client_id,
            "redirect_uris": redirect_uris,
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
        })),
    )
}

async fn strict_authorize(
    State(state): State<Arc<Mutex<StrictIdpState>>>,
    axum::extract::Query(params): axum::extract::Query<BTreeMap<String, String>>,
) -> impl IntoResponse {
    let mut guard = state.lock().unwrap();
    let client_id = params.get("client_id").cloned().unwrap_or_default();
    let redirect_uri = params.get("redirect_uri").cloned().unwrap_or_default();
    let csrf = params.get("state").cloned().unwrap_or_default();
    let Some(registered) = guard.clients.get(&client_id) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid_client"})),
        )
            .into_response();
    };
    if !registered.iter().any(|uri| uri == &redirect_uri) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid_request", "error_description": "Mismatching Redirect URL"})),
        )
            .into_response();
    }
    guard.token_seq += 1;
    let code = format!("code-{}", guard.token_seq);
    guard
        .codes
        .insert(code.clone(), (client_id, redirect_uri.clone()));
    let location = format!("{redirect_uri}?code={code}&state={csrf}");
    (StatusCode::FOUND, [("location", location)]).into_response()
}

async fn strict_token(
    State(state): State<Arc<Mutex<StrictIdpState>>>,
    Form(form): Form<Vec<(String, String)>>,
) -> impl IntoResponse {
    let mut guard = state.lock().unwrap();
    guard.token_seq += 1;
    let seq = guard.token_seq;
    let grant_type = form
        .iter()
        .find(|(k, _)| k == "grant_type")
        .map(|(_, v)| v.as_str())
        .unwrap_or("");
    if grant_type == "authorization_code" {
        let code = form
            .iter()
            .find(|(k, _)| k == "code")
            .map(|(_, v)| v.as_str())
            .unwrap_or("");
        if !guard.codes.contains_key(code) {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "invalid_grant"})),
            )
                .into_response();
        }
    }
    (
        StatusCode::OK,
        Json(json!({
            "access_token": format!("SENTINEL_AT_{seq}"),
            "refresh_token": format!("SENTINEL_RT_{seq}"),
            "token_type": "Bearer",
            "expires_in": 3600,
            "scope": "mcp",
        })),
    )
        .into_response()
}

struct StrictIdp {
    base: String,
    mcp_url: String,
    state: Arc<Mutex<StrictIdpState>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for StrictIdp {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn spawn_strict_idp() -> StrictIdp {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let state = Arc::new(Mutex::new(StrictIdpState {
        base: base.clone(),
        ..Default::default()
    }));
    let app = Router::new()
        .route(
            "/.well-known/oauth-authorization-server",
            get(strict_metadata),
        )
        .route("/register", post(strict_register))
        .route("/authorize", get(strict_authorize))
        .route("/token", post(strict_token))
        .with_state(state.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    StrictIdp {
        mcp_url: format!("{base}/mcp"),
        base,
        state,
        task,
    }
}

fn no_redirect_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

fn deps_for(dir: &std::path::Path, mcp_url: &str, client: reqwest::Client) -> AuthCommandDeps {
    AuthCommandDeps {
        server_name: "fixture".into(),
        config: McpServerConfig {
            transport: Some(Transport::Http),
            url: Some(mcp_url.into()),
            enabled: Some(true),
            ..Default::default()
        },
        agent_dir: dir.to_path_buf(),
        has_ui: true,
        notify: Arc::new(|_, _| {}),
        callback_url: None,
        open_browser: None,
        on_reconnect: Arc::new(|| Box::pin(async {})),
        pending: Arc::default(),
        interactive_guard: None,
        client,
    }
}

fn resolve_bg_provider(
    dir: &std::path::Path,
    config: &McpServerConfig,
    client: reqwest::Client,
) -> McpOAuthProvider {
    let mut plan = resolve_server_auth_with(ServerAuthDeps {
        server_name: "fixture",
        config,
        agent_dir: Some(dir),
        logger: None,
        redirect_url: None,
        on_redirect: None,
        client,
    });
    plan.refresh = None;
    let provider = plan.provider.take().unwrap();
    drop(plan);
    Arc::try_unwrap(provider).ok().expect("sole provider")
}

#[tokio::test]
async fn interactive_auth_after_background_connect_registers_bound_loopback_and_stages_pending() {
    let idp = spawn_strict_idp().await;
    let dir = tempfile::tempdir().unwrap();
    let client = no_redirect_client();
    let deps = deps_for(dir.path(), &idp.mcp_url, client.clone());
    let store = McpTokenStore::new(dir.path(), "fixture", &idp.mcp_url);

    // 1. Background connect registers the port-0 placeholder (via resolve_server_auth_with(redirect_url: None)).
    let mut bg = resolve_bg_provider(dir.path(), &deps.config, client.clone());
    bg.require_https = false;
    assert!(
        !begin_authorization(&mut bg, &client)
            .await
            .unwrap()
            .authorized
    );
    assert_eq!(idp.state.lock().unwrap().registrations.len(), 1);
    assert_eq!(
        idp.state.lock().unwrap().registrations[0].1,
        vec!["http://127.0.0.1:0/callback"]
    );

    // 2. Interactive `/mcp auth` binds a real loopback port and registers its bound URI instead of reusing port 0.
    // (`run_loopback_auth` enforces `require_registered_redirect = true`.)
    let mut loopback = McpOAuthProvider::new(store.clone());
    loopback.require_https = false;
    maho_ext_mcp::auth::commands_auth::run_loopback_auth(loopback, None, &client, |url| {
        let http = client.clone();
        async move {
            let auth_resp = http.get(url).send().await.unwrap();
            assert_eq!(auth_resp.status(), StatusCode::FOUND);
            let callback = auth_resp
                .headers()
                .get("location")
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned();
            http.get(callback).send().await.unwrap();
            Ok(())
        }
    })
    .await
    .unwrap();
    assert_eq!(idp.state.lock().unwrap().registrations.len(), 2);
    let signed_in = store.read().unwrap().unwrap();
    let signed_in_client = signed_in.client_info.clone().unwrap()["client_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let signed_in_epoch = signed_in.authorization.as_ref().unwrap().id.clone();
    let signed_in_identity =
        mcp_credential_identity(&deps.config, "fixture", Some(dir.path()), None).unwrap();

    // 3. Starting another authorize flow with a new redirect URI stages `pending_client_info`
    // while keeping the signed-in client + refresh token active until code exchange succeeds.
    let mut paste = McpOAuthProvider::new(store.clone());
    paste.require_https = false;
    paste.redirect_url = Some("http://127.0.0.1:44444/callback".into());
    paste.require_registered_redirect = true;
    let mut pending = BTreeMap::new();
    let paste_url = run_auth_start_command_with(&mut paste, &mut pending, &client).await;
    assert_eq!(idp.state.lock().unwrap().registrations.len(), 3);

    // Signed-in client, refresh token, epoch and identity are untouched while abandoned!
    let during_pending = store.read().unwrap().unwrap();
    assert_eq!(
        during_pending.client_info.as_ref().unwrap()["client_id"].as_str(),
        Some(signed_in_client.as_str())
    );
    assert!(during_pending.pending_client_info.is_some());
    assert_eq!(during_pending.refresh_token, signed_in.refresh_token);
    assert_eq!(
        during_pending.authorization.as_ref().unwrap().id,
        signed_in_epoch
    );
    assert_eq!(
        mcp_credential_identity(&deps.config, "fixture", Some(dir.path()), None).unwrap(),
        signed_in_identity
    );

    // Background refresh still works with the signed-in client while another flow is pending!
    store
        .update(|r| {
            r.map(|mut r| {
                r.expires_at = Some(0.0);
                r
            })
        })
        .unwrap();
    let mut bg_refresh = resolve_bg_provider(dir.path(), &deps.config, client.clone());
    bg_refresh.require_https = false;
    let refreshed = McpRefreshManager::new(Arc::new(bg_refresh), client.clone())
        .refresh()
        .await
        .unwrap();
    assert!(refreshed.access_token.starts_with("SENTINEL_AT_"));
    assert_eq!(
        store
            .read()
            .unwrap()
            .unwrap()
            .authorization
            .as_ref()
            .unwrap()
            .id,
        signed_in_epoch
    );

    // 4. Completing the staged flow promotes `pending_client_info` to `client_info` and replaces the grant!
    let auth_resp = client.get(&paste_url).send().await.unwrap();
    let callback = auth_resp
        .headers()
        .get("location")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    maho_ext_mcp::auth::commands_auth::run_auth_complete(
        "fixture",
        &callback,
        &mut pending,
        &client,
    )
    .await
    .unwrap();
    let promoted = store.read().unwrap().unwrap();
    assert!(promoted.pending_client_info.is_none());
    assert_eq!(
        promoted.client_info.as_ref().unwrap()["client_id"].as_str(),
        Some("dcr-3")
    );
    assert_ne!(promoted.authorization.as_ref().unwrap().id, signed_in_epoch);
    assert_ne!(
        mcp_credential_identity(&deps.config, "fixture", Some(dir.path()), None).unwrap(),
        signed_in_identity
    );
    drop(deps);
    let _ = (run_auth, run_auth_start_command, run_auth_complete_command);
}

async fn run_auth_start_command_with(
    provider: &mut McpOAuthProvider,
    pending: &mut BTreeMap<String, McpOAuthProvider>,
    client: &reqwest::Client,
) -> String {
    let begin = begin_authorization_forced(provider, client).await.unwrap();
    let url = begin.authorization_url.unwrap().to_string();
    let taken = std::mem::replace(provider, McpOAuthProvider::new(provider.store.clone()));
    pending.insert("fixture".into(), taken);
    url
}

#[tokio::test]
async fn client_credentials_reuses_stored_registration_and_echo_matches_sent_redirect() {
    let idp = spawn_strict_idp().await;
    let dir = tempfile::tempdir().unwrap();
    let client = no_redirect_client();
    let store = McpTokenStore::new(dir.path(), "fixture", &idp.mcp_url);

    // Rewritten echo matching: saved with redirectUrisSent = 127.0.0.1 even if server echoed localhost.
    let mut flow = McpOAuthProvider::new(store.clone());
    flow.redirect_url = Some("http://127.0.0.1:2/callback".into());
    flow.require_registered_redirect = true;
    flow.save_client_information(
        json!({"client_id": "dcr-echo", "redirect_uris": ["http://localhost:2/callback"]}),
        Some(&idp.base),
    )
    .unwrap();
    assert_eq!(
        flow.client_information().unwrap().unwrap()["client_id"],
        "dcr-echo"
    );

    // client_credentials (`require_registered_redirect = false`) reuses stored registration.
    let mut cc = McpOAuthProvider::new(store.clone());
    cc.require_https = false;
    cc.require_registered_redirect = false;
    maho_ext_mcp::auth::oauth::client_credentials_grant(&cc, &client)
        .await
        .unwrap();
    assert!(
        store
            .read()
            .unwrap()
            .unwrap()
            .access_token
            .unwrap()
            .starts_with("SENTINEL_AT_")
    );

    // Config with client_credentials flow builds provider with require_registered_redirect = false.
    let cc_config = McpServerConfig {
        transport: Some(Transport::Http),
        url: Some(idp.mcp_url.clone()),
        oauth: Some(OAuthConfig {
            flow: Some(OAuthFlow::ClientCredentials),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bg = resolve_bg_provider(dir.path(), &cc_config, client);
    assert!(!bg.require_registered_redirect);
}
