mod support;

use maho_agent::assistant_terminal_state::{
    demote_tool_use_without_tool_calls, promote_stop_with_pending_tool_calls, should_terminate_assistant_turn,
};
use maho_ai::types::{ContentBlock, StopReason, ToolResultMessage, Usage};
use maho_ai::utils::diagnostics::AssistantMessageDiagnostic;
use support::{assistant, text_block, tool_call};

fn stop_message(content: Vec<ContentBlock>) -> maho_ai::types::AssistantMessage {
    assistant(content, StopReason::Stop)
}

#[test]
fn promotes_stop_with_tool_calls_to_tool_use() {
    let next = promote_stop_with_pending_tool_calls(stop_message(vec![
        text_block("ok"),
        tool_call("1", "eval", serde_json::json!({})),
    ]));
    assert_eq!(next.stop_reason, StopReason::ToolUse);
    assert!(!should_terminate_assistant_turn(&next));
}

#[test]
fn leaves_text_only_stop_alone() {
    let next = promote_stop_with_pending_tool_calls(stop_message(vec![text_block("done")]));
    assert_eq!(next.stop_reason, StopReason::Stop);
}

#[test]
fn promotes_only_a_stop_reason_with_a_tool_call() {
    let tool_use = promote_stop_with_pending_tool_calls(assistant(
        vec![text_block("ok")],
        StopReason::ToolUse,
    ));
    assert_eq!(tool_use.stop_reason, StopReason::ToolUse);

    let errored = promote_stop_with_pending_tool_calls(assistant(
        vec![tool_call("1", "eval", serde_json::json!({}))],
        StopReason::Error,
    ));
    assert_eq!(errored.stop_reason, StopReason::Error);
}

#[test]
fn demotes_tool_use_without_tool_calls_and_records_the_diagnostic() {
    let message = assistant(vec![text_block("thinking out loud")], StopReason::ToolUse);
    let demoted = demote_tool_use_without_tool_calls(message);
    assert_eq!(demoted.stop_reason, StopReason::Stop);
    let diagnostics: Vec<AssistantMessageDiagnostic> = demoted.diagnostics.clone().unwrap_or_default();
    assert!(diagnostics.iter().any(|entry| entry.kind == maho_agent::EMPTY_TOOL_USE_DEMOTION_DIAGNOSTIC));
}

#[test]
fn demote_keeps_a_tool_use_message_that_carries_a_tool_call() {
    let message = assistant(vec![tool_call("1", "eval", serde_json::json!({}))], StopReason::ToolUse);
    let kept = demote_tool_use_without_tool_calls(message);
    assert_eq!(kept.stop_reason, StopReason::ToolUse);
    assert!(kept.diagnostics.is_none());
}

#[test]
fn terminate_covers_error_aborted_and_classifier_refusals() {
    assert!(should_terminate_assistant_turn(&assistant(vec![], StopReason::Error)));
    assert!(should_terminate_assistant_turn(&assistant(vec![], StopReason::Aborted)));
    assert!(!should_terminate_assistant_turn(&assistant(vec![text_block("hi")], StopReason::Stop)));

    let refusal = support::assistant_with(
        vec![tool_call("refused", "calculate", serde_json::json!({}))],
        StopReason::ToolUse,
        Some("This request triggered restrictions on violative cyber content and was blocked under Anthropic's Usage Policy.".to_owned()),
        Some(maho_ai::types::AssistantStopDetails::Refusal { explanation: None }),
    );
    assert!(should_terminate_assistant_turn(&refusal));
}

#[test]
fn empty_usage_helper_reports_zeroes() {
    let usage: Usage = maho_agent::agent::empty_failure_usage();
    assert_eq!(usage.total_tokens, 0);
    let _: Vec<ToolResultMessage> = Vec::new();
}
