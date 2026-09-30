//! Port of senpi packages/ai/src/auth/oauth/xai.ts.

use crate::auth::oauth::device_code::{
    OAuthDeviceCodePollOptions, OAuthDeviceCodePollResult, poll_oauth_device_code_flow,
};
use crate::auth::oauth::transport::{OAuthTransport, default_transport};
use crate::auth::types::{AuthEvent, ModelAuth, OAuthAuth, OAuthCredential, ProviderAuthInteraction};
use crate::utils::abort::AbortSignal;
use async_trait::async_trait;
use serde_json::{Map, Value};
use std::sync::Arc;
use url::Url;

const XAI_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const XAI_SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";
const XAI_DEVICE_CODE_URL: &str = "https://auth.x.ai/oauth2/device/code";
const XAI_TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
const REFRESH_SKEW_MS: f64 = 5.0 * 60.0 * 1000.0;
const DEFAULT_TOKEN_LIFETIME_SECONDS: f64 = 3600.0;

struct OAuthHttpResponse {
    ok: bool,
    status: u16,
    body: Map<String, Value>,
}

#[derive(Debug, Clone)]
pub struct XaiDeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub interval_seconds: Option<f64>,
    pub expires_in_seconds: f64,
}

fn required_string(body: &Map<String, Value>, field: &str) -> anyhow::Result<String> {
    match body.get(field) {
        Some(Value::String(value)) if !value.is_empty() => Ok(value.clone()),
        _ => anyhow::bail!("Invalid xAI OAuth response field: {field}"),
    }
}

fn positive_number(body: &Map<String, Value>, field: &str) -> anyhow::Result<f64> {
    match body.get(field) {
        Some(Value::Number(value)) => match value.as_f64() {
            Some(value) if value.is_finite() && value > 0.0 => Ok(value),
            _ => anyhow::bail!("Invalid xAI OAuth response field: {field}"),
        },
        _ => anyhow::bail!("Invalid xAI OAuth response field: {field}"),
    }
}

fn validate_verification_uri(raw: &str) -> anyhow::Result<String> {
    let url = Url::parse(raw).map_err(|_| anyhow::anyhow!("Untrusted verification URI in xAI OAuth response"))?;
    if url.scheme() != "https" {
        anyhow::bail!("Untrusted verification URI in xAI OAuth response");
    }
    Ok(url.to_string())
}

fn form_body(fields: &[(&str, String)]) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (key, value) in fields {
        serializer.append_pair(key, value);
    }
    serializer.finish()
}

async fn post_form(
    transport: &dyn OAuthTransport,
    url: &str,
    fields: &[(&str, String)],
    signal: &AbortSignal,
) -> anyhow::Result<OAuthHttpResponse> {
    let request = crate::auth::oauth::transport::HttpRequest {
        method: "POST".into(),
        url: url.to_string(),
        headers: vec![
            ("Accept".into(), "application/json".into()),
            ("Content-Type".into(), "application/x-www-form-urlencoded".into()),
        ],
        body: Some(form_body(fields)),
        timeout_ms: None,
    };

    let response = match transport.execute(request, signal).await {
        Ok(response) => response,
        Err(error) => {
            if signal.aborted() {
                anyhow::bail!("Login cancelled");
            }
            return Err(error);
        }
    };

    let body = match serde_json::from_str::<Value>(&response.body) {
        Ok(Value::Object(object)) => object,
        Ok(_) => Map::new(),
        Err(_) => {
            if signal.aborted() {
                anyhow::bail!("Login cancelled");
            }
            anyhow::bail!("xAI OAuth returned invalid JSON (HTTP {})", response.status);
        }
    };

    Ok(OAuthHttpResponse { ok: response.ok(), status: response.status, body })
}

fn request_failure(action: &str, response: &OAuthHttpResponse) -> anyhow::Error {
    let error = response.body.get("error").and_then(Value::as_str);
    let description = response.body.get("error_description").and_then(Value::as_str);
    let detail = [error, description].into_iter().flatten().collect::<Vec<_>>().join(": ");
    let suffix = if detail.is_empty() { String::new() } else { format!(": {detail}") };
    anyhow::anyhow!("xAI OAuth {action} failed (HTTP {}){suffix}", response.status)
}

fn parse_device_code(body: &Map<String, Value>) -> anyhow::Result<XaiDeviceCode> {
    let interval = body.get("interval");
    let interval_seconds = match interval {
        Some(Value::Number(value)) => match value.as_f64() {
            Some(value) if value.is_finite() && value > 0.0 => Some(value),
            _ => None,
        },
        _ => None,
    };
    let verification_uri_complete = match body.get("verification_uri_complete") {
        Some(Value::String(value)) if !value.is_empty() => Some(validate_verification_uri(value)?),
        _ => None,
    };
    Ok(XaiDeviceCode {
        device_code: required_string(body, "device_code")?,
        user_code: required_string(body, "user_code")?,
        verification_uri: validate_verification_uri(&required_string(body, "verification_uri")?)?,
        verification_uri_complete,
        interval_seconds,
        expires_in_seconds: positive_number(body, "expires_in")?,
    })
}

fn credentials_from_token_response(
    body: &Map<String, Value>,
    previous_refresh_token: Option<&str>,
    now_ms: f64,
) -> anyhow::Result<OAuthCredential> {
    let access = required_string(body, "access_token")?;
    let refresh = match body.get("refresh_token") {
        None => match previous_refresh_token {
            Some(previous) => previous.to_string(),
            None => required_string(body, "refresh_token")?,
        },
        Some(Value::Null) => match previous_refresh_token {
            Some(previous) => previous.to_string(),
            None => required_string(body, "refresh_token")?,
        },
        Some(_) => required_string(body, "refresh_token")?,
    };
    let expires_in_seconds = match body.get("expires_in") {
        None | Some(Value::Null) => DEFAULT_TOKEN_LIFETIME_SECONDS,
        Some(_) => positive_number(body, "expires_in")?,
    };
    Ok(OAuthCredential::new(access, refresh, now_ms + expires_in_seconds * 1000.0 - REFRESH_SKEW_MS))
}

async fn request_device_code(
    transport: &dyn OAuthTransport,
    signal: &AbortSignal,
) -> anyhow::Result<XaiDeviceCode> {
    let response = post_form(
        transport,
        XAI_DEVICE_CODE_URL,
        &[
            ("client_id", XAI_CLIENT_ID.to_string()),
            ("scope", XAI_SCOPE.to_string()),
            ("referrer", "pi".to_string()),
        ],
        signal,
    )
    .await?;
    if !response.ok {
        return Err(request_failure("device authorization", &response));
    }
    parse_device_code(&response.body)
}

async fn poll_for_tokens(
    transport: Arc<dyn OAuthTransport>,
    device: &XaiDeviceCode,
    signal: &AbortSignal,
) -> anyhow::Result<OAuthCredential> {
    let device = device.clone();
    let signal = signal.clone();
    poll_oauth_device_code_flow(OAuthDeviceCodePollOptions {
        interval_seconds: device.interval_seconds,
        expires_in_seconds: Some(device.expires_in_seconds),
        wait_before_first_poll: true,
        signal: signal.clone(),
        poll: Box::new(move || {
            let device = device.clone();
            let transport = transport.clone();
            let signal = signal.clone();
            let now_ms = transport.now_ms();
            Box::pin(async move {
                let response = post_form(
                    transport.as_ref(),
                    XAI_TOKEN_URL,
                    &[
                        ("grant_type", "urn:ietf:params:oauth:grant-type:device_code".to_string()),
                        ("client_id", XAI_CLIENT_ID.to_string()),
                        ("device_code", device.device_code.clone()),
                    ],
                    &signal,
                )
                .await?;

                if response.ok {
                    return Ok(OAuthDeviceCodePollResult::Complete(credentials_from_token_response(
                        &response.body,
                        None,
                        now_ms,
                    )?));
                }

                match response.body.get("error").and_then(Value::as_str) {
                    Some("authorization_pending") => Ok(OAuthDeviceCodePollResult::Pending),
                    Some("slow_down") => Ok(OAuthDeviceCodePollResult::SlowDown {
                        interval_seconds: response.body.get("interval").and_then(Value::as_f64),
                    }),
                    Some("access_denied" | "authorization_denied") => Ok(OAuthDeviceCodePollResult::Failed {
                        message: "xAI device authorization was denied".into(),
                    }),
                    Some("expired_token") => {
                        Ok(OAuthDeviceCodePollResult::Failed { message: "xAI device code expired".into() })
                    }
                    _ => Ok(OAuthDeviceCodePollResult::Failed {
                        message: request_failure("device token polling", &response).to_string(),
                    }),
                }
            })
        }),
    })
    .await
}

#[derive(Clone)]
pub struct XaiOAuth {
    transport: Arc<dyn OAuthTransport>,
}

impl XaiOAuth {
    pub fn new(transport: Arc<dyn OAuthTransport>) -> Self {
        Self { transport }
    }

    pub fn transport(&self) -> &Arc<dyn OAuthTransport> {
        &self.transport
    }
}

pub fn xai_oauth() -> Arc<dyn OAuthAuth> {
    Arc::new(XaiOAuth::new(default_transport()))
}

#[async_trait]
impl OAuthAuth for XaiOAuth {
    fn name(&self) -> &str {
        "xAI (Grok/X subscription)"
    }

    fn is_subscription(&self) -> bool {
        true
    }

    fn login_label(&self) -> Option<&str> {
        Some("Sign in with SuperGrok or X Premium")
    }

    async fn login(&self, interaction: &ProviderAuthInteraction) -> anyhow::Result<OAuthCredential> {
        let device = request_device_code(self.transport.as_ref(), &interaction.signal).await?;
        interaction.notify(AuthEvent::DeviceCode {
            user_code: device.user_code.clone(),
            verification_uri: device
                .verification_uri_complete
                .clone()
                .unwrap_or_else(|| device.verification_uri.clone()),
            interval_seconds: device.interval_seconds,
            expires_in_seconds: Some(device.expires_in_seconds),
        });
        poll_for_tokens(self.transport.clone(), &device, &interaction.signal).await
    }

    async fn refresh(&self, credential: &OAuthCredential, signal: &AbortSignal) -> anyhow::Result<OAuthCredential> {
        let response = post_form(
            self.transport.as_ref(),
            XAI_TOKEN_URL,
            &[
                ("grant_type", "refresh_token".to_string()),
                ("client_id", XAI_CLIENT_ID.to_string()),
                ("refresh_token", credential.refresh.clone()),
            ],
            signal,
        )
        .await?;
        if !response.ok {
            return Err(request_failure("token refresh", &response));
        }
        credentials_from_token_response(&response.body, Some(&credential.refresh), self.transport.now_ms())
    }

    async fn to_auth(&self, credential: &OAuthCredential) -> anyhow::Result<ModelAuth> {
        Ok(ModelAuth { api_key: Some(credential.access.clone()), ..Default::default() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::oauth::transport::{HttpRequest, ScriptedResponse, ScriptedTransport};
    use crate::auth::types::{AccountLoginReceipt, AuthInteraction, AuthPrompt};
    use crate::utils::abort::{AbortController, AbortReason};
    use std::sync::Mutex;
    use std::time::Duration;
    use tokio::time::Instant;

    const START_MS: f64 = 1_783_886_400_000.0;

    fn json(status: u16, body: Value) -> ScriptedResponse {
        ScriptedResponse::Json { status, body }
    }

    fn device_code_response(overrides: &[(&str, Value)]) -> Value {
        let mut body = serde_json::json!({
            "device_code": "device-code",
            "user_code": "ABCD-1234",
            "verification_uri": "https://accounts.x.ai/oauth2/device",
            "expires_in": 900,
            "interval": 5,
        });
        for (key, value) in overrides {
            if value.is_null() {
                body.as_object_mut().expect("object").remove(*key);
            } else {
                body.as_object_mut().expect("object").insert((*key).to_string(), value.clone());
            }
        }
        body
    }

    fn token_response(overrides: &[(&str, Value)]) -> Value {
        let mut body = serde_json::json!({
            "access_token": "access-token",
            "refresh_token": "refresh-token",
            "expires_in": 21_600,
            "token_type": "Bearer",
        });
        for (key, value) in overrides {
            if value.is_null() {
                body.as_object_mut().expect("object").remove(*key);
            } else {
                body.as_object_mut().expect("object").insert((*key).to_string(), value.clone());
            }
        }
        body
    }

    fn form_of(request: &HttpRequest) -> Vec<(String, String)> {
        url::form_urlencoded::parse(request.body.as_deref().unwrap_or_default().as_bytes())
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect()
    }

    fn form_value(request: &HttpRequest, key: &str) -> Option<String> {
        form_of(request).into_iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    struct RecordingInteraction {
        signal: AbortSignal,
        device_codes: Arc<Mutex<Vec<AuthEvent>>>,
        on_device_code: Option<Arc<dyn Fn() + Send + Sync>>,
    }

    #[async_trait]
    impl AuthInteraction for RecordingInteraction {
        fn signal(&self) -> Option<AbortSignal> {
            Some(self.signal.clone())
        }
        fn on_account_committed(&self, _receipt: AccountLoginReceipt) {}
        async fn prompt(&self, _prompt: AuthPrompt) -> anyhow::Result<String> {
            anyhow::bail!("Unexpected prompt")
        }
        fn notify(&self, event: AuthEvent) {
            if let AuthEvent::DeviceCode { .. } = &event {
                self.device_codes.lock().unwrap_or_else(|p| p.into_inner()).push(event.clone());
                if let Some(callback) = &self.on_device_code {
                    callback();
                }
            }
        }
    }

    fn interaction(signal: AbortSignal) -> (ProviderAuthInteraction, Arc<Mutex<Vec<AuthEvent>>>) {
        let device_codes = Arc::new(Mutex::new(Vec::new()));
        let inner = Arc::new(RecordingInteraction {
            signal: signal.clone(),
            device_codes: device_codes.clone(),
            on_device_code: None,
        });
        (ProviderAuthInteraction::new(signal, inner), device_codes)
    }

    fn interaction_with_hook(
        signal: AbortSignal,
        hook: Arc<dyn Fn() + Send + Sync>,
    ) -> (ProviderAuthInteraction, Arc<Mutex<Vec<AuthEvent>>>) {
        let device_codes = Arc::new(Mutex::new(Vec::new()));
        let inner = Arc::new(RecordingInteraction {
            signal: signal.clone(),
            device_codes: device_codes.clone(),
            on_device_code: Some(hook),
        });
        (ProviderAuthInteraction::new(signal, inner), device_codes)
    }

    async fn advance(transport: &ScriptedTransport, ms: u64) {
        transport.advance_ms(ms as f64);
        tokio::time::advance(Duration::from_millis(ms)).await;
        for _ in 0..64 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn uses_the_device_grant_delays_polling_and_handles_pending_and_slow_down() {
        let transport = Arc::new(ScriptedTransport::new(vec![
            (Some("device/code"), json(200, device_code_response(&[]))),
            (Some("oauth2/token"), json(400, serde_json::json!({"error": "authorization_pending"}))),
            (Some("oauth2/token"), json(400, serde_json::json!({"error": "slow_down", "interval": 10}))),
            (Some("oauth2/token"), json(200, token_response(&[]))),
        ]));
        transport.set_now_ms(START_MS);
        let oauth = XaiOAuth::new(transport.clone());
        let start = Instant::now();
        let (interaction, device_codes) = interaction(AbortController::new().signal());

        let login = tokio::spawn({
            let oauth = oauth.clone();
            async move { oauth.login(&interaction).await }
        });

        advance(&transport, 0).await;
        let codes = device_codes.lock().unwrap_or_else(|p| p.into_inner()).clone();        assert_eq!(codes.len(), 1);
        assert_eq!(
            codes[0],
            AuthEvent::DeviceCode {
                user_code: "ABCD-1234".into(),
                verification_uri: "https://accounts.x.ai/oauth2/device".into(),
                interval_seconds: Some(5.0),
                expires_in_seconds: Some(900.0),
            }
        );

        let poll_times = Arc::new(Mutex::new(Vec::<Instant>::new()));
        advance(&transport, 5000).await;
        let _ = &poll_times;
        assert_eq!(transport.requests().len(), 2);
        let first_poll = &transport.requests()[1];
        assert_eq!(first_poll.url, XAI_TOKEN_URL);
        assert_eq!(form_value(first_poll, "grant_type").as_deref(), Some("urn:ietf:params:oauth:grant-type:device_code"));
        assert_eq!(form_value(first_poll, "client_id").as_deref(), Some(XAI_CLIENT_ID));
        assert_eq!(form_value(first_poll, "device_code").as_deref(), Some("device-code"));

        advance(&transport, 5000).await;
        assert_eq!(transport.requests().len(), 3);
        advance(&transport, 10_000).await;
        let credential = login.await.unwrap().unwrap();
        assert_eq!(transport.requests().len(), 4);

        let device_request = &transport.requests()[0];
        assert_eq!(device_request.url, XAI_DEVICE_CODE_URL);
        assert_eq!(form_value(device_request, "client_id").as_deref(), Some(XAI_CLIENT_ID));
        assert_eq!(form_value(device_request, "scope").as_deref(), Some(XAI_SCOPE));
        assert_eq!(form_value(device_request, "referrer").as_deref(), Some("pi"));

        assert_eq!(credential.access, "access-token");
        assert_eq!(credential.refresh, "refresh-token");
        let _ = start;
        assert_eq!(credential.expires, START_MS + 20_000.0 + 21_600_000.0 - 300_000.0);
    }

    #[tokio::test(start_paused = true)]
    async fn falls_back_to_the_default_poll_interval_when_the_response_reports_interval_zero() {
        let transport = Arc::new(ScriptedTransport::new(vec![
            (Some("device/code"), json(200, device_code_response(&[("interval", serde_json::json!(0))]))),
            (Some("oauth2/token"), json(200, token_response(&[]))),
        ]));
        transport.set_now_ms(START_MS);
        let oauth = XaiOAuth::new(transport.clone());
        let (interaction, _) = interaction(AbortController::new().signal());
        let login = tokio::spawn({
            let oauth = oauth.clone();
            async move { oauth.login(&interaction).await }
        });

        advance(&transport, 4999).await;
        assert_eq!(transport.requests().len(), 1);
        advance(&transport, 1).await;
        login.await.unwrap().unwrap();
        assert_eq!(transport.requests().len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn prefers_verification_uri_complete_when_the_server_provides_it() {
        let transport = Arc::new(ScriptedTransport::new(vec![
            (
                Some("device/code"),
                json(
                    200,
                    device_code_response(&[(
                        "verification_uri_complete",
                        serde_json::json!("https://accounts.x.ai/oauth2/device?user_code=ABCD-1234"),
                    )]),
                ),
            ),
            (Some("oauth2/token"), json(200, token_response(&[]))),
        ]));
        transport.set_now_ms(START_MS);
        let oauth = XaiOAuth::new(transport.clone());
        let (interaction, device_codes) = interaction(AbortController::new().signal());
        let login = tokio::spawn({
            let oauth = oauth.clone();
            async move { oauth.login(&interaction).await }
        });

        advance(&transport, 5000).await;
        login.await.unwrap().unwrap();
        let codes = device_codes.lock().unwrap_or_else(|p| p.into_inner()).clone();
        assert_eq!(
            codes,
            vec![AuthEvent::DeviceCode {
                user_code: "ABCD-1234".into(),
                verification_uri: "https://accounts.x.ai/oauth2/device?user_code=ABCD-1234".into(),
                interval_seconds: Some(5.0),
                expires_in_seconds: Some(900.0),
            }]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn rejects_a_non_https_verification_uri_complete() {
        let transport = Arc::new(ScriptedTransport::new(vec![(
            Some("device/code"),
            json(
                200,
                device_code_response(&[(
                    "verification_uri_complete",
                    serde_json::json!("http://accounts.x.ai/oauth2/device?user_code=ABCD-1234"),
                )]),
            ),
        )]));
        transport.set_now_ms(START_MS);
        let oauth = XaiOAuth::new(transport);
        let (interaction, _) = interaction(AbortController::new().signal());
        let error = oauth.login(&interaction).await.unwrap_err();
        assert!(error.to_string().contains("Untrusted verification URI"), "{error}");
    }

    #[tokio::test(start_paused = true)]
    async fn rejects_non_https_and_malformed_verification_uris() {
        for uri in ["http://accounts.x.ai/oauth2/device", "file:///etc/passwd", "not a url"] {
            let transport = Arc::new(ScriptedTransport::new(vec![(
                Some("device/code"),
                json(200, device_code_response(&[("verification_uri", serde_json::json!(uri))])),
            )]));
            transport.set_now_ms(START_MS);
            let oauth = XaiOAuth::new(transport);
            let (interaction, _) = interaction(AbortController::new().signal());
            let error = oauth.login(&interaction).await.unwrap_err();
            assert!(error.to_string().contains("Untrusted verification URI"), "{uri}: {error}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn fails_when_device_authorization_is_denied() {
        for code in ["access_denied", "authorization_denied"] {
            let transport = Arc::new(ScriptedTransport::new(vec![
                (Some("device/code"), json(200, device_code_response(&[("interval", serde_json::json!(1))]))),
                (Some("oauth2/token"), json(400, serde_json::json!({ "error": code }))),
            ]));
            transport.set_now_ms(START_MS);
            let oauth = XaiOAuth::new(transport.clone());
            let (interaction, _) = interaction(AbortController::new().signal());
            let login = tokio::spawn({
                let oauth = oauth.clone();
                async move { oauth.login(&interaction).await }
            });
            advance(&transport, 1000).await;
            let error = login.await.unwrap().unwrap_err();
            assert_eq!(error.to_string(), "xAI device authorization was denied");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn cancels_while_waiting_for_the_first_token_poll() {
        let transport = Arc::new(ScriptedTransport::new(vec![(
            Some("device/code"),
            json(200, device_code_response(&[])),
        )]));
        transport.set_now_ms(START_MS);
        let oauth = XaiOAuth::new(transport.clone());
        let controller = AbortController::new();
        let signal = controller.signal();
        let aborting = controller.clone();
        let (interaction, _) =
            interaction_with_hook(signal, Arc::new(move || aborting.abort(Some(AbortReason::new("AbortError", "aborted")))));
        let error = oauth.login(&interaction).await.unwrap_err();
        assert_eq!(error.to_string(), "Login cancelled");
        assert_eq!(transport.requests().len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn refreshes_tokens_and_preserves_an_unrotated_refresh_token() {
        let transport = Arc::new(ScriptedTransport::new(vec![
            (
                Some("oauth2/token"),
                json(200, token_response(&[("access_token", serde_json::json!("new-access")), ("refresh_token", serde_json::json!("new-refresh"))])),
            ),
            (
                Some("oauth2/token"),
                json(200, token_response(&[("access_token", serde_json::json!("newer-access")), ("refresh_token", Value::Null)])),
            ),
        ]));
        transport.set_now_ms(START_MS);
        let oauth = XaiOAuth::new(transport.clone());
        let signal = AbortController::new().signal();

        let rotated = oauth
            .refresh(&OAuthCredential::new("old-access", "old-refresh", 0.0), &signal)
            .await
            .unwrap();
        let preserved = oauth
            .refresh(&OAuthCredential::new("old-access", "keep-refresh", 0.0), &signal)
            .await
            .unwrap();

        assert_eq!(rotated.access, "new-access");
        assert_eq!(rotated.refresh, "new-refresh");
        assert_eq!(preserved.access, "newer-access");
        assert_eq!(preserved.refresh, "keep-refresh");
        assert_eq!(oauth.name(), "xAI (Grok/X subscription)");
        assert_eq!(oauth.to_auth(&preserved).await.unwrap().api_key.as_deref(), Some("newer-access"));

        let requests = transport.requests();
        assert_eq!(requests[0].url, XAI_TOKEN_URL);
        assert_eq!(form_value(&requests[0], "grant_type").as_deref(), Some("refresh_token"));
        assert_eq!(form_value(&requests[0], "client_id").as_deref(), Some(XAI_CLIENT_ID));
        assert_eq!(form_value(&requests[0], "refresh_token").as_deref(), Some("old-refresh"));
        assert_eq!(form_value(&requests[1], "refresh_token").as_deref(), Some("keep-refresh"));
    }

    #[tokio::test(start_paused = true)]
    async fn assumes_a_one_hour_lifetime_when_expires_in_is_missing() {
        let transport = Arc::new(ScriptedTransport::new(vec![(
            Some("oauth2/token"),
            json(200, token_response(&[("expires_in", Value::Null)])),
        )]));
        transport.set_now_ms(START_MS);
        let oauth = XaiOAuth::new(transport);
        let credential = oauth
            .refresh(&OAuthCredential::new("old-access", "old-refresh", 0.0), &AbortController::new().signal())
            .await
            .unwrap();
        assert_eq!(credential.expires, START_MS + 3_600_000.0 - 300_000.0);
    }

    #[tokio::test(start_paused = true)]
    async fn rejects_token_responses_with_missing_fields() {
        let transport = Arc::new(ScriptedTransport::new(vec![(
            Some("oauth2/token"),
            json(200, token_response(&[("access_token", Value::Null)])),
        )]));
        transport.set_now_ms(START_MS);
        let oauth = XaiOAuth::new(transport);
        let error = oauth
            .refresh(&OAuthCredential::new("old-access", "old-refresh", 0.0), &AbortController::new().signal())
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "Invalid xAI OAuth response field: access_token");
    }

    #[tokio::test(start_paused = true)]
    async fn surfaces_the_upstream_error_code_and_description_on_refresh_failure() {
        let transport = Arc::new(ScriptedTransport::new(vec![(
            Some("oauth2/token"),
            json(400, serde_json::json!({"error": "invalid_grant", "error_description": "refresh token revoked"})),
        )]));
        transport.set_now_ms(START_MS);
        let oauth = XaiOAuth::new(transport);
        let error = oauth
            .refresh(&OAuthCredential::new("old-access", "old-refresh", 0.0), &AbortController::new().signal())
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "xAI OAuth token refresh failed (HTTP 400): invalid_grant: refresh token revoked");
    }
}
