//! Port of senpi `packages/coding-agent/src/core/retry-fallback/limit-reset.ts`.

use maho_ai::types::{AssistantMessage, ProviderDiagnosticCategory};
use maho_ai::utils::retry_hint::{
    RetryHintInput, extract_429_retry_after_ms, parse_retry_after_ms_marker,
};

use crate::credential_pool::reset_time::usage_limit_reset_ms;

/// The wall-clock moment (epoch ms) a failed request's limit lifts, when the failure names one:
/// the adapter's structured quota/rate-limit reset first, then usage-limit reset prose or fields,
/// then a Retry-After hint. `None` when the failure names no future reset.
pub fn limit_reset_at(message: &AssistantMessage, wall_now: i64) -> Option<i64> {
    if let Some(diagnostic) = message.provider_diagnostic.as_ref()
        && matches!(
            diagnostic.category,
            ProviderDiagnosticCategory::Quota | ProviderDiagnosticCategory::RateLimit
        )
        && let Some(resets_at) = diagnostic.resets_at
        && resets_at > wall_now
    {
        return Some(resets_at);
    }
    let text = message.error_message.as_deref().unwrap_or("");
    let reset_ms = usage_limit_reset_ms(text, wall_now)
        .or_else(|| parse_retry_after_ms_marker(text))
        .or_else(|| {
            extract_429_retry_after_ms(
                &RetryHintInput {
                    status: Some(429),
                    headers: None,
                    body_text: text,
                },
                Some(wall_now),
            )
        });
    reset_ms
        .filter(|ms| *ms > 0)
        .and_then(|ms| i64::try_from(ms).ok())
        .map(|ms| wall_now.saturating_add(ms))
}
