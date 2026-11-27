//! One account block of a TUI account list (`senpi#6288`): a header line with the label and
//! short tags, then aligned `label  value` rows for status and each limit window.

use chrono::{DateTime, Local, TimeZone};
use maho_ext_api::AccountLimits;

pub struct AccountListEntry {
    pub label: String,
    pub tags: Vec<String>,
    /// One phrase per reason the account sits out, or `available`.
    pub status: Vec<String>,
    pub limits: Option<AccountLimits>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountBlockReason {
    Disabled,
    AuthError,
    RateLimit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccountBlock {
    pub blocked: bool,
    pub blocked_reason: Option<AccountBlockReason>,
    pub blocked_until: Option<i64>,
}

const MINUTE_MS: i64 = 60_000;
const DAY_MS: i64 = 24 * 60 * MINUTE_MS;

fn relative(ms: i64) -> String {
    let minutes = (ms as f64 / MINUTE_MS as f64).ceil() as i64;
    if minutes < 1 {
        return "now".into();
    }
    if minutes < 60 {
        return format!("in {minutes} min");
    }
    let hours = minutes / 60;
    if hours < 24 {
        let rem = minutes % 60;
        return if rem == 0 {
            format!("in {hours} h")
        } else {
            format!("in {hours} h {rem} min")
        };
    }
    let days = hours / 24;
    let rem = hours % 24;
    if rem == 0 {
        format!("in {days} d")
    } else {
        format!("in {days} d {rem} h")
    }
}

fn clock_in_tz<Tz: TimeZone>(epoch_ms: i64, now: i64, zone: &Tz) -> Option<String>
where
    Tz::Offset: std::fmt::Display,
{
    let at = zone.timestamp_millis_opt(epoch_ms).single()?;
    let now_dt = zone.timestamp_millis_opt(now).single()?;
    let formatted = if at.date_naive() == now_dt.date_naive() {
        at.format("%-I:%M %p").to_string()
    } else if epoch_ms - now < 6 * DAY_MS {
        at.format("%a %-I:%M %p").to_string()
    } else {
        at.format("%b %-d, %-I:%M %p").to_string()
    };
    Some(formatted.replace('\u{202f}', " "))
}

/// `"in 3 h (10:44 PM)"`: how long first, then the local clock time without year or time zone.
pub fn format_when(epoch_ms: i64, now: i64) -> String {
    format_when_in_tz(epoch_ms, now, &Local)
}

pub fn format_when_in_tz<Tz: TimeZone>(epoch_ms: i64, now: i64, zone: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let rel = relative(epoch_ms - now);
    match clock_in_tz(epoch_ms, now, zone) {
        Some(clock) => format!("{rel} ({clock})"),
        None => rel,
    }
}

/// An account-level block in list wording; the reasons match `describeAccountBlock` (`senpi#4092`).
pub fn describe_block_status(block: &AccountBlock, now: i64) -> String {
    describe_block_status_in_tz(block, now, &Local)
}

pub fn describe_block_status_in_tz<Tz: TimeZone>(
    block: &AccountBlock,
    now: i64,
    zone: &Tz,
) -> String
where
    Tz::Offset: std::fmt::Display,
{
    if !block.blocked {
        return "available".into();
    }
    let when = block
        .blocked_until
        .map(|until| format_when_in_tz(until, now, zone));
    match block.blocked_reason {
        Some(AccountBlockReason::Disabled) => "account disabled; needs attention".into(),
        Some(AccountBlockReason::AuthError) => match when {
            None => "sign-in rejected; sign in again".into(),
            Some(when) => format!("sign-in rejected, next check {when}"),
        },
        Some(AccountBlockReason::RateLimit) => match when {
            None => "rate limited".into(),
            Some(when) => format!("rate limited, retry {when}"),
        },
        None => match when {
            None => "blocked; reason and retry time unavailable".into(),
            Some(when) => format!("blocked, retry {when}"),
        },
    }
}

fn reading_age(now: i64, reported_at: &str) -> String {
    let Ok(at) = DateTime::parse_from_rfc3339(reported_at) else {
        return "unknown".into();
    };
    let minutes = ((now - at.timestamp_millis()) as f64 / MINUTE_MS as f64).floor() as i64;
    if minutes < 1 {
        return "just now".into();
    }
    let hours = minutes / 60;
    if hours < 1 {
        return format!("{minutes} min ago");
    }
    if hours < 48 {
        return format!("{hours} h ago");
    }
    format!("{} d ago", hours / 24)
}

fn limit_rows<'a, Tz: TimeZone>(
    limits: Option<&'a AccountLimits>,
    now: i64,
    zone: &Tz,
) -> Vec<(&'a str, String)>
where
    Tz::Offset: std::fmt::Display,
{
    let Some(limits) = limits else {
        return vec![("limits", "not reported yet".into())];
    };
    if limits.windows.is_empty() {
        return vec![(
            "limits",
            limits.error.as_deref().map_or_else(
                || "not reported yet".into(),
                |error| format!("last check failed: {error}"),
            ),
        )];
    }
    let mut rows: Vec<(&'a str, String)> = limits
        .windows
        .iter()
        .map(|window| {
            let resets_at = window
                .resets_at
                .as_deref()
                .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
                .map(|at| at.timestamp_millis());
            if let Some(resets_at) = resets_at
                && resets_at <= now
            {
                return (window.label.as_str(), "reset since this reading".into());
            }
            let used = format!("{:>4}", format!("{}%", window.used_percent.round() as i64));
            let value = match resets_at {
                None => format!("{used} used"),
                Some(resets_at) => {
                    format!(
                        "{used} used, resets {}",
                        format_when_in_tz(resets_at, now, zone)
                    )
                }
            };
            (window.label.as_str(), value)
        })
        .collect();
    let failed = limits
        .error
        .as_deref()
        .map_or_else(String::new, |error| format!("; last check failed: {error}"));
    rows.push((
        "updated",
        format!("{}{failed}", reading_age(now, &limits.reported_at)),
    ));
    rows
}

/// The whole list: a counted title, one block per account, then footer lines after a blank line.
pub fn render_account_list(
    title: &str,
    entries: &[AccountListEntry],
    footer: &[&str],
    now: i64,
) -> String {
    render_account_list_in_tz(title, entries, footer, now, &Local)
}

pub fn render_account_list_in_tz<Tz: TimeZone>(
    title: &str,
    entries: &[AccountListEntry],
    footer: &[&str],
    now: i64,
    zone: &Tz,
) -> String
where
    Tz::Offset: std::fmt::Display,
{
    if entries.is_empty() {
        let mut lines = vec![format!("{title}: none")];
        lines.extend(footer.iter().map(|line| (*line).to_owned()));
        return lines.join("\n");
    }
    let blocks: Vec<(&AccountListEntry, Vec<(&str, String)>)> = entries
        .iter()
        .map(|entry| {
            let mut rows: Vec<(&str, String)> = entry
                .status
                .iter()
                .enumerate()
                .map(|(index, status)| (if index == 0 { "status" } else { "" }, status.clone()))
                .collect();
            rows.extend(limit_rows(entry.limits.as_ref(), now, zone));
            (entry, rows)
        })
        .collect();
    let width = blocks
        .iter()
        .flat_map(|(_, rows)| rows.iter().map(|(label, _)| label.len()))
        .max()
        .unwrap_or(0);
    let mut lines = vec![format!("{title} ({})", entries.len())];
    for (entry, rows) in blocks {
        lines.push(String::new());
        if entry.tags.is_empty() {
            lines.push(entry.label.clone());
        } else {
            lines.push(format!("{}  [{}]", entry.label, entry.tags.join(", ")));
        }
        for (label, value) in rows {
            lines.push(format!("  {label:<width$}  {value}"));
        }
    }
    if !footer.is_empty() {
        lines.push(String::new());
        lines.extend(footer.iter().map(|line| (*line).to_owned()));
    }
    lines.join("\n")
}
