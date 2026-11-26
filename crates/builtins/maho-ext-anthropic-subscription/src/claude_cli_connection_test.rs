//! One isolated SDK turn, used after sign-in and by an explicit account re-test.
use crate::{
    accounts::{AccountSlot, ClaudeCliAccountProfile},
    auth_lane::{PROVIDER_ID, prepare_claude_cli_probe},
    claude_cli_token_source::verify_claude_cli_initialization,
    errors::{SdkErrorKind, classify_sdk_error, sdk_assistant_failure, sdk_result_failure},
    refusal::refusal_error,
    sdk_boundary::SdkQueryHandle,
};
use maho_ai::utils::abort::AbortSignal;
use serde::{Deserialize, Serialize};
use serde_json::{Number, Value, json};
use std::{collections::BTreeMap, future::Future, path::Path, time::Duration};

const CONNECTION_TEST_TIMEOUT_MS: u64 = 60_000;
const STORED_CHECK_MESSAGE_MAX_CHARS: usize = 2_000;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ClaudeCliAccountCheck {
    pub model: String,
    pub outcome: ClaudeCliCheckOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<Number>,
    pub at: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeCliCheckOutcome {
    Ok,
    Failed,
}

pub fn connection_test_model<'a>(provider: Option<&str>, model: Option<&'a str>) -> &'a str {
    match (provider, model) {
        (Some(PROVIDER_ID), Some(model)) if !model.is_empty() => model,
        _ => "claude-opus-5-5", // senpi defaultModelPerProvider at v2026.11.19-5
    }
}

pub struct ClaudeCliConnectionTestInput<'a> {
    pub account: &'a AccountSlot,
    pub model: &'a str,
    pub executable: &'a Path,
    pub cwd: &'a Path,
    pub environment: &'a BTreeMap<String, String>,
    pub agent_dir: &'a Path,
    pub signal: Option<&'a AbortSignal>,
    pub timeout_ms: Option<u64>,
    pub now: &'a (dyn Fn() -> u64 + Sync),
}

pub(crate) struct ConnectionTestResult {
    pub check: ClaudeCliAccountCheck,
    pub profile: Option<ClaudeCliAccountProfile>,
}

struct CheckFailure {
    reason: &'static str,
    message: String,
    status: Option<Number>,
}

impl CheckFailure {
    fn new(reason: &'static str, message: impl Into<String>) -> Self {
        Self {
            reason,
            message: message.into(),
            status: None,
        }
    }

    fn classified(message: String) -> Self {
        let reason = match classify_sdk_error(&json!(message)).kind {
            SdkErrorKind::RateLimit => "rate_limit",
            SdkErrorKind::Overloaded => "overloaded",
            SdkErrorKind::AuthError => "auth_error",
            SdkErrorKind::LoginUnreadable => "login_unreadable",
            SdkErrorKind::Billing => "billing",
            SdkErrorKind::OrgNotAllowed => "org_not_allowed",
            SdkErrorKind::Entitlement => "entitlement",
            SdkErrorKind::LongContextUnavailable => "long_context_unavailable",
            SdkErrorKind::Other => "other",
        };
        Self::new(reason, message)
    }
}

fn stored_message(message: &str) -> String {
    let mut redacted = maho_core::sensitive_output::redact_secrets(message, "[REDACTED]");
    // Match upstream's UTF-16 limit without splitting a Unicode scalar.
    if redacted.encode_utf16().count() > STORED_CHECK_MESSAGE_MAX_CHARS {
        let mut units = 0;
        let end = redacted
            .char_indices()
            .find_map(|(offset, ch)| {
                units += ch.len_utf16();
                (units >= STORED_CHECK_MESSAGE_MAX_CHARS).then_some(offset)
            })
            .unwrap_or(redacted.len());
        redacted.truncate(end);
        redacted.push('…');
    }
    redacted
}

/// The caller owns consent and sign-in orchestration. This function does not consult or write auth.json.
pub async fn run_claude_cli_connection_test(
    input: ClaudeCliConnectionTestInput<'_>,
) -> ClaudeCliAccountCheck {
    run_connection_test_with_profile(input).await.check
}

pub(crate) async fn run_connection_test_with_profile(
    input: ClaudeCliConnectionTestInput<'_>,
) -> ConnectionTestResult {
    let mut profile = None;
    let check = run_connection_test_with(&input, probe(&input, &mut profile)).await;
    ConnectionTestResult { check, profile }
}

async fn run_connection_test_with(
    input: &ClaudeCliConnectionTestInput<'_>,
    turn: impl Future<Output = anyhow::Result<Option<CheckFailure>>>,
) -> ClaudeCliAccountCheck {
    let expired = || CheckFailure::new("timeout", "Connection test timed out.");
    let cancelled = || CheckFailure::new("cancelled", "Connection test was cancelled.");
    let timeout = Duration::from_millis(input.timeout_ms.unwrap_or(CONNECTION_TEST_TIMEOUT_MS));
    let deadline = tokio::time::Instant::now() + timeout;
    let cancel = async {
        match input.signal {
            Some(signal) => signal.cancelled().await,
            None => std::future::pending::<()>().await,
        }
    };
    let failure = if timeout.is_zero() {
        Some(expired())
    } else {
        tokio::select! {
            biased;
            _ = tokio::time::sleep_until(deadline) => Some(expired()),
            _ = cancel => Some(cancelled()),
            result = turn => {
                if tokio::time::Instant::now() >= deadline {
                    Some(expired())
                } else if input.signal.is_some_and(AbortSignal::aborted) {
                    Some(cancelled())
                } else {
                    match result {
                        Ok(failure) => failure,
                        Err(error) => Some(CheckFailure::classified(error.to_string())),
                    }
                }
            }
        }
    };
    let (outcome, reason, message, status) = match failure {
        Some(failure) => (
            ClaudeCliCheckOutcome::Failed,
            Some(failure.reason.into()),
            Some(stored_message(&failure.message)),
            failure.status,
        ),
        None => (ClaudeCliCheckOutcome::Ok, None, None, None),
    };
    ClaudeCliAccountCheck {
        model: input.model.into(),
        outcome,
        reason,
        message,
        status,
        at: (input.now)(),
    }
}

fn probe_options(input: &ClaudeCliConnectionTestInput<'_>) -> Value {
    json!({
        "cwd": input.cwd.to_string_lossy(),
        "model": input.model,
        "tools": [],
        "maxTurns": 1,
        "settingSources": [],
        "permissionMode": "dontAsk",
        "mcpServers": {},
        "extraArgs": {
            "print": null,
            "no-session-persistence": null,
            "strict-mcp-config": null,
        },
    })
}

async fn probe(
    input: &ClaudeCliConnectionTestInput<'_>,
    profile: &mut Option<ClaudeCliAccountProfile>,
) -> anyhow::Result<Option<CheckFailure>> {
    crate::session_cwd::require_anthropic_subscription_cwd(Some(input.cwd))?;
    let prepared = prepare_claude_cli_probe(input.environment, input.account, input.agent_dir)?;
    let options = probe_options(input);
    // The SDK handle's Drop closes the child if the outer deadline or signal drops this future.
    let mut handle = SdkQueryHandle::spawn(input.executable, &options, &prepared.env).await?;
    let result = async {
        *profile = Some(verify_claude_cli_initialization(&handle.initialization)?);
        handle
            .send(json!({
                "type": "user",
                "message": { "role": "user", "content": "Reply with OK" },
                "parent_tool_use_id": null,
                "session_id": "",
            }))
            .await?;
        while let Some(message) = handle.next().await {
            let message = message?;
            if let Some(failure) = message_failure(&message) {
                return Ok(Some(failure));
            }
            if message["type"] == "result" {
                return Ok(None);
            }
        }
        Ok(Some(CheckFailure::new(
            "no_result",
            "Claude Code ended the connection test without a result.",
        )))
    }
    .await;
    handle.close().await?;
    result
}

fn message_failure(message: &Value) -> Option<CheckFailure> {
    let error = match refusal_error(message) {
        Some(error) => Some(error.to_string()),
        None => match message["type"].as_str() {
            Some("assistant") => sdk_assistant_failure(message),
            Some("result") => sdk_result_failure(message).map(|failure| failure.message),
            _ => None,
        },
    }?;
    let mut failure = CheckFailure::classified(error);
    if message["type"] == "result"
        && let Some(Value::Number(status)) = message.get("api_error_status")
    {
        failure.status = Some(status.clone());
    }
    Some(failure)
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ai::utils::abort::AbortController;
    use std::future::{pending, ready};

    fn input(signal: Option<&AbortSignal>) -> ClaudeCliConnectionTestInput<'_> {
        static SLOT: std::sync::LazyLock<AccountSlot> =
            std::sync::LazyLock::new(AccountSlot::default);
        static ENV: BTreeMap<String, String> = BTreeMap::new();
        ClaudeCliConnectionTestInput {
            account: &SLOT,
            model: "claude-sonnet-5-5",
            executable: Path::new("must-not-spawn"),
            cwd: Path::new("must-not-open"),
            environment: &ENV,
            agent_dir: Path::new("must-not-open"),
            signal,
            timeout_ms: None,
            now: &|| 123,
        }
    }

    #[test]
    fn model_uses_only_this_providers_nonempty_configuration() {
        assert_eq!(
            connection_test_model(Some(PROVIDER_ID), Some("claude-sonnet-5-5")),
            "claude-sonnet-5-5",
        );
        for (provider, model) in [
            (Some("anthropic"), Some("custom")),
            (Some(PROVIDER_ID), Some("")),
            (None, None),
        ] {
            assert_eq!(connection_test_model(provider, model), "claude-opus-5-5");
        }
    }

    #[test]
    fn probe_does_not_enable_tools_settings_or_transcript_persistence() {
        let options = probe_options(&input(None));
        let args = crate::sdk_boundary::arguments(&options);
        assert_eq!(options["model"], "claude-sonnet-5-5");
        assert!(args.windows(2).any(|arg| arg == ["--max-turns", "1"]));
        assert!(args.windows(2).any(|arg| arg == ["--tools", ""]));
        assert!(
            args.windows(2)
                .any(|arg| arg == ["--permission-mode", "dontAsk"])
        );
        assert!(
            args.windows(2)
                .any(|arg| arg == ["--mcp-config", r#"{"mcpServers":{}}"#])
        );
        assert!(args.iter().any(|arg| arg == "--setting-sources="));
        assert!(args.iter().any(|arg| arg == "--print"));
        assert!(args.iter().any(|arg| arg == "--no-session-persistence"));
        assert!(args.iter().any(|arg| arg == "--strict-mcp-config"));
    }

    #[tokio::test]
    async fn result_retains_status_and_redacts_and_bounds_provider_text() {
        let secret = "sk-ant-api03-Abcdef0123456789Abcdef0123456789Abcdef0123456789";
        let message = format!(
            "Unauthorized ANTHROPIC_API_KEY={secret} {}",
            "💬".repeat(2_000)
        );
        let frame = json!({
            "type": "result", "subtype": "success", "is_error": true,
            "api_error_status": 401, "result": message,
        });
        let check =
            run_connection_test_with(&input(None), ready(Ok(message_failure(&frame)))).await;
        assert_eq!(check.outcome, ClaudeCliCheckOutcome::Failed);
        assert_eq!(check.reason.as_deref(), Some("auth_error"));
        assert_eq!(check.status, Some(401.into()));
        let text = check.message.expect("error message");
        assert!(!text.contains(secret));
        assert!(text.contains("[REDACTED]"));
        assert!(text.ends_with('…'));
        assert!(text.encode_utf16().count() <= 2_000);
    }

    #[test]
    fn assistant_auth_and_refusal_are_failures_even_without_terminal_result() {
        let auth = json!({"type": "assistant", "error": "authentication_failed"});
        assert_eq!(
            message_failure(&auth).expect("auth issue").reason,
            "auth_error"
        );
        let refusal = json!({"type": "assistant", "message": {"stop_reason": "refusal"}});
        assert!(message_failure(&refusal).is_some());
        let success = json!({"type": "result", "subtype": "success", "is_error": false});
        assert!(message_failure(&success).is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_cancels_an_unsettled_turn() {
        struct DropWitness<'a>(&'a std::cell::Cell<bool>);
        impl Drop for DropWitness<'_> {
            fn drop(&mut self) {
                self.0.set(true);
            }
        }
        let dropped = std::cell::Cell::new(false);
        let witness = DropWitness(&dropped);
        let check = run_connection_test_with(&input(None), async move {
            let _witness = witness;
            pending().await
        })
        .await;
        assert!(dropped.get());
        assert_eq!(check.reason.as_deref(), Some("timeout"));
    }

    #[tokio::test]
    async fn cancellation_wins_over_sdk_outcome() {
        let controller = AbortController::new();
        let signal = controller.signal();
        let check = run_connection_test_with(&input(Some(&signal)), async {
            controller.abort(None);
            Ok(None)
        })
        .await;
        assert_eq!(check.reason.as_deref(), Some("cancelled"));
        assert_eq!(check.at, 123);
    }
}
