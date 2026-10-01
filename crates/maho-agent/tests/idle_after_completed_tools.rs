mod support;

use maho_agent::assistant_terminal_state::{
    AgentStreamError, is_stream_idle_timeout_error, should_finalize_idle_as_stop,
};
use maho_ai::types::{ToolCall, ToolResultMessage};
use support::{assistant, text_block, tool_call};

fn resolved(id: &str) -> ToolCall {
    match tool_call(id, "task", serde_json::json!({})) {
        maho_ai::types::ContentBlock::ToolCall(call) => call,
        _ => unreachable!(),
    }
}

fn buffered_result(tool_call_id: &str) -> ToolResultMessage {
    ToolResultMessage {
        tool_call_id: tool_call_id.to_owned(),
        tool_name: "bash".to_owned(),
        content: Vec::new(),
        details: None,
        usage: None,
        added_tool_names: None,
        is_error: false,
        timestamp: 0,
    }
}

#[test]
fn finalizes_resolved_cursor_tools_after_idle() {
    let message = assistant(
        vec![maho_ai::types::ContentBlock::ToolCall(resolved("t1"))],
        maho_ai::types::StopReason::Stop,
    );
    assert!(should_finalize_idle_as_stop(Some(&message), &[], |_| true));
}

#[test]
fn does_not_finalize_text_only_idle() {
    let message = assistant(vec![text_block("hi")], maho_ai::types::StopReason::Stop);
    assert!(!should_finalize_idle_as_stop(Some(&message), &[], |_| true));
}

#[test]
fn finalizes_when_exec_results_are_already_buffered() {
    let message = assistant(
        vec![tool_call("t1", "bash", serde_json::json!({}))],
        maho_ai::types::StopReason::Stop,
    );
    let results = vec![buffered_result("t1")];
    assert!(should_finalize_idle_as_stop(Some(&message), &results, |_| false));
}

#[test]
fn recognizes_the_idle_timeout_error() {
    assert!(is_stream_idle_timeout_error(&AgentStreamError::IdleTimeout { timeout_ms: 40 }));
    assert!(!is_stream_idle_timeout_error(&AgentStreamError::Other { message: "other".to_owned() }));
    assert_eq!(
        AgentStreamError::IdleTimeout { timeout_ms: 40 }.message(),
        "Idle timeout waiting for provider stream after 40ms"
    );
}

#[test]
fn an_absent_partial_never_finalizes() {
    assert!(!should_finalize_idle_as_stop(None, &[], |_| true));
}

#[test]
fn an_unresolved_tool_call_never_finalizes() {
    let message = assistant(
        vec![tool_call("t1", "bash", serde_json::json!({}))],
        maho_ai::types::StopReason::Stop,
    );
    assert!(!should_finalize_idle_as_stop(Some(&message), &[], |_| false));
}
