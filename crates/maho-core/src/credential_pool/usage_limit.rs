//! Port of senpi `packages/coding-agent/src/core/credential-pool/usage-limit.ts`.

use std::sync::LazyLock;

use maho_ai::utils::retry::USAGE_LIMIT_EXHAUSTION;
use regex::Regex;

/// An account-scoped usage limit that reaches the pool as text alone (senpi#1768). Every alternative is
/// a shape a supported provider is known to send. It stays anchored on exhaustion verbs: "You are
/// approaching your usage limit" is a warning, and the overflow prose (token limit, context limit) is
/// excluded by the caller, which runs this before the overflow branch.
static ACCOUNT_USAGE_LIMIT_TEXT: LazyLock<Regex> = LazyLock::new(|| {
    let mut alternatives: Vec<String> = USAGE_LIMIT_EXHAUSTION
        .markers
        .iter()
        .map(|marker| regex::escape(marker))
        .collect();
    alternatives.extend(
        [
            r"\b(?:hit|reached)\s+your\b[^.]*?\blimit\b",
            r"\busage\s+limit\s+reached\b",
            r"\b(?:Go|Free)UsageLimitError\b",
            r"\bblocking_limit\b",
            r"\brapid_refill_breaker\b",
            r"\bquota\s+exceeded\b",
        ]
        .map(str::to_owned),
    );
    Regex::new(&format!("(?i){}", alternatives.join("|")))
        .unwrap_or_else(|e| panic!("account usage limit: {e}"))
});

pub fn is_account_usage_limit_text(text: &str) -> bool {
    ACCOUNT_USAGE_LIMIT_TEXT.is_match(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_exhaustion_shapes_match() {
        for text in [
            "usage_limit_reached",
            "The usage limit has been reached",
            "You have hit your ChatGPT usage limit",
            "You've reached your 5-hour limit",
            "Monthly usage limit reached",
            "GoUsageLimitError",
            "terminal reason: blocking_limit",
            "rapid_refill_breaker",
            "Quota exceeded for this account",
        ] {
            assert!(is_account_usage_limit_text(text), "{text}");
        }
    }

    #[test]
    fn warnings_and_overflow_prose_do_not_match() {
        for text in [
            "You are approaching your usage limit",
            "prompt is too long: token limit exceeded",
            "context limit",
        ] {
            assert!(!is_account_usage_limit_text(text), "{text}");
        }
    }
}
