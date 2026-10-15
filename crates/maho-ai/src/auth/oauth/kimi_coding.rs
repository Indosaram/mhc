//! Port of senpi packages/ai/src/auth/oauth/kimi-coding.ts.

use crate::auth::oauth::device_code::{
    OAuthDeviceCodePollOptions, OAuthDeviceCodePollResult, poll_oauth_device_code_flow,
};
use crate::auth::oauth::kimi_identity::kimi_code_identity_headers;
use crate::auth::oauth::kimi_region::{
    KimiCodeEndpoints, choose_kimi_code_login_endpoints, kimi_code_credential_env, kimi_code_endpoints_for_credential,
};
use crate::auth::oauth::transport::{HttpRequest, OAuthTransport, default_transport};
use crate::auth::types::{AuthEvent, ModelAuth, OAuthAuth, OAuthCredential, ProviderAuthInteraction};
use crate::types::{ProviderEnv, ProviderHeaders};
use crate::utils::abort::AbortSignal;
use crate::utils::sleep::sleep;
use async_trait::async_trait;
use serde_json::{Map, Value};
use std::sync::{Arc, LazyLock};
use url::Url;

const CLIENT_ID: &str = "17e5f671-d194-4dfb-9706-5516cb48c098";
const DEVICE_CODE_TIMEOUT_SECONDS: f64 = 15.0 * 60.0;
const DEFAULT_POLL_INTERVAL_SECONDS: f64 = 5.0;
const REQUEST_TIMEOUT_MS: u64 = 30 * 1000;
const REFRESH_MAX_RETRIES: usize = 3;

#[derive(Debug, Clone)]
pub struct DeviceAuthorization {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    pub interval_seconds: f64,
    pub expires_in_seconds: f64,
}

#[derive(Debug, Clone)]
pub struct TokenResponse {
    pub access: String,
    pub refresh: String,
    pub expires: f64,
}

fn form_url_encode(fields: &[(&str, &str)]) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (key, value) in fields {
        serializer.append_pair(key, value);
    }
    serializer.finish()
}

fn read_json(body: &str) -> Option<Map<String, Value>> {
    match serde_json::from_str::<Value>(body) {
        Ok(Value::Object(object)) => Some(object),
        _ => None,
    }
}

fn trusted_http_url(value: Option<&Value>) -> Option<String> {
    let value = value?.as_str()?;
    if value.is_empty() {
        return None;
    }
    let url = Url::parse(value).ok()?;
    if url.scheme() != "https" && url.scheme() != "http" {
        return None;
    }
    Some(url.to_string())
}

fn headers_for(transport: &dyn OAuthTransport) -> Vec<(String, String)> {
    let _ = transport;
    let mut headers: Vec<(String, String)> = kimi_code_identity_headers();
    headers.push(("Content-Type".into(), "application/x-www-form-urlencoded".into()));
    headers.push(("Accept".into(), "application/json".into()));
    headers
}

async fn post_form(
    transport: &dyn OAuthTransport,
    url: String,
    fields: &[(&str, &str)],
    signal: &AbortSignal,
) -> anyhow::Result<crate::auth::oauth::transport::HttpResponse> {
    let request = HttpRequest {
        method: "POST".into(),
        url,
        headers: headers_for(transport),
        body: Some(form_url_encode(fields)),
        timeout_ms: Some(REQUEST_TIMEOUT_MS),
    };
    transport.execute(request, signal).await
}

async fn start_device_authorization(
    transport: &dyn OAuthTransport,
    oauth_host: &str,
    signal: &AbortSignal,
) -> anyhow::Result<DeviceAuthorization> {
    let response =
        post_form(transport, format!("{oauth_host}/api/oauth/device_authorization"), &[("client_id", CLIENT_ID)], signal)
            .await?;

    if !response.ok() {
        let suffix = if response.body.is_empty() { String::new() } else { format!(": {}", response.body) };
        anyhow::bail!("Kimi Code device authorization failed with status {}{suffix}", response.status);
    }

    let json = read_json(&response.body);
    let device_code = json.as_ref().and_then(|json| json.get("device_code")).and_then(Value::as_str);
    let user_code = json.as_ref().and_then(|json| json.get("user_code")).and_then(Value::as_str);
    let verification_uri = json.as_ref().and_then(|json| json.get("verification_uri")).and_then(Value::as_str);
    let verification_uri_complete =
        json.as_ref().and_then(|json| json.get("verification_uri_complete")).and_then(Value::as_str);
    let complete_url = trusted_http_url(json.as_ref().and_then(|json| json.get("verification_uri_complete")));
    let plain_url = trusted_http_url(json.as_ref().and_then(|json| json.get("verification_uri")));

    let (Some(device_code), Some(user_code), Some(verification_uri), Some(verification_uri_complete)) =
        (device_code, user_code, verification_uri, verification_uri_complete)
    else {
        anyhow::bail!(
            "Invalid Kimi Code device authorization response: {}",
            serde_json::to_string(&json.unwrap_or_default()).unwrap_or_default()
        );
    };
    if complete_url.is_none() || plain_url.is_none() {
        anyhow::bail!(
            "Invalid Kimi Code device authorization response: {}",
            serde_json::to_string(&json.unwrap_or_default()).unwrap_or_default()
        );
    }

    let interval = json.as_ref().and_then(|json| json.get("interval")).and_then(Value::as_f64);
    let expires_in = json.as_ref().and_then(|json| json.get("expires_in")).and_then(Value::as_f64);
    Ok(DeviceAuthorization {
        device_code: device_code.to_string(),
        user_code: user_code.to_string(),
        verification_uri: verification_uri.to_string(),
        verification_uri_complete: verification_uri_complete.to_string(),
        interval_seconds: interval.filter(|value| value.is_finite() && *value > 0.0).unwrap_or(DEFAULT_POLL_INTERVAL_SECONDS),
        expires_in_seconds: expires_in
            .filter(|value| value.is_finite() && *value > 0.0)
            .unwrap_or(DEVICE_CODE_TIMEOUT_SECONDS),
    })
}

fn parse_token_response(json: Option<&Map<String, Value>>, operation: &str, now_ms: f64) -> anyhow::Result<TokenResponse> {
    let access = json.and_then(|json| json.get("access_token")).and_then(Value::as_str).filter(|value| !value.is_empty());
    let refresh =
        json.and_then(|json| json.get("refresh_token")).and_then(Value::as_str).filter(|value| !value.is_empty());
    let expires_in = json.and_then(|json| json.get("expires_in")).and_then(Value::as_f64);
    let (Some(access), Some(refresh), Some(expires_in)) = (access, refresh, expires_in) else {
        anyhow::bail!(
            "Kimi Code token {operation} response missing fields: {}",
            serde_json::to_string(&json.cloned().unwrap_or_default()).unwrap_or_default()
        );
    };
    if !expires_in.is_finite() || expires_in <= 0.0 {
        anyhow::bail!(
            "Kimi Code token {operation} response missing fields: {}",
            serde_json::to_string(&json.cloned().unwrap_or_default()).unwrap_or_default()
        );
    }
    Ok(TokenResponse { access: access.to_string(), refresh: refresh.to_string(), expires: now_ms + expires_in * 1000.0 })
}

async fn poll_for_token(
    transport: Arc<dyn OAuthTransport>,
    oauth_host: &str,
    device: &DeviceAuthorization,
    signal: &AbortSignal,
) -> anyhow::Result<TokenResponse> {
    let device = device.clone();
    let oauth_host = oauth_host.to_string();
    let signal = signal.clone();
    poll_oauth_device_code_flow(OAuthDeviceCodePollOptions {
        interval_seconds: Some(device.interval_seconds),
        expires_in_seconds: Some(device.expires_in_seconds),
        wait_before_first_poll: true,
        signal: signal.clone(),
        poll: Box::new(move || {
            let device = device.clone();
            let oauth_host = oauth_host.clone();
            let transport = transport.clone();
            let signal = signal.clone();
            let now_ms = transport.now_ms();
            Box::pin(async move {
                let response = post_form(
                    transport.as_ref(),
                    format!("{oauth_host}/api/oauth/token"),
                    &[
                        ("client_id", CLIENT_ID),
                        ("device_code", device.device_code.as_str()),
                        ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                    ],
                    &signal,
                )
                .await?;

                if response.status >= 500 {
                    let suffix = if response.body.is_empty() { String::new() } else { format!(": {}", response.body) };
                    return Ok(OAuthDeviceCodePollResult::Failed {
                        message: format!("Kimi Code device token request failed with status {}{suffix}", response.status),
                    });
                }

                let json = read_json(&response.body);
                if response.ok() && json.as_ref().and_then(|json| json.get("access_token")).and_then(Value::as_str).is_some()
                {
                    return Ok(match parse_token_response(json.as_ref(), "poll", now_ms) {
                        Ok(value) => OAuthDeviceCodePollResult::Complete(value),
                        Err(error) => OAuthDeviceCodePollResult::Failed { message: error.to_string() },
                    });
                }

                let error = json.as_ref().and_then(|json| json.get("error")).and_then(Value::as_str);
                let description = json
                    .as_ref()
                    .and_then(|json| json.get("error_description"))
                    .and_then(Value::as_str)
                    .map(|description| format!(": {description}"))
                    .unwrap_or_default();
                match error {
                    Some("authorization_pending") => Ok(OAuthDeviceCodePollResult::Pending),
                    Some("slow_down") => Ok(OAuthDeviceCodePollResult::SlowDown {
                        interval_seconds: json
                            .as_ref()
                            .and_then(|json| json.get("interval"))
                            .and_then(Value::as_f64)
                            .filter(|value| *value > 0.0),
                    }),
                    Some("expired_token") => Ok(OAuthDeviceCodePollResult::Failed {
                        message: "Kimi Code device authorization expired. Please restart login.".into(),
                    }),
                    Some("access_denied") => {
                        Ok(OAuthDeviceCodePollResult::Failed { message: "Kimi Code login was denied.".into() })
                    }
                    _ => {
                        let detail = if error.is_some() { format!(": {}{description}", error.unwrap_or_default()) } else { String::new() };
                        Ok(OAuthDeviceCodePollResult::Failed {
                            message: format!("Kimi Code device token request failed (status {}){detail}", response.status),
                        })
                    }
                }
            })
        }),
    })
    .await
}

fn is_retryable_refresh_failure(status: u16) -> bool {
    status == 429 || status >= 500
}

async fn refresh_token(
    transport: &dyn OAuthTransport,
    oauth_host: &str,
    refresh_token_value: &str,
    signal: &AbortSignal,
) -> anyhow::Result<TokenResponse> {
    let mut last_error: Option<anyhow::Error> = None;
    for attempt in 0..=REFRESH_MAX_RETRIES {
        if attempt > 0 {
            let delay = 1000u64 * 2u64.pow((attempt - 1) as u32);
            sleep(delay, signal).await.map_err(|reason| anyhow::anyhow!("{reason}"))?;
        }
        if signal.aborted() {
            anyhow::bail!("Kimi Code token refresh aborted");
        }

        let response = match post_form(
            transport,
            format!("{oauth_host}/api/oauth/token"),
            &[("client_id", CLIENT_ID), ("grant_type", "refresh_token"), ("refresh_token", refresh_token_value)],
            signal,
        )
        .await
        {
            Ok(response) => response,
            Err(error) => {
                last_error = Some(error);
                continue;
            }
        };

        let json = read_json(&response.body);
        if response.ok() {
            return parse_token_response(json.as_ref(), "refresh", transport.now_ms());
        }

        let error_code = json.as_ref().and_then(|json| json.get("error")).and_then(Value::as_str);
        if response.status == 401 || response.status == 403 || error_code == Some("invalid_grant") {
            let description = json
                .as_ref()
                .and_then(|json| json.get("error_description"))
                .and_then(Value::as_str)
                .map(|description| format!(": {description}"))
                .unwrap_or_default();
            anyhow::bail!("Kimi Code token refresh unauthorized (status {}){description}", response.status);
        }

        if is_retryable_refresh_failure(response.status) && attempt < REFRESH_MAX_RETRIES {
            last_error = Some(anyhow::anyhow!("Kimi Code token refresh failed with status {}", response.status));
            continue;
        }

        let text = serde_json::to_string(&json.unwrap_or_default()).unwrap_or_default();
        let suffix = if text.is_empty() { String::new() } else { format!(": {text}") };
        anyhow::bail!("Kimi Code token refresh failed with status {}{suffix}", response.status);
    }

    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("Kimi Code token refresh failed")))
}

fn credential_env(credential: &OAuthCredential) -> Option<ProviderEnv> {
    kimi_code_credential_env(&credential.extra)
}

fn endpoints_for_credential(credential: &OAuthCredential) -> KimiCodeEndpoints {
    kimi_code_endpoints_for_credential(&credential.extra)
}

fn with_env(credential: OAuthCredential, env: Option<ProviderEnv>) -> OAuthCredential {
    match env {
        Some(env) => credential.with_extra("env", serde_json::to_value(env).unwrap_or(Value::Null)),
        None => credential,
    }
}

#[derive(Clone)]
pub struct KimiCodingOAuth {
    transport: Arc<dyn OAuthTransport>,
}

impl KimiCodingOAuth {
    pub fn new(transport: Arc<dyn OAuthTransport>) -> Self {
        Self { transport }
    }

    pub fn transport(&self) -> &Arc<dyn OAuthTransport> {
        &self.transport
    }
}

pub fn kimi_coding_oauth() -> Arc<dyn OAuthAuth> {
    static INSTANCE: LazyLock<Arc<dyn OAuthAuth>> =
        LazyLock::new(|| Arc::new(KimiCodingOAuth::new(default_transport())));
    INSTANCE.clone()
}

#[async_trait]
impl OAuthAuth for KimiCodingOAuth {
    fn name(&self) -> &str {
        "Kimi Code (subscription)"
    }

    fn is_subscription(&self) -> bool {
        true
    }

    fn login_label(&self) -> Option<&str> {
        Some("Sign in with Kimi Code")
    }

    async fn login(&self, interaction: &ProviderAuthInteraction) -> anyhow::Result<OAuthCredential> {
        let endpoints = choose_kimi_code_login_endpoints(interaction).await?;
        let oauth_host = endpoints.oauth_host.clone();
        let device = start_device_authorization(self.transport.as_ref(), &oauth_host, &interaction.signal).await?;
        interaction.notify(AuthEvent::DeviceCode {
            user_code: device.user_code.clone(),
            verification_uri: device.verification_uri_complete.clone(),
            interval_seconds: Some(device.interval_seconds),
            expires_in_seconds: Some(device.expires_in_seconds),
        });
        let token = poll_for_token(self.transport.clone(), &oauth_host, &device, &interaction.signal).await?;
        Ok(with_env(OAuthCredential::new(token.access, token.refresh, token.expires), endpoints.env))
    }

    async fn refresh(&self, credential: &OAuthCredential, signal: &AbortSignal) -> anyhow::Result<OAuthCredential> {
        let env = credential_env(credential);
        let endpoints = endpoints_for_credential(credential);
        let token =
            refresh_token(self.transport.as_ref(), &endpoints.oauth_host, &credential.refresh, signal).await?;
        Ok(with_env(OAuthCredential::new(token.access, token.refresh, token.expires), env))
    }

    async fn to_auth(&self, credential: &OAuthCredential) -> anyhow::Result<ModelAuth> {
        let endpoints = endpoints_for_credential(credential);
        let mut headers: ProviderHeaders = kimi_code_identity_headers()
            .into_iter()
            .map(|(name, value)| (name, Some(value)))
            .collect();
        headers.insert("Authorization".into(), Some(format!("Bearer {}", credential.access)));
        Ok(ModelAuth { api_key: None, headers: Some(headers), base_url: endpoints.api_base_url })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::oauth::transport::{ScriptedResponse, ScriptedTransport};
    use crate::auth::types::{AccountLoginReceipt, AuthInteraction, AuthPrompt};
    use crate::utils::abort::AbortController;
    use std::sync::Mutex;
    use std::time::Duration;

    const START_MS: f64 = 1_784_505_600_000.0;
    const MAINLAND_OAUTH_HOST: &str = "https://auth.kimi.com";
    const GLOBAL_OAUTH_HOST: &str = "https://auth.kimi.ai";
    const GLOBAL_API_BASE_URL: &str = "https://api.kimi.ai/coding";

    fn json(status: u16, body: Value) -> ScriptedResponse {
        ScriptedResponse::Json { status, body }
    }

    fn device_authorization_response(overrides: &[(&str, Value)]) -> ScriptedResponse {
        let mut body = serde_json::json!({
            "user_code": "ABCD-1234",
            "device_code": "device-code-123",
            "verification_uri": "https://www.kimi.com/code",
            "verification_uri_complete": "https://www.kimi.com/code?user_code=ABCD-1234",
            "interval": 5,
            "expires_in": 600,
        });
        for (key, value) in overrides {
            body.as_object_mut().expect("object").insert((*key).to_string(), value.clone());
        }
        json(200, body)
    }

    /// The TS tests stub the Kimi env; Rust cannot mutate the process env under the
    /// crate-wide unsafe_code = "forbid" lint, so the same precondition is asserted instead.
    fn require_clean_kimi_env() {
        for name in ["KIMI_CODE_OAUTH_HOST", "KIMI_OAUTH_HOST", "KIMI_CODE_REGION"] {
            assert!(std::env::var_os(name).is_none(), "{name} must be unset for this test");
        }
    }

    type CapturedEvents = Arc<Mutex<Vec<AuthEvent>>>;
    type CapturedPrompts = Arc<Mutex<Vec<AuthPrompt>>>;

    struct RecordingInteraction {
        signal: AbortSignal,
        region: Option<String>,
        events: Arc<Mutex<Vec<AuthEvent>>>,
        prompts: Arc<Mutex<Vec<AuthPrompt>>>,
    }

    #[async_trait]
    impl AuthInteraction for RecordingInteraction {
        fn signal(&self) -> Option<AbortSignal> {
            Some(self.signal.clone())
        }

        fn on_account_committed(&self, _receipt: AccountLoginReceipt) {}

        async fn prompt(&self, prompt: AuthPrompt) -> anyhow::Result<String> {
            self.prompts.lock().unwrap_or_else(|p| p.into_inner()).push(prompt.clone());
            if let (AuthPromptKind::Select { .. }, Some(region)) = (&prompt.kind, &self.region) {
                return Ok(region.clone());
            }
            anyhow::bail!("Kimi Code login should not prompt")
        }

        fn notify(&self, event: AuthEvent) {
            self.events.lock().unwrap_or_else(|p| p.into_inner()).push(event);
        }
    }

    use crate::auth::types::AuthPromptKind;

    fn interaction(region: Option<&str>) -> (ProviderAuthInteraction, Arc<Mutex<Vec<AuthEvent>>>) {
        let (interaction, events, _) = interaction_with_prompts(region);
        (interaction, events)
    }

    fn interaction_with_prompts(
        region: Option<&str>,
    ) -> (ProviderAuthInteraction, CapturedEvents, CapturedPrompts) {
        let signal = AbortController::new().signal();
        let events = Arc::new(Mutex::new(Vec::new()));
        let prompts = Arc::new(Mutex::new(Vec::new()));
        let inner = Arc::new(RecordingInteraction {
            signal: signal.clone(),
            region: region.map(str::to_string),
            events: events.clone(),
            prompts: prompts.clone(),
        });
        (ProviderAuthInteraction::new(signal, inner), events, prompts)
    }

    async fn advance(transport: &ScriptedTransport, ms: u64) {
        transport.advance_ms(ms as f64);
        tokio::time::advance(Duration::from_millis(ms)).await;
        for _ in 0..64 {
            tokio::task::yield_now().await;
        }
    }

    fn token_polls(transport: &ScriptedTransport) -> usize {
        transport.requests().iter().filter(|request| request.url.ends_with("/api/oauth/token")).count()
    }

    fn mainland_credential(access: &str, refresh: &str, expires: f64) -> OAuthCredential {
        OAuthCredential::new(access, refresh, expires)
            .with_extra("env", serde_json::json!({ "KIMI_CODE_REGION": "mainland-cn" }))
    }

    #[tokio::test(start_paused = true)]
    async fn logs_in_with_the_device_authorization_flow() {
        require_clean_kimi_env();
        let transport = Arc::new(ScriptedTransport::new(vec![
            (Some("/api/oauth/device_authorization"), device_authorization_response(&[])),
            (Some("/api/oauth/token"), json(400, serde_json::json!({ "error": "authorization_pending" }))),
            (
                Some("/api/oauth/token"),
                json(
                    200,
                    serde_json::json!({ "access_token": "access-token", "refresh_token": "refresh-token", "expires_in": 3600 }),
                ),
            ),
        ]));
        transport.set_now_ms(START_MS);
        let oauth = KimiCodingOAuth::new(transport.clone());
        let (interaction, events) = interaction(Some("mainland-cn"));

        let login = tokio::spawn({
            let oauth = oauth.clone();
            async move { oauth.login(&interaction).await }
        });

        advance(&transport, 0).await;
        assert_eq!(
            *events.lock().unwrap_or_else(|p| p.into_inner()),
            vec![AuthEvent::DeviceCode {
                user_code: "ABCD-1234".into(),
                verification_uri: "https://www.kimi.com/code?user_code=ABCD-1234".into(),
                interval_seconds: Some(5.0),
                expires_in_seconds: Some(600.0),
            }]
        );

        // waitBeforeFirstPoll: the first poll happens after the 5s interval.
        advance(&transport, 4999).await;
        assert_eq!(token_polls(&transport), 0);
        advance(&transport, 1).await;
        assert_eq!(token_polls(&transport), 1);

        advance(&transport, 5000).await;
        let credential = login.await.unwrap().unwrap();
        assert_eq!(credential.access, "access-token");
        assert_eq!(credential.refresh, "refresh-token");
        assert_eq!(credential.expires, START_MS + 10_000.0 + 3600.0 * 1000.0);
        assert_eq!(credential.get_extra("env"), Some(&serde_json::json!({ "KIMI_CODE_REGION": "mainland-cn" })));
        assert_eq!(token_polls(&transport), 2);

        let poll = transport.requests().into_iter().find(|request| request.url.ends_with("/api/oauth/token")).expect("poll");
        let form: Vec<(String, String)> = url::form_urlencoded::parse(poll.body.as_deref().unwrap_or_default().as_bytes())
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        let value = |key: &str| form.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
        assert_eq!(value("grant_type").as_deref(), Some("urn:ietf:params:oauth:grant-type:device_code"));
        assert_eq!(value("client_id").as_deref(), Some(CLIENT_ID));
        assert_eq!(value("device_code").as_deref(), Some("device-code-123"));
    }

    #[tokio::test(start_paused = true)]
    async fn fails_when_the_device_code_expires() {
        require_clean_kimi_env();
        let transport = Arc::new(ScriptedTransport::new(vec![
            (Some("/api/oauth/device_authorization"), device_authorization_response(&[])),
            (Some("/api/oauth/token"), json(400, serde_json::json!({ "error": "expired_token" }))),
        ]));
        transport.set_now_ms(START_MS);
        let oauth = KimiCodingOAuth::new(transport.clone());
        let (interaction, _) = interaction(Some("mainland-cn"));

        let login = tokio::spawn({
            let oauth = oauth.clone();
            async move { oauth.login(&interaction).await }
        });

        advance(&transport, 5000).await;
        let error = login.await.unwrap().unwrap_err();
        assert!(error.to_string().contains("expired"), "{error}");
    }

    #[tokio::test(start_paused = true)]
    async fn fails_when_the_user_denies_the_login() {
        require_clean_kimi_env();
        let transport = Arc::new(ScriptedTransport::new(vec![
            (Some("/api/oauth/device_authorization"), device_authorization_response(&[])),
            (Some("/api/oauth/token"), json(400, serde_json::json!({ "error": "access_denied" }))),
        ]));
        transport.set_now_ms(START_MS);
        let oauth = KimiCodingOAuth::new(transport.clone());
        let (interaction, _) = interaction(Some("mainland-cn"));

        let login = tokio::spawn({
            let oauth = oauth.clone();
            async move { oauth.login(&interaction).await }
        });

        advance(&transport, 5000).await;
        let error = login.await.unwrap().unwrap_err();
        assert!(error.to_string().contains("denied"), "{error}");
    }

    #[tokio::test]
    async fn refreshes_tokens_and_returns_a_bearer_header_for_requests() {
        let transport = Arc::new(ScriptedTransport::new(vec![(
            Some("/api/oauth/token"),
            json(
                200,
                serde_json::json!({ "access_token": "new-access", "refresh_token": "new-refresh", "expires_in": 3600 }),
            ),
        )]));
        transport.set_now_ms(START_MS);
        let oauth = KimiCodingOAuth::new(transport.clone());
        let signal = AbortController::new().signal();

        let credential = oauth.refresh(&mainland_credential("old-access", "old-refresh", START_MS), &signal).await.unwrap();
        assert_eq!(credential.access, "new-access");
        assert_eq!(credential.refresh, "new-refresh");
        assert_eq!(credential.expires, START_MS + 3600.0 * 1000.0);

        let request = &transport.requests()[0];
        assert_eq!(request.url, "https://auth.kimi.com/api/oauth/token");
        let form: Vec<(String, String)> = url::form_urlencoded::parse(request.body.as_deref().unwrap_or_default().as_bytes())
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        let value = |key: &str| form.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
        assert_eq!(value("grant_type").as_deref(), Some("refresh_token"));
        assert_eq!(value("refresh_token").as_deref(), Some("old-refresh"));
        assert_eq!(value("client_id").as_deref(), Some(CLIENT_ID));

        let auth = oauth.to_auth(&credential).await.unwrap();
        assert_eq!(auth.headers.as_ref().and_then(|headers| headers.get("Authorization")).cloned().flatten(), Some("Bearer new-access".into()));
    }

    #[tokio::test(start_paused = true)]
    async fn retries_refresh_on_429_and_fails_unauthorized_on_invalid_grant() {
        let transport = Arc::new(ScriptedTransport::new(vec![
            (Some("/api/oauth/token"), json(429, serde_json::json!({ "error": "temporarily_unavailable" }))),
            (Some("/api/oauth/token"), json(200, serde_json::json!({ "access_token": "a", "refresh_token": "r", "expires_in": 60 }))),
        ]));
        transport.set_now_ms(START_MS);
        let oauth = KimiCodingOAuth::new(transport.clone());
        let signal = AbortController::new().signal();
        let credential = mainland_credential("old", "old", 0.0);

        let refresh = tokio::spawn({
            let oauth = oauth.clone();
            let credential = credential.clone();
            let signal = signal.clone();
            async move { oauth.refresh(&credential, &signal).await }
        });
        advance(&transport, 1000).await;
        let refreshed = refresh.await.unwrap().unwrap();
        assert_eq!(refreshed.access, "a");
        assert_eq!(token_polls(&transport), 2);

        // invalid_grant is not retried.
        let failing = Arc::new(ScriptedTransport::new(vec![(
            Some("/api/oauth/token"),
            json(400, serde_json::json!({ "error": "invalid_grant" })),
        )]));
        failing.set_now_ms(START_MS);
        let oauth = KimiCodingOAuth::new(failing.clone());
        let error = oauth.refresh(&mainland_credential("old", "old", 0.0), &signal).await.unwrap_err();
        assert!(error.to_string().contains("unauthorized"), "{error}");
        assert_eq!(token_polls(&failing), 1);
    }

    use crate::auth::oauth::kimi_identity::{
        kimi_code_identity_headers, reset_kimi_device_id_for_tests, with_env_overrides,
    };
    use crate::auth::oauth::kimi_region::{
        KIMI_CODE_OAUTH_HOST_ENV, KIMI_CODE_REGION_ENV, KimiCodeEndpointInput, resolve_kimi_code_endpoints,
    };

    fn env_of(entries: &[(&str, &str)]) -> ProviderEnv {
        let mut env = ProviderEnv::new();
        for (key, value) in entries {
            env.insert((*key).to_string(), (*value).to_string());
        }
        env
    }

    const IDENTITY_HEADER_NAMES: [&str; 6] = [
        "X-Msh-Platform",
        "X-Msh-Version",
        "X-Msh-Device-Name",
        "X-Msh-Device-Model",
        "X-Msh-Os-Version",
        "X-Msh-Device-Id",
    ];

    fn credential() -> OAuthCredential {
        OAuthCredential::new("access-token", "refresh-token", START_MS + 3_600_000.0)
            .with_extra("env", serde_json::json!({ "KIMI_CODE_REGION": "mainland-cn" }))
    }

    fn header_names(request: &HttpRequest) -> Vec<String> {
        request.headers.iter().map(|(name, _)| name.to_ascii_lowercase()).collect()
    }

    fn assert_identity_headers(request: &HttpRequest) {
        let names = header_names(request);
        for name in IDENTITY_HEADER_NAMES {
            assert!(names.contains(&name.to_ascii_lowercase()), "missing {name} in {names:?}");
        }
    }

    fn temp_agent_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("temp dir")
    }

    /// Runs an async flow with the agent dir overridden, without mutating the process env
    /// (the crate-wide unsafe_code = "forbid" lint forbids set_var).
    fn with_agent_dir<R>(dir: &str, f: impl FnOnce() -> R) -> R {
        with_env_overrides(&[("MAHO_CODING_AGENT_DIR", Some(dir))], f)
    }

    #[test]
    fn derives_request_auth_carrying_the_bearer_token_and_every_identity_header() {
        let dir = temp_agent_dir();
        reset_kimi_device_id_for_tests();
        let oauth = KimiCodingOAuth::new(Arc::new(ScriptedTransport::default()));
        let credential = credential();

        let auth = with_agent_dir(dir.path().to_str().expect("path"), || {
            futures::executor::block_on(oauth.to_auth(&credential)).expect("to_auth")
        });
        reset_kimi_device_id_for_tests();

        let headers = auth.headers.expect("headers");
        assert_eq!(headers.get("Authorization").cloned().flatten(), Some("Bearer access-token".into()));
        let user_agent = headers.get("User-Agent").cloned().flatten().expect("User-Agent");
        assert!(user_agent.starts_with("KimiCLI/"), "{user_agent}");
        for name in IDENTITY_HEADER_NAMES {
            let value = headers.get(name).cloned().flatten();
            assert!(value.is_some_and(|value| !value.is_empty()), "missing {name}");
        }
    }

    #[test]
    fn keeps_every_header_value_printable_ascii() {
        let dir = temp_agent_dir();
        reset_kimi_device_id_for_tests();
        let oauth = KimiCodingOAuth::new(Arc::new(ScriptedTransport::default()));
        let credential = credential();

        let auth = with_agent_dir(dir.path().to_str().expect("path"), || {
            futures::executor::block_on(oauth.to_auth(&credential)).expect("to_auth")
        });
        reset_kimi_device_id_for_tests();

        for (name, value) in auth.headers.expect("headers") {
            let value = value.unwrap_or_default();
            assert!(
                value.chars().all(|ch| ('\u{20}'..='\u{7e}').contains(&ch)),
                "non-ascii in {name}"
            );
        }
    }

    #[test]
    fn persists_one_device_id_under_the_agent_dir_and_reuses_it() {
        let dir = temp_agent_dir();
        let path = dir.path().join("kimi-device-id");
        reset_kimi_device_id_for_tests();

        let first = with_agent_dir(dir.path().to_str().expect("path"), kimi_code_identity_headers);
        let device_id = first
            .iter()
            .find(|(name, _)| name == "X-Msh-Device-Id")
            .map(|(_, value)| value.clone())
            .expect("device id header");
        assert!(!device_id.is_empty());
        assert!(path.exists(), "the device id store is written under the agent dir");
        assert_eq!(std::fs::read_to_string(&path).expect("read store").trim(), device_id);

        reset_kimi_device_id_for_tests();
        let second = with_agent_dir(dir.path().to_str().expect("path"), kimi_code_identity_headers);
        assert_eq!(
            second.iter().find(|(name, _)| name == "X-Msh-Device-Id").map(|(_, value)| value.clone()),
            Some(device_id)
        );
        reset_kimi_device_id_for_tests();
    }

    #[test]
    fn falls_back_to_an_ephemeral_device_id_when_the_agent_dir_cannot_be_written() {
        reset_kimi_device_id_for_tests();
        let headers = with_agent_dir("/dev/null/unwritable-agent-dir", kimi_code_identity_headers);
        reset_kimi_device_id_for_tests();

        let device_id = headers
            .iter()
            .find(|(name, _)| name == "X-Msh-Device-Id")
            .map(|(_, value)| value.clone())
            .expect("device id header");
        assert!(!device_id.is_empty());
    }

    #[test]
    fn sends_the_identity_headers_on_device_authorization_and_token_polling() {
        let dir = temp_agent_dir();
        reset_kimi_device_id_for_tests();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .expect("runtime");

        let transport = Arc::new(ScriptedTransport::new(vec![
            (Some("/api/oauth/device_authorization"), device_authorization_response(&[("interval", serde_json::json!(5))])),
            (Some("/api/oauth/token"), json(400, serde_json::json!({ "error": "authorization_pending" }))),
            (
                Some("/api/oauth/token"),
                json(
                    200,
                    serde_json::json!({ "access_token": "access-token", "refresh_token": "refresh-token", "expires_in": 3600 }),
                ),
            ),
        ]));
        transport.set_now_ms(START_MS);
        let oauth = KimiCodingOAuth::new(transport.clone());

        let credential = with_agent_dir(dir.path().to_str().expect("path"), || {
            runtime.block_on(async {
                let (interaction, _) = interaction(Some("mainland-cn"));
                let login = tokio::spawn({
                    let oauth = oauth.clone();
                    async move { oauth.login(&interaction).await }
                });
                tokio::time::advance(Duration::from_millis(10_000)).await;
                login.await.expect("join").expect("login")
            })
        });
        reset_kimi_device_id_for_tests();

        assert_eq!(credential.access, "access-token");
        let requests = transport.requests();
        assert!(requests.len() >= 2, "{}", requests.len());
        for request in &requests {
            assert_identity_headers(request);
        }
    }

    #[test]
    fn sends_the_identity_headers_on_token_refresh() {
        let dir = temp_agent_dir();
        reset_kimi_device_id_for_tests();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");

        let transport = Arc::new(ScriptedTransport::new(vec![(
            Some("/api/oauth/token"),
            json(200, serde_json::json!({ "access_token": "a", "refresh_token": "r", "expires_in": 3600 })),
        )]));
        transport.set_now_ms(START_MS);
        let oauth = KimiCodingOAuth::new(transport.clone());

        with_agent_dir(dir.path().to_str().expect("path"), || {
            runtime.block_on(async {
                let signal = AbortController::new().signal();
                oauth.refresh(&mainland_credential("old", "old-refresh", 0.0), &signal).await.expect("refresh")
            })
        });
        reset_kimi_device_id_for_tests();

        let requests = transport.requests();
        assert_eq!(requests.len(), 1);
        assert_identity_headers(&requests[0]);
    }

    // ---- kimi-coding-region.test.ts (#given an OAuth login / #given a stored credential) ----
    //
    // The TS suite drives the region facts with vi.stubEnv. This crate forbids set_var
    // (unsafe_code = "forbid"), so each case asserts the same resolution through the seam the
    // flow itself reads: resolve_kimi_code_endpoints' injectable input (the login path) and the
    // credential's stored env (the refresh and to_auth paths).

    #[tokio::test(start_paused = true)]
    async fn offers_both_regions_and_stores_the_chosen_one_with_the_credential() {
        let transport = Arc::new(ScriptedTransport::new(vec![
            (Some("/api/oauth/device_authorization"), device_authorization_response(&[("interval", serde_json::json!(1))])),
            (
                Some("/api/oauth/token"),
                json(200, serde_json::json!({ "access_token": "access", "refresh_token": "refresh", "expires_in": 3600 })),
            ),
        ]));
        transport.set_now_ms(START_MS);
        let oauth = KimiCodingOAuth::new(transport.clone());
        let (interaction, _, prompts) = interaction_with_prompts(Some("global"));

        let login = tokio::spawn({
            let oauth = oauth.clone();
            async move { oauth.login(&interaction).await }
        });
        advance(&transport, 2000).await;
        let credential = login.await.unwrap().unwrap();

        let prompts = prompts.lock().unwrap_or_else(|p| p.into_inner()).clone();
        assert_eq!(prompts.len(), 1);
        let AuthPromptKind::Select { options, .. } = &prompts[0].kind else {
            panic!("expected a select prompt");
        };
        assert_eq!(
            options.iter().map(|option| option.id.as_str()).collect::<Vec<_>>(),
            vec!["mainland-cn", "global"]
        );

        let urls: Vec<String> = transport.requests().into_iter().map(|request| request.url).collect();
        assert_eq!(
            urls,
            vec![
                format!("{GLOBAL_OAUTH_HOST}/api/oauth/device_authorization"),
                format!("{GLOBAL_OAUTH_HOST}/api/oauth/token"),
            ]
        );
        assert_eq!(credential.access, "access");
        assert_eq!(credential.get_extra("env"), Some(&serde_json::json!({ "KIMI_CODE_REGION": "global" })));
    }

    #[test]
    fn skips_the_prompt_when_the_region_env_names_the_region() {
        // choose_kimi_code_login_endpoints returns before prompting whenever the env-derived
        // resolution carries an env; that is exactly this input's outcome.
        let from_env = resolve_kimi_code_endpoints(KimiCodeEndpointInput {
            env_region: Some("global".into()),
            ..Default::default()
        });
        assert!(from_env.env.is_some(), "an env-derived resolution suppresses the region prompt");
        assert_eq!(from_env.oauth_host, GLOBAL_OAUTH_HOST);
        assert_eq!(from_env.env, Some(env_of(&[(KIMI_CODE_REGION_ENV, "global")])));
    }

    #[test]
    fn keeps_a_custom_oauth_host_with_the_credential_without_inventing_a_region() {
        let from_env = resolve_kimi_code_endpoints(KimiCodeEndpointInput {
            env_oauth_host: Some("https://auth.example.com/".into()),
            ..Default::default()
        });
        assert_eq!(from_env.region, None);
        assert_eq!(
            from_env.env,
            Some(env_of(&[(KIMI_CODE_OAUTH_HOST_ENV, "https://auth.example.com")]))
        );
    }

    #[tokio::test]
    async fn refreshes_an_international_credential_at_kimi_ai_even_when_the_env_points_at_kimi_com() {
        let transport = Arc::new(ScriptedTransport::new(vec![(
            Some("/api/oauth/token"),
            json(200, serde_json::json!({ "access_token": "access", "refresh_token": "refresh", "expires_in": 3600 })),
        )]));
        transport.set_now_ms(START_MS);
        let oauth = KimiCodingOAuth::new(transport.clone());
        let signal = AbortController::new().signal();
        let international = OAuthCredential::new("old", "old-refresh", 0.0)
            .with_extra("env", serde_json::json!({ "KIMI_CODE_REGION": "global" }));

        oauth.refresh(&international, &signal).await.unwrap();

        let urls: Vec<String> = transport.requests().into_iter().map(|request| request.url).collect();
        assert_eq!(urls, vec![format!("{GLOBAL_OAUTH_HOST}/api/oauth/token")]);
    }

    #[tokio::test]
    async fn refreshes_a_credential_that_predates_regions_at_kimi_com() {
        let transport = Arc::new(ScriptedTransport::new(vec![(
            Some("/api/oauth/token"),
            json(200, serde_json::json!({ "access_token": "access", "refresh_token": "refresh", "expires_in": 3600 })),
        )]));
        transport.set_now_ms(START_MS);
        let oauth = KimiCodingOAuth::new(transport.clone());
        let signal = AbortController::new().signal();

        oauth.refresh(&OAuthCredential::new("old", "old-refresh", 0.0), &signal).await.unwrap();

        let urls: Vec<String> = transport.requests().into_iter().map(|request| request.url).collect();
        assert_eq!(urls, vec![format!("{MAINLAND_OAUTH_HOST}/api/oauth/token")]);
    }

    #[tokio::test]
    async fn routes_international_requests_to_kimi_ai_and_leaves_the_mainland_base_untouched() {
        let oauth = KimiCodingOAuth::new(Arc::new(ScriptedTransport::default()));
        let international = OAuthCredential::new("access", "refresh", START_MS + 3_600_000.0)
            .with_extra("env", serde_json::json!({ "KIMI_CODE_REGION": "global" }));
        let mainland = OAuthCredential::new("access", "refresh", START_MS + 3_600_000.0)
            .with_extra("env", serde_json::json!({ "KIMI_CODE_REGION": "mainland-cn" }));
        let legacy = OAuthCredential::new("access", "refresh", START_MS + 3_600_000.0);

        let international = oauth.to_auth(&international).await.unwrap();
        let mainland = oauth.to_auth(&mainland).await.unwrap();
        let legacy = oauth.to_auth(&legacy).await.unwrap();

        assert_eq!(international.base_url.as_deref(), Some(GLOBAL_API_BASE_URL));
        assert_eq!(
            international.headers.as_ref().and_then(|headers| headers.get("Authorization")).cloned().flatten(),
            Some("Bearer access".into())
        );
        assert_eq!(mainland.base_url, None);
        assert_eq!(legacy.base_url, None);
    }
}
