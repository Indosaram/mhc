use chrono::FixedOffset;
use maho_ext_api::{AccountLimits, LimitWindow};
use maho_ext_builtin_loose::account_list_view::{
    AccountBlock, AccountBlockReason, AccountListEntry, describe_block_status_in_tz,
    format_when_in_tz, render_account_list_in_tz,
};

fn utc() -> FixedOffset {
    FixedOffset::east_opt(0).expect("UTC offset")
}

#[test]
fn formats_relative_and_clock_times_and_block_reasons() {
    let now = 1_700_000_000_000_i64;
    let formatted = format_when_in_tz(now + 3 * 3_600_000, now, &utc());
    assert!(formatted.starts_with("in 3 h ("));
    assert_eq!(
        describe_block_status_in_tz(
            &AccountBlock {
                blocked: false,
                blocked_reason: None,
                blocked_until: None,
            },
            now,
            &utc()
        ),
        "available"
    );
    assert_eq!(
        describe_block_status_in_tz(
            &AccountBlock {
                blocked: true,
                blocked_reason: Some(AccountBlockReason::Disabled),
                blocked_until: None,
            },
            now,
            &utc()
        ),
        "account disabled; needs attention"
    );
    assert!(
        describe_block_status_in_tz(
            &AccountBlock {
                blocked: true,
                blocked_reason: Some(AccountBlockReason::RateLimit),
                blocked_until: Some(now + 30 * 60_000),
            },
            now,
            &utc()
        )
        .starts_with("rate limited, retry in 30 min")
    );
}

#[test]
fn renders_empty_and_populated_account_lists() {
    let now = 1_700_000_000_000_i64;
    assert_eq!(
        render_account_list_in_tz("Accounts", &[], &["Use /login"], now, &utc()),
        "Accounts: none\nUse /login"
    );
    let rendered = render_account_list_in_tz(
        "Accounts",
        &[AccountListEntry {
            label: "primary".into(),
            tags: vec!["pinned".into()],
            status: vec!["available".into()],
            limits: Some(AccountLimits {
                account: "primary".into(),
                provider: "anthropic-subscription".into(),
                reported_at: "2023-11-14T22:13:20Z".into(),
                plan: None,
                error: None,
                windows: vec![LimitWindow {
                    id: "5h".into(),
                    label: "5h".into(),
                    window_minutes: Some(300),
                    used_percent: 42.0,
                    resets_at: None,
                }],
            }),
        }],
        &["Footer"],
        now,
        &utc(),
    );
    assert!(rendered.contains("Accounts (1)"));
    assert!(rendered.contains("primary  [pinned]"));
    assert!(rendered.contains("status   available"));
    assert!(rendered.contains("5h        42% used"));
    assert!(rendered.ends_with("\n\nFooter"));
}
