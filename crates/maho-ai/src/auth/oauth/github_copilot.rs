//! Port of senpi packages/ai/src/auth/oauth/github-copilot.ts.

use crate::auth::oauth::device_code::{
    OAuthDeviceCodePollOptions, OAuthDeviceCodePollResult, poll_oauth_device_code_flow,
};
use crate::auth::oauth::transport::{HttpRequest, HttpResponse, OAuthTransport, default_transport};
use crate::auth::types::{AuthEvent, AuthPrompt, AuthPromptKind, ModelAuth, OAuthAuth, OAuthCredential, ProviderAuthInteraction};
use crate::utils::abort::AbortSignal;
use crate::utils::sleep::sleep;
use async_trait::async_trait;
use base64::Engine as _;
use serde_json::{Map, Value};
use std::sync::{Arc, LazyLock};
use url::Url;

const CLIENT_ID_BASE64: &str = "SXYxLmI1MDdhMDhjODdlY2ZlOTg=";
const COPILOT_USER_AGENT: &str = "GitHubCopilotChat/0.35.0";
const COPILOT_API_VERSION: &str = "2026-06-01";
const REQUEST_TIMEOUT_MS: u64 = 5_000;

fn client_id() -> String {
    base64::engine::general_purpose::STANDARD
        .decode(CLIENT_ID_BASE64)
        .unwrap_or_default()
        .iter()
        .map(|byte| *byte as char)
        .collect()
}

fn copilot_headers() -> Vec<(String, String)> {
    vec![
        ("User-Agent".into(), COPILOT_USER_AGENT.into()),
        ("Editor-Version".into(), "vscode/1.107.0".into()),
        ("Editor-Plugin-Version".into(), "copilot-chat/0.35.0".into()),
        ("Copilot-Integration-Id".into(), "vscode-chat".into()),
    ]
}

#[derive(Debug, Clone)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub interval: Option<f64>,
    pub expires_in: f64,
}

#[derive(Debug, Clone)]
struct CopilotUrls {
    device_code_url: String,
    access_token_url: String,
    copilot_token_url: String,
}

fn get_urls(domain: &str) -> CopilotUrls {
    CopilotUrls {
        device_code_url: format!("https://{domain}/login/device/code"),
        access_token_url: format!("https://{domain}/login/oauth/access_token"),
        copilot_token_url: format!("https://api.{domain}/copilot_internal/v2/token"),
    }
}

pub fn normalize_domain(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let candidate = if trimmed.contains("://") { trimmed.to_string() } else { format!("https://{trimmed}") };
    Url::parse(&candidate).ok().and_then(|url| url.host_str().map(str::to_string))
}

pub fn get_base_url_from_token(token: &str) -> Option<String> {
    let marker = "proxy-ep=";
    let start = token.find(marker)? + marker.len();
    let rest = &token[start..];
    let end = rest.find(';').unwrap_or(rest.len());
    let proxy_host = &rest[..end];
    if proxy_host.is_empty() {
        return None;
    }
    let api_host = proxy_host.strip_prefix("proxy.").map(|rest| format!("api.{rest}")).unwrap_or_else(|| proxy_host.to_string());
    Some(format!("https://{api_host}"))
}

pub fn get_github_copilot_base_url(token: Option<&str>, enterprise_domain: Option<&str>) -> String {
    if let Some(token) = token
        && let Some(url) = get_base_url_from_token(token) {
            return url;
        }
    match enterprise_domain {
        Some(domain) => format!("https://copilot-api.{domain}"),
        None => "https://api.individual.githubcopilot.com".to_string(),
    }
}

fn as_record(value: &Value) -> Option<&Map<String, Value>> {
    value.as_object()
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct GitHubCopilotModelCatalog {
    pub available_model_ids: Vec<String>,
    pub policy_model_ids: Vec<String>,
}

fn is_known_model(model_id: &str) -> bool {
    crate::models_generated::MODELS
        .get("github-copilot")
        .is_some_and(|models| models.contains_key(model_id))
}

pub fn parse_github_copilot_model_catalog(raw: &Value, allow_policy_fallback: bool) -> anyhow::Result<GitHubCopilotModelCatalog> {
    let data = as_record(raw)
        .and_then(|record| record.get("data"))
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("Invalid Copilot models response"))?;

    let mut account_models: Vec<(String, bool, Option<String>)> = Vec::new();
    for raw_item in data {
        let Some(item) = as_record(raw_item) else { continue };
        let Some(id) = item.get("id").and_then(Value::as_str) else { continue };
        let supports = item
            .get("capabilities")
            .and_then(as_record)
            .and_then(|capabilities| capabilities.get("supports"))
            .and_then(as_record);
        if supports.and_then(|supports| supports.get("tool_calls")) == Some(&Value::Bool(false)) {
            continue;
        }
        let picker_enabled = item.get("model_picker_enabled") == Some(&Value::Bool(true));
        let policy_state = item
            .get("policy")
            .and_then(as_record)
            .and_then(|policy| policy.get("state"))
            .and_then(Value::as_str)
            .map(str::to_string);
        account_models.push((id.to_string(), picker_enabled, policy_state));
    }

    let picker_model_ids: Vec<String> = account_models
        .iter()
        .filter(|(_, picker_enabled, policy_state)| *picker_enabled && policy_state.as_deref() != Some("disabled"))
        .map(|(id, _, _)| id.clone())
        .collect();
    let use_policy_fallback = allow_policy_fallback && picker_model_ids.is_empty();
    let available_model_ids = if !picker_model_ids.is_empty() || !allow_policy_fallback {
        picker_model_ids
    } else {
        account_models
            .iter()
            .filter(|(_, _, policy_state)| policy_state.as_deref() == Some("enabled"))
            .map(|(id, _, _)| id.clone())
            .collect()
    };
    let policy_model_ids = account_models
        .iter()
        .filter(|(id, picker_enabled, policy_state)| {
            policy_state.as_deref() == Some("unconfigured")
                && is_known_model(id)
                && (*picker_enabled || use_policy_fallback)
        })
        .map(|(id, _, _)| id.clone())
        .collect();
    Ok(GitHubCopilotModelCatalog { available_model_ids, policy_model_ids })
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
        _ => "Unknown",
    }
}

async fn execute(
    transport: &dyn OAuthTransport,
    request: HttpRequest,
    signal: &AbortSignal,
) -> anyhow::Result<HttpResponse> {
    transport.execute(request, signal).await
}

async fn fetch_json(
    transport: &dyn OAuthTransport,
    url: &str,
    headers: Vec<(String, String)>,
    method: &str,
    body: Option<String>,
    signal: &AbortSignal,
) -> anyhow::Result<Value> {
    let response = execute(
        transport,
        HttpRequest { method: method.into(), url: url.into(), headers, body, timeout_ms: Some(REQUEST_TIMEOUT_MS) },
        signal,
    )
    .await?;
    if !response.ok() {
        anyhow::bail!("{} {}: {}", response.status, status_text(response.status), response.body);
    }
    Ok(serde_json::from_str(&response.body).unwrap_or(Value::Null))
}

#[allow(clippy::too_many_arguments)]
async fn fetch_with_rate_limit_retry(
    transport: &dyn OAuthTransport,
    url: &str,
    headers: Vec<(String, String)>,
    method: &str,
    body: Option<String>,
    signal: &AbortSignal,
    max_retries: u32,
    max_elapsed_ms: f64,
) -> anyhow::Result<HttpResponse> {
    let budget_deadline = if max_retries > 0 && max_elapsed_ms > 0.0 {
        Some(transport.now_ms() + max_elapsed_ms)
    } else {
        None
    };
    let mut retry = 0u32;
    loop {
        let response = execute(
            transport,
            HttpRequest {
                method: method.into(),
                url: url.into(),
                headers: headers.clone(),
                body: body.clone(),
                timeout_ms: Some(REQUEST_TIMEOUT_MS),
            },
            signal,
        )
        .await?;
        if response.status != 429 || retry == max_retries {
            return Ok(response);
        }

        let retry_after = response.header("retry-after").map(str::to_string);
        let mut delay_ms = 500.0 * 2f64.powi(retry as i32);
        if let Some(retry_after) = retry_after {
            let seconds = retry_after.parse::<f64>();
            delay_ms = match seconds {
                Ok(seconds) => seconds * 1000.0,
                Err(_) => match chrono::DateTime::parse_from_rfc2822(&retry_after) {
                    Ok(when) => when.timestamp_millis() as f64 - transport.now_ms(),
                    Err(_) => f64::NAN,
                },
            };
            if !delay_ms.is_finite() {
                return Ok(response);
            }
        }
        delay_ms = delay_ms.max(0.0);
        if let Some(deadline) = budget_deadline
            && delay_ms >= deadline - transport.now_ms() {
                return Ok(response);
            }
        sleep(delay_ms as u64, signal).await.map_err(|reason| anyhow::anyhow!("{reason}"))?;
        retry += 1;
    }
}

async fn fetch_github_copilot_models(
    transport: &dyn OAuthTransport,
    copilot_token: &str,
    enterprise_domain: Option<&str>,
    signal: &AbortSignal,
    max_retries: u32,
    max_elapsed_ms: f64,
) -> anyhow::Result<GitHubCopilotModelCatalog> {
    let base_url = get_github_copilot_base_url(Some(copilot_token), enterprise_domain);
    let allow_policy_fallback = base_url == "https://api.individual.githubcopilot.com";
    let mut headers = vec![
        ("Accept".into(), "application/json".into()),
        ("Authorization".into(), format!("Bearer {copilot_token}")),
    ];
    headers.extend(copilot_headers());
    headers.push(("X-GitHub-Api-Version".into(), COPILOT_API_VERSION.into()));

    let response = fetch_with_rate_limit_retry(
        transport,
        &format!("{base_url}/models"),
        headers,
        "GET",
        None,
        signal,
        max_retries,
        max_elapsed_ms,
    )
    .await?;
    if !response.ok() {
        anyhow::bail!("{} {}: {}", response.status, status_text(response.status), response.body);
    }
    let raw: Value = serde_json::from_str(&response.body).unwrap_or(Value::Null);
    parse_github_copilot_model_catalog(&raw, allow_policy_fallback)
}

async fn start_device_flow(
    transport: &dyn OAuthTransport,
    domain: &str,
    signal: &AbortSignal,
) -> anyhow::Result<DeviceCodeResponse> {
    let urls = get_urls(domain);
    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("client_id", &client_id())
        .append_pair("scope", "read:user")
        .finish();
    let data = fetch_json(
        transport,
        &urls.device_code_url,
        vec![
            ("Accept".into(), "application/json".into()),
            ("Content-Type".into(), "application/x-www-form-urlencoded".into()),
            ("User-Agent".into(), COPILOT_USER_AGENT.into()),
        ],
        "POST",
        Some(body),
        signal,
    )
    .await?;

    let Some(record) = as_record(&data) else {
        anyhow::bail!("Invalid device code response");
    };
    let device_code = record.get("device_code").and_then(Value::as_str);
    let user_code = record.get("user_code").and_then(Value::as_str);
    let verification_uri = record.get("verification_uri").and_then(Value::as_str);
    let interval = record.get("interval");
    let expires_in = record.get("expires_in").and_then(Value::as_f64);
    let interval_ok = interval.is_none() || interval.and_then(Value::as_f64).is_some();
    let (Some(device_code), Some(user_code), Some(verification_uri), Some(expires_in)) =
        (device_code, user_code, verification_uri, expires_in)
    else {
        anyhow::bail!("Invalid device code response fields");
    };
    if !interval_ok {
        anyhow::bail!("Invalid device code response fields");
    }

    let parsed = Url::parse(verification_uri)
        .map_err(|_| anyhow::anyhow!("Untrusted verification_uri in device code response"))?;
    if parsed.scheme() != "https" && parsed.scheme() != "http" {
        anyhow::bail!("Untrusted verification_uri in device code response");
    }

    Ok(DeviceCodeResponse {
        device_code: device_code.to_string(),
        user_code: user_code.to_string(),
        verification_uri: parsed.to_string(),
        interval: interval.and_then(Value::as_f64),
        expires_in,
    })
}

async fn poll_for_github_access_token(
    transport: Arc<dyn OAuthTransport>,
    domain: String,
    device: DeviceCodeResponse,
    signal: &AbortSignal,
) -> anyhow::Result<String> {
    let poll_signal = signal.clone();
    poll_oauth_device_code_flow(OAuthDeviceCodePollOptions {
        interval_seconds: device.interval,
        expires_in_seconds: Some(device.expires_in),
        wait_before_first_poll: true,
        signal: signal.clone(),
        poll: Box::new(move || {
            let transport = transport.clone();
            let domain = domain.clone();
            let device = device.clone();
            let signal = poll_signal.clone();
            Box::pin(async move {
                let urls = get_urls(&domain);
                let body = url::form_urlencoded::Serializer::new(String::new())
                    .append_pair("client_id", &client_id())
                    .append_pair("device_code", &device.device_code)
                    .append_pair("grant_type", "urn:ietf:params:oauth:grant-type:device_code")
                    .finish();
                let raw = match fetch_json(
                    transport.as_ref(),
                    &urls.access_token_url,
                    vec![
                        ("Accept".into(), "application/json".into()),
                        ("Content-Type".into(), "application/x-www-form-urlencoded".into()),
                        ("User-Agent".into(), COPILOT_USER_AGENT.into()),
                    ],
                    "POST",
                    Some(body),
                    &signal,
                )
                .await
                {
                    Ok(raw) => raw,
                    Err(error) => {
                        return Ok(OAuthDeviceCodePollResult::Failed { message: error.to_string() });
                    }
                };

                if let Some(access_token) = as_record(&raw).and_then(|record| record.get("access_token")).and_then(Value::as_str) {
                    return Ok(OAuthDeviceCodePollResult::Complete(access_token.to_string()));
                }

                if let Some(error) = as_record(&raw).and_then(|record| record.get("error")).and_then(Value::as_str) {
                    let record = as_record(&raw).expect("checked above");
                    let description = record.get("error_description").and_then(Value::as_str);
                    if error == "authorization_pending" {
                        return Ok(OAuthDeviceCodePollResult::Pending);
                    }
                    if error == "slow_down" {
                        return Ok(OAuthDeviceCodePollResult::SlowDown {
                            interval_seconds: record.get("interval").and_then(Value::as_f64),
                        });
                    }
                    let suffix = description.map(|description| format!(": {description}")).unwrap_or_default();
                    return Ok(OAuthDeviceCodePollResult::Failed {
                        message: format!("Device flow failed: {error}{suffix}"),
                    });
                }

                Ok(OAuthDeviceCodePollResult::Failed { message: "Invalid device token response".into() })
            })
        }),
    })
    .await
}

async fn refresh_github_copilot_access_token(
    transport: &dyn OAuthTransport,
    refresh_token: &str,
    enterprise_domain: Option<&str>,
    signal: &AbortSignal,
) -> anyhow::Result<OAuthCredential> {
    let domain = enterprise_domain.unwrap_or("github.com");
    let urls = get_urls(domain);
    let mut headers = vec![
        ("Accept".into(), "application/json".into()),
        ("Authorization".into(), format!("Bearer {refresh_token}")),
    ];
    headers.extend(copilot_headers());

    let raw = fetch_json(transport, &urls.copilot_token_url, headers, "GET", None, signal).await?;
    let Some(record) = as_record(&raw) else {
        anyhow::bail!("Invalid Copilot token response");
    };
    let token = record.get("token").and_then(Value::as_str);
    let expires_at = record.get("expires_at").and_then(Value::as_f64);
    let (Some(token), Some(expires_at)) = (token, expires_at) else {
        anyhow::bail!("Invalid Copilot token response fields");
    };

    let mut credential = OAuthCredential::new(token, refresh_token, expires_at * 1000.0 - 5.0 * 60.0 * 1000.0);
    credential = credential.with_extra("enterpriseUrl", serde_json::to_value(enterprise_domain).unwrap_or(Value::Null));
    Ok(credential)
}

async fn refresh_github_copilot_token(
    transport: &dyn OAuthTransport,
    refresh_token: &str,
    enterprise_domain: Option<&str>,
    signal: &AbortSignal,
) -> anyhow::Result<OAuthCredential> {
    let credentials = refresh_github_copilot_access_token(transport, refresh_token, enterprise_domain, signal).await?;
    let catalog = fetch_github_copilot_models(transport, &credentials.access, enterprise_domain, signal, 0, 0.0).await?;
    Ok(credentials.with_extra("availableModelIds", serde_json::to_value(catalog.available_model_ids).unwrap_or(Value::Null)))
}

async fn enable_github_copilot_model(
    transport: &dyn OAuthTransport,
    token: &str,
    model_id: &str,
    enterprise_domain: Option<&str>,
    signal: &AbortSignal,
) -> anyhow::Result<bool> {
    let base_url = get_github_copilot_base_url(Some(token), enterprise_domain);
    let url = format!("{base_url}/models/{model_id}/policy");
    let mut headers = vec![
        ("Content-Type".into(), "application/json".into()),
        ("Authorization".into(), format!("Bearer {token}")),
    ];
    headers.extend(copilot_headers());
    headers.push(("openai-intent".into(), "chat-policy".into()));
    headers.push(("x-interaction-type".into(), "chat-policy".into()));

    let response = match fetch_with_rate_limit_retry(
        transport,
        &url,
        headers,
        "POST",
        Some(serde_json::json!({ "state": "enabled" }).to_string()),
        signal,
        2,
        5000.0,
    )
    .await
    {
        Ok(response) => response,
        Err(error) => {
            if signal.aborted() {
                return Err(error);
            }
            return Ok(false);
        }
    };
    if response.status == 429 {
        anyhow::bail!("{} {}: {}", response.status, status_text(response.status), response.body);
    }
    Ok(response.ok())
}

async fn enable_github_copilot_models(
    transport: &dyn OAuthTransport,
    token: &str,
    model_ids: &[String],
    enterprise_domain: Option<&str>,
    signal: &AbortSignal,
) -> anyhow::Result<Vec<String>> {
    let mut enabled_model_ids = Vec::new();
    for model_id in model_ids {
        match enable_github_copilot_model(transport, token, model_id, enterprise_domain, signal).await {
            Ok(true) => enabled_model_ids.push(model_id.clone()),
            Ok(false) => {}
            Err(error) => {
                if signal.aborted() {
                    return Err(error);
                }
                break;
            }
        }
    }
    Ok(enabled_model_ids)
}

pub struct GitHubCopilotOAuth {
    transport: Arc<dyn OAuthTransport>,
}

impl GitHubCopilotOAuth {
    pub fn new(transport: Arc<dyn OAuthTransport>) -> Self {
        Self { transport }
    }

    pub fn transport(&self) -> &Arc<dyn OAuthTransport> {
        &self.transport
    }

    async fn login_github_copilot(&self, interaction: &ProviderAuthInteraction) -> anyhow::Result<OAuthCredential> {
        let input = interaction
            .prompt(AuthPrompt {
                kind: AuthPromptKind::Text {
                    message: "GitHub Enterprise URL/domain (blank for github.com)".into(),
                    placeholder: Some("company.ghe.com".into()),
                },
                signal: None,
            })
            .await?;
        if interaction.signal.aborted() {
            anyhow::bail!("Login cancelled");
        }

        let trimmed = input.trim();
        let enterprise_domain = normalize_domain(&input);
        if !trimmed.is_empty() && enterprise_domain.is_none() {
            anyhow::bail!("Invalid GitHub Enterprise URL/domain");
        }
        let domain = enterprise_domain.clone().unwrap_or_else(|| "github.com".to_string());

        let device = start_device_flow(self.transport.as_ref(), &domain, &interaction.signal).await?;
        interaction.notify(AuthEvent::DeviceCode {
            user_code: device.user_code.clone(),
            verification_uri: device.verification_uri.clone(),
            interval_seconds: device.interval,
            expires_in_seconds: Some(device.expires_in),
        });

        let github_access_token =
            poll_for_github_access_token(self.transport.clone(), domain, device, &interaction.signal).await?;
        let credentials = refresh_github_copilot_access_token(
            self.transport.as_ref(),
            &github_access_token,
            enterprise_domain.as_deref(),
            &interaction.signal,
        )
        .await?;
        let models = fetch_github_copilot_models(
            self.transport.as_ref(),
            &credentials.access,
            enterprise_domain.as_deref(),
            &interaction.signal,
            2,
            5000.0,
        )
        .await?;
        let mut enabled_model_ids: Vec<String> = Vec::new();
        if !models.policy_model_ids.is_empty() {
            interaction.notify(AuthEvent::Progress { message: "Enabling models...".into() });
            enabled_model_ids = enable_github_copilot_models(
                self.transport.as_ref(),
                &credentials.access,
                &models.policy_model_ids,
                enterprise_domain.as_deref(),
                &interaction.signal,
            )
            .await?;
        }

        let mut combined = models.available_model_ids.clone();
        for model_id in enabled_model_ids {
            if !combined.contains(&model_id) {
                combined.push(model_id);
            }
        }
        Ok(credentials.with_extra("availableModelIds", serde_json::to_value(combined).unwrap_or(Value::Null)))
    }
}

fn copilot_enterprise_domain(credential: &OAuthCredential) -> Option<String> {
    let enterprise_url = credential.get_extra_str("enterpriseUrl")?;
    if enterprise_url.is_empty() {
        return None;
    }
    normalize_domain(enterprise_url)
}

pub fn github_copilot_oauth() -> Arc<dyn OAuthAuth> {
    static INSTANCE: LazyLock<Arc<dyn OAuthAuth>> =
        LazyLock::new(|| Arc::new(GitHubCopilotOAuth::new(default_transport())));
    INSTANCE.clone()
}

#[async_trait]
impl OAuthAuth for GitHubCopilotOAuth {
    fn name(&self) -> &str {
        "GitHub Copilot"
    }

    fn is_subscription(&self) -> bool {
        true
    }

    async fn login(&self, interaction: &ProviderAuthInteraction) -> anyhow::Result<OAuthCredential> {
        self.login_github_copilot(interaction).await
    }

    async fn refresh(&self, credential: &OAuthCredential, signal: &AbortSignal) -> anyhow::Result<OAuthCredential> {
        refresh_github_copilot_token(
            self.transport.as_ref(),
            &credential.refresh,
            copilot_enterprise_domain(credential).as_deref(),
            signal,
        )
        .await
    }

    async fn to_auth(&self, credential: &OAuthCredential) -> anyhow::Result<ModelAuth> {
        Ok(ModelAuth {
            api_key: Some(credential.access.clone()),
            base_url: Some(get_github_copilot_base_url(
                Some(&credential.access),
                copilot_enterprise_domain(credential).as_deref(),
            )),
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::oauth::transport::{ScriptedResponse, ScriptedTransport};
    use crate::auth::types::{AccountLoginReceipt, AuthInteraction};
    use crate::utils::abort::AbortController;
    use std::sync::Mutex;

    const TEST_ACCESS_TOKEN: &str = "tid=test;exp=9999999999;proxy-ep=proxy.individual.githubcopilot.com;";
    const TEST_MODELS_URL: &str = "https://api.individual.githubcopilot.com/models";

    fn json(status: u16, body: Value) -> ScriptedResponse {
        ScriptedResponse::Json { status, body }
    }

    fn signal() -> AbortSignal {
        AbortController::new().signal()
    }

    #[test]
    fn normalize_domain_accepts_bare_and_schemed_hosts() {
        assert_eq!(normalize_domain("company.ghe.com").as_deref(), Some("company.ghe.com"));
        assert_eq!(normalize_domain("  https://company.ghe.com/path  ").as_deref(), Some("company.ghe.com"));
        assert_eq!(normalize_domain(""), None);
        assert_eq!(normalize_domain("   "), None);
    }

    #[test]
    fn base_url_prefers_the_token_proxy_endpoint() {
        assert_eq!(
            get_github_copilot_base_url(Some(TEST_ACCESS_TOKEN), None).as_str(),
            "https://api.individual.githubcopilot.com"
        );
        assert_eq!(
            get_github_copilot_base_url(Some("no-proxy-ep"), Some("company.ghe.com")).as_str(),
            "https://copilot-api.company.ghe.com"
        );
        assert_eq!(
            get_github_copilot_base_url(Some("no-proxy-ep"), None).as_str(),
            "https://api.individual.githubcopilot.com"
        );
    }

    #[test]
    fn catalog_keeps_picker_models_and_drops_tool_incapable_ones() {
        let raw = serde_json::json!({
            "data": [
                {"id": "picker-model", "model_picker_enabled": true, "capabilities": {"supports": {"tool_calls": true}}},
                {"id": "disabled-model", "model_picker_enabled": true, "policy": {"state": "disabled"}, "capabilities": {"supports": {"tool_calls": true}}},
                {"id": "hidden-model", "model_picker_enabled": false, "policy": {"state": "enabled"}, "capabilities": {"supports": {"tool_calls": true}}},
                {"id": "no-tools", "model_picker_enabled": true, "capabilities": {"supports": {"tool_calls": false}}},
            ]
        });
        let catalog = parse_github_copilot_model_catalog(&raw, false).unwrap();
        assert_eq!(catalog.available_model_ids, vec!["picker-model"]);
        assert!(catalog.policy_model_ids.is_empty());
    }

    #[test]
    fn catalog_falls_back_to_enabled_policies_only_for_the_individual_endpoint() {
        let raw = serde_json::json!({
            "data": [
                {"id": "enabled-model", "model_picker_enabled": false, "policy": {"state": "enabled"}, "capabilities": {"supports": {"tool_calls": true}}},
                {"id": "policy-disabled-model", "model_picker_enabled": false, "policy": {"state": "disabled"}, "capabilities": {"supports": {"tool_calls": true}}},
                {"id": "tool-incapable-model", "model_picker_enabled": false, "policy": {"state": "enabled"}, "capabilities": {"supports": {"tool_calls": false}}},
            ]
        });
        let catalog = parse_github_copilot_model_catalog(&raw, true).unwrap();
        assert_eq!(catalog.available_model_ids, vec!["enabled-model"]);

        let catalog = parse_github_copilot_model_catalog(&raw, false).unwrap();
        assert!(catalog.available_model_ids.is_empty());
    }

    #[test]
    fn catalog_rejects_a_non_object_data_field() {
        let error = parse_github_copilot_model_catalog(&serde_json::json!({"data": "nope"}), false).unwrap_err();
        assert_eq!(error.to_string(), "Invalid Copilot models response");
    }

    async fn refresh_with(transport: Arc<ScriptedTransport>) -> anyhow::Result<OAuthCredential> {
        let oauth = GitHubCopilotOAuth::new(transport);
        oauth
            .refresh(&OAuthCredential::new("old-access-token", "ghu_refresh_token", 0.0), &signal())
            .await
    }

    #[tokio::test]
    async fn does_not_fall_back_to_policy_models_for_non_individual_accounts() {
        let transport = Arc::new(ScriptedTransport::new(vec![
            (
                Some("/copilot_internal/v2/token"),
                json(200, serde_json::json!({"token": "tid=test;exp=9999999999;proxy-ep=proxy.business.githubcopilot.com;", "expires_at": 9_999_999_999i64})),
            ),
            (
                Some("api.business.githubcopilot.com/models"),
                json(
                    200,
                    serde_json::json!({"data": [{"id": "gpt-4.1", "model_picker_enabled": false, "policy": {"state": "enabled"}, "capabilities": {"supports": {"tool_calls": true}}}]}),
                ),
            ),
        ]));
        let credential = refresh_with(transport.clone()).await.unwrap();
        assert_eq!(credential.get_extra("availableModelIds"), Some(&serde_json::json!([])));
    }

    #[tokio::test]
    async fn does_not_retry_model_catalog_throttling_during_credential_refresh() {
        let transport = Arc::new(ScriptedTransport::new(vec![
            (
                Some("/copilot_internal/v2/token"),
                json(200, serde_json::json!({"token": TEST_ACCESS_TOKEN, "expires_at": 9_999_999_999i64})),
            ),
            (
                Some(TEST_MODELS_URL),
                ScriptedResponse::Text { status: 429, body: "{\"error\":\"too many requests\"}".into() },
            ),
        ]));
        let error = refresh_with(transport.clone()).await.unwrap_err();
        assert!(error.to_string().contains("429"), "{error}");
        assert_eq!(transport.requests().iter().filter(|request| request.url == TEST_MODELS_URL).count(), 1);
    }

    #[tokio::test]
    async fn refresh_preserves_the_enterprise_domain_and_queries_the_derived_host() {
        let transport = Arc::new(ScriptedTransport::new(vec![
            (
                Some("/copilot_internal/v2/token"),
                json(200, serde_json::json!({"token": "new-token", "expires_at": 9_999_999_999i64})),
            ),
            (Some("/models"), json(200, serde_json::json!({"data": []}))),
        ]));
        let oauth = GitHubCopilotOAuth::new(transport.clone());
        let credential = OAuthCredential::new("old", "gh-token", 0.0).with_extra("enterpriseUrl", serde_json::json!("company.ghe.com"));
        let refreshed = oauth.refresh(&credential, &signal()).await.unwrap();
        assert_eq!(refreshed.access, "new-token");
        assert_eq!(refreshed.get_extra_str("enterpriseUrl"), Some("company.ghe.com"));
        assert!(transport.requests()[0].url.contains("api.company.ghe.com"), "{:?}", transport.requests()[0].url);
    }

    #[tokio::test]
    async fn to_auth_derives_the_base_url_from_the_token_then_the_enterprise_domain() {
        let oauth = GitHubCopilotOAuth::new(Arc::new(ScriptedTransport::default()));
        let access = "tid=abc;exp=123;proxy-ep=proxy.enterprise.example;rest";
        let auth = oauth.to_auth(&OAuthCredential::new(access, "r", 0.0)).await.unwrap();
        assert_eq!(auth.api_key.as_deref(), Some(access));
        assert_eq!(auth.base_url.as_deref(), Some("https://api.enterprise.example"));

        let enterprise = OAuthCredential::new("no-proxy-ep", "r", 0.0)
            .with_extra("enterpriseUrl", serde_json::json!("https://company.ghe.com"));
        let auth = oauth.to_auth(&enterprise).await.unwrap();
        assert_eq!(auth.base_url.as_deref(), Some("https://copilot-api.company.ghe.com"));

        let individual = OAuthCredential::new("no-proxy-ep", "r", 0.0);
        let auth = oauth.to_auth(&individual).await.unwrap();
        assert_eq!(auth.base_url.as_deref(), Some("https://api.individual.githubcopilot.com"));
    }

    type CapturedEvents = Arc<Mutex<Vec<AuthEvent>>>;
    type CapturedMessages = Arc<Mutex<Vec<String>>>;

    struct RecordingInteraction {
        signal: AbortSignal,
        device_codes: Arc<Mutex<Vec<AuthEvent>>>,
        progress: Arc<Mutex<Vec<String>>>,
        answer: String,
    }

    #[async_trait]
    impl AuthInteraction for RecordingInteraction {
        fn signal(&self) -> Option<AbortSignal> {
            Some(self.signal.clone())
        }
        fn on_account_committed(&self, _receipt: AccountLoginReceipt) {}
        async fn prompt(&self, _prompt: AuthPrompt) -> anyhow::Result<String> {
            Ok(self.answer.clone())
        }
        fn notify(&self, event: AuthEvent) {
            match event {
                AuthEvent::DeviceCode { .. } => {
                    self.device_codes.lock().unwrap_or_else(|p| p.into_inner()).push(event);
                }
                AuthEvent::Progress { message } => {
                    self.progress.lock().unwrap_or_else(|p| p.into_inner()).push(message);
                }
                _ => {}
            }
        }
    }

    fn interaction(
        answer: &str,
    ) -> (ProviderAuthInteraction, CapturedEvents, CapturedMessages) {
        let signal = signal();
        let device_codes = Arc::new(Mutex::new(Vec::new()));
        let progress = Arc::new(Mutex::new(Vec::new()));
        let inner = Arc::new(RecordingInteraction {
            signal: signal.clone(),
            device_codes: device_codes.clone(),
            progress: progress.clone(),
            answer: answer.to_string(),
        });
        (ProviderAuthInteraction::new(signal, inner), device_codes, progress)
    }

    #[tokio::test]
    async fn reports_device_code_details_and_enables_known_unconfigured_models() {
        let known = crate::models_generated::MODELS
            .get("github-copilot")
            .and_then(|models| models.keys().find(|id| !id.is_empty()).cloned())
            .expect("copilot catalog has a model id");

        let transport = Arc::new(ScriptedTransport::new(vec![
            (
                Some("/login/device/code"),
                json(
                    200,
                    serde_json::json!({"device_code": "device-code", "user_code": "ABCD-EFGH", "verification_uri": "https://github.com/login/device", "interval": 1, "expires_in": 900}),
                ),
            ),
            (Some("/login/oauth/access_token"), json(200, serde_json::json!({"access_token": "ghu_refresh_token"}))),
            (
                Some("/copilot_internal/v2/token"),
                json(200, serde_json::json!({"token": TEST_ACCESS_TOKEN, "expires_at": 9_999_999_999i64})),
            ),
            (
                Some(TEST_MODELS_URL),
                json(
                    200,
                    serde_json::json!({"data": [
                        {"id": known, "model_picker_enabled": true, "policy": {"state": "unconfigured"}, "capabilities": {"supports": {"tool_calls": true}}},
                        {"id": "remote-only-model", "model_picker_enabled": true, "policy": {"state": "unconfigured"}, "capabilities": {"supports": {"tool_calls": true}}},
                    ]}),
                ),
            ),
            (Some("/policy"), ScriptedResponse::Text { status: 200, body: String::new() }),
        ]));
        let oauth = GitHubCopilotOAuth::new(transport.clone());
        let (interaction, device_codes, progress) = interaction("");
        let credential = oauth.login(&interaction).await.unwrap();

        assert_eq!(credential.access, TEST_ACCESS_TOKEN);
        let device_codes = device_codes.lock().unwrap_or_else(|p| p.into_inner()).clone();
        assert_eq!(
            device_codes,
            vec![AuthEvent::DeviceCode {
                user_code: "ABCD-EFGH".into(),
                verification_uri: "https://github.com/login/device".into(),
                interval_seconds: Some(1.0),
                expires_in_seconds: Some(900.0),
            }]
        );
        assert_eq!(*progress.lock().unwrap_or_else(|p| p.into_inner()), vec!["Enabling models...".to_string()]);
        let requests = transport.requests();
        let policy_requests: Vec<&HttpRequest> =
            requests.iter().filter(|request| request.url.ends_with("/policy")).collect();
        assert_eq!(policy_requests.len(), 1);
        assert!(policy_requests[0].url.contains(&known), "{}", policy_requests[0].url);
    }

    #[tokio::test]
    async fn rejects_a_non_http_verification_uri_before_it_reaches_on_device_code() {
        let transport = Arc::new(ScriptedTransport::new(vec![(
            Some("/login/device/code"),
            json(
                200,
                serde_json::json!({"device_code": "device-code", "user_code": "ABCD-EFGH", "verification_uri": "file:///etc/passwd", "interval": 1, "expires_in": 900}),
            ),
        )]));
        let oauth = GitHubCopilotOAuth::new(transport);
        let (interaction, device_codes, _) = interaction("");
        let error = oauth.login(&interaction).await.unwrap_err();
        assert!(error.to_string().contains("Untrusted verification_uri"), "{error}");
        assert!(device_codes.lock().unwrap_or_else(|p| p.into_inner()).is_empty());
    }

    #[tokio::test]
    async fn rejects_an_invalid_enterprise_domain_before_starting_the_device_flow() {
        let transport = Arc::new(ScriptedTransport::default());
        let oauth = GitHubCopilotOAuth::new(transport.clone());
        let (interaction, _, _) = interaction("not a domain");
        let error = oauth.login(&interaction).await.unwrap_err();
        assert_eq!(error.to_string(), "Invalid GitHub Enterprise URL/domain");
        assert!(transport.requests().is_empty());
    }

    #[tokio::test]
    async fn continues_policy_updates_after_a_transport_failure() {
        let known: Vec<String> = crate::models_generated::MODELS
            .get("github-copilot")
            .map(|models| models.keys().filter(|id| !id.is_empty()).take(2).cloned().collect())
            .unwrap_or_default();
        assert!(known.len() == 2, "copilot catalog needs two model ids");

        let transport = Arc::new(ScriptedTransport::new(vec![
            (
                Some("/login/device/code"),
                json(
                    200,
                    serde_json::json!({"device_code": "device-code", "user_code": "ABCD-EFGH", "verification_uri": "https://github.com/login/device", "interval": 1, "expires_in": 900}),
                ),
            ),
            (Some("/login/oauth/access_token"), json(200, serde_json::json!({"access_token": "ghu_refresh_token"}))),
            (
                Some("/copilot_internal/v2/token"),
                json(200, serde_json::json!({"token": TEST_ACCESS_TOKEN, "expires_at": 9_999_999_999i64})),
            ),
            (
                Some(TEST_MODELS_URL),
                json(
                    200,
                    serde_json::json!({"data": [
                        {"id": known[0], "model_picker_enabled": true, "policy": {"state": "unconfigured"}, "capabilities": {"supports": {"tool_calls": true}}},
                        {"id": known[1], "model_picker_enabled": true, "policy": {"state": "unconfigured"}, "capabilities": {"supports": {"tool_calls": true}}},
                    ]}),
                ),
            ),
            (Some("/policy"), ScriptedResponse::Failure { message: "fetch failed".into() }),
            (Some("/policy"), ScriptedResponse::Text { status: 200, body: String::new() }),
        ]));
        let oauth = GitHubCopilotOAuth::new(transport.clone());
        let (interaction, _, _) = interaction("");
        let credential = oauth.login(&interaction).await.unwrap();
        let policy_urls: Vec<String> =
            transport.requests().iter().filter(|request| request.url.ends_with("/policy")).map(|r| r.url.clone()).collect();
        assert_eq!(policy_urls.len(), 2, "{policy_urls:?}");
        assert!(policy_urls[0].contains(&known[0]));
        assert!(policy_urls[1].contains(&known[1]));
        assert_eq!(credential.get_extra("availableModelIds"), Some(&serde_json::json!(known)));
    }
}
