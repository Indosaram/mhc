//! Port of senpi goal/provider-retry.ts.
use maho_agent::types::AgentMessage;
use maho_ai::types::ProviderDiagnosticCategory;

pub const PROVIDER_RETRY_BASE_MS: i64 = 15_000;
pub const PROVIDER_RETRY_CAP_MS: i64 = 30 * 60_000;
pub const PROVIDER_RETRY_WAKE_CAP_MS: i64 = 6 * 60 * 60_000;
const WAKE_MARGIN_MS: i64 = 1_000;

fn iso_instant() -> Option<&'static regex::Regex> {
    static PATTERN: std::sync::OnceLock<Option<regex::Regex>> = std::sync::OnceLock::new();
    PATTERN
        .get_or_init(|| {
            regex::Regex::new(r"\b\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z\b").ok()
        })
        .as_ref()
}

/// Prefer a provider's structured usage reset, then its retry-after marker. Legacy pool guidance
/// falls back to the earliest future ISO instant in the error only. All values are epoch ms.
pub fn provider_retry_at(messages: &[AgentMessage], now: i64) -> Option<i64> {
    let failure = crate::last_assistant_message::last_assistant_message(messages);
    if let Some(diagnostic) = failure.and_then(|message| message.provider_diagnostic.as_ref())
        && matches!(
            diagnostic.category,
            ProviderDiagnosticCategory::Quota | ProviderDiagnosticCategory::RateLimit
        )
        && let Some(reset) = diagnostic.resets_at
        && reset > now
    {
        return Some(reset);
    }
    let text = failure
        .and_then(|message| message.error_message.as_deref())
        .unwrap_or("");
    if let Some(retry_after) =
        maho_ai::utils::retry_hint::parse_retry_after_ms_marker(text).filter(|ms| *ms > 0)
        && let Ok(retry_after) = i64::try_from(retry_after)
    {
        return Some(now.saturating_add(retry_after));
    }
    iso_instant()?
        .find_iter(text)
        .filter_map(|found| {
            found
                .as_str()
                .parse::<jiff::Timestamp>()
                .ok()
                .map(|at| at.as_millisecond())
        })
        .filter(|at| *at > now)
        .min()
}

/// How long to wait before the `attempt`-th consecutive provider-recovery continuation.
pub fn provider_retry_delay_ms(attempt: u32, retry_at: Option<i64>, now: i64) -> i64 {
    if let Some(retry_at) = retry_at {
        return PROVIDER_RETRY_WAKE_CAP_MS.min(retry_at - now + WAKE_MARGIN_MS);
    }
    if attempt <= 1 {
        return 0;
    }
    PROVIDER_RETRY_CAP_MS.min(PROVIDER_RETRY_BASE_MS.saturating_mul(1_i64 << (attempt - 2).min(40)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn delay_prefers_stated_time_then_backs_off() {
        assert_eq!(provider_retry_delay_ms(1, Some(10_000), 0), 11_000);
        assert_eq!(
            provider_retry_delay_ms(1, Some(i64::MAX / 4), 0),
            PROVIDER_RETRY_WAKE_CAP_MS
        );
        assert_eq!(provider_retry_delay_ms(1, None, 0), 0);
        assert_eq!(provider_retry_delay_ms(2, None, 0), PROVIDER_RETRY_BASE_MS);
        assert_eq!(
            provider_retry_delay_ms(3, None, 0),
            2 * PROVIDER_RETRY_BASE_MS
        );
        assert_eq!(provider_retry_delay_ms(30, None, 0), PROVIDER_RETRY_CAP_MS);
    }
    #[test]
    fn no_messages_means_no_retry_time() {
        assert_eq!(provider_retry_at(&[], 0), None);
    }
}
