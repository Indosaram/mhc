//! Port of senpi packages/agent/src/assistant-terminal-state.ts.

use std::collections::BTreeMap;

use maho_ai::model::Model;
use maho_ai::types::{
    AssistantMessage, ContentBlock, StopReason, ToolResultMessage, Usage, UsageCost,
};
use maho_ai::utils::diagnostics::AssistantMessageDiagnostic;
use maho_ai::utils::stop_details::is_classifier_refusal;
use serde_json::Value;

/// Terminal assistant message event (\`done\` or \`error\`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalAssistantMessageEvent {
    Done,
    Error { reason: StopReason },
}

impl TerminalAssistantMessageEvent {
    /// The stop reason the event carries (\`done\` keeps the message's own reason).
    pub fn reason(self) -> Option<StopReason> {
        match self {
            TerminalAssistantMessageEvent::Done => None,
            TerminalAssistantMessageEvent::Error { reason } => Some(reason),
        }
    }
}

/// Failure carried by the loop's terminal paths.
///
/// TS distinguishes these by \`instanceof\` (\`StreamIdleTimeoutError\`,
/// \`ProviderRetryWatchdogAbortError\`) and by message text; the Rust port keeps them as one
/// enum so the loop can classify without string matching.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AgentStreamError {
    /// \`StreamIdleTimeoutError\`.
    #[error("Idle timeout waiting for provider stream after {timeout_ms}ms")]
    IdleTimeout { timeout_ms: u64 },
    /// \`StreamStartTimeoutError\`. The wording must keep matching the retryable-error
    /// classifier ("timed out" in packages/ai/src/utils/retry.ts) so a dead stream start is
    /// retried instead of dead-ending the session.
    #[error(
        "Provider stream start timed out after {timeout_ms}ms (raise streamStartTimeoutMs — retry.provider.streamStartTimeoutMs in senpi settings; 0 disables)"
    )]
    StartTimeout { timeout_ms: u64 },
    /// \`ProviderRetryWatchdogAbortError\`.
    #[error("{provider_cause}")]
    ProviderRetryWatchdogAbort { provider_cause: String },
    #[error("{message}")]
    ProviderFailure {
        message: String,
        diagnostic: maho_ai::types::ProviderDiagnostic,
    },
    #[error("{message}")]
    OAuthRefreshUnavailable { message: String },
    /// Any other thrown value, normalized to its message.
    #[error("{message}")]
    Other { message: String },
}

impl AgentStreamError {
    pub fn message(&self) -> String {
        match self {
            AgentStreamError::IdleTimeout { timeout_ms } => {
                format!("Idle timeout waiting for provider stream after {timeout_ms}ms")
            }
            AgentStreamError::StartTimeout { timeout_ms } => format!(
                "Provider stream start timed out after {timeout_ms}ms (raise streamStartTimeoutMs — retry.provider.streamStartTimeoutMs in senpi settings; 0 disables)"
            ),
            AgentStreamError::ProviderRetryWatchdogAbort { provider_cause } => {
                provider_cause.clone()
            }
            AgentStreamError::Other { message }
            | AgentStreamError::ProviderFailure { message, .. }
            | AgentStreamError::OAuthRefreshUnavailable { message } => message.clone(),
        }
    }
}

/// \`ProviderRetryWatchdogAbortError\`: a retry watchdog aborted the provider request.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{provider_cause}")]
pub struct ProviderRetryWatchdogAbortError {
    pub provider_cause: String,
}

/// The `name` a `ProviderRetryWatchdogAbortError` carries as an abort-signal reason.
pub const PROVIDER_RETRY_WATCHDOG_ABORT_ERROR_NAME: &str = "ProviderRetryWatchdogAbortError";

impl ProviderRetryWatchdogAbortError {
    pub fn new(provider_cause: impl Into<String>) -> Self {
        Self {
            provider_cause: provider_cause.into(),
        }
    }

    /// The reason passed to `agent.abort(new ProviderRetryWatchdogAbortError(...))`.
    pub fn into_abort_reason(self) -> maho_ai::utils::abort::AbortReason {
        maho_ai::utils::abort::AbortReason::new(
            PROVIDER_RETRY_WATCHDOG_ABORT_ERROR_NAME,
            self.provider_cause,
        )
    }
}

/// Marks a terminal message whose \`toolUse\` stop reason was demoted because the provider sent no
/// tool call. Demotion rewrites the stop reason, so this diagnostic is the only surviving evidence
/// that the turn was malformed rather than a clean stop; downstream recovery keys on it.
pub const EMPTY_TOOL_USE_DEMOTION_DIAGNOSTIC: &str = "empty_tool_use_terminal_state";

fn empty_usage() -> Usage {
    Usage {
        context_usage: None,
        cost_unknown: None,
        input: 0,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 0,
        cost: UsageCost::default(),
    }
}

/// \`Date.now()\`.
pub fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_millis() as i64,
        Err(_) => 0,
    }
}

pub fn demote_tool_use_without_tool_calls(message: AssistantMessage) -> AssistantMessage {
    // Count raw blocks: cursor-resolved calls are legitimate completed tool calls and must not be demoted.
    if message.stop_reason != StopReason::ToolUse
        || message
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::ToolCall(_)))
    {
        return message;
    }
    let mut diagnostics = message.diagnostics.clone().unwrap_or_default();
    diagnostics.push(AssistantMessageDiagnostic {
        kind: EMPTY_TOOL_USE_DEMOTION_DIAGNOSTIC.to_owned(),
        timestamp: now_ms(),
        error: None,
        details: Some(serde_json::Map::new()),
    });
    AssistantMessage {
        stop_reason: StopReason::Stop,
        diagnostics: Some(diagnostics),
        ..message
    }
}

pub fn promote_stop_with_pending_tool_calls(message: AssistantMessage) -> AssistantMessage {
    if message.stop_reason != StopReason::Stop {
        return message;
    }
    if !message
        .content
        .iter()
        .any(|block| matches!(block, ContentBlock::ToolCall(_)))
    {
        return message;
    }
    AssistantMessage {
        stop_reason: StopReason::ToolUse,
        ..message
    }
}

pub fn should_terminate_assistant_turn(message: &AssistantMessage) -> bool {
    message.stop_reason == StopReason::Error
        || message.stop_reason == StopReason::Aborted
        || is_classifier_refusal(message)
}

pub fn is_stream_idle_timeout_error(error: &AgentStreamError) -> bool {
    matches!(error, AgentStreamError::IdleTimeout { .. })
}

/// After tools already finished (Cursor exec-resolved or buffered results),
/// a silent provider is a finished turn, not a failed one.
pub fn should_finalize_idle_as_stop(
    partial_message: Option<&AssistantMessage>,
    provider_tool_results: &[ToolResultMessage],
    is_cursor_exec_resolved: impl Fn(&maho_ai::types::ToolCall) -> bool,
) -> bool {
    let Some(partial_message) = partial_message else {
        return false;
    };
    let tool_calls: Vec<&maho_ai::types::ToolCall> = partial_message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolCall(call) => Some(call),
            _ => None,
        })
        .collect();
    if tool_calls.is_empty() {
        return false;
    }
    if !provider_tool_results.is_empty() {
        return true;
    }
    tool_calls.into_iter().all(is_cursor_exec_resolved)
}

pub fn create_terminal_failure_assistant_message(
    model: &Model,
    reason: StopReason,
    error: &AgentStreamError,
    partial_message: Option<&AssistantMessage>,
) -> AssistantMessage {
    let error_message = error.message();
    let provider_diagnostic = match error {
        AgentStreamError::ProviderFailure { diagnostic, .. } if reason == StopReason::Error => {
            maho_ai::types::sanitize_provider_diagnostic(diagnostic)
        }
        _ => None,
    };
    let mut diagnostics = partial_message.and_then(|message| message.diagnostics.clone());
    if reason == StopReason::Error
        && matches!(error, AgentStreamError::OAuthRefreshUnavailable { .. })
    {
        diagnostics
            .get_or_insert_default()
            .push(AssistantMessageDiagnostic {
                kind: maho_ai::utils::retry::OAUTH_REFRESH_UNAVAILABLE_DIAGNOSTIC.to_owned(),
                timestamp: now_ms(),
                error: None,
                details: Some(serde_json::Map::from_iter([(
                    "provider".to_owned(),
                    serde_json::json!(model.provider),
                )])),
            });
    }
    let fallback = if reason == StopReason::Aborted {
        "Request was aborted"
    } else {
        "Error"
    };
    // TS sets abortSource only for a ProviderRetryWatchdogAbortError; the field stays absent
    // otherwise, even when the partial message carried one.
    let abort_source = match error {
        AgentStreamError::ProviderRetryWatchdogAbort { .. } => {
            Some(maho_ai::types::AbortSource::Provider)
        }
        _ => None,
    };
    AssistantMessage {
        content: partial_message
            .map(|message| message.content.clone())
            .unwrap_or_else(|| vec![ContentBlock::text("")]),
        api: partial_message
            .map(|message| message.api.clone())
            .unwrap_or_else(|| model.api.clone()),
        provider: partial_message
            .map(|message| message.provider.clone())
            .unwrap_or_else(|| model.provider.clone()),
        model: partial_message
            .map(|message| message.model.clone())
            .unwrap_or_else(|| model.id.clone()),
        response_model: partial_message.and_then(|message| message.response_model.clone()),
        response_id: partial_message.and_then(|message| message.response_id.clone()),
        thinking_level: None,
        provider_thinking_level: partial_message
            .and_then(|message| message.provider_thinking_level.clone()),
        diagnostics,
        prompt_cache_breakpoint: None,
        prompt_cache_diagnostics: partial_message
            .and_then(|message| message.prompt_cache_diagnostics.clone()),
        usage: partial_message
            .map(|message| message.usage)
            .unwrap_or_else(empty_usage),
        stop_reason: reason,
        stop_details: None,
        deferred: None,
        error_message: Some(if error_message.is_empty() {
            fallback.to_owned()
        } else {
            error_message
        }),
        provider_diagnostic,
        error_cause: partial_message.and_then(|message| message.error_cause),
        abort_source,
        failure_source: None,
        raw_stop_reason: None,
        end_turn: None,
        timing: None,
        timestamp: partial_message
            .map(|message| message.timestamp)
            .unwrap_or_else(now_ms),
    }
}

pub fn normalize_terminal_assistant_message(
    message: AssistantMessage,
    event: TerminalAssistantMessageEvent,
) -> AssistantMessage {
    let Some(reason) = event.reason() else {
        return message;
    };
    let fallback = if reason == StopReason::Aborted {
        "Request was aborted"
    } else {
        "Error"
    };
    let error_message = message
        .error_message
        .clone()
        .unwrap_or_else(|| fallback.to_owned());
    if message.stop_reason == reason
        && message.error_message.as_deref() == Some(error_message.as_str())
    {
        return message;
    }
    AssistantMessage {
        stop_reason: reason,
        error_message: Some(error_message),
        ..message
    }
}

/// \`details: {}\` diagnostics payload helper (TS writes \`details: {}\u{2026}\`).
pub fn empty_details() -> Option<BTreeMap<String, Value>> {
    Some(BTreeMap::new())
}

#[cfg(test)]
mod diagnostic_tests {
    use super::*;
    use maho_ai::types::{
        ProviderDiagnostic, ProviderDiagnosticCategory, ProviderDiagnosticEvidence,
    };

    fn diagnostic() -> ProviderDiagnostic {
        ProviderDiagnostic { resets_at: None,
            category: ProviderDiagnosticCategory::Quota,
            http_status: Some(429),
            code: Some("insufficient_quota".into()),
            evidence: ProviderDiagnosticEvidence::StructuredCode,
        }
    }

    #[test]
    fn terminal_failure_retains_typed_usage_limit_without_parsing_text() {
        let model = crate::agent::default_model();
        let error = AgentStreamError::OAuthRefreshUnavailable {
            message: "ordinary failure".into(),
        };
        let mut partial =
            create_terminal_failure_assistant_message(&model, StopReason::Error, &error, None);
        partial.error_cause = Some(maho_ai::types::AssistantErrorCause::UsageLimit {
            scope: maho_ai::types::UsageLimitScope::Model,
        });
        let terminal = create_terminal_failure_assistant_message(
            &model,
            StopReason::Error,
            &error,
            Some(&partial),
        );
        assert_eq!(terminal.error_cause, partial.error_cause);
        assert!(
            create_terminal_failure_assistant_message(&model, StopReason::Error, &error, None)
                .error_cause
                .is_none()
        );
    }

    #[test]
    fn terminal_failure_preserves_only_valid_error_diagnostics() {
        // Given: a structured provider failure and a contradictory classification.
        let model = crate::agent::default_model();
        for reason in [StopReason::Error, StopReason::Aborted] {
            for valid in [true, false] {
                let mut metadata = diagnostic();
                if !valid {
                    metadata.http_status = Some(401);
                }
                let error = AgentStreamError::ProviderFailure {
                    message: "failure".into(),
                    diagnostic: metadata,
                };
                // When: the failure is projected into a terminal message.
                let message =
                    create_terminal_failure_assistant_message(&model, reason, &error, None);
                // Then: only an error with consistent structured evidence survives.
                assert_eq!(
                    message.provider_diagnostic,
                    (valid && reason == StopReason::Error).then(diagnostic)
                );
            }
        }
    }

    #[test]
    fn oauth_refresh_unavailable_appends_without_replacing_partial_diagnostics() {
        // Given: a partial response with existing diagnostics.
        let model = crate::agent::default_model();
        let error = AgentStreamError::OAuthRefreshUnavailable {
            message: "refresh failed".into(),
        };
        let mut partial =
            create_terminal_failure_assistant_message(&model, StopReason::Error, &error, None);
        partial.diagnostics.as_mut().expect("refresh diagnostic")[0].kind = "existing".into();
        for reason in [StopReason::Error, StopReason::Aborted] {
            // When: a typed refresh failure settles a partial response.
            let message =
                create_terminal_failure_assistant_message(&model, reason, &error, Some(&partial));
            // Then: an error appends the retry marker; abort only retains the partial metadata.
            let diagnostics = message.diagnostics.expect("partial diagnostics");
            assert_eq!(diagnostics[0].kind, "existing");
            assert_eq!(
                diagnostics.len(),
                if reason == StopReason::Error { 2 } else { 1 }
            );
            if reason == StopReason::Error {
                assert_eq!(
                    diagnostics[1].kind,
                    maho_ai::utils::retry::OAUTH_REFRESH_UNAVAILABLE_DIAGNOSTIC
                );
                assert_eq!(
                    diagnostics[1]
                        .details
                        .as_ref()
                        .expect("provider details")
                        .get("provider"),
                    Some(&serde_json::json!(model.provider))
                );
            }
        }
    }
}
