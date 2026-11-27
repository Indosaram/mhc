//! Port of senpi `packages/coding-agent/src/core/subscription-limits/limit-summary.ts`.

use super::limit_store::{AccountLimits, SubscriptionLimitStore, subscription_limits};

fn format_account_retry_time(epoch_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(epoch_ms)
        .map(|dt| dt.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| format!("{epoch_ms}ms"))
}

/// How old a limit reading is, for account lists.
pub fn reading_age(ms: i64) -> String {
    let minutes = ms.max(0) / 60_000;
    if minutes < 1 {
        return "just now".to_owned();
    }
    if minutes < 60 {
        return format!("{minutes} min ago");
    }
    let hours = minutes / 60;
    if hours < 48 {
        format!("{hours} h ago")
    } else {
        format!("{} d ago", hours / 24)
    }
}

/// One account's limit windows for an account list.
pub fn describe_account_limits(limits: Option<&AccountLimits>, now_ms: i64) -> String {
    let Some(limits) = limits else {
        return "limits: not reported yet".to_owned();
    };
    if limits.windows.is_empty() {
        return match &limits.error {
            None => "limits: not reported yet".to_owned(),
            Some(err) => format!("limits: last check failed: {err}"),
        };
    }
    let windows: Vec<String> = limits
        .windows
        .iter()
        .map(|window| {
            let resets_at = window
                .resets_at
                .as_deref()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.timestamp_millis());
            if let Some(resets) = resets_at
                && resets <= now_ms
            {
                return format!("{} reset since this reading", window.label);
            }
            let used = format!(
                "{} {}% used",
                window.label,
                window.used_percent.round() as i64
            );
            match resets_at {
                None => used,
                Some(resets) => format!("{used}, resets {}", format_account_retry_time(resets)),
            }
        })
        .collect();
    let reported_ms = chrono::DateTime::parse_from_rfc3339(&limits.reported_at)
        .map(|dt| dt.timestamp_millis())
        .unwrap_or(now_ms);
    let reading = format!("updated {}", reading_age(now_ms - reported_ms));
    let failed = match &limits.error {
        None => String::new(),
        Some(err) => format!("; last check failed: {err}"),
    };
    format!("limits: {} ({reading}{failed})", windows.join("; "))
}

/// The latest limits the process holds for one provider account.
pub fn account_limits_for(
    provider: &str,
    account: &str,
    store: Option<&SubscriptionLimitStore>,
) -> Option<AccountLimits> {
    let store = store.unwrap_or_else(|| subscription_limits());
    store
        .snapshot()
        .into_iter()
        .find(|limits| limits.provider == provider && limits.account == account)
}
