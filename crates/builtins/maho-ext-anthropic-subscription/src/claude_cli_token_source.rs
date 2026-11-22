//! Port of senpi `anthropic-subscription/claude-cli-token-source.ts` (v2026.11.19-5).
use crate::accounts::ClaudeCliAccountProfile;
use serde_json::Value;

/// A `claude-cli` turn whose CLI authenticated from an inherited credential instead of the account's own sign-in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaudeCliTokenSourceLeak {
    TokenSource,
    ApiKeySource,
    ApiProvider,
    Account,
}

impl std::fmt::Display for ClaudeCliTokenSourceLeak {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let reported = match self {
            Self::Account => "no signed-in account",
            Self::ApiProvider => "an API provider other than first-party claude.ai",
            Self::TokenSource => "tokenSource from an inherited environment credential",
            Self::ApiKeySource => "apiKeySource from an inherited environment credential",
        };
        write!(
            formatter,
            "ClaudeCliTokenSourceLeak: the Claude CLI reported {reported} instead of this account's own sign-in; the turn was refused before it was sent"
        )
    }
}

impl std::error::Error for ClaudeCliTokenSourceLeak {}

fn text(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn is_env_variable_name(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes.next().is_some_and(|first| first.is_ascii_uppercase())
        && bytes.all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

/// Checks the account the CLI initialized with (the `initialize` control response's `account`). A `tokenSource` that
/// names an environment variable (`CLAUDE_CODE_OAUTH_TOKEN`, ...) or any `apiKeySource` other than `none` means the
/// turn would bill something other than the account's own claude.ai login, so it fails. Like `claude auth status`,
/// only a first-party account counts: no account, or any `apiProvider` other than `firstParty`, fails too. Returns
/// the reported profile (`checked_at` unset).
pub fn verify_claude_cli_initialization(
    initialization: &Value,
) -> Result<ClaudeCliAccountProfile, ClaudeCliTokenSourceLeak> {
    let Some(account) = initialization
        .get("account")
        .filter(|account| account.is_object())
    else {
        return Err(ClaudeCliTokenSourceLeak::Account);
    };
    if account.get("apiProvider").and_then(Value::as_str) != Some("firstParty") {
        return Err(ClaudeCliTokenSourceLeak::ApiProvider);
    }
    if text(account.get("tokenSource")).is_some_and(|source| is_env_variable_name(&source)) {
        return Err(ClaudeCliTokenSourceLeak::TokenSource);
    }
    if text(account.get("apiKeySource")).is_some_and(|source| source != "none") {
        return Err(ClaudeCliTokenSourceLeak::ApiKeySource);
    }
    Ok(ClaudeCliAccountProfile {
        email: text(account.get("email")),
        org_name: text(account.get("organization")),
        subscription_type: text(account.get("subscriptionType")),
        checked_at: None,
    })
}

pub fn profile_changed(
    current: Option<&ClaudeCliAccountProfile>,
    reported: &ClaudeCliAccountProfile,
) -> bool {
    current.is_none_or(|current| {
        current.email != reported.email
            || current.org_name != reported.org_name
            || current.subscription_type != reported.subscription_type
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn refuses_inherited_credentials_and_non_first_party_accounts() {
        let account = |fields: Value| json!({ "account": fields });
        assert_eq!(
            verify_claude_cli_initialization(&json!({})),
            Err(ClaudeCliTokenSourceLeak::Account)
        );
        assert_eq!(
            verify_claude_cli_initialization(&account(json!({"apiProvider":"bedrock"}))),
            Err(ClaudeCliTokenSourceLeak::ApiProvider)
        );
        assert_eq!(
            verify_claude_cli_initialization(&account(
                json!({"apiProvider":"firstParty","tokenSource":"CLAUDE_CODE_OAUTH_TOKEN"})
            )),
            Err(ClaudeCliTokenSourceLeak::TokenSource)
        );
        assert_eq!(
            verify_claude_cli_initialization(&account(
                json!({"apiProvider":"firstParty","apiKeySource":"ANTHROPIC_API_KEY"})
            )),
            Err(ClaudeCliTokenSourceLeak::ApiKeySource)
        );
        let profile = verify_claude_cli_initialization(&account(json!({
            "apiProvider":"firstParty","tokenSource":"claude.ai","apiKeySource":"none",
            "email":"a@example.com","organization":"Org","subscriptionType":"max"
        })))
        .expect("own sign-in");
        assert_eq!(profile.org_name.as_deref(), Some("Org"));
        assert!(!profile_changed(Some(&profile), &profile));
        assert!(profile_changed(None, &profile));
    }
}
