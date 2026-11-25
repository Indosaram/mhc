//! The `/claude-account` list: account state, next-turn selection and cached limit windows.

use crate::{
    account_management::timed_block,
    accounts::{AccountSlot, AccountSource, SignInRequiredReason},
    affinity::PinSource,
    provider_id::ANTHROPIC_SUBSCRIPTION_PROVIDER_ID,
};
use chrono::{DateTime, Local, TimeZone};
use maho_ai::auth::pool::slots::account_label;
use maho_core::subscription_limits::limit_store::{AccountLimits, subscription_limits};

pub struct ClaudeAccountListInput<'a> {
    pub accounts: &'a [AccountSlot],
    pub pinned: Option<&'a str>,
    pub pin_source: PinSource,
    pub affinity_pick: Option<&'a str>,
    pub affinity_error: Option<&'a str>,
    pub now: f64,
}

const TITLE: &str = "Anthropic Subscription accounts";
const MANAGE_HINT: &str =
    "Manage: /claude-account add | pin <id> | unpin | unblock <id> | remove <id>";

fn format_when<Tz: TimeZone>(at: f64, now: f64, zone: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let minutes = ((at - now) / 60_000.0).ceil();
    let hours = (minutes / 60.0).floor();
    let days = (hours / 24.0).floor();
    let relative = if minutes < 1.0 {
        "now".to_owned()
    } else if hours < 1.0 {
        format!("in {minutes} min")
    } else if days < 1.0 {
        match minutes % 60.0 {
            0.0 => format!("in {hours} h"),
            rest => format!("in {hours} h {rest} min"),
        }
    } else {
        match hours % 24.0 {
            0.0 => format!("in {days} d"),
            rest => format!("in {days} d {rest} h"),
        }
    };
    // The slot and limit store carry epoch milliseconds; Chrono takes whole milliseconds.
    let (Some(at_clock), Some(now_clock)) = (
        zone.timestamp_millis_opt(at as i64).single(),
        zone.timestamp_millis_opt(now as i64).single(),
    ) else {
        return relative;
    };
    let clock = if at_clock.date_naive() == now_clock.date_naive() {
        "%-I:%M %p"
    } else if at - now < 6.0 * 86_400_000.0 {
        "%a %-I:%M %p"
    } else {
        "%b %-d, %-I:%M %p"
    };
    format!("{relative} ({})", at_clock.format(clock))
}

fn reading_age(now: f64, reported_at: &str) -> String {
    let Ok(at) = DateTime::parse_from_rfc3339(reported_at) else {
        return "unknown".into();
    };
    let minutes = ((now - at.timestamp_millis() as f64) / 60_000.0).floor();
    let hours = (minutes / 60.0).floor();
    match (minutes, hours) {
        (minutes, _) if minutes < 1.0 => "just now".into(),
        (_, hours) if hours < 1.0 => format!("{minutes} min ago"),
        (_, hours) if hours < 48.0 => format!("{hours} h ago"),
        _ => format!("{} d ago", (hours / 24.0).floor()),
    }
}

fn slot_status(slot: &AccountSlot, now: f64, when: &impl Fn(f64) -> String) -> Vec<String> {
    let (blocked, reason, until) =
        timed_block(slot.blocked_until, slot.block_reason.as_deref(), now);
    let account = if slot.is_claude_cli()
        && slot.sign_in_required == Some(SignInRequiredReason::LoginExpired)
    {
        Some("sign-in expired; run /login anthropic-subscription".into())
    } else if blocked {
        Some(match (reason, until.map(when)) {
            (Some("disabled"), _) => "account disabled; needs attention".into(),
            (Some("auth_error"), Some(retry)) => format!("sign-in rejected, next check {retry}"),
            (Some("auth_error"), None) => "sign-in rejected; sign in again".into(),
            (Some("rate_limit"), Some(retry)) => format!("rate limited, retry {retry}"),
            (Some("rate_limit"), None) => "rate limited".into(),
            (_, Some(retry)) => format!("blocked, retry {retry}"),
            (_, None) => "blocked; reason and retry time unavailable".into(),
        })
    } else {
        None
    };
    let states: Vec<String> = account
        .into_iter()
        .chain(
            slot.model_blocks
                .iter()
                .flatten()
                .filter(|(_, block)| block.blocked_until > now)
                .map(|(name, block)| {
                    format!("{name} blocked, retry {}", when(block.blocked_until))
                }),
        )
        .collect();
    if states.is_empty() {
        vec!["available".into()]
    } else {
        states
    }
}

fn limit_rows<'a>(
    limits: Option<&'a AccountLimits>,
    now: f64,
    when: &impl Fn(f64) -> String,
) -> Vec<(&'a str, String)> {
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
    let mut rows: Vec<_> = limits
        .windows
        .iter()
        .map(|window| {
            let reset = window
                .resets_at
                .as_deref()
                .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
                .map(|at| at.timestamp_millis() as f64);
            let value = if reset.is_some_and(|at| at <= now) {
                "reset since this reading".into()
            } else {
                let used = format!("{:.0}%", window.used_percent.round());
                reset.map_or_else(
                    || format!("{used:>4} used"),
                    |at| format!("{used:>4} used, resets {}", when(at)),
                )
            };
            (window.label.as_str(), value)
        })
        .collect();
    let age = reading_age(now, &limits.reported_at);
    let updated = limits.error.as_ref().map_or_else(
        || age.clone(),
        |error| format!("{age}; last check failed: {error}"),
    );
    rows.push(("updated", updated));
    rows
}

/// Renders one readable block per account without polling or touching OAuth credentials.
pub fn format_claude_account_list(input: ClaudeAccountListInput<'_>) -> String {
    let limits = subscription_limits().snapshot();
    let now = input.now;
    render(input, &limits, |at| format_when(at, now, &Local))
}

fn render(
    input: ClaudeAccountListInput<'_>,
    limits: &[AccountLimits],
    when: impl Fn(f64) -> String,
) -> String {
    let ClaudeAccountListInput {
        accounts,
        pinned,
        pin_source,
        affinity_pick,
        affinity_error,
        now,
    } = input;
    let (pin_label, pin_tag) = match pin_source {
        PinSource::Cli => ("cli", "pinned for this session"),
        PinSource::Settings => ("settings", "pinned in settings"),
        PinSource::Stored => ("stored", "pinned"),
    };
    let missing_pin = pinned
        .filter(|pin| !accounts.iter().any(|a| a.name == *pin))
        .map(|pin| format!("Pinned account: {pin} ({pin_label}) - not in this list"));
    if accounts.is_empty() {
        let mut lines = vec![format!("{TITLE}: none")];
        lines.extend(missing_pin);
        lines.push("Add one with /claude-account add.".into());
        return lines.join("\n");
    }

    let entries: Vec<_> = accounts
        .iter()
        .map(|account| {
            let mut label = account_label(&account.name, account.display_name.as_deref());
            let tags: Vec<_> = [
                (pinned == Some(account.name.as_str())).then_some(pin_tag),
                (affinity_pick == Some(account.name.as_str())).then_some("next turn"),
                (account.source == AccountSource::Env).then_some("from environment"),
            ]
            .into_iter()
            .flatten()
            .collect();
            if !tags.is_empty() {
                label.push_str(&format!("  [{}]", tags.join(", ")));
            }
            let limits = limits.iter().find(|limit| {
                limit.provider == ANTHROPIC_SUBSCRIPTION_PROVIDER_ID
                    && limit.account == account.name
            });
            let rows = slot_status(account, now, &when)
                .into_iter()
                .enumerate()
                .map(|(i, status)| (if i == 0 { "status" } else { "" }, status))
                .chain(limit_rows(limits, now, &when))
                .collect::<Vec<_>>();
            (label, rows)
        })
        .collect();
    let width = entries
        .iter()
        .flat_map(|(_, rows)| rows)
        .map(|(key, _)| key.chars().count())
        .max()
        .unwrap_or(0);
    let mut lines = vec![format!("{TITLE} ({})", accounts.len())];
    for (label, rows) in entries {
        lines.extend([String::new(), label]);
        lines.extend(
            rows.into_iter()
                .map(|(key, value)| format!("  {key:width$}  {value}")),
        );
    }
    lines.push(String::new());
    lines.extend(missing_pin);
    if let Some(error) = affinity_error {
        lines.push(format!("Next turn: unavailable - {error}"));
    }
    lines.push(MANAGE_HINT.into());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::AccountStore;
    use maho_core::subscription_limits::limit_windows::LimitWindow;

    #[test]
    fn renders_aligned_status_and_limit_rows_with_relative_retry_times() {
        let now = DateTime::parse_from_rfc3339("2026-11-09T12:00:00Z")
            .expect("fixture clock")
            .timestamp_millis() as f64;
        let hour = 3_600_000.0;
        let at = |hours: f64| {
            DateTime::from_timestamp_millis((now + hours * hour) as i64)
                .expect("fixture clock")
                .to_rfc3339()
        };
        let window = |label: &str, used_percent, reset_hours| LimitWindow {
            id: label.into(),
            label: label.into(),
            used_percent,
            resets_at: Some(at(reset_hours)),
            window_minutes: None,
        };
        let accounts = [
            AccountSlot {
                name: "alpha".into(),
                display_name: Some("Personal".into()),
                ..Default::default()
            },
            AccountSlot {
                name: "bravo".into(),
                blocked_until: Some(now + 3.0 * hour),
                block_reason: Some("rate_limit".into()),
                ..Default::default()
            },
            AccountSlot {
                name: "charlie".into(),
                model_blocks: Some(
                    [
                        (
                            "fable".into(),
                            crate::model_blocks::ModelBlock {
                                blocked_until: now + 26.0 * hour,
                            },
                        ),
                        (
                            "opus".into(),
                            crate::model_blocks::ModelBlock {
                                blocked_until: now - hour,
                            },
                        ),
                    ]
                    .into(),
                ),
                ..Default::default()
            },
        ];
        let limits = [AccountLimits {
            provider: ANTHROPIC_SUBSCRIPTION_PROVIDER_ID.into(),
            account: "alpha".into(),
            windows: vec![
                window("5h", 72.4, 2.0),
                window("7d", 31.0, 72.0),
                window("opus", 77.0, -1.0),
            ],
            reported_at: at(0.0),
            plan: None,
            error: None,
        }];
        let output = render(
            ClaudeAccountListInput {
                accounts: &accounts,
                pinned: Some("alpha"),
                pin_source: PinSource::Settings,
                affinity_pick: Some("alpha"),
                affinity_error: None,
                now,
            },
            &limits,
            |at| format_when(at, now, &chrono::Utc),
        );
        assert!(output.contains(
            "\nPersonal (alpha)  [pinned in settings, next turn]\n  status   available\n"
        ));
        assert!(output.contains("\n  5h        72% used, resets in 2 h (2:00 PM)\n"));
        assert!(output.contains("\n  7d        31% used, resets in 3 d (Thu 12:00 PM)\n"));
        assert!(output.contains("\n  opus     reset since this reading\n  updated  just now\n"));
        assert!(output.contains("\nbravo\n  status   rate limited, retry in 3 h (3:00 PM)\n"));
        assert!(
            output
                .contains("\ncharlie\n  status   fable blocked, retry in 1 d 2 h (Tue 2:00 PM)\n")
        );
        assert!(!output.contains("opus blocked") && !output.contains("T00:"));
    }

    #[test]
    fn expired_cli_login_prompts_sign_in_instead_of_waiting_for_reprobe() {
        let slot = AccountSlot {
            name: "work".into(),
            store: Some(AccountStore::ClaudeCli),
            sign_in_required: Some(SignInRequiredReason::LoginExpired),
            blocked_until: Some(60_000.0),
            block_reason: Some("auth_error".into()),
            ..Default::default()
        };
        assert_eq!(
            slot_status(&slot, 0.0, &|_| panic!("must not format reprobe time")),
            ["sign-in expired; run /login anthropic-subscription"]
        );
    }
}
