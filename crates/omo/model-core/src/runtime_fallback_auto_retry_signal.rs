use std::sync::LazyLock;

use regex::Regex;
use serde_json::Map;
use serde_json::Value;

use crate::runtime_fallback_retryable_patterns::matches_retryable_pattern;

/// The combined provider status text that indicated an auto-retry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeFallbackAutoRetrySignal {
    pub signal: String,
}

/// Joins the string `status`/`summary`/`message`/`details` fields and reports them when they
/// look like a provider-side retry or exhaustion notice.
#[must_use]
#[expect(clippy::expect_used, reason = "static regex literals are valid")]
pub fn extract_runtime_fallback_auto_retry_signal(
    info: Option<&Map<String, Value>>,
) -> Option<RuntimeFallbackAutoRetrySignal> {
    static RETRYING_IN: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)retrying\s+in").expect("valid regex"));
    static USAGE_LIMIT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)usage\s+limit|limit\s+reached").expect("valid regex"));

    let info = info?;
    let combined = ["status", "summary", "message", "details"]
        .iter()
        .filter_map(|key| info.get(*key).and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    if combined.is_empty() {
        return None;
    }

    let signalled = RETRYING_IN.is_match(&combined)
        || USAGE_LIMIT.is_match(&combined)
        || matches_retryable_pattern(&combined);
    signalled.then_some(RuntimeFallbackAutoRetrySignal { signal: combined })
}
