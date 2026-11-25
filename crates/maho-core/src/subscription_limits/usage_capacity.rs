//! Port of senpi `packages/coding-agent/src/core/subscription-limits/usage-capacity.ts`.

use chrono::DateTime;

use super::limit_store::AccountLimits;

/// Whether usage the provider reported AFTER a limit hold began shows the account can serve again
/// (senpi#6536). Only a reading newer than `since` counts, every window must be under its limit (or
/// already past its reset), and a reading with no windows or an error says nothing.
pub fn usage_shows_capacity(limits: &AccountLimits, since: i64, now: i64) -> bool {
    if limits.error.is_some() || limits.windows.is_empty() {
        return false;
    }
    let Ok(reported_at) = DateTime::parse_from_rfc3339(&limits.reported_at) else {
        return false;
    };
    if reported_at.timestamp_millis() <= since {
        return false;
    }
    limits.windows.iter().all(|window| {
        if window.used_percent < 100.0 {
            return true;
        }
        window
            .resets_at
            .as_deref()
            .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
            .is_some_and(|dt| dt.timestamp_millis() <= now)
    })
}

/// The accounts of `provider` whose reported usage, newer than `since`, shows they can serve again.
pub fn accounts_with_capacity(
    snapshot: &[AccountLimits],
    provider: &str,
    since: i64,
    now: i64,
) -> Vec<String> {
    snapshot
        .iter()
        .filter(|limits| limits.provider == provider && usage_shows_capacity(limits, since, now))
        .map(|limits| limits.account.clone())
        .collect()
}
