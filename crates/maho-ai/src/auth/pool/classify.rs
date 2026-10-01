//! Port of senpi packages/ai/src/auth/pool/classify.ts.

use fancy_regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolFailureAction {
    Rotate,
    Retry,
    Fail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolBlockReason {
    AuthError,
    RateLimit,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PoolFailureClassification {
    pub action: PoolFailureAction,
    pub block_reason: Option<PoolBlockReason>,
    pub retry_after_ms: Option<f64>,
}

impl PoolFailureClassification {
    fn rotate(block_reason: PoolBlockReason) -> Self {
        Self { action: PoolFailureAction::Rotate, block_reason: Some(block_reason), retry_after_ms: None }
    }
    fn retry() -> Self {
        Self { action: PoolFailureAction::Retry, block_reason: None, retry_after_ms: None }
    }
    fn fail() -> Self {
        Self { action: PoolFailureAction::Fail, block_reason: None, retry_after_ms: None }
    }
}

const MAX_NESTING_DEPTH: u8 = 3;

/// A pool failure error, mirroring the shapes senpi encounters (a bare string, an `Error`-like
/// object with `message`/`status`/`statusCode`/`retryAfterMs`, or one nested under `error`).
#[derive(Debug, Clone)]
pub enum PoolFailureError {
    Text(String),
    Structured(Value),
}

impl std::fmt::Display for PoolFailureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&message_of(self, 0))
    }
}

impl From<&str> for PoolFailureError {
    fn from(value: &str) -> Self {
        Self::Text(value.to_string())
    }
}

impl From<Value> for PoolFailureError {
    fn from(value: Value) -> Self {
        Self::Structured(value)
    }
}

fn field_of(value: &Value, key: &str) -> Option<Value> {
    value.as_object()?.get(key).cloned()
}

fn status_of(error: &PoolFailureError, depth: u8) -> Option<f64> {
    if depth >= MAX_NESTING_DEPTH {
        return None;
    }
    let PoolFailureError::Structured(value) = error else { return None };
    for key in ["status", "statusCode"] {
        if let Some(found) = field_of(value, key).and_then(|v| v.as_f64()) {
            return Some(found);
        }
    }
    let nested = field_of(value, "error")?;
    status_of(&PoolFailureError::Structured(nested), depth + 1)
}

fn message_of(error: &PoolFailureError, depth: u8) -> String {
    match error {
        PoolFailureError::Text(text) => text.clone(),
        PoolFailureError::Structured(value) => {
            if depth >= MAX_NESTING_DEPTH {
                return value.to_string();
            }
            if let Some(message) = field_of(value, "message").and_then(|v| v.as_str().map(str::to_owned)) {
                return message;
            }
            match field_of(value, "error") {
                Some(nested) => message_of(&PoolFailureError::Structured(nested), depth + 1),
                None => value.to_string(),
            }
        }
    }
}

static RETRY_AFTER_MS_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bretry[-_ ]?after[-_ ]?ms\s*[:=]\s*(\d+(?:\.\d+)?)").expect("valid regex"));
static RETRY_AFTER_SECONDS_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bretry[-_ ]?after\s*[:=]\s*(\d+(?:\.\d+)?)").expect("valid regex"));
static RATE_LIMIT_TEXT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)rate.?limit|resource_exhausted|quota|too many requests|overloaded").expect("valid regex")
});
static AUTH_TEXT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)invalid.?api.?key|unauthorized|authentication|forbidden|token (?:is )?expired|revoked")
        .expect("valid regex")
});
static TRANSIENT_TEXT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)econnreset|econnrefused|etimedout|socket hang up|fetch failed|network error|internal server error|service unavailable|bad gateway|gateway timeout",
    )
    .expect("valid regex")
});

fn retry_after_ms_of(error: &PoolFailureError, text: &str) -> Option<f64> {
    if let PoolFailureError::Structured(value) = error
        && let Some(explicit) = field_of(value, "retryAfterMs").and_then(|v| v.as_f64())
            && explicit.is_finite() && explicit > 0.0 {
                return Some(explicit.ceil());
            }
    if let Ok(Some(captures)) = RETRY_AFTER_MS_PATTERN.captures(text)
        && let Some(m) = captures.get(1)
            && let Ok(ms) = m.as_str().parse::<f64>() {
                return Some(ms.ceil());
            }
    if let Ok(Some(captures)) = RETRY_AFTER_SECONDS_PATTERN.captures(text)
        && let Some(m) = captures.get(1)
            && let Ok(seconds) = m.as_str().parse::<f64>() {
                return Some((seconds * 1_000.0).ceil());
            }
    None
}

fn rotate_on_rate_limit(error: &PoolFailureError, text: &str) -> PoolFailureClassification {
    let retry_after_ms = retry_after_ms_of(error, text);
    PoolFailureClassification { retry_after_ms, ..PoolFailureClassification::rotate(PoolBlockReason::RateLimit) }
}

/// Three-way in-lane failure taxonomy. Rotation is reserved for failures another account can
/// plausibly absorb (rate/capacity and per-account auth); transient transport failures stay on
/// the same slot for the outer retry policy, and everything unrecognized default-denies to
/// `Fail` so the model fallback chain above this engine keeps owning unknown errors.
pub fn classify_pool_failure(error: &PoolFailureError) -> PoolFailureClassification {
    let text = message_of(error, 0);
    let status = status_of(error, 0);
    if status == Some(429.0) || status == Some(529.0) {
        return rotate_on_rate_limit(error, &text);
    }
    if status == Some(401.0) || status == Some(403.0) {
        return PoolFailureClassification::rotate(PoolBlockReason::AuthError);
    }
    if let Some(status) = status
        && (500.0..600.0).contains(&status) {
            return PoolFailureClassification::retry();
        }
    if RATE_LIMIT_TEXT.is_match(&text).unwrap_or(false) {
        return rotate_on_rate_limit(error, &text);
    }
    if AUTH_TEXT.is_match(&text).unwrap_or(false) {
        return PoolFailureClassification::rotate(PoolBlockReason::AuthError);
    }
    if TRANSIENT_TEXT.is_match(&text).unwrap_or(false) {
        return PoolFailureClassification::retry();
    }
    PoolFailureClassification::fail()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn status_429_rotates_on_rate_limit() {
        let error = PoolFailureError::from(json!({"status": 429, "message": "too many requests"}));
        let result = classify_pool_failure(&error);
        assert_eq!(result.action, PoolFailureAction::Rotate);
        assert_eq!(result.block_reason, Some(PoolBlockReason::RateLimit));
    }

    #[test]
    fn status_529_rotates_on_rate_limit() {
        let error = PoolFailureError::from(json!({"status": 529}));
        assert_eq!(classify_pool_failure(&error).block_reason, Some(PoolBlockReason::RateLimit));
    }

    #[test]
    fn status_401_and_403_rotate_on_auth_error() {
        for status in [401, 403] {
            let error = PoolFailureError::from(json!({"status": status}));
            let result = classify_pool_failure(&error);
            assert_eq!(result.action, PoolFailureAction::Rotate);
            assert_eq!(result.block_reason, Some(PoolBlockReason::AuthError));
        }
    }

    #[test]
    fn status_5xx_retries() {
        let error = PoolFailureError::from(json!({"status": 503}));
        assert_eq!(classify_pool_failure(&error).action, PoolFailureAction::Retry);
    }

    #[test]
    fn text_pattern_matches_rate_limit_auth_and_transient() {
        assert_eq!(
            classify_pool_failure(&PoolFailureError::from("Resource exhausted, quota reached")).block_reason,
            Some(PoolBlockReason::RateLimit)
        );
        assert_eq!(
            classify_pool_failure(&PoolFailureError::from("Invalid API key provided")).block_reason,
            Some(PoolBlockReason::AuthError)
        );
        assert_eq!(classify_pool_failure(&PoolFailureError::from("ECONNRESET")).action, PoolFailureAction::Retry);
    }

    #[test]
    fn unrecognized_error_defaults_to_fail() {
        let error = PoolFailureError::from("completely unrelated error text");
        assert_eq!(classify_pool_failure(&error).action, PoolFailureAction::Fail);
    }

    #[test]
    fn extracts_retry_after_ms_from_explicit_field_and_text() {
        let explicit = PoolFailureError::from(json!({"status": 429, "retryAfterMs": 2500}));
        assert_eq!(classify_pool_failure(&explicit).retry_after_ms, Some(2500.0));

        let from_text = PoolFailureError::from("rate limited, retry-after-ms: 1200");
        assert_eq!(classify_pool_failure(&from_text).retry_after_ms, Some(1200.0));

        let from_seconds = PoolFailureError::from("rate limited, retry-after: 3");
        assert_eq!(classify_pool_failure(&from_seconds).retry_after_ms, Some(3000.0));
    }

    #[test]
    fn nested_error_field_is_unwrapped_up_to_max_depth() {
        let nested = PoolFailureError::from(json!({"error": {"error": {"status": 429}}}));
        assert_eq!(classify_pool_failure(&nested).block_reason, Some(PoolBlockReason::RateLimit));
    }
}
