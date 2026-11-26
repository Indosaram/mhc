//! Port of senpi `anthropic-subscription/claude-cli-auth-status.ts` (v2026.11.19-5).
use crate::claude_cli_process::{
    ClaudeCliRun, DEFAULT_SHORT_COMMAND_TIMEOUT_MS, run_claude_cli_to_exit,
};
use maho_ai::utils::abort::AbortSignal;
use serde_json::Value;

/// The local-only profile a CLI-store account shows; never logged or sent to telemetry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClaudeCliProfile {
    pub email: Option<String>,
    pub org_name: Option<String>,
    pub subscription_type: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaudeCliRejection {
    AuthMethod,
    ApiProvider,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClaudeCliAuthStatus {
    SignedIn(ClaudeCliProfile),
    SignedOut,
    /// Logged in, but not as a first-party claude.ai subscription (an API key, Bedrock, Vertex...).
    Rejected(ClaudeCliRejection),
    Unreadable,
}

fn string_field(record: &serde_json::Map<String, Value>, name: &str) -> Option<String> {
    record
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// Reads `claude auth status --json` (2.1.306: `{loggedIn, authMethod, apiProvider, email, orgName,
/// subscriptionType, ...}`, exit 0 when logged in, 1 when not). Only a first-party claude.ai login with exit 0 is
/// signed in; an exit code and a `loggedIn` that disagree are unreadable, never trusted.
pub fn parse_claude_cli_auth_status(stdout: &str, exit_code: Option<i32>) -> ClaudeCliAuthStatus {
    let Ok(Value::Object(parsed)) = serde_json::from_str::<Value>(stdout) else {
        return ClaudeCliAuthStatus::Unreadable;
    };
    let Some(logged_in) = parsed.get("loggedIn").and_then(Value::as_bool) else {
        return ClaudeCliAuthStatus::Unreadable;
    };
    if !logged_in {
        return if exit_code == Some(0) {
            ClaudeCliAuthStatus::Unreadable
        } else {
            ClaudeCliAuthStatus::SignedOut
        };
    }
    if exit_code != Some(0) {
        return ClaudeCliAuthStatus::Unreadable;
    }
    if parsed.get("authMethod").and_then(Value::as_str) != Some("claude.ai") {
        return ClaudeCliAuthStatus::Rejected(ClaudeCliRejection::AuthMethod);
    }
    if parsed.get("apiProvider").and_then(Value::as_str) != Some("firstParty") {
        return ClaudeCliAuthStatus::Rejected(ClaudeCliRejection::ApiProvider);
    }
    ClaudeCliAuthStatus::SignedIn(ClaudeCliProfile {
        email: string_field(&parsed, "email"),
        org_name: string_field(&parsed, "orgName"),
        subscription_type: string_field(&parsed, "subscriptionType"),
    })
}

pub async fn read_claude_cli_auth_status(
    run: &ClaudeCliRun<'_>,
    timeout_ms: u64,
) -> ClaudeCliAuthStatus {
    match run_claude_cli_to_exit(run, &["auth", "status", "--json"], timeout_ms, None).await {
        Some(exit) => parse_claude_cli_auth_status(&exit.stdout, exit.exit_code),
        None => ClaudeCliAuthStatus::Unreadable,
    }
}

/// Deletes the CLI credential-store entry keyed by this account's config dir.
pub async fn logout_claude_cli(
    run: &ClaudeCliRun<'_>,
    timeout_ms: Option<u64>,
    signal: Option<&AbortSignal>,
) -> bool {
    run_claude_cli_to_exit(
        run,
        &["auth", "logout"],
        timeout_ms.unwrap_or(DEFAULT_SHORT_COMMAND_TIMEOUT_MS),
        signal,
    )
    .await
    .is_some_and(|exit| exit.exit_code == Some(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_first_party_claude_ai_login_with_exit_zero_is_signed_in() {
        let signed_in = r#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","email":"a@example.com","orgName":"","subscriptionType":"max"}"#;
        assert_eq!(
            parse_claude_cli_auth_status(signed_in, Some(0)),
            ClaudeCliAuthStatus::SignedIn(ClaudeCliProfile {
                email: Some("a@example.com".into()),
                org_name: None,
                subscription_type: Some("max".into()),
            })
        );
        assert_eq!(
            parse_claude_cli_auth_status(signed_in, Some(1)),
            ClaudeCliAuthStatus::Unreadable
        );
        assert_eq!(
            parse_claude_cli_auth_status(r#"{"loggedIn":false}"#, Some(1)),
            ClaudeCliAuthStatus::SignedOut
        );
        assert_eq!(
            parse_claude_cli_auth_status(r#"{"loggedIn":false}"#, Some(0)),
            ClaudeCliAuthStatus::Unreadable
        );
        assert_eq!(
            parse_claude_cli_auth_status(
                r#"{"loggedIn":true,"authMethod":"api_key","apiProvider":"firstParty"}"#,
                Some(0)
            ),
            ClaudeCliAuthStatus::Rejected(ClaudeCliRejection::AuthMethod)
        );
        assert_eq!(
            parse_claude_cli_auth_status(
                r#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"bedrock"}"#,
                Some(0)
            ),
            ClaudeCliAuthStatus::Rejected(ClaudeCliRejection::ApiProvider)
        );
        assert_eq!(
            parse_claude_cli_auth_status("not json", Some(0)),
            ClaudeCliAuthStatus::Unreadable
        );
    }
}
