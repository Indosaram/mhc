//! Port of senpi packages/agent/test/thrown-provider-error-boundary.test.ts (senpi#4282).
//!
//! A provider error the stream function throws, or one that fails the run between turns, never
//! passed the provider stream boundary. The agent classifies and redacts it the same way before
//! any event carries it. A Rust stream function cannot throw; the equivalent is a stream that
//! fails before yielding a terminal event, which the loop turns into a terminal failure message.

use crate::support;

use std::sync::{Arc, Mutex, OnceLock};

use maho_agent::{
    Agent, AgentEvent, AgentLoopTurnUpdate, AgentMessage, AgentOptions, AgentPromptInput,
    AgentToolResult, PartialAgentState, StreamFn,
};
use maho_ai::types::{AssistantErrorCause, AssistantMessage, StopReason};
use maho_ai::utils::event_stream::{AssistantMessageEventStream, StreamError};
use support::{assistant, message_stream, result_tool, tool_call};

fn secret() -> String {
    ["zzfixture", "4282", "thrownQ"].join("")
}

fn model_not_found() -> String {
    format!(
        "404 {{\"error\":{{\"message\":\"The model `faux-1` does not exist\",\"code\":\"model_not_found\"}}}} x-api-key: {}",
        secret()
    )
}

fn failing_stream() -> AssistantMessageEventStream {
    let stream = AssistantMessageEventStream::assistant();
    stream.fail(StreamError::new(model_not_found()));
    stream
}

fn record_events(agent: &Agent) -> Arc<Mutex<Vec<AgentEvent>>> {
    let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::default();
    let sink = events.clone();
    agent.subscribe(Arc::new(move |event, _signal| {
        sink.lock().expect("events").push(event);
        Box::pin(async {})
    }));
    events
}

fn last_assistant(agent: &Agent) -> AssistantMessage {
    agent
        .state()
        .messages()
        .last()
        .and_then(AgentMessage::as_assistant)
        .cloned()
        .expect("Expected assistant message")
}

fn serialized(events: &Arc<Mutex<Vec<AgentEvent>>>) -> String {
    serde_json::to_string(&*events.lock().expect("events")).expect("serialize events")
}

#[tokio::test]
async fn classifies_and_redacts_an_error_the_stream_function_throws() {
    let stream_fn: StreamFn = Arc::new(|_model, _context, _options| failing_stream());
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(stream_fn),
        ..AgentOptions::default()
    });
    let events = record_events(&agent);

    agent.prompt(AgentPromptInput::text("hello")).await;

    let last = last_assistant(&agent);
    assert_eq!(last.stop_reason, StopReason::Error);
    assert!(matches!(
        last.error_cause,
        Some(AssistantErrorCause::ModelConfig { .. })
    ));
    assert!(
        last.error_message
            .as_deref()
            .is_some_and(|message| message.contains("model_not_found"))
    );
    assert!(!serialized(&events).contains(&secret()));
    assert!(
        !agent
            .state()
            .error_message
            .unwrap_or_default()
            .contains(&secret())
    );
}

#[tokio::test]
async fn classifies_and_redacts_an_error_that_fails_the_run_between_turns() {
    let stream_fn: StreamFn = Arc::new(|_model, _context, _options| {
        message_stream(assistant(
            vec![tool_call("tool-1", "noop", serde_json::json!({}))],
            StopReason::ToolUse,
        ))
    });
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(stream_fn),
        initial_state: Some(PartialAgentState {
            tools: Some(vec![result_tool("noop", AgentToolResult::text("done"))]),
            ..PartialAgentState::default()
        }),
        prepare_next_turn_with_context: Some(Arc::new(|_context, _signal| {
            Box::pin(async {
                Some(AgentLoopTurnUpdate {
                    failure: Some(model_not_found()),
                    ..AgentLoopTurnUpdate::default()
                })
            })
        })),
        ..AgentOptions::default()
    });
    let events = record_events(&agent);

    agent.prompt(AgentPromptInput::text("hello")).await;

    let last = last_assistant(&agent);
    assert_eq!(last.stop_reason, StopReason::Error);
    assert!(matches!(
        last.error_cause,
        Some(AssistantErrorCause::ModelConfig { .. })
    ));
    assert!(!serialized(&events).contains(&secret()));
    assert!(
        !agent
            .state()
            .error_message
            .unwrap_or_default()
            .contains(&secret())
    );
}

#[tokio::test]
async fn leaves_an_aborted_run_unclassified() {
    let slot: Arc<OnceLock<Agent>> = Arc::new(OnceLock::new());
    let inner = slot.clone();
    let stream_fn: StreamFn = Arc::new(move |_model, _context, _options| {
        if let Some(agent) = inner.get() {
            agent.abort(None);
        }
        failing_stream()
    });
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(stream_fn),
        ..AgentOptions::default()
    });
    assert!(slot.set(agent.clone()).is_ok());

    agent.prompt(AgentPromptInput::text("hello")).await;

    let last = last_assistant(&agent);
    assert_eq!(last.stop_reason, StopReason::Aborted);
    assert!(last.error_cause.is_none());
    assert!(!last.error_message.unwrap_or_default().contains(&secret()));
}
