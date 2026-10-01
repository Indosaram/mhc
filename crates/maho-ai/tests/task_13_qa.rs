//! Todo 13 QA evidence: the plan's happy and failure scenarios for the auth lane.
//! `cargo nextest run -p maho-ai --test task_13_qa`

use async_trait::async_trait;
use axum::extract::{Form, Query, State};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use maho_ai::auth::credential_store::InMemoryCredentialStore;
use maho_ai::auth::oauth::chatgpt_subscription::ChatGptSubscriptionOAuth;
use maho_ai::auth::oauth::transport::{HttpRequest, HttpResponse, OAuthTransport};
use maho_ai::auth::resolve::{ModelsError, resolve_stored_oauth};
use maho_ai::auth::types::{
    AccountLoginReceipt, AuthEvent, AuthInteraction, AuthPrompt, AuthPromptKind, Credential, CredentialStore, OAuthAuth,
    OAuthCredential, ProviderAuthInteraction,
};
use maho_ai::utils::abort::{AbortController, AbortSignal};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const PROVIDER_ID: &str = "chatgpt-subscription";
const ACCOUNT_ID: &str = "acct_qa_13";
const POLL_INTERVAL: Duration = Duration::from_millis(5);
const POLL_ATTEMPTS: usize = 1000;

/// senpi writes these five keys for a stored OAuth credential:
/// `packages/ai/src/auth/types.ts` (`OAuthCredential extends OAuthCredentials`) plus the
/// `accountId` that chatgpt-subscription's `credentialsFromToken` adds.
const SENPI_OAUTH_CREDENTIAL_KEYS: [&str; 5] = ["access", "accountId", "expires", "refresh", "type"];

/// senpi's `AUTH_FILE_WRITE_OPTIONS` staging from `packages/coding-agent/src/core/auth-storage.ts`:
/// write a fresh 0o600 file and rename it over the store.
fn write_auth_file(path: &Path, content: &str) {
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&temporary, content).expect("stage the auth file");
    std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600)).expect("restrict the mode");
    std::fs::rename(&temporary, path).expect("rename the auth file into place");
}

fn redact(value: &str) -> String {
    format!("<redacted {} chars>", value.len())
}

#[derive(Default)]
struct MockState {
    challenge: Mutex<Option<String>>,
    token_requests: Mutex<Vec<HashMap<String, String>>>,
}

fn access_token() -> String {
    use base64::Engine as _;
    let encode = |value: Value| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.to_string());
    let header = encode(json!({ "alg": "none" }));
    let payload =
        encode(json!({ "https://api.openai.com/auth": { "chatgpt_account_id": ACCOUNT_ID } }));
    format!("{header}.{payload}.signature")
}

fn pkce_challenge(verifier: &str) -> String {
    use base64::Engine as _;
    use sha2::{Digest, Sha256};
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Routes match on the path alone, so this file never restates the flow's absolute URLs.
async fn start_mock_authorization_server(state: Arc<MockState>) -> (String, tokio::task::JoinHandle<()>) {
    async fn authorize(
        State(state): State<Arc<MockState>>,
        Query(query): Query<HashMap<String, String>>,
    ) -> impl IntoResponse {
        *state.challenge.lock().unwrap_or_else(|p| p.into_inner()) = query.get("code_challenge").cloned();
        Json(json!({
            "code": "qa-authorization-code",
            "state": query.get("state").cloned(),
            "code_challenge_method": query.get("code_challenge_method").cloned(),
            "client_id": query.get("client_id").cloned(),
        }))
    }

    async fn token(
        State(state): State<Arc<MockState>>,
        Form(form): Form<HashMap<String, String>>,
    ) -> impl IntoResponse {
        state.token_requests.lock().unwrap_or_else(|p| p.into_inner()).push(form.clone());
        if form.get("grant_type").map(String::as_str) == Some("refresh_token") {
            return (axum::http::StatusCode::BAD_REQUEST, Json(json!({ "error": "invalid_grant" })));
        }

        let expected = state.challenge.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let verifier = form.get("code_verifier").cloned().unwrap_or_default();
        if expected.as_deref() != Some(pkce_challenge(&verifier).as_str()) {
            return (axum::http::StatusCode::BAD_REQUEST, Json(json!({ "error": "invalid_grant" })));
        }

        (
            axum::http::StatusCode::OK,
            Json(json!({
                "access_token": access_token(),
                "refresh_token": "qa-refresh-token",
                "expires_in": 3600,
            })),
        )
    }

    let app = Router::new()
        .route("/oauth/authorize", get(authorize))
        .route("/oauth/token", post(token))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.expect("bind the mock server");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (base, task)
}

struct MockServerTransport {
    base: String,
}

#[async_trait]
impl OAuthTransport for MockServerTransport {
    async fn execute(&self, request: HttpRequest, _signal: &AbortSignal) -> anyhow::Result<HttpResponse> {
        let parsed = url::Url::parse(&request.url)?;
        let target = format!("{}{}", self.base, parsed.path());
        let method = reqwest::Method::from_bytes(request.method.as_bytes())?;
        let mut builder = reqwest::Client::new().request(method, &target);
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        if let Some(body) = &request.body {
            builder = builder.body(body.clone());
        }
        let response = builder.send().await?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| (name.as_str().to_string(), value.to_str().unwrap_or_default().to_string()))
            .collect();
        Ok(HttpResponse { status, headers, body: response.text().await? })
    }
}

struct BrowserInteraction {
    signal: AbortSignal,
    authorize_url: Arc<Mutex<Option<String>>>,
    redirect: Arc<Mutex<Option<String>>>,
}

#[async_trait]
impl AuthInteraction for BrowserInteraction {
    fn signal(&self) -> Option<AbortSignal> {
        Some(self.signal.clone())
    }

    fn on_account_committed(&self, _receipt: AccountLoginReceipt) {}

    async fn prompt(&self, prompt: AuthPrompt) -> anyhow::Result<String> {
        match prompt.kind {
            AuthPromptKind::Select { .. } => Ok("browser".into()),
            AuthPromptKind::ManualCode { .. } => {
                for _ in 0..POLL_ATTEMPTS {
                    if let Some(value) = self.redirect.lock().unwrap_or_else(|p| p.into_inner()).clone() {
                        return Ok(value);
                    }
                    tokio::time::sleep(POLL_INTERVAL).await;
                }
                anyhow::bail!("the browser leg never produced a redirect URL")
            }
            other => anyhow::bail!("unexpected prompt: {other:?}"),
        }
    }

    fn notify(&self, event: AuthEvent) {
        if let AuthEvent::AuthUrl { url, .. } = event {
            *self.authorize_url.lock().unwrap_or_else(|p| p.into_inner()) = Some(url);
        }
    }
}

async fn wait_for_authorize_url(url: &Arc<Mutex<Option<String>>>) -> String {
    for _ in 0..POLL_ATTEMPTS {
        if let Some(value) = url.lock().unwrap_or_else(|p| p.into_inner()).clone() {
            return value;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("the flow never notified an authorize URL");
}

#[tokio::test]
async fn pkce_login_against_a_mock_authorization_server_writes_senpis_auth_json_fields() {
    let state = Arc::new(MockState::default());
    let (base, server) = start_mock_authorization_server(state.clone()).await;
    let oauth = ChatGptSubscriptionOAuth::new(Arc::new(MockServerTransport { base: base.clone() }));

    let authorize_url = Arc::new(Mutex::new(None));
    let redirect = Arc::new(Mutex::new(None));
    let signal = AbortController::new().signal();
    let interaction = ProviderAuthInteraction::new(
        signal.clone(),
        Arc::new(BrowserInteraction {
            signal: signal.clone(),
            authorize_url: authorize_url.clone(),
            redirect: redirect.clone(),
        }),
    );

    let login = tokio::spawn(async move { oauth.login(&interaction).await });

    let url = wait_for_authorize_url(&authorize_url).await;
    let parsed = url::Url::parse(&url).expect("authorize url");
    let params: HashMap<String, String> =
        parsed.query_pairs().map(|(key, value)| (key.into_owned(), value.into_owned())).collect();
    assert_eq!(params.get("code_challenge_method").map(String::as_str), Some("S256"));
    assert_eq!(params.get("response_type").map(String::as_str), Some("code"));
    assert!(params.get("code_challenge").is_some_and(|value| !value.is_empty()));
    assert_eq!(params.get("originator").map(String::as_str), Some("senpi"));

    let authorize_response: Value =
        reqwest::get(format!("{base}/oauth/authorize?{}", parsed.query().unwrap_or_default()))
            .await
            .expect("authorize request")
            .json()
            .await
            .expect("authorize json");
    let code = authorize_response["code"].as_str().expect("code").to_string();
    let state_param = authorize_response["state"].as_str().expect("state").to_string();
    let redirect_uri = params.get("redirect_uri").expect("redirect_uri").clone();
    *redirect.lock().unwrap_or_else(|p| p.into_inner()) =
        Some(format!("{redirect_uri}?code={code}&state={state_param}"));

    let credential = login.await.expect("join").expect("login completes");

    let requests = state.token_requests.lock().unwrap_or_else(|p| p.into_inner()).clone();
    assert_eq!(requests.len(), 1, "exactly one token exchange");
    assert_eq!(requests[0].get("grant_type").map(String::as_str), Some("authorization_code"));
    assert!(requests[0].contains_key("code_verifier"));

    let store = json!({ PROVIDER_ID: Credential::OAuth(credential) });
    let directory = tempfile::tempdir().expect("temp dir");
    let auth_path = directory.path().join("auth.json");
    write_auth_file(&auth_path, &serde_json::to_string_pretty(&store).expect("serialize"));

    let written: Value = serde_json::from_str(&std::fs::read_to_string(&auth_path).expect("read")).expect("parse");
    let entry = written[PROVIDER_ID].as_object().expect("the credential object");
    let mut keys: Vec<&str> = entry.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, SENPI_OAUTH_CREDENTIAL_KEYS);
    assert_eq!(entry["type"], "oauth");
    assert_eq!(entry["accountId"], ACCOUNT_ID);
    assert!(entry["expires"].as_f64().is_some_and(|expires| expires > 0.0));

    let mode = std::fs::metadata(&auth_path).expect("stat").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);

    println!("QA happy path: PKCE authorization-code flow against a local mock authorization server");
    println!("  authorize: code_challenge_method=S256, response_type=code, originator=senpi");
    println!("  token exchange: grant_type=authorization_code, code_verifier verified by the mock");
    println!("  auth.json top-level key: {PROVIDER_ID}");
    println!("  auth.json credential keys: {keys:?} (senpi: {SENPI_OAUTH_CREDENTIAL_KEYS:?})");
    println!("  auth.json mode: {mode:o}");
    println!("  access: {}", redact(entry["access"].as_str().unwrap_or_default()));
    println!("  refresh: {}", redact(entry["refresh"].as_str().unwrap_or_default()));
    println!("  accountId: {ACCOUNT_ID}");

    server.abort();
}

#[tokio::test]
async fn expired_refresh_token_yields_senpis_relogin_guidance() {
    let state = Arc::new(MockState::default());
    let (base, server) = start_mock_authorization_server(state.clone()).await;

    let store: Arc<dyn CredentialStore> = Arc::new(InMemoryCredentialStore::new());
    let expired = OAuthCredential::new("qa-expired-access", "qa-expired-refresh", 0.0)
        .with_extra("accountId", Value::String(ACCOUNT_ID.into()));
    store
        .modify(
            PROVIDER_ID,
            Box::new(move |_| {
                let expired = expired.clone();
                Box::pin(async move { Ok(Some(Credential::OAuth(expired))) })
            }),
            None,
        )
        .await
        .expect("seed the store");

    let oauth: Arc<dyn OAuthAuth> = Arc::new(ChatGptSubscriptionOAuth::new(Arc::new(MockServerTransport { base })));
    let signal = AbortController::new().signal();
    let stored = Credential::OAuth(OAuthCredential::new("qa-expired-access", "qa-expired-refresh", 0.0));

    let error: ModelsError = resolve_stored_oauth(
        &store,
        PROVIDER_ID,
        &oauth,
        &stored,
        None,
        1_700_000_000_000.0,
        300_000.0,
        &signal,
    )
    .await
    .expect_err("an expired refresh token must fail resolution");

    assert_eq!(error.code, "oauth");
    assert!(error.message.starts_with("OAuth refresh failed for chatgpt-subscription"), "{}", error.message);
    assert!(error.message.contains("invalid_grant"), "{}", error.message);

    let requests = state.token_requests.lock().unwrap_or_else(|p| p.into_inner()).clone();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].get("grant_type").map(String::as_str), Some("refresh_token"));

    println!("QA failure path: expired refresh token");
    println!("  mock /oauth/token answered 400 {{\"error\":\"invalid_grant\"}}");
    println!("  ModelsError code: {}", error.code);
    println!("  ModelsError message: {}", error.message);
    println!("  senpi source: resolve.ts oauthRefreshModelsError -> \"OAuth refresh failed for ${{providerId}}\"");
    println!("  access: {}", redact("qa-expired-access"));
    println!("  refresh: {}", redact("qa-expired-refresh"));

    server.abort();
}
