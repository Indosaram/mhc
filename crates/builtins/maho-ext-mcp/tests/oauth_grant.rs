//! Tests for OAuth grant continuity across token refresh (senpi#2861 / #6366).
use axum::{
    Form, Json, Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use maho_ext_mcp::{
    auth::{
        catalog_identity::{legacy_mcp_credential_identity, mcp_credential_identity},
        oauth::{begin_authorization, client_credentials_grant, complete_authorization},
        oauth_errors::OAuthFailureKind,
        oauth_grant::{
            McpGrantContext, McpTokenResponse, merge_oauth_token_response, oauth_grant_identity,
            valid_grant_proof,
        },
        oauth_provider::{McpOAuthProvider, OAuthTokens},
        oauth_refresh::McpRefreshManager,
        token_store::{McpStoredAuth, McpTokenStore},
    },
    config_schema::{Auth, AuthMode, McpServerDeclaredConfig, OAuthConfig, Transport},
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct IdpState {
    seq: u64,
    token_hits: u64,
    refresh_hits: u64,
    next_refresh_issuer: Option<String>,
    server_base: String,
}

async fn discovery_metadata(State(state): State<Arc<Mutex<IdpState>>>) -> Json<Value> {
    let base = state.lock().unwrap().server_base.clone();
    Json(json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/authorize"),
        "token_endpoint": format!("{base}/token"),
        "registration_endpoint": format!("{base}/register"),
        "code_challenge_methods_supported": ["S256"],
    }))
}

async fn authorize(
    axum::extract::Query(params): axum::extract::Query<std::collections::BTreeMap<String, String>>,
) -> impl IntoResponse {
    let redirect_uri = params.get("redirect_uri").cloned().unwrap_or_default();
    let state = params.get("state").cloned().unwrap_or_default();
    let location = format!("{redirect_uri}?code=auth-code&state={state}");
    (StatusCode::FOUND, [("location", location)])
}

async fn token_endpoint(
    State(state): State<Arc<Mutex<IdpState>>>,
    Form(form): Form<Vec<(String, String)>>,
) -> impl IntoResponse {
    let mut guard = state.lock().unwrap();
    guard.seq += 1;
    guard.token_hits += 1;
    let seq = guard.seq;
    let grant_type = form
        .iter()
        .find(|(k, _)| k == "grant_type")
        .map(|(_, v)| v.as_str())
        .unwrap_or("");
    if grant_type == "refresh_token" {
        guard.refresh_hits += 1;
    }
    let mut body = json!({
        "access_token": format!("AT_{seq}"),
        "refresh_token": format!("RT_{seq}"),
        "token_type": "Bearer",
        "expires_in": 3600,
        "scope": "read write",
    });
    if grant_type == "refresh_token"
        && let Some(issuer) = guard.next_refresh_issuer.take()
    {
        body["issuer"] = Value::String(issuer);
    }
    (StatusCode::OK, Json(body))
}

struct GrantFixture {
    mcp_url: String,
    state: Arc<Mutex<IdpState>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for GrantFixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn spawn_idp() -> GrantFixture {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let state = Arc::new(Mutex::new(IdpState {
        server_base: base.clone(),
        ..Default::default()
    }));
    let app = Router::new()
        .route(
            "/.well-known/oauth-authorization-server",
            get(discovery_metadata),
        )
        .route("/authorize", get(authorize))
        .route("/token", post(token_endpoint))
        .with_state(state.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    GrantFixture {
        mcp_url: format!("{base}/mcp"),
        state,
        task,
    }
}

fn oauth_config(url: &str) -> McpServerDeclaredConfig {
    McpServerDeclaredConfig {
        transport: Some(Transport::Http),
        url: Some(url.to_owned()),
        auth: Some(Auth::Mode(AuthMode::Oauth)),
        oauth: Some(OAuthConfig {
            client_id: Some("static-client".into()),
            scopes: Some(vec!["write".into(), "read".into()]),
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[tokio::test]
async fn proven_grant_keeps_epoch_and_catalog_identity_across_refresh_rotation() {
    let fixture = spawn_idp().await;
    let dir = tempfile::tempdir().unwrap();
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let store = McpTokenStore::new(dir.path(), "srv", &fixture.mcp_url);
    let mut provider = McpOAuthProvider::new(store.clone());
    provider.require_https = false;
    provider.client_id = Some("static-client".into());
    provider.redirect_url = Some("http://127.0.0.1:8123/callback".into());
    provider.require_registered_redirect = true;

    let begin = begin_authorization(&mut provider, &client).await.unwrap();
    let resp = client
        .get(begin.authorization_url.unwrap())
        .send()
        .await
        .unwrap();
    let location = resp
        .headers()
        .get("location")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    complete_authorization(&mut provider, &location, &client)
        .await
        .unwrap();

    let signed_in = store.read().unwrap().unwrap();
    let initial_epoch = signed_in.authorization.as_ref().unwrap().id.clone();
    assert_eq!(signed_in.scope.as_deref(), Some("read write"));
    assert_eq!(signed_in.refresh_scope.as_deref(), Some("read write"));
    assert!(signed_in.authorization.as_ref().unwrap().proof.is_some());

    let config = oauth_config(&fixture.mcp_url);
    assert_eq!(
        legacy_mcp_credential_identity(&config, "srv", Some(dir.path()), None),
        None
    );
    let initial_identity = mcp_credential_identity(&config, "srv", Some(dir.path()), None).unwrap();

    // Expire token and refresh; access_token & refresh_token rotate, but epoch & catalog identity stay identical.
    store
        .update(|r| {
            r.map(|mut r| {
                r.expires_at = Some(0.0);
                r
            })
        })
        .unwrap();
    let mut bg_provider = McpOAuthProvider::new(store.clone());
    bg_provider.require_https = false;
    bg_provider.client_id = Some("static-client".into());
    let manager = McpRefreshManager::new(Arc::new(bg_provider), client.clone());
    let refreshed = manager.refresh().await.unwrap();
    assert_ne!(refreshed.access_token, signed_in.access_token.unwrap());

    let after_refresh = store.read().unwrap().unwrap();
    assert_eq!(
        after_refresh.authorization.as_ref().unwrap().id,
        initial_epoch
    );
    assert_ne!(after_refresh.refresh_token, signed_in.refresh_token);
    assert!(
        after_refresh
            .authorization
            .as_ref()
            .unwrap()
            .proof
            .as_ref()
            .unwrap()
            .previous_credential_identity
            .is_some()
    );
    let refreshed_identity =
        mcp_credential_identity(&config, "srv", Some(dir.path()), None).unwrap();
    assert_eq!(refreshed_identity, initial_identity);

    // Fresh replacement (new sign-in via client_credentials) starts a new epoch and catalog identity.
    let mut cc_provider = McpOAuthProvider::new(store.clone());
    cc_provider.require_https = false;
    cc_provider.client_id = Some("static-client".into());
    client_credentials_grant(&cc_provider, &client)
        .await
        .unwrap();
    let replaced = store.read().unwrap().unwrap();
    assert_ne!(replaced.authorization.as_ref().unwrap().id, initial_epoch);
    let replaced_identity =
        mcp_credential_identity(&config, "srv", Some(dir.path()), None).unwrap();
    assert_ne!(replaced_identity, initial_identity);
}

#[tokio::test]
async fn refresh_rejects_mismatched_response_issuer_and_clears_grant() {
    let fixture = spawn_idp().await;
    let dir = tempfile::tempdir().unwrap();
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let store = McpTokenStore::new(dir.path(), "srv", &fixture.mcp_url);
    let mut provider = McpOAuthProvider::new(store.clone());
    provider.require_https = false;
    provider.client_id = Some("static-client".into());
    provider.redirect_url = Some("http://127.0.0.1:8123/callback".into());

    let begin = begin_authorization(&mut provider, &client).await.unwrap();
    let resp = client
        .get(begin.authorization_url.unwrap())
        .send()
        .await
        .unwrap();
    let location = resp
        .headers()
        .get("location")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    complete_authorization(&mut provider, &location, &client)
        .await
        .unwrap();

    store
        .update(|r| {
            r.map(|mut r| {
                r.expires_at = Some(0.0);
                r
            })
        })
        .unwrap();
    fixture.state.lock().unwrap().next_refresh_issuer = Some("https://wrong-issuer.invalid".into());

    let mut bg_provider = McpOAuthProvider::new(store.clone());
    bg_provider.require_https = false;
    bg_provider.client_id = Some("static-client".into());
    let manager = McpRefreshManager::new(Arc::new(bg_provider), client);
    let Err(err) = manager.refresh().await else {
        panic!("expected NeedsAuth error when issuer mismatches");
    };
    assert!(matches!(
        err,
        maho_ext_mcp::auth::oauth::OAuthRequestError::Flow(flow)
            if flow.oauth_kind == OAuthFailureKind::NeedsAuth
    ));
    assert!(store.read().unwrap().is_none());
}

#[test]
fn tampered_proof_falls_back_to_opaque_identity_and_non_fresh_response_is_unproved() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = McpGrantContext {
        server_name: "srv",
        server_url: "https://example.test/mcp",
        agent_dir: dir.path(),
        client_id: Some("cid"),
        client_metadata_url: None,
    };
    let tokens = OAuthTokens {
        access_token: "at-1".into(),
        refresh_token: Some("rt-1".into()),
        token_type: "Bearer".into(),
        expires_in: Some(3600.0),
        scope: Some("b a a".into()),
        issuer: Some("https://as.example/".into()),
    };
    let proved = merge_oauth_token_response(
        McpStoredAuth::default(),
        &ctx,
        McpTokenResponse {
            tokens: &tokens,
            resource: "https://example.test/mcp",
            fresh: true,
            presented: None,
            client_info: None,
            now: 1000.0,
        },
    );
    assert!(valid_grant_proof(&proved, &ctx).is_some());
    assert_eq!(proved.scope.as_deref(), Some("a b"));

    let mut tampered = proved.clone();
    tampered.access_token = Some("at-tampered".into());
    assert!(valid_grant_proof(&tampered, &ctx).is_none());
    assert_ne!(
        oauth_grant_identity(&tampered, &ctx),
        oauth_grant_identity(&proved, &ctx)
    );

    let unproved = merge_oauth_token_response(
        McpStoredAuth::default(),
        &ctx,
        McpTokenResponse {
            tokens: &tokens,
            resource: "https://example.test/mcp",
            fresh: false,
            presented: None,
            client_info: None,
            now: 1000.0,
        },
    );
    assert!(valid_grant_proof(&unproved, &ctx).is_none());
    assert!(unproved.authorization.as_ref().unwrap().proof.is_none());
}
