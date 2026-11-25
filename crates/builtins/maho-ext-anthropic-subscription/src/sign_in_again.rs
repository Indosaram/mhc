//! Port of senpi `anthropic-subscription/sign-in-again.ts` (v2026.11.19-5).
use crate::accounts::{AccountSlot, SignInRequiredReason};
use crate::claude_cli_account_dir::is_valid_account_dir_id;

/// Every sign-in-required prompt starts with this, whatever the reason; `is_sign_in_again_error` keys on it.
const SIGN_IN_LEAD: &str = "Claude sign-in ";
const SIGN_IN_AGAIN_TAIL: &str = "Sign in again with Claude to keep using: ";
const REASON_ORDER: [SignInRequiredReason; 2] = [
    SignInRequiredReason::LegacySignInRetired,
    SignInRequiredReason::LoginExpired,
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignInRequired {
    pub name: String,
    pub reason: SignInRequiredReason,
}

fn reason_word(reason: SignInRequiredReason) -> &'static str {
    match reason {
        SignInRequiredReason::LegacySignInRetired => "changed",
        SignInRequiredReason::LoginExpired => "expired",
    }
}

fn reason_message(reason: SignInRequiredReason, names: &[&str]) -> String {
    format!(
        "{SIGN_IN_LEAD}{}. {SIGN_IN_AGAIN_TAIL}{}. Run /login anthropic-subscription.",
        reason_word(reason),
        names.join(", ")
    )
}

/// One sentence per reason, the retired sign-in first; an expired CLI login (senpi#5350) says "expired".
pub fn sign_in_required_message(required: &[SignInRequired]) -> String {
    REASON_ORDER
        .iter()
        .filter_map(|reason| {
            let names: Vec<&str> = required
                .iter()
                .filter(|entry| entry.reason == *reason)
                .map(|entry| entry.name.as_str())
                .collect();
            (!names.is_empty()).then(|| reason_message(*reason, &names))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A turn error that is a sign-in-required prompt: no retry or other model can serve it until the user signs in.
pub fn is_sign_in_again_error(error_message: Option<&str>) -> bool {
    error_message
        .is_some_and(|message| message.contains(&format!("authentication_failed: {SIGN_IN_LEAD}")))
}

/// A slot this engine can serve: nothing to sign in again for, and (on the CLI store) an account dir id that names a
/// directory. A stored CLI slot whose `dirId` is missing or malformed must never reach the SDK (omo#12228).
pub fn is_servable_account_slot(slot: &AccountSlot) -> bool {
    !slot.is_claude_cli()
        || (slot.sign_in_required.is_none()
            && slot.dir_id.as_deref().is_some_and(is_valid_account_dir_id))
}

/// The accounts a turn cannot use until the user signs in again. A stored slot whose account dir id is unusable carries
/// no login this engine can read, so it is reported as expired.
pub fn sign_in_required_accounts(accounts: &[AccountSlot]) -> Vec<SignInRequired> {
    accounts
        .iter()
        .filter(|slot| slot.is_claude_cli())
        .filter_map(|slot| match slot.sign_in_required {
            Some(reason) => Some(SignInRequired {
                name: slot.name.clone(),
                reason,
            }),
            None => (!slot.dir_id.as_deref().is_some_and(is_valid_account_dir_id)).then(|| {
                SignInRequired {
                    name: slot.name.clone(),
                    reason: SignInRequiredReason::LoginExpired,
                }
            }),
        })
        .collect()
}

/// A turn found only accounts that must sign in again; the message is the prompt, prefixed so it classifies as auth.
#[derive(Debug)]
pub struct ClaudeSignInRequiredError {
    pub account_names: Vec<String>,
    message: String,
}

impl ClaudeSignInRequiredError {
    pub fn new(required: &[SignInRequired]) -> Self {
        Self {
            account_names: required.iter().map(|entry| entry.name.clone()).collect(),
            message: format!(
                "authentication_failed: {}",
                sign_in_required_message(required)
            ),
        }
    }
}

impl std::fmt::Display for ClaudeSignInRequiredError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ClaudeSignInRequiredError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::AccountStore;

    fn cli(name: &str, dir_id: Option<&str>, reason: Option<SignInRequiredReason>) -> AccountSlot {
        AccountSlot {
            name: name.into(),
            store: Some(AccountStore::ClaudeCli),
            dir_id: dir_id.map(str::to_owned),
            sign_in_required: reason,
            ..Default::default()
        }
    }

    #[test]
    fn unservable_cli_slots_are_named_and_the_prompt_classifies_as_auth() {
        let good = "0123456789abcdef0123456789abcdef";
        let accounts = [
            cli("ok", Some(good), None),
            cli("gone", Some(good), Some(SignInRequiredReason::LoginExpired)),
            cli("broken", Some("x"), None),
            cli("old", None, Some(SignInRequiredReason::LegacySignInRetired)),
            AccountSlot {
                name: "env".into(),
                ..Default::default()
            },
        ];
        let servable: Vec<_> = accounts
            .iter()
            .filter(|slot| is_servable_account_slot(slot))
            .map(|slot| slot.name.as_str())
            .collect();
        assert_eq!(servable, ["ok", "env"]);
        let error = ClaudeSignInRequiredError::new(&sign_in_required_accounts(&accounts));
        assert_eq!(error.account_names, ["gone", "broken", "old"]);
        assert_eq!(
            error.to_string(),
            "authentication_failed: Claude sign-in changed. Sign in again with Claude to keep using: old. Run /login anthropic-subscription. Claude sign-in expired. Sign in again with Claude to keep using: gone, broken. Run /login anthropic-subscription."
        );
        assert!(is_sign_in_again_error(Some(&error.to_string())));
        assert_eq!(
            crate::errors::classify_sdk_error(&serde_json::json!(error.to_string())).kind,
            crate::errors::SdkErrorKind::AuthError
        );
    }
}
