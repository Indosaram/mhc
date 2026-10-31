//! Port of senpi `anthropic-subscription/usage-limit-block.ts` (pin 903b8336).
use maho_core::credential_pool::reset_time::usage_limit_reset_ms;
use maho_core::subscription_limits::limit_store::{SubscriptionLimitStore, subscription_limits};
use regex::Regex;
use std::sync::OnceLock;

// The subscription-limit store keys accounts by this provider id (auth-attempt.ts).
const PROVIDER: &str = "anthropic-subscription";
const HOUR_MS: i64 = 60 * 60 * 1_000;
/// A limit that names no window is capped at the shortest subscription window.
const UNNAMED_WINDOW_MS: i64 = 5 * HOUR_MS;

/// Claude Code's own account usage-limit errors, anchored at the start of the message. Only a
/// window word Claude Code uses may stand before "limit". The upstream lookahead
/// `(?=$|\s*[\u00b7\u2219.,{(]|\s+resets?\b)` is checked by `limit_tail_ok`.
fn claude_usage_limit() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)^(?:(?:You['\x{2019}]ve|You have) (?:hit|reached) your (?:(session|weekly|daily|monthly|\d+-hour|usage) )?limit|(Claude AI usage limit reached)\b)",
        )
        .expect("usage limit pattern")
    })
}

fn resets_word() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^\s+resets?\b").expect("resets pattern"))
}

/// Claude Code's structured terminal reason for a spent subscription (senpi#3466).
fn terminal_limit_reason() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\bblocking_limit\b").expect("terminal reason pattern"))
}

/// A status the error names; a usage limit is a 429, so any other status is not one.
fn named_status() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)\b(?:HTTP|status(?: code)?)\s*(\d{3})\b").expect("status pattern")
    })
}

fn limit_tail_ok(rest: &str) -> bool {
    if rest.is_empty() {
        return true;
    }
    let after_space = rest.trim_start();
    if after_space
        .chars()
        .next()
        .is_some_and(|c| matches!(c, '\u{00b7}' | '\u{2219}' | '.' | ',' | '{' | '('))
    {
        return true;
    }
    resets_word().is_match(rest)
}

/// The anchored usage-limit match and its window word, mirroring `CLAUDE_USAGE_LIMIT.exec`.
fn usage_limit_shape(text: &str) -> Option<Option<String>> {
    let captures = claude_usage_limit().captures(text)?;
    if captures.get(2).is_some() {
        return Some(None);
    }
    let whole = captures.get(0)?;
    if !limit_tail_ok(&text[whole.end()..]) {
        return None;
    }
    Some(captures.get(1).map(|word| word.as_str().to_owned()))
}

/// The longest a block may last for each window word: a reset read past it is a misread.
fn named_window_ms(word: Option<&str>) -> Option<i64> {
    let lower = word?.to_lowercase();
    if let Some(hours) = lower.strip_suffix("-hour")
        && !hours.is_empty()
        && hours.chars().all(|c| c.is_ascii_digit())
    {
        return hours.parse::<i64>().ok().map(|hours| hours * HOUR_MS);
    }
    match lower.as_str() {
        "session" => Some(5 * HOUR_MS),
        "daily" => Some(24 * HOUR_MS),
        "weekly" => Some(7 * 24 * HOUR_MS),
        "monthly" => Some(31 * 24 * HOUR_MS),
        _ => None,
    }
}

struct SpentWindow {
    reset_ms: i64,
    window_ms: Option<i64>,
}

/// The latest-resetting fully used window the SDK last reported for this account.
fn spent_window(
    store: &SubscriptionLimitStore,
    account_name: &str,
    now: i64,
) -> Option<SpentWindow> {
    let limits = store
        .snapshot()
        .into_iter()
        .find(|entry| entry.provider == PROVIDER && entry.account == account_name)?;
    let mut spent: Option<SpentWindow> = None;
    for window in &limits.windows {
        let Some(resets_at) = window.resets_at.as_deref() else {
            continue;
        };
        if window.used_percent < 100.0 {
            continue;
        }
        let Ok(resets_at) = chrono::DateTime::parse_from_rfc3339(resets_at) else {
            continue;
        };
        let resets_at = resets_at.timestamp_millis();
        if resets_at <= now {
            continue;
        }
        if spent
            .as_ref()
            .is_none_or(|spent| resets_at - now > spent.reset_ms)
        {
            spent = Some(SpentWindow {
                reset_ms: resets_at - now,
                window_ms: window.window_minutes.map(|minutes| minutes as i64 * 60_000),
            });
        }
    }
    spent
}

/// Claude Code's own verdict that the account's subscription is spent: its anchored usage-limit
/// prose or its structured terminal reason, under no status other than 429 (senpi#3462, senpi#3466).
pub fn is_confirmed_account_usage_limit(text: &str) -> bool {
    let trimmed = text.trim();
    if usage_limit_shape(trimmed).is_none() && !terminal_limit_reason().is_match(trimmed) {
        return false;
    }
    match named_status().captures(trimmed).and_then(|c| c.get(1)) {
        None => true,
        Some(status) => status.as_str() == "429",
    }
}

/// How long a spent account stays blocked: until the reset its limit names, or the reset its spent
/// window reported, never past the window the limit belongs to. `None` when the error is not Claude
/// Code's account usage limit or names no reset, so the caller keeps its short default (senpi#3462).
pub fn usage_limit_block_ms(text: &str, account_name: &str, now: i64) -> Option<i64> {
    usage_limit_block_ms_in(subscription_limits(), text, account_name, now)
}

/// [`usage_limit_block_ms`] against an explicit limit store.
pub fn usage_limit_block_ms_in(
    store: &SubscriptionLimitStore,
    text: &str,
    account_name: &str,
    now: i64,
) -> Option<i64> {
    let trimmed = text.trim();
    if !is_confirmed_account_usage_limit(trimmed) {
        return None;
    }
    let shape = usage_limit_shape(trimmed).flatten();
    let window = spent_window(store, account_name, now);
    let parsed = usage_limit_reset_ms(trimmed, now).map(|ms| ms as i64);
    let reset_ms = match parsed {
        Some(parsed) if parsed > 0 => parsed,
        _ => window.as_ref()?.reset_ms,
    };
    let cap_ms = named_window_ms(shape.as_deref())
        .or_else(|| window.as_ref().and_then(|window| window.window_ms))
        .unwrap_or(UNNAMED_WINDOW_MS);
    Some(reset_ms.min(cap_ms))
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_core::subscription_limits::limit_windows::LimitWindow;

    const NOW: i64 = 1_700_000_000_000;

    fn iso(ms: i64) -> String {
        chrono::DateTime::from_timestamp_millis(ms)
            .unwrap()
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }

    #[test]
    fn confirms_claude_code_usage_limit_prose() {
        for text in [
            "You've hit your session limit \u{00b7} resets 3pm",
            "You\u{2019}ve reached your 5-hour limit",
            "You have hit your weekly limit.",
            "You've hit your limit",
            "You've hit your usage limit resets soon",
            "Claude AI usage limit reached|1700000000",
            "terminal reason blocking_limit",
            "You've hit your limit (HTTP 429)",
        ] {
            assert!(is_confirmed_account_usage_limit(text), "{text}");
        }
    }

    #[test]
    fn rejects_context_rate_warning_and_other_status() {
        for text in [
            "You've hit your context limit",
            "You're approaching your usage limit",
            "rate limit exceeded",
            "You've hit your limitless plan",
            "You've hit your limit (status 500)",
            "rapid_refill_breaker",
        ] {
            assert!(!is_confirmed_account_usage_limit(text), "{text}");
        }
    }

    #[test]
    fn named_reset_is_capped_by_the_named_window() {
        let store = SubscriptionLimitStore::new();
        assert_eq!(
            usage_limit_block_ms_in(
                &store,
                "You've hit your session limit \u{00b7} resets in 45 minutes",
                "a",
                NOW
            ),
            Some(45 * 60_000)
        );
        assert_eq!(
            usage_limit_block_ms_in(
                &store,
                "You've hit your session limit \u{00b7} resets in 30 hours",
                "a",
                NOW
            ),
            Some(5 * HOUR_MS)
        );
    }

    #[test]
    fn without_a_reset_the_spent_window_supplies_it() {
        let store = SubscriptionLimitStore::new();
        assert_eq!(
            usage_limit_block_ms_in(&store, "You've hit your limit", "a", NOW),
            None
        );
        store.record(
            PROVIDER,
            "a",
            &[
                LimitWindow {
                    id: "claude:5h".into(),
                    label: "5h".into(),
                    window_minutes: Some(300),
                    used_percent: 100.0,
                    resets_at: Some(iso(NOW + 2 * HOUR_MS)),
                },
                LimitWindow {
                    id: "claude:7d".into(),
                    label: "7d".into(),
                    window_minutes: Some(7 * 24 * 60),
                    used_percent: 50.0,
                    resets_at: Some(iso(NOW + 100 * HOUR_MS)),
                },
            ],
            chrono::DateTime::from_timestamp_millis(NOW).unwrap(),
        );
        assert_eq!(
            usage_limit_block_ms_in(&store, "You've hit your limit", "a", NOW),
            Some(2 * HOUR_MS)
        );
        assert_eq!(
            usage_limit_block_ms_in(&store, "You've hit your limit", "b", NOW),
            None
        );
        assert_eq!(
            usage_limit_block_ms_in(&store, "network error", "a", NOW),
            None
        );
    }

    #[test]
    fn window_words_map_to_caps() {
        assert_eq!(named_window_ms(Some("Weekly")), Some(7 * 24 * HOUR_MS));
        assert_eq!(named_window_ms(Some("12-hour")), Some(12 * HOUR_MS));
        assert_eq!(named_window_ms(Some("usage")), None);
        assert_eq!(named_window_ms(None), None);
    }
}
