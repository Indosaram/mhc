//! Port of senpi packages/coding-agent/src/core/subscription-limits/usage-endpoints.ts.
//!
//! The usage endpoints subscription providers publish, read into the store's window shape. Claude
//! answers `GET /api/oauth/usage` with `limits[]` (kind `session` / `weekly_all` / `weekly_scoped`,
//! percent 0-100) or, from older servers, the `five_hour` / `seven_day[_<model>]` pair (utilization
//! 0-100); ChatGPT answers `GET /backend-api/wham/usage` with `rate_limit.{primary,secondary}_window`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::Value;

use super::limit_windows::{LimitWindow, window_label};

pub const CLAUDE_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
pub const CODEX_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";

const USAGE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UsageProvider {
    AnthropicSubscription,
    ChatgptSubscription,
}

impl UsageProvider {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AnthropicSubscription => "anthropic-subscription",
            Self::ChatgptSubscription => "chatgpt-subscription",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct UsageReading {
    pub windows: Vec<LimitWindow>,
    pub plan: Option<String>,
}

/// A refused usage request: the status and how long the endpoint asked to be left alone.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("HTTP {status}")]
pub struct UsageHttpError {
    pub status: u16,
    pub retry_after_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UsageError {
    #[error(transparent)]
    Http(#[from] UsageHttpError),
    #[error("{0}")]
    Other(String),
}

pub type UsageFuture = Pin<Box<dyn Future<Output = Result<Value, UsageError>> + Send>>;
/// `(url, headers)` -> the response's JSON body.
pub type UsageFetch = Arc<dyn Fn(String, Vec<(String, String)>) -> UsageFuture + Send + Sync>;

/// Redirects are never followed: a redirecting usage endpoint is a login wall, and the bearer token
/// must not follow it.
pub fn fetch_usage_json() -> UsageFetch {
    Arc::new(|url, headers| {
        Box::pin(async move {
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(USAGE_TIMEOUT)
                .build()
                .map_err(|error| UsageError::Other(error.to_string()))?;
            let mut request = client.get(&url);
            for (name, value) in &headers {
                request = request.header(name, value);
            }
            let response = request.send().await.map_err(|error| {
                if error.is_timeout() {
                    UsageError::Other("request timed out".into())
                } else {
                    UsageError::Other(error.to_string())
                }
            })?;
            let status = response.status();
            if status.is_redirection() {
                return Err(UsageError::Other(format!(
                    "unexpected redirect ({})",
                    status.as_u16()
                )));
            }
            if !status.is_success() {
                let header = response
                    .headers()
                    .get("retry-after")
                    .and_then(|value| value.to_str().ok());
                return Err(UsageHttpError {
                    status: status.as_u16(),
                    retry_after_ms: retry_after_ms(header, now_ms()),
                }
                .into());
            }
            response
                .json::<Value>()
                .await
                .map_err(|error| UsageError::Other(error.to_string()))
        })
    })
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub fn retry_after_ms(value: Option<&str>, now: i64) -> Option<i64> {
    let value = value?;
    if let Ok(seconds) = value.trim().parse::<f64>()
        && seconds.is_finite()
        && seconds > 0.0
    {
        return Some((seconds * 1000.0) as i64);
    }
    let delay = chrono::DateTime::parse_from_rfc2822(value)
        .ok()?
        .timestamp_millis()
        - now;
    (delay > 0).then_some(delay)
}

pub async fn read_usage(
    provider: UsageProvider,
    access: &str,
    fetch_json: &UsageFetch,
) -> Result<UsageReading, UsageError> {
    let mut headers = vec![
        ("Authorization".to_owned(), format!("Bearer {access}")),
        ("Accept".to_owned(), "application/json".to_owned()),
    ];
    if provider == UsageProvider::AnthropicSubscription {
        let payload = fetch_json(CLAUDE_USAGE_URL.to_owned(), headers).await?;
        return Ok(UsageReading {
            windows: claude_windows_from_usage(&payload),
            plan: None,
        });
    }
    if let Some(account_id) = codex_account_id(access) {
        headers.push(("ChatGPT-Account-Id".to_owned(), account_id));
    }
    let payload = fetch_json(CODEX_USAGE_URL.to_owned(), headers).await?;
    Ok(codex_usage(&payload))
}

fn finite(value: Option<&Value>) -> Option<f64> {
    value
        .and_then(Value::as_f64)
        .filter(|number| number.is_finite())
}

fn iso_instant(value: Option<&Value>) -> Option<String> {
    let parsed = chrono::DateTime::parse_from_rfc3339(value?.as_str()?).ok()?;
    Some(
        parsed
            .with_timezone(&chrono::Utc)
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string(),
    )
}

fn claude_window(id: &str, label: &str, percent: f64, resets_at: Option<String>) -> LimitWindow {
    LimitWindow {
        id: id.to_owned(),
        label: label.to_owned(),
        window_minutes: Some(if id == "claude:5h" { 300 } else { 10080 }),
        used_percent: percent,
        resets_at,
    }
}

/// `Opus` -> `claude:7d_opus` / `7d opus`, the ids the SDK's `rate_limit_event` uses.
fn scoped_window(model: &str, percent: f64, resets_at: Option<String>) -> LimitWindow {
    let lower = model.to_lowercase();
    let mut slug = String::new();
    let mut in_run = false;
    for character in lower.chars() {
        if character.is_ascii_lowercase() || character.is_ascii_digit() {
            slug.push(character);
            in_run = false;
        } else if !in_run {
            slug.push('_');
            in_run = true;
        }
    }
    claude_window(
        &format!("claude:7d_{slug}"),
        &format!("7d {lower}"),
        percent,
        resets_at,
    )
}

pub fn claude_windows_from_usage(payload: &Value) -> Vec<LimitWindow> {
    let mut windows = Vec::new();
    for limit in payload
        .get("limits")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(percent) = finite(limit.get("percent")) else {
            continue;
        };
        let resets_at = iso_instant(limit.get("resets_at"));
        match limit.get("kind").and_then(Value::as_str) {
            Some("session") => windows.push(claude_window("claude:5h", "5h", percent, resets_at)),
            Some("weekly_all") => {
                windows.push(claude_window("claude:7d", "7d", percent, resets_at))
            }
            Some("weekly_scoped") => {
                let model = limit
                    .get("scope")
                    .and_then(|scope| scope.get("model"))
                    .and_then(|model| model.get("display_name"))
                    .and_then(Value::as_str);
                if let Some(model) = model.filter(|model| !model.is_empty()) {
                    windows.push(scoped_window(model, percent, resets_at));
                }
            }
            _ => {}
        }
    }
    if !windows.is_empty() {
        return windows;
    }
    for (key, value) in payload.as_object().into_iter().flatten() {
        let Some(utilization) = finite(value.get("utilization")) else {
            continue;
        };
        let resets_at = iso_instant(value.get("resets_at"));
        if key == "five_hour" {
            windows.push(claude_window("claude:5h", "5h", utilization, resets_at));
        } else if key == "seven_day" {
            windows.push(claude_window("claude:7d", "7d", utilization, resets_at));
        } else if let Some(model) = key.strip_prefix("seven_day_") {
            windows.push(scoped_window(model, utilization, resets_at));
        }
    }
    windows
}

pub fn codex_usage(payload: &Value) -> UsageReading {
    let rate_limit = payload.get("rate_limit");
    let mut windows = Vec::new();
    for slot in ["primary", "secondary"] {
        let Some(window) = rate_limit.and_then(|limit| limit.get(format!("{slot}_window"))) else {
            continue;
        };
        let Some(used) = finite(window.get("used_percent")) else {
            continue;
        };
        let minutes = finite(window.get("limit_window_seconds"))
            .map(|seconds| (seconds / 60.0).round() as u64);
        let resets_at = finite(window.get("reset_at"))
            .and_then(|seconds| chrono::DateTime::from_timestamp_millis((seconds * 1000.0) as i64))
            .map(|instant| instant.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string());
        windows.push(LimitWindow {
            id: format!("codex:{slot}"),
            label: window_label(minutes, slot),
            window_minutes: minutes,
            used_percent: used,
            resets_at,
        });
    }
    let plan = payload
        .get("plan_type")
        .and_then(Value::as_str)
        .filter(|plan| !plan.is_empty())
        .map(str::to_owned);
    UsageReading { windows, plan }
}

/// The ChatGPT account id lives in the access token's claims; a token that will not decode sends none.
fn codex_account_id(token: &str) -> Option<String> {
    let payload = token
        .split('.')
        .nth(1)
        .filter(|payload| !payload.is_empty())?;
    let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    let claims: Value = serde_json::from_slice(&bytes).ok()?;
    let auth = claims.get("https://api.openai.com/auth")?;
    ["chatgpt_account_id", "account_id"].iter().find_map(|key| {
        auth.get(*key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn retry_after_parses_seconds_and_dates() {
        assert_eq!(retry_after_ms(None, 0), None);
        assert_eq!(retry_after_ms(Some("30"), 0), Some(30_000));
        assert_eq!(retry_after_ms(Some("0"), 0), None);
        assert_eq!(
            retry_after_ms(Some("Thu, 01 Jan 1970 00:00:10 GMT"), 4_000),
            Some(6_000)
        );
        assert_eq!(
            retry_after_ms(Some("Thu, 01 Jan 1970 00:00:10 GMT"), 20_000),
            None
        );
        assert_eq!(retry_after_ms(Some("soon"), 0), None);
    }

    #[test]
    fn claude_limits_array_maps_to_store_ids() {
        let windows = claude_windows_from_usage(&json!({"limits": [
            {"kind": "session", "percent": 12.5, "resets_at": "2026-01-01T00:00:00Z"},
            {"kind": "weekly_all", "percent": 40},
            {"kind": "weekly_scoped", "percent": 7, "scope": {"model": {"display_name": "Opus 4"}}},
            {"kind": "session"}
        ]}));
        let ids: Vec<_> = windows.iter().map(|window| window.id.as_str()).collect();
        assert_eq!(ids, ["claude:5h", "claude:7d", "claude:7d_opus_4"]);
        assert_eq!(
            windows[0].resets_at.as_deref(),
            Some("2026-01-01T00:00:00.000Z")
        );
        assert_eq!(windows[2].label, "7d opus 4");
        assert_eq!(windows[2].window_minutes, Some(10080));
    }

    #[test]
    fn claude_legacy_shape_is_the_fallback() {
        let windows = claude_windows_from_usage(
            &json!({"five_hour": {"utilization": 3}, "seven_day": {"utilization": 4}, "seven_day_sonnet": {"utilization": 5}, "other": {}}),
        );
        let ids: Vec<_> = windows.iter().map(|window| window.id.as_str()).collect();
        assert_eq!(ids, ["claude:5h", "claude:7d", "claude:7d_sonnet"]);
    }

    #[test]
    fn codex_windows_and_plan_are_read() {
        let reading = codex_usage(
            &json!({"plan_type": "pro", "rate_limit": {"primary_window": {"used_percent": 20, "limit_window_seconds": 18000, "reset_at": 1}, "secondary_window": {"used_percent": 1}}}),
        );
        assert_eq!(reading.plan.as_deref(), Some("pro"));
        assert_eq!(reading.windows[0].id, "codex:primary");
        assert_eq!(reading.windows[0].window_minutes, Some(300));
        assert_eq!(
            reading.windows[0].resets_at.as_deref(),
            Some("1970-01-01T00:00:01.000Z")
        );
        assert_eq!(reading.windows[1].id, "codex:secondary");
        assert_eq!(reading.windows[1].window_minutes, None);
    }

    #[test]
    fn account_id_comes_from_the_token_claims() {
        let claims = URL_SAFE_NO_PAD.encode(
            json!({"https://api.openai.com/auth": {"chatgpt_account_id": "acct"}}).to_string(),
        );
        assert_eq!(
            codex_account_id(&format!("h.{claims}.s")).as_deref(),
            Some("acct")
        );
        assert_eq!(codex_account_id("not-a-jwt"), None);
        assert_eq!(codex_account_id("h.!!!.s"), None);
    }
}
