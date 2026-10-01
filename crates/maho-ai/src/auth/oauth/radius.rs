//! Port of senpi packages/ai/src/auth/oauth/radius.ts.

use crate::auth::oauth::device_code::{
    OAuthDeviceCodePollOptions, OAuthDeviceCodePollResult, poll_oauth_device_code_flow,
};
use crate::auth::oauth::loopback::{LoopbackRequest, LoopbackResponse, bind_loopback};
use crate::auth::oauth::oauth_page::{oauth_error_html, oauth_success_html};
use crate::auth::oauth::pkce::generate_pkce;
use crate::auth::oauth::transport::{HttpRequest, HttpResponse, OAuthTransport, default_transport};
use crate::auth::types::{
    AuthEvent, AuthPrompt, AuthPromptKind, AuthPromptOption, ModelAuth, OAuthAuth, OAuthCredential,
    ProviderAuthInteraction,
};
use crate::utils::abort::AbortSignal;
use async_trait::async_trait;
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::watch;
use url::Url;

const CALLBACK_HOST: &str = "127.0.0.1";
const CALLBACK_PORT: u16 = 1456;
const CALLBACK_PATH: &str = "/oauth/callback";
const TOKEN_EXPIRY_SKEW_MS: f64 = 60_000.0;
const LOGIN_METHOD_BROWSER: &str = "browser";
const LOGIN_METHOD_DEVICE_CODE: &str = "device-code";
const OAUTH_CLIENT_ID: &str = "pi-gateway";
const OAUTH_SCOPE: &str = "gateway offline_access";
const OAUTH_DEVICE_CODE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";

fn callback_redirect_uri() -> String {
    format!("http://{CALLBACK_HOST}:{CALLBACK_PORT}{CALLBACK_PATH}")
}

fn normalize_gateway_url(value: &str) -> String {
    let with_scheme = if value.starts_with("http://") || value.starts_with("https://") {
        value.to_string()
    } else {
        format!("https://{value}")
    };
    with_scheme.trim_end_matches('/').to_string()
}

#[derive(Debug, Clone)]
pub struct RadiusOAuthDiscovery {
    pub authorization_endpoint: String,
}

async fn load_radius_oauth_discovery(
    transport: &dyn OAuthTransport,
    gateway: &str,
    signal: &AbortSignal,
) -> anyhow::Result<RadiusOAuthDiscovery> {
    let url = Url::parse(gateway)?.join("/v1/oauth")?.to_string();
    let response = transport
        .execute(
            HttpRequest {
                method: "GET".into(),
                url,
                headers: vec![("accept".into(), "application/json".into())],
                body: None,
                timeout_ms: None,
            },
            signal,
        )
        .await?;

    if !response.ok() {
        anyhow::bail!("Could not load Radius OAuth config from {gateway}: {} {}", response.status, response.body);
    }

    let discovery: Value = serde_json::from_str(&response.body).unwrap_or(Value::Null);
    match discovery.get("authorizationEndpoint").and_then(Value::as_str) {
        Some(endpoint) => Ok(RadiusOAuthDiscovery { authorization_endpoint: endpoint.to_string() }),
        None => anyhow::bail!("Invalid Radius OAuth config from {gateway}"),
    }
}

#[derive(Debug, Clone)]
pub struct OAuthResponseError {
    pub status: u16,
    pub oauth_error: Option<String>,
    pub message: String,
}

impl std::fmt::Display for OAuthResponseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for OAuthResponseError {}

fn read_oauth_response_error(response: &HttpResponse, message: &str) -> OAuthResponseError {
    let text = response.body.as_str();
    let mut oauth_error = None;
    let mut description = None;
    if !text.is_empty() {
        match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(object)) => {
                oauth_error = object.get("error").and_then(Value::as_str).map(str::to_string);
                description = object.get("error_description").and_then(Value::as_str).map(str::to_string);
            }
            _ => description = Some(text.to_string()),
        }
    }

    let detail = match (&oauth_error, &description) {
        (Some(oauth_error), Some(description)) => format!("{oauth_error}: {description}"),
        (Some(oauth_error), None) => oauth_error.clone(),
        (None, Some(description)) => description.clone(),
        (None, None) => response.status.to_string(),
    };
    OAuthResponseError { status: response.status, oauth_error, message: format!("{message}: {detail}") }
}

fn form_body(fields: &[(&str, &str)]) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (key, value) in fields {
        serializer.append_pair(key, value);
    }
    serializer.finish()
}

async fn request_oauth_token(
    transport: &dyn OAuthTransport,
    gateway: &str,
    fields: &[(&str, &str)],
    signal: &AbortSignal,
) -> anyhow::Result<OAuthCredential> {
    let url = Url::parse(gateway)?.join("/v1/oauth/token")?.to_string();
    let response = transport
        .execute(
            HttpRequest {
                method: "POST".into(),
                url,
                headers: vec![
                    ("accept".into(), "application/json".into()),
                    ("content-type".into(), "application/x-www-form-urlencoded".into()),
                ],
                body: Some(form_body(fields)),
                timeout_ms: None,
            },
            signal,
        )
        .await
        .map_err(|error| {
            if signal.aborted() {
                anyhow::anyhow!("Login cancelled")
            } else {
                error
            }
        })?;

    if !response.ok() {
        return Err(read_oauth_response_error(&response, "Radius OAuth token request failed").into());
    }

    let data: Value = serde_json::from_str(&response.body).unwrap_or(Value::Null);
    let access = data.get("access_token").and_then(Value::as_str).unwrap_or_default();
    let refresh = data.get("refresh_token").and_then(Value::as_str).unwrap_or_default();
    let expires_in = data.get("expires_in").and_then(Value::as_f64).unwrap_or_default();
    let scope = data.get("scope").cloned().unwrap_or(Value::Null);
    let credential = OAuthCredential::new(access, refresh, transport.now_ms() + expires_in * 1000.0 - TOKEN_EXPIRY_SKEW_MS);
    Ok(credential.with_extra("scope", scope))
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

pub struct OAuthCallbackServer {
    shared: Arc<CallbackShared>,
    code: watch::Receiver<Option<Option<String>>>,
    listener: Mutex<Option<crate::auth::oauth::loopback::LoopbackListener>>,
    signal: Option<AbortSignal>,
    listener_id: Option<crate::utils::abort::ListenerId>,
}

impl OAuthCallbackServer {
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

    pub fn close(&self) {
        if let (Some(signal), Some(listener_id)) = (&self.signal, self.listener_id) {
            signal.remove_abort_listener(listener_id);
        }
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
        return LoopbackResponse::html(400, oauth_error_html("OAuth state mismatch.", None));
    }

    if let Some(error) = request.query_param("error") {
        let description = request.query_param("error_description").unwrap_or(error);
        shared.settle(None);
        return LoopbackResponse::html(400, oauth_error_html(&description, None));
    }

    let Some(code) = request.query_param("code") else {
        return LoopbackResponse::html(400, oauth_error_html("Missing authorization code.", None));
    };

    shared.settle(Some(code));
    LoopbackResponse::html(200, oauth_success_html("Signed in to Radius. You may now close this page."))
}

pub async fn start_oauth_callback_server(expected_state: &str, signal: &AbortSignal) -> anyhow::Result<OAuthCallbackServer> {
    start_oauth_callback_server_on_port(expected_state, signal, CALLBACK_PORT).await
}

pub async fn start_oauth_callback_server_on_port(
    expected_state: &str,
    signal: &AbortSignal,
    port: u16,
) -> anyhow::Result<OAuthCallbackServer> {
    let (code_tx, code_rx) = watch::channel(None);
    let shared = Arc::new(CallbackShared {
        expected_state: expected_state.to_string(),
        settled: AtomicBool::new(false),
        code: code_tx,
    });

    let handler = {
        let shared = shared.clone();
        Arc::new(move |request: LoopbackRequest| -> crate::auth::oauth::loopback::LoopbackFuture {
            let response = handle_callback(&shared, &request);
            Box::pin(std::future::ready(response))
        })
    };

    let listener = match bind_loopback(port, CALLBACK_HOST, handler).await {
        Ok(listener) => listener,
        Err(_) => {
            shared.settle(None);
            return Ok(OAuthCallbackServer {
                shared,
                code: code_rx,
                listener: Mutex::new(None),
                signal: None,
                listener_id: None,
            });
        }
    };

    let abort_shared = shared.clone();
    let listener_id = signal.add_abort_listener(move |_| abort_shared.settle(None));

    Ok(OAuthCallbackServer {
        shared,
        code: code_rx,
        listener: Mutex::new(Some(listener)),
        signal: Some(signal.clone()),
        listener_id: Some(listener_id),
    })
}

async fn login_with_browser(
    transport: &dyn OAuthTransport,
    gateway: &str,
    authorization_endpoint: &str,
    interaction: &ProviderAuthInteraction,
) -> anyhow::Result<OAuthCredential> {
    let pkce = generate_pkce();
    let state = uuid::Uuid::new_v4().to_string();
    let redirect_uri = callback_redirect_uri();
    let mut authorize_url = Url::parse(authorization_endpoint)?;
    let query = {
        let mut serializer = url::form_urlencoded::Serializer::new(String::new());
        serializer.append_pair("response_type", "code");
        serializer.append_pair("client_id", OAUTH_CLIENT_ID);
        serializer.append_pair("redirect_uri", &redirect_uri);
        serializer.append_pair("scope", OAUTH_SCOPE);
        serializer.append_pair("code_challenge", &pkce.challenge);
        serializer.append_pair("code_challenge_method", "S256");
        serializer.append_pair("handoff", "url");
        serializer.append_pair("state", &state);
        serializer.finish()
    };
    authorize_url.set_query(Some(&query));

    let callback_server = start_oauth_callback_server(&state, &interaction.signal).await?;
    interaction.notify(AuthEvent::Progress { message: format!("Listening for OAuth callback on {redirect_uri}") });
    interaction.notify(AuthEvent::AuthUrl {
        url: authorize_url.to_string(),
        instructions: Some("Continue in your browser.".into()),
    });

    let result = async {
        let Some(code) = callback_server.wait_for_code().await else {
            if interaction.signal.aborted() {
                anyhow::bail!("Login cancelled");
            }
            anyhow::bail!("OAuth callback did not complete.");
        };
        request_oauth_token(
            transport,
            gateway,
            &[
                ("grant_type", "authorization_code"),
                ("client_id", OAUTH_CLIENT_ID),
                ("redirect_uri", &redirect_uri),
                ("code", &code),
                ("code_verifier", &pkce.verifier),
            ],
            &interaction.signal,
        )
        .await
    }
    .await;

    callback_server.close();
    result
}

#[derive(Debug, Clone)]
pub struct DeviceAuthorizationResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: f64,
    pub interval: Option<f64>,
}

async fn request_device_authorization(
    transport: &dyn OAuthTransport,
    gateway: &str,
    signal: &AbortSignal,
) -> anyhow::Result<DeviceAuthorizationResponse> {
    let url = Url::parse(gateway)?.join("/v1/oauth/device")?.to_string();
    let response = transport
        .execute(
            HttpRequest {
                method: "POST".into(),
                url,
                headers: vec![
                    ("accept".into(), "application/json".into()),
                    ("content-type".into(), "application/x-www-form-urlencoded".into()),
                ],
                body: Some(form_body(&[("client_id", OAUTH_CLIENT_ID), ("scope", OAUTH_SCOPE)])),
                timeout_ms: None,
            },
            signal,
        )
        .await
        .map_err(|error| {
            if signal.aborted() {
                anyhow::anyhow!("Login cancelled")
            } else {
                error
            }
        })?;

    if !response.ok() {
        return Err(read_oauth_response_error(&response, "Radius OAuth device authorization failed").into());
    }

    let data: Value = serde_json::from_str(&response.body).unwrap_or(Value::Null);
    let device_code = data.get("device_code").and_then(Value::as_str).filter(|value| !value.is_empty());
    let user_code = data.get("user_code").and_then(Value::as_str).filter(|value| !value.is_empty());
    let verification_uri = data.get("verification_uri").and_then(Value::as_str).filter(|value| !value.is_empty());
    let expires_in = data.get("expires_in").and_then(Value::as_f64).filter(|value| *value != 0.0);
    let (Some(device_code), Some(user_code), Some(verification_uri), Some(expires_in)) =
        (device_code, user_code, verification_uri, expires_in)
    else {
        anyhow::bail!("Radius OAuth device authorization response is missing required fields");
    };

    Ok(DeviceAuthorizationResponse {
        device_code: device_code.to_string(),
        user_code: user_code.to_string(),
        verification_uri: verification_uri.to_string(),
        expires_in,
        interval: data.get("interval").and_then(Value::as_f64),
    })
}

async fn login_with_device_code(
    transport: Arc<dyn OAuthTransport>,
    gateway: String,
    interaction: &ProviderAuthInteraction,
) -> anyhow::Result<OAuthCredential> {
    let device = request_device_authorization(transport.as_ref(), &gateway, &interaction.signal).await?;
    let poll_signal = interaction.signal.clone();
    interaction.notify(AuthEvent::DeviceCode {
        user_code: device.user_code.clone(),
        verification_uri: device.verification_uri.clone(),
        interval_seconds: device.interval,
        expires_in_seconds: Some(device.expires_in),
    });

    poll_oauth_device_code_flow(OAuthDeviceCodePollOptions {
        interval_seconds: device.interval,
        expires_in_seconds: Some(device.expires_in),
        wait_before_first_poll: false,
        signal: interaction.signal.clone(),
        poll: Box::new(move || {
            let transport = transport.clone();
            let gateway = gateway.clone();
            let device = device.clone();
            let signal = poll_signal.clone();
            Box::pin(async move {
                match request_oauth_token(
                    transport.as_ref(),
                    &gateway,
                    &[
                        ("grant_type", OAUTH_DEVICE_CODE_GRANT_TYPE),
                        ("client_id", OAUTH_CLIENT_ID),
                        ("device_code", &device.device_code),
                    ],
                    &signal,
                )
                .await
                {
                    Ok(credential) => Ok(OAuthDeviceCodePollResult::Complete(credential)),
                    Err(error) => match error.downcast_ref::<OAuthResponseError>() {
                        None => Err(error),
                        Some(response_error) => match response_error.oauth_error.as_deref() {
                            Some("authorization_pending") => Ok(OAuthDeviceCodePollResult::Pending),
                            Some("slow_down") => Ok(OAuthDeviceCodePollResult::SlowDown { interval_seconds: None }),
                            Some("expired_token") => Ok(OAuthDeviceCodePollResult::Failed {
                                message: "Device authorization expired.".into(),
                            }),
                            Some("access_denied") => Ok(OAuthDeviceCodePollResult::Failed {
                                message: "Device authorization was denied.".into(),
                            }),
                            _ => Err(error),
                        },
                    },
                }
            })
        }),
    })
    .await
}

pub struct RadiusOAuth {
    name: String,
    gateway: String,
    transport: Arc<dyn OAuthTransport>,
    callback_port: u16,
}

impl RadiusOAuth {
    pub fn new(name: &str, gateway: &str, transport: Arc<dyn OAuthTransport>) -> Self {
        Self {
            name: name.to_string(),
            gateway: normalize_gateway_url(gateway),
            transport,
            callback_port: CALLBACK_PORT,
        }
    }

    pub fn with_callback_port(mut self, port: u16) -> Self {
        self.callback_port = port;
        self
    }
}

pub fn create_radius_oauth(name: &str, gateway: &str, transport: Arc<dyn OAuthTransport>) -> RadiusOAuth {
    RadiusOAuth::new(name, gateway, transport)
}

pub fn radius_oauth(name: &str, gateway: &str) -> Arc<dyn OAuthAuth> {
    Arc::new(RadiusOAuth::new(name, gateway, default_transport()))
}

#[async_trait]
impl OAuthAuth for RadiusOAuth {
    fn name(&self) -> &str {
        &self.name
    }

    async fn login(&self, interaction: &ProviderAuthInteraction) -> anyhow::Result<OAuthCredential> {
        let login_method = interaction
            .prompt(AuthPrompt {
                kind: AuthPromptKind::Select {
                    message: format!("Sign in to {}:", self.name),
                    options: vec![
                        AuthPromptOption {
                            id: LOGIN_METHOD_BROWSER.into(),
                            label: "Sign in with browser (recommended)".into(),
                            description: None,
                        },
                        AuthPromptOption {
                            id: LOGIN_METHOD_DEVICE_CODE.into(),
                            label: "Sign in with device code (when signing in from another device)".into(),
                            description: None,
                        },
                    ],
                },
                signal: None,
            })
            .await?;

        if login_method == LOGIN_METHOD_DEVICE_CODE {
            return login_with_device_code(self.transport.clone(), self.gateway.clone(), interaction).await;
        }
        if login_method == LOGIN_METHOD_BROWSER {
            let discovery = load_radius_oauth_discovery(self.transport.as_ref(), &self.gateway, &interaction.signal).await?;
            return login_with_browser(
                self.transport.as_ref(),
                &self.gateway,
                &discovery.authorization_endpoint,
                interaction,
            )
            .await;
        }
        anyhow::bail!("Unknown {} sign-in method: {login_method}", self.name)
    }

    async fn refresh(&self, credential: &OAuthCredential, signal: &AbortSignal) -> anyhow::Result<OAuthCredential> {
        request_oauth_token(
            self.transport.as_ref(),
            &self.gateway,
            &[
                ("grant_type", "refresh_token"),
                ("client_id", OAUTH_CLIENT_ID),
                ("refresh_token", &credential.refresh),
            ],
            signal,
        )
        .await
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
    use crate::utils::abort::AbortController;
    use std::sync::Mutex as StdMutex;

    const GATEWAY: &str = "https://radius.example";

    fn json(status: u16, body: Value) -> ScriptedResponse {
        ScriptedResponse::Json { status, body }
    }

    struct ScriptedInteraction {
        signal: AbortSignal,
        login_method: String,
        events: Arc<StdMutex<Vec<AuthEvent>>>,
    }

    #[async_trait]
    impl AuthInteraction for ScriptedInteraction {
        fn signal(&self) -> Option<AbortSignal> {
            Some(self.signal.clone())
        }
        fn on_account_committed(&self, _receipt: AccountLoginReceipt) {}
        async fn prompt(&self, _prompt: AuthPrompt) -> anyhow::Result<String> {
            Ok(self.login_method.clone())
        }
        fn notify(&self, event: AuthEvent) {
            self.events.lock().unwrap_or_else(|p| p.into_inner()).push(event);
        }
    }

    fn interaction(login_method: &str) -> (ProviderAuthInteraction, Arc<StdMutex<Vec<AuthEvent>>>) {
        let events = Arc::new(StdMutex::new(Vec::new()));
        let signal = AbortController::new().signal();
        let inner = Arc::new(ScriptedInteraction {
            signal: signal.clone(),
            login_method: login_method.to_string(),
            events: events.clone(),
        });
        (ProviderAuthInteraction::new(signal, inner), events)
    }

    fn form_of(request: &HttpRequest) -> Vec<(String, String)> {
        url::form_urlencoded::parse(request.body.as_deref().unwrap_or_default().as_bytes())
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect()
    }

    fn form_value(request: &HttpRequest, key: &str) -> Option<String> {
        form_of(request).into_iter().find(|(k, _)| k == key).map(|(_, value)| value)
    }

    #[test]
    fn gateway_urls_get_a_scheme_and_no_trailing_slash() {
        assert_eq!(normalize_gateway_url("radius.example"), "https://radius.example");
        assert_eq!(normalize_gateway_url("http://radius.example/"), "http://radius.example");
    }

    #[tokio::test(start_paused = true)]
    async fn uses_gateway_endpoints_directly_for_device_login() {
        let transport = Arc::new(ScriptedTransport::new(vec![
            (
                Some("/v1/oauth/device"),
                json(
                    200,
                    serde_json::json!({"device_code": "device-code", "user_code": "ABCD-1234", "verification_uri": "https://radius-ui.example/pair", "expires_in": 600, "interval": 5}),
                ),
            ),
            (
                Some("/v1/oauth/token"),
                json(
                    200,
                    serde_json::json!({"access_token": "access-token", "refresh_token": "refresh-token", "expires_in": 3600, "scope": "gateway offline_access"}),
                ),
            ),
        ]));
        transport.set_now_ms(1_784_851_200_000.0);
        let oauth = RadiusOAuth::new("Radius", GATEWAY, transport.clone());
        let (interaction, events) = interaction("device-code");

        let credential = oauth.login(&interaction).await.unwrap();

        assert_eq!(credential.access, "access-token");
        assert_eq!(credential.refresh, "refresh-token");
        assert_eq!(credential.expires, 1_784_851_200_000.0 + 3600.0 * 1000.0 - 60_000.0);
        assert_eq!(credential.get_extra_str("scope"), Some("gateway offline_access"));
        assert_eq!(
            *events.lock().unwrap_or_else(|p| p.into_inner()),
            vec![AuthEvent::DeviceCode {
                user_code: "ABCD-1234".into(),
                verification_uri: "https://radius-ui.example/pair".into(),
                interval_seconds: Some(5.0),
                expires_in_seconds: Some(600.0),
            }]
        );

        let urls: Vec<String> = transport.requests().iter().map(|request| request.url.clone()).collect();
        assert_eq!(urls, vec![format!("{GATEWAY}/v1/oauth/device"), format!("{GATEWAY}/v1/oauth/token")]);
        let device_request = &transport.requests()[0];
        assert_eq!(form_value(device_request, "client_id").as_deref(), Some("pi-gateway"));
        assert_eq!(form_value(device_request, "scope").as_deref(), Some("gateway offline_access"));
        let token_request = &transport.requests()[1];
        assert_eq!(form_value(token_request, "grant_type").as_deref(), Some(OAUTH_DEVICE_CODE_GRANT_TYPE));
        assert_eq!(form_value(token_request, "device_code").as_deref(), Some("device-code"));
    }

    #[tokio::test(start_paused = true)]
    async fn refreshes_directly_through_the_gateway_without_discovery() {
        let transport = Arc::new(ScriptedTransport::new(vec![(
            Some("/v1/oauth/token"),
            json(
                200,
                serde_json::json!({"access_token": "new-access", "refresh_token": "new-refresh", "expires_in": 3600}),
            ),
        )]));
        let oauth = RadiusOAuth::new("Radius", GATEWAY, transport.clone());
        let credential = oauth
            .refresh(&OAuthCredential::new("old-access", "old-refresh", 0.0), &AbortController::new().signal())
            .await
            .unwrap();
        assert_eq!(credential.access, "new-access");
        assert_eq!(credential.refresh, "new-refresh");

        let requests = transport.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].url, format!("{GATEWAY}/v1/oauth/token"));
        assert_eq!(form_value(&requests[0], "grant_type").as_deref(), Some("refresh_token"));
        assert_eq!(form_value(&requests[0], "client_id").as_deref(), Some("pi-gateway"));
        assert_eq!(form_value(&requests[0], "refresh_token").as_deref(), Some("old-refresh"));
    }

    #[tokio::test(start_paused = true)]
    async fn discovers_only_the_interactive_browser_authorization_endpoint() {
        let transport = Arc::new(ScriptedTransport::new(vec![(
            Some("/v1/oauth"),
            json(200, serde_json::json!({"issuer": "https://radius-ui.example"})),
        )]));
        let oauth = RadiusOAuth::new("Radius", GATEWAY, transport.clone());
        let (interaction, _) = interaction("browser");
        let error = oauth.login(&interaction).await.unwrap_err();
        assert_eq!(error.to_string(), format!("Invalid Radius OAuth config from {GATEWAY}"));
        assert_eq!(transport.requests().len(), 1);
        assert_eq!(transport.requests()[0].url, format!("{GATEWAY}/v1/oauth"));
    }

    #[tokio::test(start_paused = true)]
    async fn unknown_sign_in_methods_are_rejected() {
        let transport = Arc::new(ScriptedTransport::default());
        let oauth = RadiusOAuth::new("Radius", GATEWAY, transport.clone());
        let (interaction, _) = interaction("carrier-pigeon");
        let error = oauth.login(&interaction).await.unwrap_err();
        assert_eq!(error.to_string(), "Unknown Radius sign-in method: carrier-pigeon");
        assert!(transport.requests().is_empty());
    }
}
