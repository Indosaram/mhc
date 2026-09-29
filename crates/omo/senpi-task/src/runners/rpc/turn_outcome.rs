//! `runners/rpc/turn-outcome.ts`: derive one turn's [`RunnerOutcome`] from RPC wire facts.
//!
//! Every event arrives as untrusted JSON, so hostile shapes degrade to terminal errors instead of
//! panicking.

use serde_json::Value;

use crate::runners::rpc::exit_mapping::map_exit_outcome_to_error;
use crate::runners::types::ChildExitOutcome;
use crate::runners::{RunnerFailure, RunnerFailureKind, RunnerOutcome};

/// A prompt command that failed before the turn started.
pub fn prompt_failure_outcome(message: &str) -> RunnerOutcome {
    RunnerOutcome::error(RunnerFailureKind::ChildPromptFailed, message)
}

/// Classify a terminal `agent_end` event. `baseline` is the assistant text observed before the
/// turn began; `observed_text` is the latest text seen during it.
pub fn agent_end_outcome(
    event: &Value,
    baseline: Option<&str>,
    observed_text: Option<&str>,
) -> RunnerOutcome {
    let assistant = event
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| messages.iter().rev().find(|message| is_assistant(message)));
    let stop_reason = assistant.and_then(|message| message.get("stopReason")?.as_str());
    let error_message = assistant.and_then(|message| message.get("errorMessage")?.as_str());
    let aborted = event.get("aborted").and_then(Value::as_bool) == Some(true);
    if aborted || matches!(stop_reason, Some("error" | "aborted")) {
        let message = error_message.map_or_else(
            || {
                let reason = stop_reason.unwrap_or("aborted");
                format!("RPC child turn ended with stopReason \"{reason}\"")
            },
            str::to_string,
        );
        return RunnerOutcome::error(RunnerFailureKind::ChildTurnFailed, message);
    }
    let message_text = assistant.and_then(extract_assistant_text);
    let final_text = message_text.or_else(|| {
        (observed_text != baseline)
            .then(|| observed_text.map(str::to_string))
            .flatten()
    });
    match final_text {
        Some(text) if !text.is_empty() => RunnerOutcome::completed(text),
        _ => RunnerOutcome::error(
            RunnerFailureKind::ChildTurnFailed,
            error_message.unwrap_or("RPC child turn produced no assistant output"),
        ),
    }
}

/// The turn outcome when the child process ends before a terminal `agent_end`.
pub fn exit_turn_outcome(exit: &ChildExitOutcome, final_text: Option<&str>) -> RunnerOutcome {
    if let ChildExitOutcome::Clean { .. } = exit {
        return match final_text {
            Some(text) if !text.is_empty() => RunnerOutcome::completed(text),
            _ => RunnerOutcome::error(
                RunnerFailureKind::ChildTurnFailed,
                "RPC child exited without assistant output",
            ),
        };
    }
    let facts = map_exit_outcome_to_error(exit, /*already_terminal*/ false);
    RunnerOutcome::Error {
        failure: RunnerFailure::new(
            RunnerFailureKind::ChildPromptFailed,
            facts
                .as_ref()
                .map_or("RPC child terminated abnormally", |facts| {
                    facts.error_message.as_str()
                }),
        ),
        killed: facts.is_some_and(|facts| facts.killed),
    }
}

/// Concatenated `text` parts of an assistant message; `None` when there are none.
pub fn extract_assistant_text(message: &Value) -> Option<String> {
    if !is_assistant(message) {
        return None;
    }
    let text: String = message
        .get("content")?
        .as_array()?
        .iter()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|part| part.get("text")?.as_str())
        .collect();
    (!text.is_empty()).then_some(text)
}

fn is_assistant(value: &Value) -> bool {
    value.get("role").and_then(Value::as_str) == Some("assistant")
}
