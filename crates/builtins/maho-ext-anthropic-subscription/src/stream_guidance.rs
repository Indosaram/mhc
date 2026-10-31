use crate::{
    affinity::AllAccountsBlockedError,
    errors::{SdkErrorKind, classify_sdk_error},
};

pub fn with_auth_guidance(error: &anyhow::Error) -> String {
    with_account_auth_guidance(error, None)
}

/// senpi `withAuthGuidance` with the attempt's account context (`errorAccountContext`): a billing error or "out of
/// extra usage" names the account the turn ran on and how to move off it.
pub fn with_account_auth_guidance(
    error: &anyhow::Error,
    account: Option<&crate::account_context::AccountContext>,
) -> String {
    let message = error.to_string();
    if let Some(blocked) = error.downcast_ref::<AllAccountsBlockedError>() {
        if let Some(account) = &blocked.unavailable_account {
            return crate::guidance::single_account_unavailable_guidance(
                account,
                blocked.soonest_unblock_at,
                blocked.block_reason,
            );
        }
        return if blocked.block_reason == Some("auth_error") {
            "All Claude accounts for anthropic-subscription are currently blocked (authentication error).\n  /login anthropic-subscription - re-authenticate to refresh the blocked account\n  /claude-account list - inspect account states".into()
        } else {
            format!(
                "{message}\n  /claude-account list - inspect account states\n  /login anthropic-subscription - add another account"
            )
        };
    }
    let kind = classify_sdk_error(&serde_json::Value::String(message.clone())).kind;
    if let Some(account) = account
        && (kind == SdkErrorKind::Billing || out_of_extra_usage(&message))
    {
        let selected = format!(
            "Claude account '{}' (source: {}).",
            account.account_label,
            account.account_source.as_str()
        );
        let action =
            if account.account_source == crate::account_context::AccountContextSource::Ambient {
                "Switch the Claude Code login, or top up at claude.ai/settings/usage.".to_owned()
            } else {
                format!(
                    "{} Or top up this account at claude.ai/settings/usage.",
                    crate::guidance::switch_account_line(account.pin_source).trim()
                )
            };
        return format!("{message}\n{selected} {action}");
    }
    let hint = match kind {
        SdkErrorKind::OrgNotAllowed => Some(
            "This organization's policy disallows subscription OAuth use here. Use an API key (ANTHROPIC_API_KEY) or an account from an allowed organization.",
        ),
        SdkErrorKind::Billing => Some(
            "The selected Claude account has a billing problem. Check the plan at claude.com or switch accounts with /claude-account pin <name>.",
        ),
        SdkErrorKind::AuthError => Some(
            "The account's OAuth token was rejected. Re-run /login anthropic-subscription to refresh it, or remove the account with /claude-account remove <name>.",
        ),
        SdkErrorKind::Entitlement => Some(
            "This model needs usage credits on the selected Claude account (it is not included in the subscription). Switch models with /model, enable usage credits at claude.com, or pick another account with /claude-account pin <name>.",
        ),
        _ => None,
    };
    match hint {
        Some(hint) => format!("{message}\n{hint}"),
        None => message,
    }
}

/// `/\bout of extra usage\b/i`.
fn out_of_extra_usage(message: &str) -> bool {
    static PATTERN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)\bout of extra usage\b").expect("extra usage pattern")
    });
    PATTERN.is_match(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account_context::{AccountContext, AccountContextSource};
    use crate::affinity::PinSource;

    fn account(source: AccountContextSource, pin_source: Option<PinSource>) -> AccountContext {
        AccountContext {
            account_label: "work".into(),
            account_source: source,
            pin_source,
        }
    }

    #[test]
    fn billing_guidance_names_the_attempted_account_and_its_action() {
        let error =
            anyhow::Error::msg("You're out of extra usage. Add more at claude.ai/settings/usage");
        let text = with_account_auth_guidance(
            &error,
            Some(&account(AccountContextSource::Login, Some(PinSource::Cli))),
        );
        assert!(
            text.contains("Claude account 'work' (source: login)."),
            "{text}"
        );
        assert!(text.contains("start a new session with another --claude-account to switch. Or top up this account at claude.ai/settings/usage."), "{text}");
        let ambient =
            with_account_auth_guidance(&error, Some(&account(AccountContextSource::Ambient, None)));
        assert!(ambient.ends_with("Claude account 'work' (source: ambient). Switch the Claude Code login, or top up at claude.ai/settings/usage."), "{ambient}");
        let stored =
            with_account_auth_guidance(&error, Some(&account(AccountContextSource::Import, None)));
        assert!(
            stored.contains(
                "/claude-account pin <name>  - switch to another account Or top up this account"
            ),
            "{stored}"
        );
    }

    #[test]
    fn without_an_account_or_a_billing_error_the_generic_guidance_stays() {
        let error = anyhow::Error::msg("You're out of extra usage.");
        assert_eq!(
            with_account_auth_guidance(&error, None),
            "You're out of extra usage."
        );
        let other = anyhow::Error::msg("connection reset");
        assert_eq!(
            with_account_auth_guidance(&other, Some(&account(AccountContextSource::Login, None))),
            "connection reset"
        );
    }
}
