//! Port of `agent-end-eligibility.ts`: the terminal-outcome gate for the continuation producer.
//!
//! Continuation is not error recovery: a turn that ended because the host aborted it, is retrying
//! it, or was refused must not be answered with an automatic continuation. The admitted shapes are
//! the intersection of the two gates the pinned host ships for the same question; the required
//! auto-compaction decision is unobservable from an `agent_end` payload, so the producer records
//! this outcome at `agent_end` and acts on `agent_settled`.

use serde_json::Value;

pub use maho_omo_fallback_architect::is_refusal_like_message;

/// `EMPTY_TOOL_USE_DEMOTION_DIAGNOSTIC`: the diagnostic `demoteToolUseWithoutToolCalls` leaves
/// behind, the only surviving evidence that a `toolUse` stop was malformed rather than a clean stop.
pub const EMPTY_TOOL_USE_DEMOTION_DIAGNOSTIC: &str = "empty_tool_use_terminal_state";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContinuationBlockReason {
    Aborted,
    HostRetry,
    MissingOutcome,
    Refusal,
    UnfinishedTurn,
    AbortedToolResult,
}

impl ContinuationBlockReason {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Aborted => "aborted",
            Self::HostRetry => "host-retry",
            Self::MissingOutcome => "missing-outcome",
            Self::Refusal => "refusal",
            Self::UnfinishedTurn => "unfinished-turn",
            Self::AbortedToolResult => "aborted-tool-result",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentEndOutcome {
    pub stop_reason: Option<String>,
    pub aborted: bool,
    pub will_retry: bool,
    pub blocked_by: Option<ContinuationBlockReason>,
}

/// `readAgentEndOutcome(payload)`: the single blocking reason, or `None` when the turn is
/// continuable. The payload is read at decision time, never cached, because a late user abort
/// mutates the dispatched event object in place for as long as the `agent_end` boundary is open.
#[must_use]
pub fn read_agent_end_outcome(payload: &Value) -> AgentEndOutcome {
    if !payload.is_object() {
        return AgentEndOutcome {
            stop_reason: None,
            aborted: false,
            will_retry: false,
            blocked_by: Some(ContinuationBlockReason::MissingOutcome),
        };
    }

    let aborted = payload["aborted"] == Value::Bool(true);
    let will_retry = payload["willRetry"] == Value::Bool(true);
    let messages = payload["messages"].as_array();
    let assistant_index = messages.map_or(-1, |messages| last_assistant_index(messages));
    let assistant = usize::try_from(assistant_index)
        .ok()
        .and_then(|index| messages.and_then(|messages| messages.get(index)));
    let stop_reason = assistant
        .and_then(|assistant| assistant["stopReason"].as_str())
        .map(str::to_owned);
    let outcome = AgentEndOutcome {
        stop_reason: stop_reason.clone(),
        aborted,
        will_retry,
        blocked_by: None,
    };

    if aborted {
        return blocked(outcome, ContinuationBlockReason::Aborted);
    }
    if will_retry {
        return blocked(outcome, ContinuationBlockReason::HostRetry);
    }
    let Some(assistant) = assistant.filter(|assistant| assistant.is_object()) else {
        return blocked(outcome, ContinuationBlockReason::MissingOutcome);
    };
    let Some(stop_reason) = stop_reason else {
        return blocked(outcome, ContinuationBlockReason::MissingOutcome);
    };
    if refusal_like(assistant) {
        return blocked(outcome, ContinuationBlockReason::Refusal);
    }
    if !is_finished_assistant_turn(assistant, &stop_reason) {
        return blocked(outcome, ContinuationBlockReason::UnfinishedTurn);
    }
    let trailing = messages.map_or(&[][..], |messages| {
        messages.get(usize::try_from(assistant_index).unwrap_or(0) + 1..).unwrap_or(&[])
    });
    if trailing.iter().any(is_aborted_tool_result) {
        return blocked(outcome, ContinuationBlockReason::AbortedToolResult);
    }
    outcome
}

#[must_use]
pub fn can_continue_after_agent_end(payload: &Value) -> bool {
    read_agent_end_outcome(payload).blocked_by.is_none()
}

fn blocked(mut outcome: AgentEndOutcome, reason: ContinuationBlockReason) -> AgentEndOutcome {
    outcome.blocked_by = Some(reason);
    outcome
}

/// The host's refusal definition, extended with its own normalization: `maho-omo-fallback-architect`
/// is the single predicate, but it does not yet admit the demoted empty tool-use turn, so the
/// admission term is added here by re-probing the shared predicate with an eligible stop reason.
fn refusal_like(assistant: &Value) -> bool {
    if is_refusal_like_message(assistant) {
        return true;
    }
    if !is_demoted_empty_tool_use(assistant) {
        return false;
    }
    let mut probe = assistant.clone();
    probe["stopReason"] = Value::String("error".to_string());
    is_refusal_like_message(&probe)
}

fn is_demoted_empty_tool_use(assistant: &Value) -> bool {
    assistant["stopReason"] == Value::String("stop".to_string())
        && assistant["diagnostics"].as_array().is_some_and(|diagnostics| {
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic["type"] == Value::String(EMPTY_TOOL_USE_DEMOTION_DIAGNOSTIC.to_string()))
        })
}

fn is_finished_assistant_turn(assistant: &Value, stop_reason: &str) -> bool {
    if stop_reason == "stop" || stop_reason == "length" {
        return true;
    }
    stop_reason == "toolUse" && !has_tool_call_content(assistant)
}

fn has_tool_call_content(assistant: &Value) -> bool {
    assistant["content"].as_array().is_some_and(|content| {
        content
            .iter()
            .any(|block| block["type"] == Value::String("toolCall".to_string()))
    })
}

/// The host's `isAbortedToolResult`: an error result whose text names an abort, which marks an
/// interrupted turn even when the assistant message itself looks finished.
fn is_aborted_tool_result(message: &Value) -> bool {
    if message["role"] != Value::String("toolResult".to_string())
        || message["isError"] != Value::Bool(true)
    {
        return false;
    }
    message["content"].as_array().is_some_and(|content| {
        content.iter().any(|block| {
            block["type"] == Value::String("text".to_string())
                && block["text"].as_str().is_some_and(mentions_abort)
        })
    })
}

/// `/\babort(?:ed)?\b/i` without a regex dependency: an `abort` or `aborted` word.
fn mentions_abort(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    let bytes = lowered.as_bytes();
    let mut index = 0;
    while let Some(found) = lowered[index..].find("abort") {
        let start = index + found;
        let end = start + "abort".len();
        let after = if bytes.get(end) == Some(&b'e') && bytes.get(end + 1) == Some(&b'd') {
            end + 2
        } else {
            end
        };
        let before_ok = start == 0 || !is_word_byte(bytes[start - 1]);
        let after_ok = after >= bytes.len() || !is_word_byte(bytes[after]);
        if before_ok && after_ok {
            return true;
        }
        index = end;
    }
    false
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn last_assistant_index(messages: &[Value]) -> i64 {
    for index in (0..messages.len()).rev() {
        if messages[index]["role"] == Value::String("assistant".to_string()) {
            return i64::try_from(index).unwrap_or(i64::MAX);
        }
    }
    -1
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const CODEX_ERROR: &str = "Codex error: This request was blocked by our safety systems. Reason: Potentially unintended activity.";

    fn clean() -> Value {
        json!({ "messages": [{ "role": "assistant", "stopReason": "stop" }], "willRetry": false })
    }

    fn with_messages(messages: Value) -> Value {
        json!({ "messages": messages, "willRetry": false })
    }

    fn error_message(error_message: &str) -> Value {
        json!({ "role": "assistant", "stopReason": "error", "errorMessage": error_message, "content": [] })
    }

    fn demotion_diagnostic() -> Value {
        json!({ "type": EMPTY_TOOL_USE_DEMOTION_DIAGNOSTIC, "timestamp": 0, "details": {} })
    }

    fn aborted_tool_result(text: &str) -> Value {
        json!({ "role": "toolResult", "toolCallId": "call-1", "toolName": "bash", "isError": true, "content": [{ "type": "text", "text": text }] })
    }

    #[test]
    fn blocked_outcomes_name_the_single_reason() {
        // given / when / then
        let cases: Vec<(&str, Value, ContinuationBlockReason)> = vec![
            ("terminal safety error", with_messages(json!([error_message(CODEX_ERROR)])), ContinuationBlockReason::UnfinishedTurn),
            ("ordinary provider failure", with_messages(json!([error_message("Connection failed")])), ContinuationBlockReason::UnfinishedTurn),
            ("aborted run", json!({ "messages": [{ "role": "assistant", "stopReason": "stop" }], "aborted": true }), ContinuationBlockReason::Aborted),
            ("aborted assistant", with_messages(json!([{ "role": "assistant", "stopReason": "aborted" }])), ContinuationBlockReason::UnfinishedTurn),
            ("host-owned retry", json!({ "messages": [{ "role": "assistant", "stopReason": "stop" }], "willRetry": true }), ContinuationBlockReason::HostRetry),
            ("empty tool-use refusal", with_messages(json!([{ "role": "assistant", "stopReason": "toolUse", "content": [], "stopDetails": { "type": "refusal" } }])), ContinuationBlockReason::Refusal),
            ("sensitive stop", with_messages(json!([{ "role": "assistant", "stopReason": "toolUse", "content": [], "stopDetails": { "type": "sensitive" } }])), ContinuationBlockReason::Refusal),
            ("normalized empty-tool-use refusal", with_messages(json!([{ "role": "assistant", "stopReason": "stop", "content": [], "diagnostics": [demotion_diagnostic()], "stopDetails": { "type": "refusal" } }])), ContinuationBlockReason::Refusal),
            ("normalized sensitive stop", with_messages(json!([{ "role": "assistant", "stopReason": "stop", "content": [], "diagnostics": [demotion_diagnostic()], "stopDetails": { "type": "sensitive" } }])), ContinuationBlockReason::Refusal),
            ("missing outcome", json!({ "type": "agent_end" }), ContinuationBlockReason::MissingOutcome),
            ("empty outcome", with_messages(json!([])), ContinuationBlockReason::MissingOutcome),
            ("missing stop reason", with_messages(json!([{ "role": "assistant" }])), ContinuationBlockReason::MissingOutcome),
            ("error behind custom tail", with_messages(json!([error_message(CODEX_ERROR), { "role": "custom", "content": "notice" }])), ContinuationBlockReason::UnfinishedTurn),
            ("unfinished tool-use turn", with_messages(json!([{ "role": "assistant", "stopReason": "toolUse", "content": [{ "type": "toolCall", "id": "call-1", "name": "bash", "arguments": {} }] }])), ContinuationBlockReason::UnfinishedTurn),
            ("trailing aborted tool result", with_messages(json!([{ "role": "assistant", "stopReason": "toolUse", "content": [{ "type": "toolCall", "id": "call-1", "name": "bash", "arguments": {} }] }, aborted_tool_result("Command aborted by user")])), ContinuationBlockReason::UnfinishedTurn),
            ("aborted tool result behind a finished assistant", with_messages(json!([{ "role": "assistant", "stopReason": "stop" }, aborted_tool_result("Command aborted by user")])), ContinuationBlockReason::AbortedToolResult),
        ];
        for (name, payload, expected) in cases {
            let outcome = read_agent_end_outcome(&payload);
            assert_eq!(outcome.blocked_by, Some(expected), "case: {name}");
            assert!(!can_continue_after_agent_end(&payload), "case: {name}");
        }
    }

    #[test]
    fn continuable_outcomes_admit_the_turn() {
        // given / when / then
        let cases: Vec<(&str, Value)> = vec![
            ("explanatory assistant text", with_messages(json!([{ "role": "assistant", "stopReason": "stop", "content": [{ "type": "text", "text": CODEX_ERROR }] }]))),
            ("an earlier failed attempt behind a clean tail", with_messages(json!([error_message(CODEX_ERROR), { "role": "assistant", "stopReason": "stop" }, { "role": "custom", "content": "notice" }]))),
            ("a demoted empty tool-use turn without refusal details", with_messages(json!([{ "role": "assistant", "stopReason": "stop", "content": [], "diagnostics": [demotion_diagnostic()] }]))),
            ("a plain stop carrying stale refusal details", with_messages(json!([{ "role": "assistant", "stopReason": "stop", "content": [], "stopDetails": { "type": "refusal" } }]))),
            ("a trailing tool error that is not an abort", with_messages(json!([{ "role": "assistant", "stopReason": "stop" }, aborted_tool_result("exit status 1: file not found")]))),
            ("a length stop", with_messages(json!([{ "role": "assistant", "stopReason": "length" }]))),
        ];
        for (name, payload) in cases {
            assert_eq!(read_agent_end_outcome(&payload).blocked_by, None, "case: {name}");
            assert!(can_continue_after_agent_end(&payload), "case: {name}");
        }
    }

    #[test]
    fn abort_word_matching_respects_word_boundaries() {
        // given / when / then
        assert!(mentions_abort("Command aborted by user"));
        assert!(mentions_abort("ABORT"));
        assert!(!mentions_abort("noabortx"));
        assert!(!mentions_abort("exit status 1"));
    }

    #[test]
    fn clean_tail_reports_the_stop_reason() {
        // given / when / then
        let outcome = read_agent_end_outcome(&clean());
        assert_eq!(outcome.stop_reason.as_deref(), Some("stop"));
        assert!(!outcome.aborted);
        assert!(!outcome.will_retry);
    }
}
