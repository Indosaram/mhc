mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use maho_agent::agent_loop::{agent_loop, agent_loop_continue, run_agent_loop};
use maho_agent::{
    AgentContext, AgentEvent, AgentLoopConfig, AgentMessage, AgentTool, AgentToolResult, StreamFn,
    ToolExecutionMode, identity_convert_to_llm,
};
use maho_ai::types::{
    AssistantMessage, AssistantMessageEvent, ContentBlock, Message, StopReason, ThinkingContent, ToolResultMessage,
};
use maho_ai::utils::abort::AbortController;
use maho_ai::utils::event_stream::AssistantMessageEventStream;
use serde_json::json;
use support::{
    assistant, collect_events, event_names, message_stream, recording_tool, result_tool, scripted_stream_fn,
    test_model, text_block, tool_call, user_message,
};

fn thinking_block(message: &AgentMessage, index: usize) -> &ThinkingContent {
    let assistant = message.as_assistant().expect("assistant message");
    match &assistant.content[index] {
        ContentBlock::Thinking(block) => block,
        other => panic!("expected thinking block, got {}", other.type_name()),
    }
}

fn thinking_partial(thinking: &str) -> AssistantMessage {
    assistant(vec![ContentBlock::Thinking(ThinkingContent { thinking: thinking.to_owned(), ..Default::default() })], StopReason::Stop)
}

fn event_stream(events: Vec<AssistantMessageEvent>, end: Option<AssistantMessage>) -> AssistantMessageEventStream {
    let stream = AssistantMessageEventStream::assistant();
    for event in events {
        stream.push(event);
    }
    if let Some(message) = end {
        stream.end(Some(message));
    }
    stream
}

fn stream_fn_of(events: Vec<AssistantMessageEvent>) -> StreamFn {
    Arc::new(move |_model, _context, _options| event_stream(events.clone(), None))
}

fn stream_fn_ending_with(events: Vec<AssistantMessageEvent>, end: AssistantMessage) -> StreamFn {
    Arc::new(move |_model, _context, _options| event_stream(events.clone(), Some(end.clone())))
}

async fn collect_with_timeout(
    stream: &maho_ai::utils::event_stream::EventStream<AgentEvent, Vec<AgentMessage>>,
) -> Vec<AgentEvent> {
    tokio::time::timeout(std::time::Duration::from_secs(5), collect_events(stream))
        .await
        .expect("agent loop stream did not terminate")
}

fn context_with(tools: Vec<AgentTool>) -> AgentContext {
    AgentContext { system_prompt: String::new(), messages: Vec::new(), tools: Some(tools) }
}

#[tokio::test]
async fn uses_the_configured_default_when_a_legacy_caller_omits_stream_fn() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    maho_agent::set_default_stream_fn(Some(Arc::new(move |_model, _context, _options| {
        counter.fetch_add(1, Ordering::SeqCst);
        message_stream(assistant(vec![text_block("fallback")], StopReason::Stop))
    })));

    let config = AgentLoopConfig::new(test_model(), identity_convert_to_llm());
    let stream = agent_loop(vec![user_message("Hello")], context_with(Vec::new()), config, None, None);
    let _ = collect_with_timeout(&stream).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    maho_agent::set_default_stream_fn(None);
}

#[tokio::test]
async fn stamps_thinking_timing_on_a_completed_thinking_block() {
    let stream = agent_loop(
        vec![user_message("Hello")],
        AgentContext { system_prompt: "Test".to_owned(), messages: Vec::new(), tools: Some(Vec::new()) },
        AgentLoopConfig::new(test_model(), identity_convert_to_llm()),
        None,
        Some(stream_fn_of(vec![
            AssistantMessageEvent::Start { partial: assistant(Vec::new(), StopReason::Stop) },
            AssistantMessageEvent::ThinkingStart { content_index: 0, partial: thinking_partial("") },
            AssistantMessageEvent::ThinkingDelta {
                content_index: 0,
                delta: "reasoning".to_owned(),
                partial: thinking_partial("reasoning"),
            },
            AssistantMessageEvent::ThinkingEnd {
                content_index: 0,
                content: "reasoning".to_owned(),
                partial: thinking_partial("reasoning"),
            },
            AssistantMessageEvent::Done {
                reason: maho_ai::types::DoneReason::Stop,
                message: thinking_partial("reasoning"),
            },
        ])),
    );
    let events = collect_with_timeout(&stream).await;
    let messages = stream.result().await.expect("result");
    let block = thinking_block(&messages[1], 0);
    let started_at = block.started_at.expect("startedAt");
    let ended_at = block.ended_at.expect("endedAt");
    assert!(ended_at >= started_at);
    assert!(events.iter().any(|event| matches!(event, AgentEvent::MessageUpdate { .. })));
}

#[tokio::test]
async fn stamps_independent_timing_for_two_thinking_blocks() {
    let two = |first: &str, second: &str| {
        assistant(
            vec![
                ContentBlock::Thinking(ThinkingContent { thinking: first.to_owned(), ..Default::default() }),
                ContentBlock::Thinking(ThinkingContent { thinking: second.to_owned(), ..Default::default() }),
            ],
            StopReason::Stop,
        )
    };
    let stream = agent_loop(
        vec![user_message("Hello")],
        context_with(Vec::new()),
        AgentLoopConfig::new(test_model(), identity_convert_to_llm()),
        None,
        Some(stream_fn_of(vec![
            AssistantMessageEvent::Start { partial: assistant(Vec::new(), StopReason::Stop) },
            AssistantMessageEvent::ThinkingStart { content_index: 0, partial: two("", "") },
            AssistantMessageEvent::ThinkingEnd { content_index: 0, content: "one".to_owned(), partial: two("one", "") },
            AssistantMessageEvent::ThinkingStart { content_index: 1, partial: two("one", "") },
            AssistantMessageEvent::ThinkingEnd { content_index: 1, content: "two".to_owned(), partial: two("one", "two") },
            AssistantMessageEvent::Done { reason: maho_ai::types::DoneReason::Stop, message: two("one", "two") },
        ])),
    );
    let _ = collect_with_timeout(&stream).await;
    let messages = stream.result().await.expect("result");
    for index in [0, 1] {
        let block = thinking_block(&messages[1], index);
        let started_at = block.started_at.expect("startedAt");
        let ended_at = block.ended_at.expect("endedAt");
        assert!(ended_at >= started_at);
    }
}

#[tokio::test]
async fn keeps_started_at_stable_across_thinking_updates() {
    let stream = agent_loop(
        vec![user_message("Hello")],
        context_with(Vec::new()),
        AgentLoopConfig::new(test_model(), identity_convert_to_llm()),
        None,
        Some(stream_fn_of(vec![
            AssistantMessageEvent::Start { partial: assistant(Vec::new(), StopReason::Stop) },
            AssistantMessageEvent::ThinkingStart { content_index: 0, partial: thinking_partial("") },
            AssistantMessageEvent::ThinkingDelta {
                content_index: 0,
                delta: "reasoning".to_owned(),
                partial: thinking_partial("reasoning"),
            },
            AssistantMessageEvent::ThinkingEnd {
                content_index: 0,
                content: "reasoning".to_owned(),
                partial: thinking_partial("reasoning"),
            },
            AssistantMessageEvent::Done {
                reason: maho_ai::types::DoneReason::Stop,
                message: thinking_partial("reasoning"),
            },
        ])),
    );
    let events = collect_with_timeout(&stream).await;
    let started_ats: Vec<i64> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::MessageUpdate { message, assistant_message_event } => match assistant_message_event {
                AssistantMessageEvent::ThinkingStart { .. }
                | AssistantMessageEvent::ThinkingDelta { .. }
                | AssistantMessageEvent::ThinkingEnd { .. } => thinking_block(message, 0).started_at,
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(started_ats.len(), 3);
    assert!(started_ats.iter().all(|started_at| *started_at == started_ats[0]));
}

#[tokio::test]
async fn closes_thinking_timing_on_abort_error_events() {
    let stream = agent_loop(
        vec![user_message("Hello")],
        context_with(Vec::new()),
        AgentLoopConfig::new(test_model(), identity_convert_to_llm()),
        None,
        Some(stream_fn_of(vec![
            AssistantMessageEvent::Start { partial: assistant(Vec::new(), StopReason::Stop) },
            AssistantMessageEvent::ThinkingStart { content_index: 0, partial: thinking_partial("") },
            AssistantMessageEvent::Error {
                reason: maho_ai::types::ErrorReason::Aborted,
                error: assistant(
                    vec![ContentBlock::Thinking(ThinkingContent::default())],
                    StopReason::Aborted,
                ),
            },
        ])),
    );
    let _ = collect_with_timeout(&stream).await;
    let messages = stream.result().await.expect("result");
    let block = thinking_block(&messages[1], 0);
    assert!(block.ended_at.expect("endedAt") >= block.started_at.expect("startedAt"));
}

#[tokio::test]
async fn closes_unterminated_thinking_timing_when_the_stream_falls_through() {
    let stream = agent_loop(
        vec![user_message("Hello")],
        context_with(Vec::new()),
        AgentLoopConfig::new(test_model(), identity_convert_to_llm()),
        None,
        Some(stream_fn_ending_with(
            vec![
                AssistantMessageEvent::Start { partial: assistant(Vec::new(), StopReason::Stop) },
                AssistantMessageEvent::ThinkingStart { content_index: 0, partial: thinking_partial("") },
            ],
            thinking_partial(""),
        )),
    );
    let _ = collect_with_timeout(&stream).await;
    let messages = stream.result().await.expect("result");
    let block = thinking_block(&messages[1], 0);
    assert!(block.ended_at.expect("endedAt") >= block.started_at.expect("startedAt"));
}

#[tokio::test]
async fn leaves_messages_without_thinking_events_unaffected() {
    let final_message = assistant(vec![text_block("answer")], StopReason::Stop);
    let stream = agent_loop(
        vec![user_message("Hello")],
        context_with(Vec::new()),
        AgentLoopConfig::new(test_model(), identity_convert_to_llm()),
        None,
        Some(stream_fn_of(vec![AssistantMessageEvent::Done {
            reason: maho_ai::types::DoneReason::Stop,
            message: final_message.clone(),
        }])),
    );
    let _ = collect_with_timeout(&stream).await;
    let messages = stream.result().await.expect("result");
    assert_eq!(messages[1].as_assistant().expect("assistant"), &final_message);
    assert_eq!(messages[1].as_assistant().expect("assistant").content.len(), 1);
}

#[tokio::test]
async fn emits_events_with_agent_message_types() {
    let stream = agent_loop(
        vec![user_message("Hello")],
        context_with(Vec::new()),
        AgentLoopConfig::new(test_model(), identity_convert_to_llm()),
        None,
        Some(scripted_stream_fn(vec![assistant(vec![text_block("Hi there")], StopReason::Stop)])),
    );
    let events = collect_with_timeout(&stream).await;
    assert_eq!(events.first().map(support::event_name), Some("agent_start"));
    assert_eq!(events.last().map(support::event_name), Some("agent_end"));
    let names = event_names(&events);
    assert!(names.contains(&"message_start"));
    assert!(names.contains(&"message_end"));
    assert!(names.contains(&"turn_start"));
    assert!(names.contains(&"turn_end"));
}

#[tokio::test]
async fn emits_a_terminal_assistant_error_when_stream_creation_throws() {
    let throwing: StreamFn = Arc::new(|_model, _context, _options| {
        let stream = AssistantMessageEventStream::assistant();
        stream.fail(maho_ai::utils::event_stream::StreamError::new("boom"));
        stream
    });
    let stream = agent_loop(
        vec![user_message("Hello")],
        context_with(Vec::new()),
        AgentLoopConfig::new(test_model(), identity_convert_to_llm()),
        None,
        Some(throwing),
    );
    let _ = collect_with_timeout(&stream).await;
    let messages = stream.result().await.expect("result");
    let last = messages.last().and_then(AgentMessage::as_assistant).expect("assistant message");
    assert_eq!(last.stop_reason, StopReason::Error);
}

#[tokio::test]
async fn fails_the_turn_when_provider_stream_stays_idle_past_timeout_ms() {
    let mut config = AgentLoopConfig::new(test_model(), identity_convert_to_llm());
    config.options.stream.request.timeout_ms = Some(20);
    let idle: StreamFn = Arc::new(|_model, _context, _options| {
        let stream = AssistantMessageEventStream::assistant();
        stream.push(AssistantMessageEvent::Start { partial: assistant(Vec::new(), StopReason::Pending) });
        stream
    });
    let stream = agent_loop(vec![user_message("Hello")], context_with(Vec::new()), config, None, Some(idle));
    let _ = collect_with_timeout(&stream).await;
    let messages = stream.result().await.expect("result");
    let last = messages.last().and_then(AgentMessage::as_assistant).expect("assistant message");
    assert_eq!(last.stop_reason, StopReason::Error);
    assert!(last.error_message.as_deref().unwrap_or_default().contains("Idle timeout"));
}

#[tokio::test]
async fn aborts_the_provider_request_signal_when_the_caller_aborts_mid_stream() {
    let controller = AbortController::new();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let started_tx = std::sync::Mutex::new(Some(started_tx));
    let seen_signal: Arc<std::sync::Mutex<Option<maho_ai::utils::abort::AbortSignal>>> =
        Arc::new(std::sync::Mutex::new(None));
    let sink = Arc::clone(&seen_signal);
    let hanging: StreamFn = Arc::new(move |_model, _context, options| {
        *sink.lock().unwrap() = options.and_then(|options| options.simple.stream.request.signal);
        if let Some(sender) = started_tx.lock().unwrap().take() {
            let _ = sender.send(());
        }
        let stream = AssistantMessageEventStream::assistant();
        stream.push(AssistantMessageEvent::Start { partial: assistant(Vec::new(), StopReason::Pending) });
        stream
    });
    let stream = agent_loop(
        vec![user_message("Hello")],
        context_with(Vec::new()),
        AgentLoopConfig::new(test_model(), identity_convert_to_llm()),
        Some(controller.signal()),
        Some(hanging),
    );
    let collector = stream.clone();
    let task = tokio::spawn(async move { collect_events(&collector).await });
    started_rx.await.expect("stream function called");
    controller.abort(None);
    let events = tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .expect("terminates")
        .expect("join");
    assert_eq!(events.last().map(support::event_name), Some("agent_end"));
    let signal = seen_signal.lock().unwrap().clone().expect("request signal");
    assert!(signal.aborted());
}

#[tokio::test]
async fn attaches_fallback_error_details_when_a_terminal_error_event_omits_them() {
    let stream = agent_loop(
        vec![user_message("Hello")],
        context_with(Vec::new()),
        AgentLoopConfig::new(test_model(), identity_convert_to_llm()),
        None,
        Some(stream_fn_of(vec![
            AssistantMessageEvent::Start { partial: assistant(Vec::new(), StopReason::Pending) },
            AssistantMessageEvent::Error {
                reason: maho_ai::types::ErrorReason::Error,
                error: assistant(Vec::new(), StopReason::Error),
            },
        ])),
    );
    let _ = collect_with_timeout(&stream).await;
    let messages = stream.result().await.expect("result");
    let last = messages.last().and_then(AgentMessage::as_assistant).expect("assistant message");
    assert_eq!(last.stop_reason, StopReason::Error);
    assert_eq!(last.error_message.as_deref(), Some("Error"));
}

#[tokio::test]
async fn applies_transform_context_before_convert_to_llm() {
    let order = Arc::new(std::sync::Mutex::new(Vec::new()));
    let transform_order = Arc::clone(&order);
    let convert_order = Arc::clone(&order);
    let mut config = AgentLoopConfig::new(
        test_model(),
        Arc::new(move |messages: Vec<AgentMessage>| {
            convert_order.lock().unwrap().push("convert".to_owned());
            Box::pin(async move { messages.into_iter().map(AgentMessage::into_llm).collect() })
        }),
    );
    config.transform_context = Some(Arc::new(move |messages: Vec<AgentMessage>, _signal| {
        transform_order.lock().unwrap().push("transform".to_owned());
        Box::pin(async move { messages })
    }));
    let stream = agent_loop(
        vec![user_message("Hello")],
        context_with(Vec::new()),
        config,
        None,
        Some(scripted_stream_fn(vec![assistant(vec![text_block("Hi")], StopReason::Stop)])),
    );
    let _ = collect_with_timeout(&stream).await;
    assert_eq!(*order.lock().unwrap(), vec!["transform".to_owned(), "convert".to_owned()]);
}

#[tokio::test]
async fn handles_tool_calls_and_results() {
    let (tool, seen) = recording_tool("weather");
    let stream = agent_loop(
        vec![user_message("Weather?")],
        context_with(vec![tool]),
        AgentLoopConfig::new(test_model(), identity_convert_to_llm()),
        None,
        Some(scripted_stream_fn(vec![
            assistant(vec![tool_call("call-1", "weather", json!({ "city": "Seoul" }))], StopReason::ToolUse),
            assistant(vec![text_block("done")], StopReason::Stop),
        ])),
    );
    let events = collect_with_timeout(&stream).await;
    let messages = stream.result().await.expect("result");
    assert_eq!(*seen.lock().unwrap(), vec![json!({ "city": "Seoul" })]);
    let tool_results: Vec<&ToolResultMessage> = messages
        .iter()
        .filter_map(|message| match message {
            AgentMessage::Llm(Message::ToolResult(result)) => Some(result),
            _ => None,
        })
        .collect();
    assert_eq!(tool_results.len(), 1);
    assert_eq!(tool_results[0].tool_name, "weather");
    assert_eq!(events.iter().filter(|event| matches!(event, AgentEvent::ToolExecutionStart { .. })).count(), 1);
    assert_eq!(events.iter().filter(|event| matches!(event, AgentEvent::ToolExecutionEnd { .. })).count(), 1);
}

#[tokio::test]
async fn does_not_execute_tool_calls_from_a_length_truncated_assistant_message() {
    let (tool, seen) = recording_tool("weather");
    let stream = agent_loop(
        vec![user_message("Weather?")],
        context_with(vec![tool]),
        AgentLoopConfig::new(test_model(), identity_convert_to_llm()),
        None,
        Some(scripted_stream_fn(vec![
            assistant(vec![tool_call("call-1", "weather", json!({ "city": "Seoul" }))], StopReason::Length),
            assistant(vec![text_block("done")], StopReason::Stop),
        ])),
    );
    let _ = collect_with_timeout(&stream).await;
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn returns_a_registered_removed_tool_hint_before_extension_hooks() {
    let before_calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&before_calls);
    let mut config = AgentLoopConfig::new(test_model(), identity_convert_to_llm());
    config.removed_tool_hints = Some(
        [("exec".to_owned(), "exec was removed; use eval({ language: \"js\", code }) instead.".to_owned())]
            .into_iter()
            .collect(),
    );
    config.before_tool_call = Some(Arc::new(move |_context, _signal| {
        counter.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { None })
    }));
    let stream = agent_loop(
        vec![user_message("run code")],
        context_with(Vec::new()),
        config,
        None,
        Some(scripted_stream_fn(vec![
            assistant(vec![tool_call("removed-exec", "exec", json!({}))], StopReason::ToolUse),
            assistant(vec![text_block("done")], StopReason::Stop),
        ])),
    );
    let _ = collect_with_timeout(&stream).await;
    let messages = stream.result().await.expect("result");
    let result = messages
        .iter()
        .find_map(|message| match message {
            AgentMessage::Llm(Message::ToolResult(result)) => Some(result),
            _ => None,
        })
        .expect("tool result");
    assert!(result.is_error);
    assert_eq!(
        result.content,
        vec![text_block(
            "Tool exec not found. exec was removed; use eval({ language: \"js\", code }) instead."
        )]
    );
    assert_eq!(before_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn emits_tool_execution_end_in_completion_order_but_persists_tool_results_in_source_order() {
    let first = result_tool("first", AgentToolResult::text("first done"));
    let second = result_tool("second", AgentToolResult::text("second done"));
    let stream = agent_loop(
        vec![user_message("go")],
        context_with(vec![first, second]),
        AgentLoopConfig::new(test_model(), identity_convert_to_llm()),
        None,
        Some(scripted_stream_fn(vec![
            assistant(
                vec![
                    tool_call("call-1", "first", json!({})),
                    tool_call("call-2", "second", json!({})),
                ],
                StopReason::ToolUse,
            ),
            assistant(vec![text_block("done")], StopReason::Stop),
        ])),
    );
    let _ = collect_with_timeout(&stream).await;
    let messages = stream.result().await.expect("result");
    let persisted: Vec<String> = messages
        .iter()
        .filter_map(|message| match message {
            AgentMessage::Llm(Message::ToolResult(result)) => Some(result.tool_name.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(persisted, vec!["first".to_owned(), "second".to_owned()]);
}

#[tokio::test]
async fn keeps_sequential_tool_calls_mutually_exclusive_with_default_parallel_config() {
    let mut sequential = result_tool("sequential_tool", AgentToolResult::text("ok"));
    sequential.execution_mode = Some(ToolExecutionMode::Sequential);
    let concurrent = result_tool("parallel_tool", AgentToolResult::text("ok"));
    let stream = agent_loop(
        vec![user_message("go")],
        context_with(vec![sequential, concurrent]),
        AgentLoopConfig::new(test_model(), identity_convert_to_llm()),
        None,
        Some(scripted_stream_fn(vec![
            assistant(
                vec![
                    tool_call("call-1", "sequential_tool", json!({})),
                    tool_call("call-2", "parallel_tool", json!({})),
                ],
                StopReason::ToolUse,
            ),
            assistant(vec![text_block("done")], StopReason::Stop),
        ])),
    );
    let _ = collect_with_timeout(&stream).await;
    let messages = stream.result().await.expect("result");
    let persisted: Vec<String> = messages
        .iter()
        .filter_map(|message| match message {
            AgentMessage::Llm(Message::ToolResult(result)) => Some(result.tool_name.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(persisted, vec!["sequential_tool".to_owned(), "parallel_tool".to_owned()]);
}

#[tokio::test]
async fn uses_prepare_next_turn_snapshot_before_continuing() {
    let mut config = AgentLoopConfig::new(test_model(), identity_convert_to_llm());
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    config.prepare_next_turn = Some(Arc::new(move |_context| {
        counter.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { None })
    }));
    let stream = agent_loop(
        vec![user_message("go")],
        context_with(Vec::new()),
        config,
        None,
        Some(scripted_stream_fn(vec![assistant(vec![text_block("hi")], StopReason::Stop)])),
    );
    let _ = collect_with_timeout(&stream).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn stops_after_the_current_turn_when_should_stop_after_turn_returns_true() {
    let mut config = AgentLoopConfig::new(test_model(), identity_convert_to_llm());
    config.should_stop_after_turn = Some(Arc::new(|_context| Box::pin(async { true })));
    let stream_fn = scripted_stream_fn(vec![assistant(vec![text_block("hi")], StopReason::Stop)]);
    let recording = Arc::clone(&stream_fn);
    let stream = agent_loop(
        vec![user_message("go")],
        context_with(Vec::new()),
        config,
        None,
        Some(Arc::new(move |model, context, options| recording(model, context, options))),
    );
    let _ = collect_with_timeout(&stream).await;
    let messages = stream.result().await.expect("result");
    assert_eq!(messages.len(), 2);
}

#[tokio::test]
async fn stops_after_a_tool_batch_when_every_tool_result_sets_terminate_true() {
    let mut terminating = AgentToolResult::text("stopping");
    terminating.terminate = Some(true);
    let tool = result_tool("terminating_tool", terminating);
    let stream = agent_loop(
        vec![user_message("go")],
        context_with(vec![tool]),
        AgentLoopConfig::new(test_model(), identity_convert_to_llm()),
        None,
        Some(scripted_stream_fn(vec![
            assistant(vec![tool_call("call-1", "terminating_tool", json!({}))], StopReason::ToolUse),
            assistant(vec![text_block("should not run")], StopReason::Stop),
        ])),
    );
    let _ = collect_with_timeout(&stream).await;
    let messages = stream.result().await.expect("result");
    assert!(messages.iter().all(|message| {
        message.as_assistant().map(|assistant| assistant.content.iter().all(|block| {
            !matches!(block, ContentBlock::Text(text) if text.text == "should not run")
        })).unwrap_or(true)
    }));
}

#[tokio::test]
async fn throws_when_context_has_no_messages() {
    let context = AgentContext { system_prompt: String::new(), messages: Vec::new(), tools: Some(Vec::new()) };
    let config = AgentLoopConfig::new(test_model(), identity_convert_to_llm());
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = agent_loop_continue(context, config, None, Some(scripted_stream_fn(Vec::new())));
    }));
    assert!(result.is_err());
}

#[tokio::test]
async fn continues_from_existing_context_without_emitting_user_message_events() {
    let mut context = context_with(Vec::new());
    context.messages.push(user_message("earlier"));
    let stream = agent_loop_continue(
        context,
        AgentLoopConfig::new(test_model(), identity_convert_to_llm()),
        None,
        Some(scripted_stream_fn(vec![assistant(vec![text_block("hi")], StopReason::Stop)])),
    );
    let events = collect_with_timeout(&stream).await;
    assert_eq!(events.first().map(support::event_name), Some("agent_start"));
    assert_eq!(events.last().map(support::event_name), Some("agent_end"));
}

#[tokio::test]
async fn the_recording_sink_receives_the_same_sequence_as_the_stream() {
    let (emit, recorded) = support::recording_sink();
    let stream_fn: StreamFn = scripted_stream_fn(vec![assistant(vec![text_block("hi")], StopReason::Stop)]);
    run_agent_loop(
        vec![user_message("Hello")],
        context_with(Vec::new()),
        AgentLoopConfig::new(test_model(), identity_convert_to_llm()),
        emit,
        None,
        Some(stream_fn),
    )
    .await;
    let events = recorded.lock().unwrap().clone();
    assert_eq!(events.first().map(support::event_name), Some("agent_start"));
    assert_eq!(events.last().map(support::event_name), Some("agent_end"));
}
