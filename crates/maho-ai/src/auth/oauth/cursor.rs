//! Port of senpi packages/ai/src/auth/oauth/cursor.ts.
//!
//! Cursor OAuth flow (Cursor Pro/Ultra/Teams subscription).
//!
//! Cursor uses a browser deep-link + poll handshake instead of a device-code or
//! loopback-callback grant: the CLI opens `https://cursor.com/loginDeepControl` with a PKCE S256
//! challenge and a request uuid, the user approves the login in the browser, and the CLI polls
//! `https://api2.cursor.sh/auth/poll` with the uuid and PKCE verifier until the tokens are
//! released. Refresh exchanges the stored refresh token (or a dashboard user API key) at
//! `auth/exchange_user_api_key` for a fresh session JWT.

use async_trait::async_trait;
use base64::Engine as _;
use serde_json::{Map, Value};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use url::Url;

use crate::utils::abort::AbortSignal;

use crate::auth::oauth::pkce::generate_pkce;
use crate::auth::types::{AuthEvent, ModelAuth, OAuthAuth, OAuthCredential, ProviderAuthInteraction};

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

const CURSOR_LOGIN_URL: &str = "https://cursor.com/loginDeepControl";
const CURSOR_POLL_URL: &str = "https://api2.cursor.sh/auth/poll";
const CURSOR_REFRESH_URL: &str = "https://api2.cursor.sh/auth/exchange_user_api_key";

const POLL_INITIAL_INTERVAL_MS: f64 = 1000.0;
const POLL_MAX_INTERVAL_MS: f64 = 10_000.0;
const POLL_BACKOFF_MULTIPLIER: f64 = 1.2;
// ~24 minutes of wall-clock budget once the interval has backed off to its cap.
const POLL_MAX_ATTEMPTS: u32 = 150;
// Transient failures (network errors, 5xx) are tolerated while the user is still completing the
// browser step; a pending poll resets the counter.
const POLL_MAX_CONSECUTIVE_TRANSIENT_FAILURES: u32 = 3;
// Refresh slightly before the JWT `exp` so a token cannot die mid-request.
const REFRESH_SKEW_MS: i64 = 5 * 60 * 1000;
const DEFAULT_TOKEN_LIFETIME_MS: i64 = 60 * 60 * 1000;
const CANCEL_MESSAGE: &str = "Login cancelled";

/// A Cursor OAuth failure. `Display` is the port of the TS `Error` message text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct CursorOAuthError {
    message: String,
}

impl CursorOAuthError {
    pub fn new(message: impl Into<String>) -> Self {
        Self { message: message.into() }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

/// One `fetch` call: the port of the `(url, init)` pair the TS flow passes to global `fetch`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorRequest {
    pub url: String,
    pub method: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

/// One `fetch` result: the status and the raw body text (`response.json()` runs on the text).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorResponse {
    pub status: u16,
    pub body_text: String,
}

/// The platform seams the TS flow takes from globals: `fetch`, `setTimeout`/`clearTimeout` with
/// an `AbortSignal`, and `Date.now`. Rust has no globals to stub, so they are injected; the
/// default implementation is the real network and the real clock.
pub trait CursorTransport: Send + Sync {
    fn now_ms(&self) -> i64;
    fn sleep<'a>(&'a self, ms: u64, signal: &'a AbortSignal) -> BoxFuture<'a, Result<(), CursorOAuthError>>;
    fn fetch<'a>(
        &'a self,
        request: CursorRequest,
        signal: &'a AbortSignal,
    ) -> BoxFuture<'a, Result<CursorResponse, String>>;
}

/// `fetch` over reqwest, `setTimeout` over tokio, `Date.now` over the system clock.
pub struct HttpCursorTransport {
    client: reqwest::Client,
}

impl Default for HttpCursorTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpCursorTransport {
    pub fn new() -> Self {
        Self { client: reqwest::Client::new() }
    }

    pub fn with_client(client: reqwest::Client) -> Self {
        Self { client }
    }
}

impl CursorTransport for HttpCursorTransport {
    fn now_ms(&self) -> i64 {
        crate::utils::diagnostics::now_ms()
    }

    fn sleep<'a>(&'a self, ms: u64, signal: &'a AbortSignal) -> BoxFuture<'a, Result<(), CursorOAuthError>> {
        Box::pin(async move {
            if signal.aborted() {
                return Err(CursorOAuthError::new(CANCEL_MESSAGE));
            }
            tokio::select! {
                () = tokio::time::sleep(Duration::from_millis(ms)) => Ok(()),
                () = signal.cancelled() => Err(CursorOAuthError::new(CANCEL_MESSAGE)),
            }
        })
    }

    fn fetch<'a>(
        &'a self,
        request: CursorRequest,
        signal: &'a AbortSignal,
    ) -> BoxFuture<'a, Result<CursorResponse, String>> {
        Box::pin(async move {
            let method = reqwest::Method::from_bytes(request.method.as_bytes()).map_err(|error| error.to_string())?;
            let mut builder = self.client.request(method, request.url.as_str());
            for (name, value) in &request.headers {
                builder = builder.header(name.as_str(), value.as_str());
            }
            if let Some(body) = request.body {
                builder = builder.body(body);
            }
            let response = crate::utils::abort::race_with_abort_signal(builder.send(), signal)
                .await
                .map_err(|_reason| CANCEL_MESSAGE.to_owned())?
                .map_err(|error| error.to_string())?;
            let status = response.status().as_u16();
            let body_text = response.text().await.unwrap_or_default();
            Ok(CursorResponse { status, body_text })
        })
    }
}

fn required_string(body: &Map<String, Value>, field: &str) -> Result<String, CursorOAuthError> {
    match body.get(field) {
        Some(Value::String(value)) if !value.is_empty() => Ok(value.clone()),
        _ => Err(CursorOAuthError::new(format!("Invalid Cursor OAuth response field: {field}"))),
    }
}

/// Builds a safe error detail from a Cursor error response: HTTP status plus any short
/// server-provided error strings. Never echoes the raw body, which could carry token material
/// into logs.
fn describe_failure(status: u16, body: Option<&Map<String, Value>>) -> String {
    let detail = ["error", "error_description", "message"]
        .iter()
        .filter_map(|field| body.and_then(|body| body.get(*field)).and_then(Value::as_str))
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join(": ");
    if detail.is_empty() { format!("HTTP {status}") } else { format!("HTTP {status}: {detail}") }
}

fn read_json_body(response: &CursorResponse) -> Option<Map<String, Value>> {
    match serde_json::from_str::<Value>(&response.body_text) {
        Ok(Value::Object(body)) => Some(body),
        _ => None,
    }
}

fn is_ok(status: u16) -> bool {
    (200..300).contains(&status)
}

fn decode_jwt_payload(token: &str) -> Option<Map<String, Value>> {
    let parts: Vec<&str> = token.split('.').collect();
    let payload = if parts.len() == 3 { parts[1] } else { return None };
    if payload.is_empty() {
        return None;
    }
    let normalized = payload.replace('-', "+").replace('_', "/");
    let padded = format!("{normalized}{}", "=".repeat((4 - normalized.len() % 4) % 4));
    let bytes = base64::engine::general_purpose::STANDARD.decode(padded).ok()?;
    // `atob` decodes to a latin1 string; the JSON payload is read from those bytes.
    let latin1: String = bytes.iter().map(|byte| char::from(*byte)).collect();
    match serde_json::from_str::<Value>(&latin1) {
        Ok(Value::Object(payload)) => Some(payload),
        _ => None,
    }
}

/// Cursor's poll/refresh responses carry no `expires_in`; the access token is a JWT whose `exp`
/// claim is authoritative. Tokens without a readable `exp` get a conservative one-hour lifetime.
fn access_token_expiry(access_token: &str, now: i64) -> f64 {
    let exp = decode_jwt_payload(access_token).and_then(|payload| payload.get("exp").and_then(Value::as_f64));
    match exp {
        Some(exp) if exp.is_finite() && exp * 1000.0 > now as f64 => exp * 1000.0 - REFRESH_SKEW_MS as f64,
        _ => (now + DEFAULT_TOKEN_LIFETIME_MS - REFRESH_SKEW_MS) as f64,
    }
}

fn credential_from_tokens(access_token: String, refresh_token: String, now: i64) -> OAuthCredential {
    let expires = access_token_expiry(&access_token, now);
    OAuthCredential::new(access_token, refresh_token, expires)
}

pub fn build_cursor_login_url(challenge: &str, uuid: &str) -> String {
    let Ok(mut url) = Url::parse(CURSOR_LOGIN_URL) else {
        return CURSOR_LOGIN_URL.to_owned();
    };
    url.query_pairs_mut()
        .append_pair("challenge", challenge)
        .append_pair("uuid", uuid)
        .append_pair("mode", "login")
        .append_pair("redirectTarget", "cli");
    url.to_string()
}

/// Polls the token release endpoint until the user approves the browser request. 404 means "not
/// released yet". Definitive rejections (400/401/403/410) fail immediately instead of being
/// retried as if they were network hiccups; 429 and other statuses back off without burning the
/// transient failure budget the way hard errors do.
async fn poll_for_tokens(
    transport: &dyn CursorTransport,
    uuid: &str,
    verifier: &str,
    signal: &AbortSignal,
) -> Result<OAuthCredential, CursorOAuthError> {
    let Ok(mut poll_url) = Url::parse(CURSOR_POLL_URL) else {
        return Err(CursorOAuthError::new(format!("Invalid poll url: {CURSOR_POLL_URL}")));
    };
    poll_url.query_pairs_mut().append_pair("uuid", uuid).append_pair("verifier", verifier);
    let poll_url = poll_url.to_string();

    let mut interval_ms = POLL_INITIAL_INTERVAL_MS;
    let mut consecutive_transient_failures = 0;

    for _attempt in 0..POLL_MAX_ATTEMPTS {
        transport.sleep(interval_ms as u64, signal).await?;
        interval_ms = (interval_ms * POLL_BACKOFF_MULTIPLIER).min(POLL_MAX_INTERVAL_MS);

        let request = CursorRequest {
            url: poll_url.clone(),
            method: "GET".to_owned(),
            headers: vec![("Accept".to_owned(), "application/json".to_owned())],
            body: None,
        };
        let response = match transport.fetch(request, signal).await {
            Ok(response) => response,
            Err(error) => {
                if signal.aborted() {
                    return Err(CursorOAuthError::new(CANCEL_MESSAGE));
                }
                consecutive_transient_failures += 1;
                if consecutive_transient_failures >= POLL_MAX_CONSECUTIVE_TRANSIENT_FAILURES {
                    return Err(CursorOAuthError::new(format!(
                        "Cursor login polling failed after repeated network errors: {error}"
                    )));
                }
                continue;
            }
        };

        if response.status == 404 {
            // Not approved yet; keep waiting.
            consecutive_transient_failures = 0;
            continue;
        }

        if is_ok(response.status) {
            let body = read_json_body(&response).unwrap_or_default();
            let access_token = required_string(&body, "accessToken")?;
            let refresh_token = required_string(&body, "refreshToken")?;
            return Ok(credential_from_tokens(access_token, refresh_token, transport.now_ms()));
        }

        if matches!(response.status, 400 | 401 | 403 | 410) {
            let body = read_json_body(&response);
            return Err(CursorOAuthError::new(format!(
                "Cursor login was rejected ({})",
                describe_failure(response.status, body.as_ref())
            )));
        }

        if response.status == 429 {
            // Rate limited: the next capped-backoff wait already slows us down.
            continue;
        }

        consecutive_transient_failures += 1;
        if consecutive_transient_failures >= POLL_MAX_CONSECUTIVE_TRANSIENT_FAILURES {
            let body = read_json_body(&response);
            return Err(CursorOAuthError::new(format!(
                "Cursor login polling failed ({})",
                describe_failure(response.status, body.as_ref())
            )));
        }
    }

    Err(CursorOAuthError::new("Cursor login timed out waiting for browser approval"))
}

async fn login_cursor(
    flow: &CursorOAuth,
    interaction: &ProviderAuthInteraction,
) -> Result<OAuthCredential, CursorOAuthError> {
    if let Err(reason) = interaction.signal.throw_if_aborted() {
        return Err(CursorOAuthError::new(reason.message));
    }
    let pkce = generate_pkce();
    let uuid = uuid::Uuid::new_v4().to_string();

    interaction.notify(AuthEvent::AuthUrl {
        url: build_cursor_login_url(&pkce.challenge, &uuid),
        instructions: Some("Approve the login request in your browser to connect your Cursor account.".to_owned()),
    });
    interaction.notify(AuthEvent::Progress { message: "Waiting for browser authentication...".to_owned() });

    poll_for_tokens(flow.transport.as_ref(), &uuid, &pkce.verifier, &interaction.signal).await
}

/// Exchanges the stored refresh token (Cursor also accepts a dashboard user API key here) for a
/// fresh session JWT. Cursor may omit or rotate the refresh token in the response; an omitted
/// token keeps the previous one so a non-rotating server cannot strand the credential.
async fn refresh_cursor_credential(
    flow: &CursorOAuth,
    credential: &OAuthCredential,
    signal: &AbortSignal,
) -> Result<OAuthCredential, CursorOAuthError> {
    let refresh_token = credential.refresh.clone();
    if refresh_token.is_empty() {
        return Err(CursorOAuthError::new("Cursor token refresh failed: no refresh token stored; run login again"));
    }

    let request = CursorRequest {
        url: CURSOR_REFRESH_URL.to_owned(),
        method: "POST".to_owned(),
        headers: vec![
            ("Authorization".to_owned(), format!("Bearer {refresh_token}")),
            ("Content-Type".to_owned(), "application/json".to_owned()),
            ("Accept".to_owned(), "application/json".to_owned()),
        ],
        body: Some("{}".to_owned()),
    };
    let response = match flow.transport.fetch(request, signal).await {
        Ok(response) => response,
        Err(error) => {
            if signal.aborted() {
                return Err(CursorOAuthError::new("Cursor token refresh cancelled"));
            }
            return Err(CursorOAuthError::new(error));
        }
    };

    let body = read_json_body(&response);
    if !is_ok(response.status) {
        return Err(CursorOAuthError::new(format!(
            "Cursor token refresh failed ({})",
            describe_failure(response.status, body.as_ref())
        )));
    }

    let empty = Map::new();
    let body = body.as_ref().unwrap_or(&empty);
    let access = required_string(body, "accessToken")?;
    let rotated = body
        .get("refreshToken")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let expires = access_token_expiry(&access, flow.transport.now_ms());
    Ok(OAuthCredential::new(access, rotated.unwrap_or(refresh_token), expires))
}

/// The port of the TS `cursorOAuth` singleton.
pub struct CursorOAuth {
    transport: Arc<dyn CursorTransport>,
}

impl Default for CursorOAuth {
    fn default() -> Self {
        Self::new()
    }
}

impl CursorOAuth {
    pub fn new() -> Self {
        Self { transport: Arc::new(HttpCursorTransport::new()) }
    }

    pub fn with_transport(transport: Arc<dyn CursorTransport>) -> Self {
        Self { transport }
    }

    pub fn transport(&self) -> &Arc<dyn CursorTransport> {
        &self.transport
    }
}

/// `export const cursorOAuth`.
pub fn cursor_oauth() -> &'static CursorOAuth {
    static INSTANCE: LazyLock<CursorOAuth> = LazyLock::new(CursorOAuth::new);
    &INSTANCE
}

#[async_trait]
impl OAuthAuth for CursorOAuth {
    fn name(&self) -> &str {
        "Cursor (Pro/Ultra/Teams)"
    }

    fn is_subscription(&self) -> bool {
        true
    }

    fn login_label(&self) -> Option<&str> {
        Some("Sign in with Cursor")
    }

    async fn login(&self, interaction: &ProviderAuthInteraction) -> anyhow::Result<OAuthCredential> {
        login_cursor(self, interaction).await.map_err(anyhow::Error::from)
    }

    async fn refresh(&self, credential: &OAuthCredential, signal: &AbortSignal) -> anyhow::Result<OAuthCredential> {
        refresh_cursor_credential(self, credential, signal).await.map_err(anyhow::Error::from)
    }

    async fn to_auth(&self, credential: &OAuthCredential) -> anyhow::Result<ModelAuth> {
        Ok(ModelAuth { api_key: Some(credential.access.clone()), ..Default::default() })
    }
}

#[cfg(test)]
mod tests {
    use crate::auth::types::{
        AuthEvent, AuthInteraction, AuthPrompt, ModelAuth, OAuthAuth, OAuthCredential, ProviderAuthInteraction,
    };
    use super::*;
    use crate::utils::abort::AbortController;
    use async_trait::async_trait;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use serde_json::json;
    use sha2::{Digest, Sha256};
    use std::collections::{HashMap, VecDeque};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    const POLL_URL: &str = CURSOR_POLL_URL;
    const REFRESH_URL: &str = CURSOR_REFRESH_URL;
    /// 2026-08-16T12:00:00Z, the clock the TS suite pins.
    const TEST_START_MS: i64 = 1_786_881_600_000;

    fn base64url(value: &str) -> String {
        URL_SAFE_NO_PAD.encode(value.as_bytes())
    }

    fn sha256_base64url(value: &str) -> String {
        URL_SAFE_NO_PAD.encode(Sha256::digest(value.as_bytes()))
    }

    /// Unsigned JWT with the given payload - enough for expiry-claim parsing.
    fn fake_jwt(payload: Value) -> String {
        format!("{}.{}.sig", base64url(r#"{"alg":"none","typ":"JWT"}"#), base64url(&payload.to_string()))
    }

    fn query_params(url: &str) -> HashMap<String, String> {
        Url::parse(url)
            .unwrap()
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect()
    }

    enum FakeReply {
        Response(u16, String),
        Failure(String),
    }

    /// The port of the suite's `vi.stubGlobal("fetch")` + fake timers: scripted replies, a fake
    /// clock advanced by the sleeps, and a gate the test releases one poll at a time.
    struct FakeTransport {
        now: Mutex<i64>,
        sleeps: Mutex<Vec<u64>>,
        poll_times: Mutex<Vec<i64>>,
        requests: Mutex<Vec<CursorRequest>>,
        replies: Mutex<VecDeque<FakeReply>>,
        gate: tokio::sync::Notify,
        auto: AtomicBool,
    }

    impl FakeTransport {
        fn new(now: i64) -> Self {
            Self {
                now: Mutex::new(now),
                sleeps: Mutex::new(Vec::new()),
                poll_times: Mutex::new(Vec::new()),
                requests: Mutex::new(Vec::new()),
                replies: Mutex::new(VecDeque::new()),
                gate: tokio::sync::Notify::new(),
                auto: AtomicBool::new(false),
            }
        }

        fn push_response(&self, status: u16, body: impl Into<String>) {
            self.replies.lock().unwrap().push_back(FakeReply::Response(status, body.into()));
        }

        fn push_failure(&self, message: impl Into<String>) {
            self.replies.lock().unwrap().push_back(FakeReply::Failure(message.into()));
        }

        /// Every sleep completes immediately (the suite's `vi.runAllTimersAsync`).
        fn set_auto(&self) {
            self.auto.store(true, Ordering::SeqCst);
        }

        /// Releases the pending sleep (the suite's `vi.advanceTimersByTimeAsync`).
        fn advance(&self) {
            self.gate.notify_one();
        }

        fn sleep_count(&self) -> usize {
            self.sleeps.lock().unwrap().len()
        }

        fn requests(&self) -> Vec<CursorRequest> {
            self.requests.lock().unwrap().clone()
        }

        fn poll_times(&self) -> Vec<i64> {
            self.poll_times.lock().unwrap().clone()
        }
    }

    impl CursorTransport for FakeTransport {
        fn now_ms(&self) -> i64 {
            *self.now.lock().unwrap()
        }

        fn sleep<'a>(&'a self, ms: u64, signal: &'a AbortSignal) -> BoxFuture<'a, Result<(), CursorOAuthError>> {
            Box::pin(async move {
                if signal.aborted() {
                    return Err(CursorOAuthError::new(CANCEL_MESSAGE));
                }
                self.sleeps.lock().unwrap().push(ms);
                if self.auto.load(Ordering::SeqCst) {
                    *self.now.lock().unwrap() += ms as i64;
                    return Ok(());
                }
                tokio::select! {
                    () = self.gate.notified() => {
                        *self.now.lock().unwrap() += ms as i64;
                        Ok(())
                    }
                    () = signal.cancelled() => Err(CursorOAuthError::new(CANCEL_MESSAGE)),
                }
            })
        }

        fn fetch<'a>(
            &'a self,
            request: CursorRequest,
            _signal: &'a AbortSignal,
        ) -> BoxFuture<'a, Result<CursorResponse, String>> {
            Box::pin(async move {
                if request.url.starts_with(POLL_URL) {
                    let now = self.now_ms();
                    self.poll_times.lock().unwrap().push(now);
                }
                self.requests.lock().unwrap().push(request);
                match self.replies.lock().unwrap().pop_front() {
                    Some(FakeReply::Response(status, body)) => Ok(CursorResponse { status, body_text: body }),
                    Some(FakeReply::Failure(message)) => Err(message),
                    None => Err("Unexpected poll".to_owned()),
                }
            })
        }
    }

    struct TestInteraction {
        events: Arc<Mutex<Vec<AuthEvent>>>,
    }

    #[async_trait]
    impl AuthInteraction for TestInteraction {
        fn signal(&self) -> Option<AbortSignal> {
            None
        }

        async fn prompt(&self, _prompt: AuthPrompt) -> anyhow::Result<String> {
            Err(anyhow::anyhow!("Unexpected prompt"))
        }

        fn notify(&self, event: AuthEvent) {
            self.events.lock().unwrap().push(event);
        }
    }

    fn flow(transport: &Arc<FakeTransport>) -> Arc<CursorOAuth> {
        Arc::new(CursorOAuth::with_transport(transport.clone()))
    }

    fn old_credential(refresh: &str) -> OAuthCredential {
        OAuthCredential::new("old-access", refresh, 0.0)
    }

    /// Yields real event-loop turns until the condition holds (the suite's
    /// `flushUntilPollScheduled`), with a bounded budget instead of an open wait.
    async fn wait_until(label: &str, mut condition: impl FnMut() -> bool) {
        for _ in 0..2000 {
            if condition() {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("timed out waiting for {label}");
    }

    struct Login {
        handle: tokio::task::JoinHandle<anyhow::Result<OAuthCredential>>,
        events: Arc<Mutex<Vec<AuthEvent>>>,
    }

    fn start_login(flow: &Arc<CursorOAuth>, signal: AbortSignal) -> Login {
        let events = Arc::new(Mutex::new(Vec::new()));
        let handle = tokio::spawn({
            let flow = flow.clone();
            let events = events.clone();
            async move {
                let interaction = ProviderAuthInteraction::new(signal, Arc::new(TestInteraction { events }));
                flow.login(&interaction).await
            }
        });
        Login { handle, events }
    }

    fn auth_url(events: &Arc<Mutex<Vec<AuthEvent>>>) -> String {
        events
            .lock()
            .unwrap()
            .iter()
            .find_map(|event| match event {
                AuthEvent::AuthUrl { url, .. } => Some(url.clone()),
                _ => None,
            })
            .expect("expected an auth_url event")
    }

    #[tokio::test]
    async fn opens_the_deep_control_url_with_a_valid_pkce_challenge_and_polls_until_the_tokens_release() {
        let transport = Arc::new(FakeTransport::new(TEST_START_MS));
        let start_time = TEST_START_MS;
        let access_jwt = fake_jwt(json!({ "sub": "auth0|user_123", "exp": start_time / 1000 + 3600 }));
        transport.push_response(404, json!({ "error": "not found" }).to_string());
        transport.push_response(404, json!({ "error": "not found" }).to_string());
        transport.push_response(200, json!({ "accessToken": access_jwt, "refreshToken": "refresh-1" }).to_string());

        let flow = flow(&transport);
        let login = start_login(&flow, AbortController::new().signal());

        wait_until("first poll scheduled", || transport.sleep_count() == 1).await;
        let login_url = Url::parse(&auth_url(&login.events)).unwrap();
        assert_eq!(
            format!("{}://{}{}", login_url.scheme(), login_url.host_str().unwrap(), login_url.path()),
            "https://cursor.com/loginDeepControl"
        );
        let params = query_params(login_url.as_str());
        assert_eq!(params.get("mode").map(String::as_str), Some("login"));
        assert_eq!(params.get("redirectTarget").map(String::as_str), Some("cli"));
        let challenge = params.get("challenge").cloned().unwrap_or_default();
        let uuid = params.get("uuid").cloned().unwrap_or_default();
        assert!(!challenge.is_empty());
        assert!(!uuid.is_empty());
        assert!(login.events.lock().unwrap().iter().any(|event| matches!(event, AuthEvent::Progress { .. })));
        assert!(transport.poll_times().is_empty());

        // Backoff: 1000ms, then 1200ms, then 1440ms.
        transport.advance();
        wait_until("second poll scheduled", || transport.sleep_count() == 2).await;
        transport.advance();
        wait_until("third poll scheduled", || transport.sleep_count() == 3).await;
        transport.advance();
        let credential = login.handle.await.unwrap().unwrap();

        assert_eq!(transport.poll_times(), vec![start_time + 1000, start_time + 2200, start_time + 3640]);
        for request in transport.requests() {
            let params = query_params(&request.url);
            assert_eq!(params.get("uuid").map(String::as_str), Some(uuid.as_str()));
            // The poll verifier must be the PKCE preimage of the challenge in the login URL.
            assert_eq!(sha256_base64url(params.get("verifier").map(String::as_str).unwrap_or("")), challenge);
        }

        assert_eq!(credential.access, access_jwt);
        assert_eq!(credential.refresh, "refresh-1");
        assert_eq!(credential.expires, (start_time + 3_600_000 - 300_000) as f64);
    }

    #[tokio::test]
    async fn fails_fast_when_the_poll_endpoint_definitively_rejects_the_login() {
        let transport = Arc::new(FakeTransport::new(TEST_START_MS));
        transport.set_auto();
        transport.push_response(404, json!({ "error": "not found" }).to_string());
        transport.push_response(401, json!({ "error": "invalid_request", "error_description": "challenge mismatch" }).to_string());

        let flow = flow(&transport);
        let error = start_login(&flow, AbortController::new().signal()).handle.await.unwrap().unwrap_err();

        assert_eq!(error.to_string(), "Cursor login was rejected (HTTP 401: invalid_request: challenge mismatch)");
        assert_eq!(transport.requests().len(), 2);
    }

    #[tokio::test]
    async fn tolerates_transient_network_errors_between_pending_polls() {
        let transport = Arc::new(FakeTransport::new(TEST_START_MS));
        transport.set_auto();
        let access_jwt = fake_jwt(json!({ "exp": crate::utils::diagnostics::now_ms() / 1000 + 3600 }));
        transport.push_failure("socket hang up");
        transport.push_response(404, json!({ "error": "not found" }).to_string());
        transport.push_failure("socket hang up");
        transport.push_failure("socket hang up");
        transport.push_response(404, json!({ "error": "not found" }).to_string());
        transport.push_response(200, json!({ "accessToken": access_jwt, "refreshToken": "refresh-1" }).to_string());

        let flow = flow(&transport);
        let credential = start_login(&flow, AbortController::new().signal()).handle.await.unwrap().unwrap();

        assert_eq!(credential.refresh, "refresh-1");
    }

    #[tokio::test]
    async fn gives_up_after_three_consecutive_network_errors() {
        let transport = Arc::new(FakeTransport::new(TEST_START_MS));
        transport.set_auto();
        for _ in 0..10 {
            transport.push_failure("socket hang up");
        }

        let flow = flow(&transport);
        let error = start_login(&flow, AbortController::new().signal()).handle.await.unwrap().unwrap_err();

        assert_eq!(error.to_string(), "Cursor login polling failed after repeated network errors: socket hang up");
        assert_eq!(transport.requests().len(), 3);
    }

    #[tokio::test]
    async fn gives_up_after_three_consecutive_server_errors_and_reports_the_status() {
        let transport = Arc::new(FakeTransport::new(TEST_START_MS));
        transport.set_auto();
        for _ in 0..10 {
            transport.push_response(500, json!({ "message": "internal" }).to_string());
        }

        let flow = flow(&transport);
        let error = start_login(&flow, AbortController::new().signal()).handle.await.unwrap().unwrap_err();

        assert_eq!(error.to_string(), "Cursor login polling failed (HTTP 500: internal)");
        assert_eq!(transport.requests().len(), 3);
    }

    #[tokio::test]
    async fn keeps_polling_through_rate_limits_without_burning_the_transient_failure_budget() {
        let transport = Arc::new(FakeTransport::new(TEST_START_MS));
        transport.set_auto();
        let access_jwt = fake_jwt(json!({ "exp": crate::utils::diagnostics::now_ms() / 1000 + 3600 }));
        for _ in 0..4 {
            transport.push_response(429, "{}");
        }
        transport.push_response(200, json!({ "accessToken": access_jwt, "refreshToken": "refresh-1" }).to_string());

        let flow = flow(&transport);
        let credential = start_login(&flow, AbortController::new().signal()).handle.await.unwrap().unwrap();

        assert_eq!(credential.access, access_jwt);
    }

    #[tokio::test]
    async fn cancels_cleanly_when_the_login_is_aborted_while_waiting() {
        let transport = Arc::new(FakeTransport::new(TEST_START_MS));
        transport.push_response(404, json!({ "error": "not found" }).to_string());

        let flow = flow(&transport);
        let controller = AbortController::new();
        let login = start_login(&flow, controller.signal());

        wait_until("first poll scheduled", || transport.sleep_count() == 1).await;
        transport.advance();
        wait_until("second poll scheduled", || transport.sleep_count() == 2).await;
        controller.abort(None);
        let error = login.handle.await.unwrap().unwrap_err();

        assert_eq!(error.to_string(), "Login cancelled");
    }

    #[tokio::test]
    async fn times_out_after_the_poll_attempt_budget_is_exhausted() {
        let transport = Arc::new(FakeTransport::new(TEST_START_MS));
        transport.set_auto();
        for _ in 0..POLL_MAX_ATTEMPTS {
            transport.push_response(404, json!({ "error": "not found" }).to_string());
        }

        let flow = flow(&transport);
        let error = start_login(&flow, AbortController::new().signal()).handle.await.unwrap().unwrap_err();

        assert_eq!(error.to_string(), "Cursor login timed out waiting for browser approval");
        assert_eq!(transport.requests().len(), POLL_MAX_ATTEMPTS as usize);
    }

    #[tokio::test]
    async fn rejects_a_token_release_with_missing_fields() {
        let transport = Arc::new(FakeTransport::new(TEST_START_MS));
        transport.set_auto();
        transport.push_response(200, json!({ "accessToken": "only-access" }).to_string());

        let flow = flow(&transport);
        let error = start_login(&flow, AbortController::new().signal()).handle.await.unwrap().unwrap_err();

        assert_eq!(error.to_string(), "Invalid Cursor OAuth response field: refreshToken");
    }

    #[tokio::test]
    async fn falls_back_to_a_one_hour_lifetime_when_the_access_token_is_not_a_readable_jwt() {
        let transport = Arc::new(FakeTransport::new(TEST_START_MS));
        transport.push_response(200, json!({ "accessToken": "opaque-token", "refreshToken": "refresh-1" }).to_string());

        let flow = flow(&transport);
        let login = start_login(&flow, AbortController::new().signal());
        wait_until("first poll scheduled", || transport.sleep_count() == 1).await;
        transport.advance();
        let credential = login.handle.await.unwrap().unwrap();

        assert_eq!(credential.expires, (TEST_START_MS + 1000 + 3_600_000 - 300_000) as f64);
    }

    #[tokio::test]
    async fn exchanges_the_refresh_token_as_a_bearer_and_applies_the_rotated_pair() {
        let now = crate::utils::diagnostics::now_ms();
        let access_jwt = fake_jwt(json!({ "exp": now / 1000 + 7200 }));
        let transport = Arc::new(FakeTransport::new(TEST_START_MS));
        transport.push_response(200, json!({ "accessToken": access_jwt, "refreshToken": "rotated-refresh" }).to_string());

        let flow = flow(&transport);
        let signal = AbortController::new().signal();
        let credential = flow.refresh(&old_credential("old-refresh"), &signal).await.unwrap();

        let request = transport.requests().first().cloned().unwrap();
        assert_eq!(request.url, REFRESH_URL);
        assert_eq!(request.method, "POST");
        assert_eq!(request.body.as_deref(), Some("{}"));
        assert!(request.headers.contains(&("Authorization".to_owned(), "Bearer old-refresh".to_owned())));
        assert_eq!(credential.access, access_jwt);
        assert_eq!(credential.refresh, "rotated-refresh");
        assert!(credential.expires > now as f64);
    }

    #[tokio::test]
    async fn keeps_the_previous_refresh_token_when_the_server_does_not_rotate_it() {
        let access_jwt = fake_jwt(json!({ "exp": crate::utils::diagnostics::now_ms() / 1000 + 7200 }));
        for refresh_field in [None, Some("")] {
            let transport = Arc::new(FakeTransport::new(TEST_START_MS));
            let mut body = json!({ "accessToken": access_jwt });
            if let Some(value) = refresh_field {
                body["refreshToken"] = json!(value);
            }
            transport.push_response(200, body.to_string());

            let flow = flow(&transport);
            let credential = flow.refresh(&old_credential("old-refresh"), &AbortController::new().signal()).await.unwrap();

            assert_eq!(credential.refresh, "old-refresh");
        }
    }

    #[tokio::test]
    async fn fails_with_the_server_detail_on_rejection_without_echoing_the_token() {
        let transport = Arc::new(FakeTransport::new(TEST_START_MS));
        transport.push_response(401, json!({ "error": "invalid_grant" }).to_string());

        let flow = flow(&transport);
        let signal = AbortController::new().signal();
        let error = flow.refresh(&old_credential("secret-refresh-token"), &signal).await.unwrap_err();

        assert_eq!(error.to_string(), "Cursor token refresh failed (HTTP 401: invalid_grant)");
        assert!(!error.to_string().contains("secret-refresh-token"));
    }

    #[tokio::test]
    async fn fails_when_the_response_is_not_json() {
        let transport = Arc::new(FakeTransport::new(TEST_START_MS));
        transport.push_response(502, "<html>gateway error</html>");

        let flow = flow(&transport);
        let error = flow.refresh(&old_credential("old-refresh"), &AbortController::new().signal()).await.unwrap_err();

        assert_eq!(error.to_string(), "Cursor token refresh failed (HTTP 502)");
    }

    #[tokio::test]
    async fn refuses_to_refresh_without_a_stored_refresh_token() {
        let transport = Arc::new(FakeTransport::new(TEST_START_MS));

        let flow = flow(&transport);
        let error = flow.refresh(&old_credential(""), &AbortController::new().signal()).await.unwrap_err();

        assert_eq!(error.to_string(), "Cursor token refresh failed: no refresh token stored; run login again");
        assert!(transport.requests().is_empty());
    }

    #[tokio::test]
    async fn rejects_a_refresh_response_with_a_missing_access_token() {
        let transport = Arc::new(FakeTransport::new(TEST_START_MS));
        transport.push_response(200, json!({ "refreshToken": "rotated" }).to_string());

        let flow = flow(&transport);
        let error = flow.refresh(&old_credential("old-refresh"), &AbortController::new().signal()).await.unwrap_err();

        assert_eq!(error.to_string(), "Invalid Cursor OAuth response field: accessToken");
    }

    #[tokio::test]
    async fn derives_the_api_key_from_the_access_token() {
        let transport = Arc::new(FakeTransport::new(TEST_START_MS));
        let flow = flow(&transport);

        let auth: ModelAuth = flow.to_auth(&OAuthCredential::new("token", "r", 0.0)).await.unwrap();

        assert_eq!(auth.api_key.as_deref(), Some("token"));
        assert!(auth.headers.is_none());
        assert!(auth.base_url.is_none());
    }

    #[tokio::test]
    async fn is_a_subscription_flow() {
        let transport = Arc::new(FakeTransport::new(TEST_START_MS));
        let flow = flow(&transport);

        assert!(flow.is_subscription());
        assert_eq!(flow.name(), "Cursor (Pro/Ultra/Teams)");
        assert_eq!(flow.login_label(), Some("Sign in with Cursor"));
    }
}
