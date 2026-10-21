//! Port of senpi packages/ai/src/auth/oauth/meta.ts.
//! Identity tokens are retained as refresh credentials; inference keys are re-minted daily.

use super::device_code::{
    OAuthDeviceCodePollOptions, OAuthDeviceCodePollResult, poll_oauth_device_code_flow,
};
use super::transport::{HttpRequest, HttpResponse, OAuthTransport, default_transport};
use crate::auth::types::{
    AuthEvent, ModelAuth, OAuthAuth, OAuthCredential, ProviderAuthInteraction,
};
use crate::utils::abort::AbortSignal;
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use url::Url;

const CLIENT_ID: &str = "1031625952748946";
const DEVICE_AUTHORIZATION_URL: &str = "https://auth.meta.com/oidc/device/authorization/";
const DEVICE_TOKEN_URL: &str = "https://auth.meta.com/oidc/device/token/";
const API_KEY_MINT_URL: &str = "https://api.meta.ai/muse-code/key";
const API_KEY_LIFETIME_MS: f64 = 86_400_000.0;

struct DeviceAuthorization {
    device_code: String,
    user_code: String,
    verification_uri: String,
    interval_seconds: Option<f64>,
    expires_in_seconds: Option<f64>,
}

fn read_json(response: &HttpResponse) -> Value {
    response
        .json()
        .ok()
        .filter(Value::is_object)
        .unwrap_or(Value::Null)
}

fn error_detail(json: &Value) -> String {
    ["error_description", "detail", "message", "error"]
        .iter()
        .find_map(|key| json[*key].as_str().map(str::trim).filter(|s| !s.is_empty()))
        .map(|s| format!(": {s}"))
        .unwrap_or_default()
}

fn trusted_http_url(value: &Value) -> Option<String> {
    let url = Url::parse(value.as_str()?).ok()?;
    matches!(url.scheme(), "https" | "http").then(|| url.to_string())
}

fn positive_number(value: &Value) -> Option<f64> {
    value.as_f64().filter(|n| n.is_finite() && *n > 0.0)
}

fn form_request(url: &str, fields: &[(&str, &str)]) -> HttpRequest {
    HttpRequest {
        method: "POST".into(),
        url: url.into(),
        headers: vec![
            (
                "Content-Type".into(),
                "application/x-www-form-urlencoded".into(),
            ),
            ("Accept".into(), "application/json".into()),
        ],
        body: Some(
            url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs(fields.iter().copied())
                .finish(),
        ),
        timeout_ms: Some(30_000),
    }
}

async fn start_device_authorization(
    transport: &dyn OAuthTransport,
    signal: &AbortSignal,
) -> anyhow::Result<DeviceAuthorization> {
    let response = transport
        .execute(
            form_request(DEVICE_AUTHORIZATION_URL, &[("client_id", CLIENT_ID)]),
            signal,
        )
        .await?;
    let json = read_json(&response);
    if !response.ok() {
        anyhow::bail!(
            "Meta device authorization failed with status {}{}",
            response.status,
            error_detail(&json)
        );
    }
    let device_code = json["device_code"].as_str().filter(|s| !s.is_empty());
    let user_code = json["user_code"].as_str().filter(|s| !s.is_empty());
    let verification_uri = trusted_http_url(&json["verification_uri_complete"])
        .or_else(|| trusted_http_url(&json["verification_uri"]));
    let (Some(device_code), Some(user_code), Some(verification_uri)) =
        (device_code, user_code, verification_uri)
    else {
        anyhow::bail!("Invalid Meta device authorization response: {json}");
    };
    Ok(DeviceAuthorization {
        device_code: device_code.into(),
        user_code: user_code.into(),
        verification_uri,
        interval_seconds: positive_number(&json["interval"]),
        expires_in_seconds: positive_number(&json["expires_in"]),
    })
}

async fn poll_for_identity_token(
    transport: Arc<dyn OAuthTransport>,
    device: DeviceAuthorization,
    signal: &AbortSignal,
) -> anyhow::Result<String> {
    if device.expires_in_seconds.is_none() {
        super::device_code::abortable_sleep(
            std::time::Duration::from_secs_f64(device.interval_seconds.unwrap_or(5.0).max(1.0))
                .as_millis()
                .try_into()?,
            signal,
            "Login cancelled",
        )
        .await?;
    }
    poll_oauth_device_code_flow(OAuthDeviceCodePollOptions {
        interval_seconds: device.interval_seconds,
        expires_in_seconds: device.expires_in_seconds,
        wait_before_first_poll: true,
        signal: signal.clone(),
        poll: Box::new({
            let signal = signal.clone();
            move || {
                let transport = transport.clone();
                let signal = signal.clone();
                let request = form_request(
                    DEVICE_TOKEN_URL,
                    &[
                        ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                        ("device_code", &device.device_code),
                        ("client_id", CLIENT_ID),
                    ],
                );
                Box::pin(async move {
                    let response = transport.execute(request, &signal).await?;
                    let json = read_json(&response);
                    if response.ok()
                        && let Some(token) = json["access_token"].as_str().filter(|s| !s.is_empty())
                    {
                        return Ok(OAuthDeviceCodePollResult::Complete(token.to_owned()));
                    }
                    Ok(match json["error"].as_str() {
                        Some("authorization_pending") => OAuthDeviceCodePollResult::Pending,
                        Some("slow_down") => OAuthDeviceCodePollResult::SlowDown {
                            interval_seconds: positive_number(&json["interval"]),
                        },
                        Some("access_denied") => OAuthDeviceCodePollResult::Failed {
                            message: "Meta login was denied.".into(),
                        },
                        Some("expired_token") => OAuthDeviceCodePollResult::Failed {
                            message: "Meta device authorization expired. Please restart login."
                                .into(),
                        },
                        _ => OAuthDeviceCodePollResult::Failed {
                            message: format!(
                                "Meta device token request failed with status {}{}",
                                response.status,
                                error_detail(&json)
                            ),
                        },
                    })
                })
            }
        }),
    })
    .await
}

async fn mint_api_key(
    transport: &dyn OAuthTransport,
    identity_token: &str,
    signal: &AbortSignal,
) -> anyhow::Result<OAuthCredential> {
    let response = transport
        .execute(
            HttpRequest {
                method: "POST".into(),
                url: API_KEY_MINT_URL.into(),
                headers: vec![
                    ("Accept".into(), "application/json".into()),
                    ("Authorization".into(), format!("Bearer {identity_token}")),
                    ("Content-Type".into(), "application/json".into()),
                    ("x-api-version".into(), "1.0.0".into()),
                ],
                body: Some("{}".into()),
                timeout_ms: Some(30_000),
            },
            signal,
        )
        .await?;
    let json = read_json(&response);
    if matches!(response.status, 401 | 403) {
        anyhow::bail!(
            "Meta session expired (status {}). Run `/login meta` to sign in again.{}",
            response.status,
            error_detail(&json)
        );
    }
    if !response.ok() {
        anyhow::bail!(
            "Meta API key mint failed with status {}{}",
            response.status,
            error_detail(&json)
        );
    }
    let Some(api_key) = json["api_key"].as_str().filter(|s| !s.is_empty()) else {
        let action = trusted_http_url(&json["action_url"])
            .map(|url| format!(" Complete setup at {url}"))
            .unwrap_or_default();
        anyhow::bail!("Meta did not issue an API key.{action}");
    };
    Ok(OAuthCredential::new(
        api_key,
        identity_token,
        transport.now_ms() + API_KEY_LIFETIME_MS,
    ))
}

pub struct MetaOAuth {
    transport: Arc<dyn OAuthTransport>,
}
impl MetaOAuth {
    pub fn new(transport: Arc<dyn OAuthTransport>) -> Self {
        Self { transport }
    }
}

#[async_trait]
impl OAuthAuth for MetaOAuth {
    fn name(&self) -> &str {
        "Meta (Muse subscription)"
    }
    fn is_subscription(&self) -> bool {
        true
    }
    fn login_label(&self) -> Option<&str> {
        Some("Sign in with Meta")
    }
    async fn login(
        &self,
        interaction: &ProviderAuthInteraction,
    ) -> anyhow::Result<OAuthCredential> {
        let login = async {
            let device =
                start_device_authorization(self.transport.as_ref(), &interaction.signal).await?;
            interaction.notify(AuthEvent::DeviceCode {
                user_code: device.user_code.clone(),
                verification_uri: device.verification_uri.clone(),
                interval_seconds: device.interval_seconds,
                expires_in_seconds: device.expires_in_seconds,
            });
            let identity =
                poll_for_identity_token(self.transport.clone(), device, &interaction.signal)
                    .await?;
            interaction.notify(AuthEvent::Progress {
                message: "Enabling Meta Model API access...".into(),
            });
            mint_api_key(self.transport.as_ref(), &identity, &interaction.signal).await
        };
        let result = tokio::select! {
            biased;
            () = interaction.signal.cancelled() => anyhow::bail!("Login cancelled"),
            result = login => result,
        };
        if interaction.signal.aborted() {
            anyhow::bail!("Login cancelled");
        }
        result
    }
    async fn refresh(
        &self,
        credential: &OAuthCredential,
        signal: &AbortSignal,
    ) -> anyhow::Result<OAuthCredential> {
        mint_api_key(self.transport.as_ref(), &credential.refresh, signal).await
    }
    async fn to_auth(&self, credential: &OAuthCredential) -> anyhow::Result<ModelAuth> {
        Ok(ModelAuth {
            api_key: Some(credential.access.clone()),
            ..Default::default()
        })
    }
}

pub fn meta_oauth() -> Arc<dyn OAuthAuth> {
    Arc::new(MetaOAuth::new(default_transport()))
}

#[cfg(test)]
mod tests {
    use super::super::transport::{ScriptedResponse, ScriptedTransport};
    use super::*;
    use crate::auth::types::{AuthInteraction, AuthPrompt};
    use crate::utils::abort::AbortController;
    use serde_json::json;

    struct Interaction;
    #[async_trait]
    impl AuthInteraction for Interaction {
        fn signal(&self) -> Option<AbortSignal> {
            None
        }
        async fn prompt(&self, _prompt: AuthPrompt) -> anyhow::Result<String> {
            anyhow::bail!("device login must not prompt")
        }
        fn notify(&self, _event: AuthEvent) {}
    }

    #[tokio::test(start_paused = true)]
    async fn device_login_mints_and_refresh_remints_with_identity_token() -> anyhow::Result<()> {
        let transport = Arc::new(ScriptedTransport::new(vec![
            (
                Some(DEVICE_AUTHORIZATION_URL),
                ScriptedResponse::Json {
                    status: 200,
                    body: json!({"device_code":"device", "user_code":"CODE", "verification_uri_complete":"javascript:bad", "verification_uri":"https://auth.meta.com/device", "interval":1, "expires_in":60}),
                },
            ),
            (
                Some(DEVICE_TOKEN_URL),
                ScriptedResponse::Json {
                    status: 400,
                    body: json!({"error":"authorization_pending"}),
                },
            ),
            (
                Some(DEVICE_TOKEN_URL),
                ScriptedResponse::Json {
                    status: 400,
                    body: json!({"error":"slow_down", "interval":2}),
                },
            ),
            (
                Some(DEVICE_TOKEN_URL),
                ScriptedResponse::Json {
                    status: 200,
                    body: json!({"access_token":"identity"}),
                },
            ),
            (
                Some(API_KEY_MINT_URL),
                ScriptedResponse::Json {
                    status: 200,
                    body: json!({"api_key":"key"}),
                },
            ),
            (
                Some(API_KEY_MINT_URL),
                ScriptedResponse::Json {
                    status: 200,
                    body: json!({"api_key":"new-key"}),
                },
            ),
        ]));
        transport.set_now_ms(1000.0);
        let oauth = MetaOAuth::new(transport.clone());
        let signal = AbortController::new().signal();
        let interaction = ProviderAuthInteraction::new(signal.clone(), Arc::new(Interaction));
        let credential = tokio::time::timeout(
            std::time::Duration::from_secs(120),
            oauth.login(&interaction),
        )
        .await??;
        assert_eq!(credential.refresh, "identity");
        assert_eq!(credential.access, "key");
        assert_eq!(credential.expires, 86_401_000.0);
        let refreshed = oauth.refresh(&credential, &signal).await?;
        assert_eq!(refreshed.access, "new-key");
        assert_eq!(refreshed.refresh, "identity");
        let requests = transport.requests();
        assert_eq!(requests.len(), 6);
        for request in requests.iter().filter(|r| r.url == API_KEY_MINT_URL) {
            assert!(
                request
                    .headers
                    .contains(&("Authorization".into(), "Bearer identity".into()))
            );
            assert_eq!(request.body.as_deref(), Some("{}"));
            assert_eq!(request.timeout_ms, Some(30_000));
        }
        Ok(())
    }

    #[tokio::test]
    async fn remint_rejects_dead_identity_and_untrusted_setup_links() {
        for (status, body) in [
            (401, json!({"error":"expired"})),
            (403, json!({})),
            (200, json!({"action_url":"javascript:bad"})),
        ] {
            let transport =
                ScriptedTransport::new(vec![(None, ScriptedResponse::Json { status, body })]);
            assert!(
                mint_api_key(&transport, "identity", &AbortController::new().signal())
                    .await
                    .is_err()
            );
        }
        assert!(trusted_http_url(&json!("javascript:bad")).is_none());
    }
}
