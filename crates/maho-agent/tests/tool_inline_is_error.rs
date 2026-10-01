mod support;

use maho_agent::{AgentContext, AgentEvent, AgentLoopConfig, AgentMessage, AgentTool, AgentToolResult, identity_convert_to_llm};
use maho_ai::types::{ContentBlock, Message, StopReason, ToolResultMessage};
use serde_json::json;
use support::{
    assistant, collect_events, recording_sink, result_tool, scripted_stream_fn, test_model, text_block, tool_call,
    user_message,
};

fn tool_result_from(events: &[AgentEvent]) -> ToolResultMessage {
    for event in events {
        if let AgentEvent::MessageStart { message } = event
            && let AgentMessage::Llm(Message::ToolResult(result)) = message
        {
            return result.clone();
        }
    }
    panic!("tool call did not complete");
}

fn tool_execution_end(events: &[AgentEvent]) -> (serde_json::Value, bool) {
    for event in events {
        if let AgentEvent::ToolExecutionEnd { result, is_error, .. } = event {
            return (result.clone(), *is_error);
        }
    }
    panic!("tool call did not complete");
}

async fn run_single_tool_call(tool: AgentTool) -> Vec<AgentEvent> {
    let name = tool.name().to_owned();
    let context = AgentContext { system_prompt: String::new(), messages: Vec::new(), tools: Some(vec![tool]) };
    let config = AgentLoopConfig::new(test_model(), identity_convert_to_llm());
    let stream = maho_agent::agent_loop::agent_loop(
        vec![user_message("go")],
        context,
        config,
        None,
        Some(scripted_stream_fn(vec![
            assistant(vec![tool_call("tool-1", &name, json!({}))], StopReason::ToolUse),
            assistant(vec![text_block("done")], StopReason::Stop),
        ])),
    );
    collect_events(&stream).await
}

fn inline_result(content: &str, details: serde_json::Value, is_error: Option<bool>) -> AgentToolResult {
    AgentToolResult {
        content: vec![text_block(content)],
        details,
        usage: None,
        added_tool_names: None,
        terminate: None,
        is_error,
    }
}

#[tokio::test]
async fn marks_a_result_returned_with_is_error_true_as_a_tool_error_without_discarding_details() {
    let tool = result_tool("inline_fail", inline_result("member 'x' failed to start", json!({ "kind": "runtime_error" }), Some(true)));

    let events = run_single_tool_call(tool).await;
    let (result, is_error) = tool_execution_end(&events);
    let tool_result = tool_result_from(&events);

    assert!(is_error);
    assert_eq!(result["details"], json!({ "kind": "runtime_error" }));
    assert!(tool_result.is_error);
}

#[tokio::test]
async fn keeps_a_result_without_an_is_error_flag_as_a_successful_tool_result() {
    let tool = result_tool("plain_ok", inline_result("ok", json!({ "kind": "created" }), None));

    let events = run_single_tool_call(tool).await;
    let (_, is_error) = tool_execution_end(&events);
    let tool_result = tool_result_from(&events);

    assert!(!is_error);
    assert!(!tool_result.is_error);
}

#[tokio::test]
async fn treats_an_explicit_is_error_false_as_success() {
    let tool = result_tool("explicit_ok", inline_result("ok", json!({}), Some(false)));

    let events = run_single_tool_call(tool).await;
    let (_, is_error) = tool_execution_end(&events);

    assert!(!is_error);
}

#[tokio::test]
async fn the_recording_sink_sees_the_same_event_sequence() {
    let tool = result_tool("plain_ok", inline_result("ok", json!({}), None));
    let name = tool.name().to_owned();
    let (emit, recorded) = recording_sink();
    let context = AgentContext { system_prompt: String::new(), messages: Vec::new(), tools: Some(vec![tool]) };
    let config = AgentLoopConfig::new(test_model(), identity_convert_to_llm());
    maho_agent::agent_loop::run_agent_loop(
        vec![user_message("go")],
        context,
        config,
        emit,
        None,
        Some(scripted_stream_fn(vec![
            assistant(vec![tool_call("tool-1", &name, json!({}))], StopReason::ToolUse),
            assistant(vec![text_block("done")], StopReason::Stop),
        ])),
    )
    .await;
    let events = recorded.lock().unwrap().clone();
    assert_eq!(support::event_names(&events).first(), Some(&"agent_start"));
    assert_eq!(support::event_names(&events).last(), Some(&"agent_end"));
    assert!(events.iter().any(|event| matches!(event, AgentEvent::ToolExecutionEnd { .. })));
    let _ = ContentBlock::text("");
}
