//! Port of senpi packages/ai/src/auth/oauth/chatgpt-subscription.ts.

use crate::auth::oauth::authorization_input::parse_authorization_input;
use crate::auth::oauth::device_code::{
    OAuthDeviceCodePollOptions, OAuthDeviceCodePollResult, poll_oauth_device_code_flow,
};
use crate::auth::oauth::loopback::{
    LoopbackFuture, LoopbackListener, LoopbackRequest, LoopbackResponse, bind_loopback, callback_host,
};
use crate::auth::oauth::oauth_page::{oauth_error_html, oauth_success_html};
use crate::auth::oauth::pkce::generate_pkce;
use crate::auth::oauth::transport::{HttpRequest, HttpResponse, OAuthTransport, default_transport};
use crate::auth::types::{
    AuthEvent, AuthPrompt, AuthPromptKind, AuthPromptOption, ModelAuth, OAuthAuth, OAuthCredential,
    ProviderAuthInteraction,
};
use crate::utils::abort::{AbortController, AbortSignal};
use crate::utils::chatgpt_subscription_auth::extract_chatgpt_subscription_account_id;
use crate::wire_identity::get_wire_identity;
use async_trait::async_trait;
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use tokio::sync::watch;
use url::Url;

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
const CALLBACK_PORT: u16 = 1455;
const CALLBACK_PATH: &str = "/auth/callback";
const DEVICE_USER_CODE_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";
const DEVICE_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
const DEVICE_VERIFICATION_URI: &str = "https://auth.openai.com/codex/device";
const DEVICE_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";
const DEVICE_CODE_TIMEOUT_SECONDS: f64 = 15.0 * 60.0;
const BROWSER_LOGIN_METHOD: &str = "browser";
const DEVICE_CODE_LOGIN_METHOD: &str = "device_code";
const SCOPE: &str = "openid profile email offline_access";

#[derive(Debug, Clone)]
struct OAuthToken {
    access: String,
    refresh: String,
    expires: f64,
}

#[derive(Debug, Clone, Copy)]
enum TokenOperation {
    Exchange,
    Refresh,
}

impl TokenOperation {
    fn as_str(self) -> &'static str {
        match self {
            TokenOperation::Exchange => "exchange",
            TokenOperation::Refresh => "refresh",
        }
    }
}

#[derive(Debug, Clone)]
struct DeviceAuthInfo {
    device_auth_id: String,
    user_code: String,
    interval_seconds: f64,
}

#[derive(Debug, Clone)]
struct DeviceTokenSuccess {
    authorization_code: String,
    code_verifier: String,
}

/// The TS randomBytes(16).toString("hex"): the OAuth state value.
fn create_state() -> String {
    hex::encode(uuid::Uuid::new_v4().as_bytes())
}

fn form_body(fields: &[(&str, &str)]) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (key, value) in fields {
        serializer.append_pair(key, value);
    }
    serializer.finish()
}

fn status_text(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "",
    }
}

/// The TS fetchWithLoginCancellation: an aborted fetch surfaces as "Login cancelled".
async fn fetch_with_login_cancellation(
    transport: &dyn OAuthTransport,
    request: HttpRequest,
    signal: &AbortSignal,
) -> anyhow::Result<HttpResponse> {
    match transport.execute(request, signal).await {
        Ok(response) => Ok(response),
        Err(error) => {
            if signal.aborted() {
                anyhow::bail!("Login cancelled");
            }
            Err(error)
        }
    }
}

async fn read_token_response(
    response: &HttpResponse,
    operation: TokenOperation,
    now_ms: f64,
) -> anyhow::Result<OAuthToken> {
    if !response.ok() {
        let text = if response.body.is_empty() { status_text(response.status).to_string() } else { response.body.clone() };
        anyhow::bail!("ChatGPT Subscription token {} failed ({}): {text}", operation.as_str(), response.status);
    }

    let json: Value = serde_json::from_str(&response.body).unwrap_or(Value::Null);
    let access = json.get("access_token").and_then(Value::as_str).filter(|value| !value.is_empty());
    let refresh = json.get("refresh_token").and_then(Value::as_str).filter(|value| !value.is_empty());
    let expires_in = json.get("expires_in").and_then(Value::as_f64);
    let (Some(access), Some(refresh), Some(expires_in)) = (access, refresh, expires_in) else {
        anyhow::bail!("ChatGPT Subscription token {} response missing fields: {json}", operation.as_str());
    };

    Ok(OAuthToken {
        access: access.to_string(),
        refresh: refresh.to_string(),
        expires: now_ms + expires_in * 1000.0,
    })
}

async fn exchange_authorization_code(
    transport: &dyn OAuthTransport,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
    signal: &AbortSignal,
) -> anyhow::Result<OAuthToken> {
    let response = fetch_with_login_cancellation(
        transport,
        HttpRequest {
            method: "POST".into(),
            url: TOKEN_URL.into(),
            headers: vec![("Content-Type".into(), "application/x-www-form-urlencoded".into())],
            body: Some(form_body(&[
                ("grant_type", "authorization_code"),
                ("client_id", CLIENT_ID),
                ("code", code),
                ("code_verifier", verifier),
                ("redirect_uri", redirect_uri),
            ])),
            timeout_ms: None,
        },
        signal,
    )
    .await?;

    read_token_response(&response, TokenOperation::Exchange, transport.now_ms()).await
}

async fn refresh_access_token(
    transport: &dyn OAuthTransport,
    refresh_token: &str,
    signal: &AbortSignal,
) -> anyhow::Result<OAuthToken> {
    let response = match transport
        .execute(
            HttpRequest {
                method: "POST".into(),
                url: TOKEN_URL.into(),
                headers: vec![("Content-Type".into(), "application/x-www-form-urlencoded".into())],
                body: Some(form_body(&[
                    ("grant_type", "refresh_token"),
                    ("refresh_token", refresh_token),
                    ("client_id", CLIENT_ID),
                ])),
                timeout_ms: None,
            },
            signal,
        )
        .await
    {
        Ok(response) => response,
        Err(error) => anyhow::bail!("ChatGPT Subscription token refresh error: {error}"),
    };

    read_token_response(&response, TokenOperation::Refresh, transport.now_ms()).await
}

async fn start_chatgpt_subscription_device_auth(
    transport: &dyn OAuthTransport,
    signal: &AbortSignal,
) -> anyhow::Result<DeviceAuthInfo> {
    let response = fetch_with_login_cancellation(
        transport,
        HttpRequest {
            method: "POST".into(),
            url: DEVICE_USER_CODE_URL.into(),
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: Some(serde_json::json!({ "client_id": CLIENT_ID }).to_string()),
            timeout_ms: None,
        },
        signal,
    )
    .await?;

    if !response.ok() {
        if response.status == 404 {
            anyhow::bail!(
                "ChatGPT Subscription device code login is not enabled for this server. Use browser login or verify the server URL."
            );
        }
        let suffix = if response.body.is_empty() { String::new() } else { format!(": {}", response.body) };
        anyhow::bail!("ChatGPT Subscription device code request failed with status {}{suffix}", response.status);
    }

    let json: Value = serde_json::from_str(&response.body).unwrap_or(Value::Null);
    let interval_seconds = match json.get("interval") {
        Some(Value::String(value)) => value.trim().parse::<f64>().unwrap_or(f64::NAN),
        Some(Value::Number(value)) => value.as_f64().unwrap_or(f64::NAN),
        _ => f64::NAN,
    };
    let device_auth_id = json.get("device_auth_id").and_then(Value::as_str).filter(|value| !value.is_empty());
    let user_code = json.get("user_code").and_then(Value::as_str).filter(|value| !value.is_empty());
    let (Some(device_auth_id), Some(user_code)) = (device_auth_id, user_code) else {
        anyhow::bail!("Invalid ChatGPT Subscription device code response: {json}");
    };
    if !interval_seconds.is_finite() || interval_seconds < 0.0 {
        anyhow::bail!("Invalid ChatGPT Subscription device code response: {json}");
    }

    Ok(DeviceAuthInfo {
        device_auth_id: device_auth_id.to_string(),
        user_code: user_code.to_string(),
        interval_seconds,
    })
}

async fn poll_chatgpt_subscription_device_auth(
    transport: Arc<dyn OAuthTransport>,
    device: DeviceAuthInfo,
    signal: &AbortSignal,
) -> anyhow::Result<DeviceTokenSuccess> {
    let poll_signal = signal.clone();
    poll_oauth_device_code_flow(OAuthDeviceCodePollOptions {
        interval_seconds: Some(device.interval_seconds),
        expires_in_seconds: Some(DEVICE_CODE_TIMEOUT_SECONDS),
        wait_before_first_poll: false,
        signal: signal.clone(),
        poll: Box::new(move || {
            let transport = transport.clone();
            let device = device.clone();
            let signal = poll_signal.clone();
            Box::pin(async move {
                let response = fetch_with_login_cancellation(
                    transport.as_ref(),
                    HttpRequest {
                        method: "POST".into(),
                        url: DEVICE_TOKEN_URL.into(),
                        headers: vec![("Content-Type".into(), "application/json".into())],
                        body: Some(
                            serde_json::json!({
                                "device_auth_id": device.device_auth_id,
                                "user_code": device.user_code,
                            })
                            .to_string(),
                        ),
                        timeout_ms: None,
                    },
                    &signal,
                )
                .await?;

                if response.ok() {
                    let json: Value = serde_json::from_str(&response.body).unwrap_or(Value::Null);
                    let authorization_code =
                        json.get("authorization_code").and_then(Value::as_str).filter(|value| !value.is_empty());
                    let code_verifier =
                        json.get("code_verifier").and_then(Value::as_str).filter(|value| !value.is_empty());
                    let (Some(authorization_code), Some(code_verifier)) = (authorization_code, code_verifier) else {
                        return Ok(OAuthDeviceCodePollResult::Failed {
                            message: format!("Invalid ChatGPT Subscription device auth token response: {json}"),
                        });
                    };
                    return Ok(OAuthDeviceCodePollResult::Complete(DeviceTokenSuccess {
                        authorization_code: authorization_code.to_string(),
                        code_verifier: code_verifier.to_string(),
                    }));
                }

                if response.status == 403 || response.status == 404 {
                    return Ok(OAuthDeviceCodePollResult::Pending);
                }

                let response_body = response.body;
                let error_code = serde_json::from_str::<Value>(&response_body).ok().and_then(|json| match json.get("error") {
                    Some(Value::Object(_)) => json.get("error").and_then(|error| error.get("code")).and_then(Value::as_str).map(str::to_string),
                    Some(Value::String(value)) => Some(value.clone()),
                    _ => None,
                });

                if error_code.as_deref() == Some("deviceauth_authorization_pending") {
                    return Ok(OAuthDeviceCodePollResult::Pending);
                }
                if error_code.as_deref() == Some("slow_down") {
                    return Ok(OAuthDeviceCodePollResult::SlowDown { interval_seconds: None });
                }

                let suffix = if response_body.is_empty() { String::new() } else { format!(": {response_body}") };
                Ok(OAuthDeviceCodePollResult::Failed {
                    message: format!(
                        "ChatGPT Subscription device auth failed with status {}{suffix}",
                        response.status
                    ),
                })
            })
        }),
    })
    .await
}

fn create_authorization_flow(originator: &str) -> anyhow::Result<(String, String, String)> {
    let pkce = generate_pkce();
    let state = create_state();

    let mut url = Url::parse(AUTHORIZE_URL)?;
    let query = {
        let mut serializer = url::form_urlencoded::Serializer::new(String::new());
        serializer.append_pair("response_type", "code");
        serializer.append_pair("client_id", CLIENT_ID);
        serializer.append_pair("redirect_uri", REDIRECT_URI);
        serializer.append_pair("scope", SCOPE);
        serializer.append_pair("code_challenge", &pkce.challenge);
        serializer.append_pair("code_challenge_method", "S256");
        serializer.append_pair("state", &state);
        serializer.append_pair("id_token_add_organizations", "true");
        serializer.append_pair("codex_cli_simplified_flow", "true");
        serializer.append_pair("originator", originator);
        serializer.finish()
    };
    url.set_query(Some(&query));

    Ok((pkce.verifier, state, url.to_string()))
}

struct CallbackShared {
    expected_state: String,
    settled: AtomicBool,
    code: watch::Sender<Option<Option<String>>>,
}

impl CallbackShared {
    fn settle(&self, code: Option<String>) {
        if self.settled.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = self.code.send(Some(code));
    }
}

pub struct LocalOAuthServer {
    shared: Arc<CallbackShared>,
    code: watch::Receiver<Option<Option<String>>>,
    listener: Mutex<Option<LoopbackListener>>,
}

impl LocalOAuthServer {
    /// Resolves with the callback code, or None once the wait was cancelled (abort, manual
    /// input, or a failed listen).
    pub async fn wait_for_code(&self) -> Option<String> {
        let mut code = self.code.clone();
        loop {
            let current = { code.borrow().clone() };
            if let Some(current) = current {
                return current;
            }
            if code.changed().await.is_err() {
                return None;
            }
        }
    }

    pub fn cancel_wait(&self) {
        self.shared.settle(None);
    }

    pub fn close(&self) {
        self.shared.settle(None);
        if let Some(listener) = self.listener.lock().unwrap_or_else(|p| p.into_inner()).take() {
            listener.close();
        }
    }
}

fn handle_callback(shared: &CallbackShared, request: &LoopbackRequest) -> LoopbackResponse {
    if request.path != CALLBACK_PATH {
        return LoopbackResponse::html(404, oauth_error_html("Callback route not found.", None));
    }
    if request.query_param("state").as_deref() != Some(shared.expected_state.as_str()) {
        return LoopbackResponse::html(400, oauth_error_html("State mismatch.", None));
    }
    let Some(code) = request.query_param("code") else {
        return LoopbackResponse::html(400, oauth_error_html("Missing authorization code.", None));
    };

    shared.settle(Some(code));
    LoopbackResponse::html(200, oauth_success_html("OpenAI authentication completed. You can close this window."))
}

/// The node:http server on the fixed callback port; a failed listen resolves the wait with None
/// instead of throwing, matching the TS error handler.
async fn start_local_oauth_server(state: &str) -> LocalOAuthServer {
    let (code_tx, code_rx) = watch::channel(None);
    let shared = Arc::new(CallbackShared {
        expected_state: state.to_string(),
        settled: AtomicBool::new(false),
        code: code_tx,
    });

    let handler = {
        let shared = shared.clone();
        Arc::new(move |request: LoopbackRequest| -> LoopbackFuture {
            Box::pin(std::future::ready(handle_callback(&shared, &request)))
        })
    };

    let listener = bind_loopback(CALLBACK_PORT, &callback_host(), handler).await.ok();
    if listener.is_none() {
        shared.settle(None);
    }

    LocalOAuthServer { shared, code: code_rx, listener: Mutex::new(listener) }
}

fn credentials_from_token(token: OAuthToken) -> anyhow::Result<OAuthCredential> {
    let Some(account_id) = extract_chatgpt_subscription_account_id(&token.access) else {
        anyhow::bail!("Failed to extract accountId from token");
    };

    Ok(OAuthCredential::new(token.access, token.refresh, token.expires)
        .with_extra("accountId", Value::String(account_id)))
}

#[derive(Clone)]
pub struct ChatGptSubscriptionOAuth {
    transport: Arc<dyn OAuthTransport>,
}

impl ChatGptSubscriptionOAuth {
    pub fn new(transport: Arc<dyn OAuthTransport>) -> Self {
        Self { transport }
    }

    pub fn transport(&self) -> &Arc<dyn OAuthTransport> {
        &self.transport
    }

    async fn exchange_authorization_code_for_credentials(
        &self,
        code: &str,
        verifier: &str,
        redirect_uri: &str,
        signal: &AbortSignal,
    ) -> anyhow::Result<OAuthCredential> {
        credentials_from_token(
            exchange_authorization_code(self.transport.as_ref(), code, verifier, redirect_uri, signal).await?,
        )
    }

    async fn login_chatgpt_subscription_device_code(
        &self,
        interaction: &ProviderAuthInteraction,
    ) -> anyhow::Result<OAuthCredential> {
        let device = start_chatgpt_subscription_device_auth(self.transport.as_ref(), &interaction.signal).await?;
        interaction.notify(AuthEvent::DeviceCode {
            user_code: device.user_code.clone(),
            verification_uri: DEVICE_VERIFICATION_URI.into(),
            interval_seconds: Some(device.interval_seconds),
            expires_in_seconds: Some(DEVICE_CODE_TIMEOUT_SECONDS),
        });
        let code = poll_chatgpt_subscription_device_auth(self.transport.clone(), device, &interaction.signal).await?;
        self.exchange_authorization_code_for_credentials(
            &code.authorization_code,
            &code.code_verifier,
            DEVICE_REDIRECT_URI,
            &interaction.signal,
        )
        .await
    }

    async fn login_chatgpt_subscription(
        &self,
        interaction: &ProviderAuthInteraction,
    ) -> anyhow::Result<OAuthCredential> {
        let (verifier, state, url) = create_authorization_flow(&get_wire_identity())?;
        let server = start_local_oauth_server(&state).await;
        let manual_abort = AbortController::new();
        let abort_shared = server.shared.clone();
        let listener_id = interaction.signal.add_abort_listener(move |_| abort_shared.settle(None));
        if interaction.signal.aborted() {
            server.cancel_wait();
        }

        interaction.notify(AuthEvent::AuthUrl {
            url,
            instructions: Some("A browser window should open. Complete login to finish.".into()),
        });

        let outcome = async {
            let prompt = AuthPrompt {
                kind: AuthPromptKind::ManualCode {
                    message: "Complete login in your browser, or paste the authorization code / redirect URL here:"
                        .into(),
                    placeholder: Some(REDIRECT_URI.into()),
                },
                signal: Some(manual_abort.signal()),
            };

            let manual = async {
                let result = interaction.prompt(prompt).await;
                server.cancel_wait();
                match result {
                    Ok(input) => ManualOutcome::Input(input),
                    Err(error) => ManualOutcome::Error(error.to_string()),
                }
            };
            tokio::pin!(manual);

            let mut manual_settled: Option<ManualOutcome> = None;
            let mut callback_code: Option<Option<String>> = None;
            tokio::select! {
                biased;
                code = server.wait_for_code() => callback_code = Some(code),
                settled = &mut manual => manual_settled = Some(settled),
            }

            if let Some(ManualOutcome::Error(message)) = &manual_settled {
                anyhow::bail!("{message}");
            }

            let mut code = None;
            if let Some(Some(callback)) = callback_code {
                code = Some(callback);
            } else if let Some(ManualOutcome::Input(input)) = &manual_settled {
                code = apply_manual_input(input, &state)?;
            }

            if code.is_none() {
                let settled = match manual_settled {
                    Some(settled) => settled,
                    None => manual.await,
                };
                match settled {
                    ManualOutcome::Error(message) => anyhow::bail!("{message}"),
                    ManualOutcome::Input(input) => code = apply_manual_input(&input, &state)?,
                }
            }

            let Some(code) = code else {
                anyhow::bail!("Missing authorization code");
            };
            self.exchange_authorization_code_for_credentials(&code, &verifier, REDIRECT_URI, &interaction.signal)
                .await
        }
        .await;

        interaction.signal.remove_abort_listener(listener_id);
        manual_abort.abort(None);
        server.close();
        outcome
    }
}

enum ManualOutcome {
    Input(String),
    Error(String),
}

fn apply_manual_input(input: &str, state: &str) -> anyhow::Result<Option<String>> {
    let parsed = parse_authorization_input(input);
    if let Some(parsed_state) = &parsed.state
        && parsed_state != state {
            anyhow::bail!("State mismatch");
        }
    Ok(parsed.code)
}

pub fn chatgpt_subscription_oauth() -> Arc<dyn OAuthAuth> {
    static INSTANCE: LazyLock<Arc<dyn OAuthAuth>> =
        LazyLock::new(|| Arc::new(ChatGptSubscriptionOAuth::new(default_transport())));
    INSTANCE.clone()
}

#[async_trait]
impl OAuthAuth for ChatGptSubscriptionOAuth {
    fn name(&self) -> &str {
        "ChatGPT Subscription (Plus/Pro)"
    }

    fn is_subscription(&self) -> bool {
        true
    }

    async fn login(&self, interaction: &ProviderAuthInteraction) -> anyhow::Result<OAuthCredential> {
        let method = interaction
            .prompt(AuthPrompt {
                kind: AuthPromptKind::Select {
                    message: "Select ChatGPT Subscription login method:".into(),
                    options: vec![
                        AuthPromptOption {
                            id: BROWSER_LOGIN_METHOD.into(),
                            label: "Browser login (default)".into(),
                            description: None,
                        },
                        AuthPromptOption {
                            id: DEVICE_CODE_LOGIN_METHOD.into(),
                            label: "Device code login (headless)".into(),
                            description: None,
                        },
                    ],
                },
                signal: None,
            })
            .await?;

        if method == DEVICE_CODE_LOGIN_METHOD {
            return self.login_chatgpt_subscription_device_code(interaction).await;
        }
        if method != BROWSER_LOGIN_METHOD {
            anyhow::bail!("Unknown ChatGPT Subscription login method: {method}");
        }

        self.login_chatgpt_subscription(interaction).await
    }

    async fn refresh(&self, credential: &OAuthCredential, signal: &AbortSignal) -> anyhow::Result<OAuthCredential> {
        credentials_from_token(refresh_access_token(self.transport.as_ref(), &credential.refresh, signal).await?)
    }

    async fn to_auth(&self, credential: &OAuthCredential) -> anyhow::Result<ModelAuth> {
        Ok(ModelAuth { api_key: Some(credential.access.clone()), ..Default::default() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::oauth::transport::{ScriptedResponse, ScriptedTransport};
    use crate::auth::types::{AccountLoginReceipt, AuthInteraction};
    use base64::Engine as _;
    use std::time::Duration;

    const START_MS: f64 = 1_783_886_400_000.0;

    fn json(status: u16, body: Value) -> ScriptedResponse {
        ScriptedResponse::Json { status, body }
    }

    /// The TS createAccessToken: a JWT whose payload carries the account claim.
    fn access_token(account_id: &str) -> String {
        let header =
            base64::engine::general_purpose::STANDARD.encode(serde_json::json!({ "alg": "none" }).to_string());
        let payload = base64::engine::general_purpose::STANDARD.encode(
            serde_json::json!({ "https://api.openai.com/auth": { "chatgpt_account_id": account_id } }).to_string(),
        );
        format!("{header}.{payload}.signature")
    }

    fn device_auth_pending_response() -> ScriptedResponse {
        json(
            403,
            serde_json::json!({
                "error": {
                    "message": "Device authorization is pending. Please try again.",
                    "type": "invalid_request_error",
                    "param": null,
                    "code": "deviceauth_authorization_pending",
                }
            }),
        )
    }

    fn usercode_response(interval: &str) -> ScriptedResponse {
        json(
            200,
            serde_json::json!({ "device_auth_id": "device-auth-id", "user_code": "ABCD-1234", "interval": interval }),
        )
    }

    fn token_exchange_response(account_id: &str) -> ScriptedResponse {
        json(
            200,
            serde_json::json!({
                "access_token": access_token(account_id),
                "refresh_token": "refresh-token",
                "expires_in": 3600,
            }),
        )
    }

    fn device_code_success() -> ScriptedResponse {
        json(
            200,
            serde_json::json!({
                "authorization_code": "oauth-code",
                "code_challenge": "device-code-challenge",
                "code_verifier": "device-code-verifier",
            }),
        )
    }

    type CapturedEvents = Arc<Mutex<Vec<AuthEvent>>>;
    type CapturedPrompts = Arc<Mutex<Vec<AuthPrompt>>>;

    struct RecordingInteraction {
        signal: AbortSignal,
        method: String,
        prompt_error: Option<String>,
        device_codes: Arc<Mutex<Vec<AuthEvent>>>,
        selects: Arc<Mutex<Vec<AuthPrompt>>>,
    }

    #[async_trait]
    impl AuthInteraction for RecordingInteraction {
        fn signal(&self) -> Option<AbortSignal> {
            Some(self.signal.clone())
        }

        fn on_account_committed(&self, _receipt: AccountLoginReceipt) {}

        async fn prompt(&self, prompt: AuthPrompt) -> anyhow::Result<String> {
            self.selects.lock().unwrap_or_else(|p| p.into_inner()).push(prompt);
            if let Some(error) = &self.prompt_error {
                anyhow::bail!("{error}");
            }
            Ok(self.method.clone())
        }

        fn notify(&self, event: AuthEvent) {
            if let AuthEvent::DeviceCode { .. } = &event {
                self.device_codes.lock().unwrap_or_else(|p| p.into_inner()).push(event);
            }
        }
    }

    fn interaction_with(
        signal: AbortSignal,
        method: &str,
        prompt_error: Option<&str>,
    ) -> (ProviderAuthInteraction, CapturedEvents, CapturedPrompts) {
        let device_codes = Arc::new(Mutex::new(Vec::new()));
        let selects = Arc::new(Mutex::new(Vec::new()));
        let inner = Arc::new(RecordingInteraction {
            signal: signal.clone(),
            method: method.to_string(),
            prompt_error: prompt_error.map(str::to_string),
            device_codes: device_codes.clone(),
            selects: selects.clone(),
        });
        (ProviderAuthInteraction::new(signal, inner), device_codes, selects)
    }

    fn device_interaction(signal: AbortSignal) -> (ProviderAuthInteraction, Arc<Mutex<Vec<AuthEvent>>>) {
        let (interaction, device_codes, _) = interaction_with(signal, DEVICE_CODE_LOGIN_METHOD, None);
        (interaction, device_codes)
    }

    async fn advance(transport: &ScriptedTransport, ms: u64) {
        transport.advance_ms(ms as f64);
        tokio::time::advance(Duration::from_millis(ms)).await;
        for _ in 0..64 {
            tokio::task::yield_now().await;
        }
    }

    fn poll_count(transport: &ScriptedTransport) -> usize {
        transport.requests().iter().filter(|request| request.url == DEVICE_TOKEN_URL).count()
    }

    #[tokio::test(start_paused = true)]
    async fn logs_in_with_the_device_code_flow() {
        let transport = Arc::new(ScriptedTransport::new(vec![
            (Some("deviceauth/usercode"), usercode_response("5")),
            (Some("deviceauth/token"), device_auth_pending_response()),
            (Some("deviceauth/token"), device_code_success()),
            (Some("oauth/token"), token_exchange_response("account-123")),
        ]));
        transport.set_now_ms(START_MS);
        let oauth = ChatGptSubscriptionOAuth::new(transport.clone());
        let (interaction, device_codes) = device_interaction(AbortController::new().signal());

        let login = tokio::spawn({
            let oauth = oauth.clone();
            async move { oauth.login(&interaction).await }
        });

        advance(&transport, 0).await;
        assert_eq!(
            *device_codes.lock().unwrap_or_else(|p| p.into_inner()),
            vec![AuthEvent::DeviceCode {
                user_code: "ABCD-1234".into(),
                verification_uri: DEVICE_VERIFICATION_URI.into(),
                interval_seconds: Some(5.0),
                expires_in_seconds: Some(900.0),
            }]
        );
        assert_eq!(poll_count(&transport), 1);

        advance(&transport, 4999).await;
        assert_eq!(poll_count(&transport), 1);

        advance(&transport, 1).await;
        let credential = login.await.unwrap().unwrap();
        assert_eq!(credential.access, access_token("account-123"));
        assert_eq!(credential.refresh, "refresh-token");
        assert_eq!(credential.expires, START_MS + 5000.0 + 3600.0 * 1000.0);
        assert_eq!(credential.get_extra_str("accountId"), Some("account-123"));
        assert_eq!(poll_count(&transport), 2);

        let exchange = transport.requests().into_iter().find(|request| request.url == TOKEN_URL).expect("exchange");
        let form: Vec<(String, String)> = url::form_urlencoded::parse(exchange.body.as_deref().unwrap_or_default().as_bytes())
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        let value = |key: &str| form.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
        assert_eq!(value("grant_type").as_deref(), Some("authorization_code"));
        assert_eq!(value("client_id").as_deref(), Some(CLIENT_ID));
        assert_eq!(value("code").as_deref(), Some("oauth-code"));
        assert_eq!(value("redirect_uri").as_deref(), Some(DEVICE_REDIRECT_URI));
        assert_eq!(value("code_verifier").as_deref(), Some("device-code-verifier"));
    }

    #[tokio::test]
    async fn offers_browser_login_first_and_uses_the_selected_device_code_flow() {
        let transport = Arc::new(ScriptedTransport::new(vec![
            (Some("deviceauth/usercode"), usercode_response("5")),
            (Some("deviceauth/token"), device_code_success()),
            (Some("oauth/token"), token_exchange_response("account-456")),
        ]));
        transport.set_now_ms(START_MS);
        let oauth = ChatGptSubscriptionOAuth::new(transport.clone());
        let (interaction, device_codes, selects) =
            interaction_with(AbortController::new().signal(), DEVICE_CODE_LOGIN_METHOD, None);

        let credential = oauth.login(&interaction).await.unwrap();

        assert_eq!(credential.access, access_token("account-456"));
        assert_eq!(credential.refresh, "refresh-token");
        assert_eq!(credential.get_extra_str("accountId"), Some("account-456"));
        let captured = selects.lock().unwrap_or_else(|p| p.into_inner()).clone();
        assert_eq!(captured.len(), 1);
        assert_eq!(
            captured[0].kind,
            AuthPromptKind::Select {
                message: "Select ChatGPT Subscription login method:".into(),
                options: vec![
                    AuthPromptOption {
                        id: "browser".into(),
                        label: "Browser login (default)".into(),
                        description: None,
                    },
                    AuthPromptOption {
                        id: "device_code".into(),
                        label: "Device code login (headless)".into(),
                        description: None,
                    },
                ],
            }
        );
        assert!(captured[0].signal.is_none());
        assert_eq!(
            *device_codes.lock().unwrap_or_else(|p| p.into_inner()),
            vec![AuthEvent::DeviceCode {
                user_code: "ABCD-1234".into(),
                verification_uri: DEVICE_VERIFICATION_URI.into(),
                interval_seconds: Some(5.0),
                expires_in_seconds: Some(900.0),
            }]
        );
        assert!(
            transport.requests().iter().all(|request| request.url != AUTHORIZE_URL),
            "browser login should not start"
        );
    }

    #[tokio::test]
    async fn cancels_when_login_method_selection_is_cancelled() {
        let transport = Arc::new(ScriptedTransport::default());
        let oauth = ChatGptSubscriptionOAuth::new(transport);
        let (interaction, _, _) =
            interaction_with(AbortController::new().signal(), DEVICE_CODE_LOGIN_METHOD, Some("Login cancelled"));

        let error = oauth.login(&interaction).await.unwrap_err();
        assert_eq!(error.to_string(), "Login cancelled");
    }

    #[tokio::test(start_paused = true)]
    async fn cancels_the_device_code_flow_while_waiting() {
        let transport = Arc::new(ScriptedTransport::new(vec![
            (Some("deviceauth/usercode"), usercode_response("5")),
            (Some("deviceauth/token"), device_auth_pending_response()),
        ]));
        transport.set_now_ms(START_MS);
        let oauth = ChatGptSubscriptionOAuth::new(transport.clone());
        let controller = AbortController::new();
        let (interaction, _) = device_interaction(controller.signal());

        let login = tokio::spawn({
            let oauth = oauth.clone();
            async move { oauth.login(&interaction).await }
        });

        advance(&transport, 0).await;
        assert_eq!(poll_count(&transport), 1);

        controller.abort(None);
        let error = login.await.unwrap().unwrap_err();
        assert_eq!(error.to_string(), "Login cancelled");
    }

    #[tokio::test(start_paused = true)]
    async fn times_out_the_device_code_flow_after_15_minutes() {
        let transport = Arc::new(ScriptedTransport::new(vec![
            (Some("deviceauth/usercode"), usercode_response("60")),
            (Some("deviceauth/token"), device_auth_pending_response()),
        ]));
        transport.set_now_ms(START_MS);
        let oauth = ChatGptSubscriptionOAuth::new(transport.clone());
        let (interaction, _) = device_interaction(AbortController::new().signal());

        let login = tokio::spawn({
            let oauth = oauth.clone();
            async move { oauth.login(&interaction).await }
        });

        advance(&transport, 0).await;
        assert_eq!(poll_count(&transport), 1);

        advance(&transport, 15 * 60 * 1000).await;
        let error = login.await.unwrap().unwrap_err();
        assert_eq!(error.to_string(), "Device flow timed out");
    }

    #[tokio::test(start_paused = true)]
    async fn treats_device_auth_403_and_404_responses_as_pending() {
        let transport = Arc::new(ScriptedTransport::new(vec![
            (Some("deviceauth/usercode"), usercode_response("1")),
            (
                Some("deviceauth/token"),
                json(403, serde_json::json!({"error": "access_denied", "error_description": "denied"})),
            ),
            (Some("deviceauth/token"), ScriptedResponse::Text { status: 404, body: "not ready".into() }),
            (Some("deviceauth/token"), device_code_success()),
            (Some("oauth/token"), token_exchange_response("account-403-404")),
        ]));
        transport.set_now_ms(START_MS);
        let oauth = ChatGptSubscriptionOAuth::new(transport.clone());
        let (interaction, _) = device_interaction(AbortController::new().signal());

        let login = tokio::spawn({
            let oauth = oauth.clone();
            async move { oauth.login(&interaction).await }
        });

        advance(&transport, 0).await;
        advance(&transport, 1000).await;
        advance(&transport, 1000).await;

        let credential = login.await.unwrap().unwrap();
        assert_eq!(credential.access, access_token("account-403-404"));
        assert_eq!(credential.refresh, "refresh-token");
        assert_eq!(credential.get_extra_str("accountId"), Some("account-403-404"));
        assert_eq!(poll_count(&transport), 3);
    }

    #[tokio::test]
    async fn includes_the_response_body_in_device_auth_poll_failures() {
        let transport = Arc::new(ScriptedTransport::new(vec![
            (Some("deviceauth/usercode"), usercode_response("5")),
            (
                Some("deviceauth/token"),
                json(500, serde_json::json!({"error": "server_error", "error_description": "try again later"})),
            ),
        ]));
        transport.set_now_ms(START_MS);
        let oauth = ChatGptSubscriptionOAuth::new(transport);
        let (interaction, _) = device_interaction(AbortController::new().signal());

        let error = oauth.login(&interaction).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "ChatGPT Subscription device auth failed with status 500: {\"error\":\"server_error\",\"error_description\":\"try again later\"}"
        );
    }

    #[tokio::test]
    async fn does_not_write_token_refresh_failures_to_stderr() {
        let transport = Arc::new(ScriptedTransport::new(vec![(
            Some("oauth/token"),
            json(
                401,
                serde_json::json!({
                    "error": {
                        "message": "Could not validate your token. Please try signing in again.",
                        "type": "invalid_request_error",
                    }
                }),
            ),
        )]));
        let oauth = ChatGptSubscriptionOAuth::new(transport);
        let signal = AbortController::new().signal();

        let error = oauth
            .refresh(&OAuthCredential::new("invalid-access-token", "invalid-refresh-token", 0.0), &signal)
            .await
            .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("ChatGPT Subscription token refresh failed (401)"), "{message}");
        assert!(message.contains("Could not validate your token"), "{message}");
    }
}
