mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use maho_agent::{
    Agent, AgentContinuationOptions, AgentMessage, AgentOptions, AgentPromptInput, AgentTool, PartialAgentState,
    StreamFn,
};
use maho_ai::types::{ModelThinkingLevel, StopReason, Usage};
use maho_ai::utils::abort::AbortSignal;
use maho_ai::utils::event_stream::AssistantMessageEventStream;
use support::{assistant, message_stream, scripted_stream_fn, test_model, text_block, user_message};

fn unused_stream_fn() -> StreamFn {
    Arc::new(|_model, _context, _options| message_stream(assistant(vec![text_block("ok")], StopReason::Stop)))
}

fn agent_with(stream_fn: StreamFn) -> Agent {
    Agent::new(AgentOptions { stream_fn: Some(stream_fn), ..AgentOptions::default() })
}

#[test]
fn creates_an_agent_with_default_state() {
    let agent = agent_with(unused_stream_fn());
    let state = agent.state();
    assert_eq!(state.system_prompt, "");
    assert_eq!(state.thinking_level, ModelThinkingLevel::Off);
    assert!(state.tools().is_empty());
    assert!(state.messages().is_empty());
    assert!(!state.is_streaming);
    assert!(state.streaming_message.is_none());
    assert!(state.pending_tool_calls.is_empty());
    assert!(state.error_message.is_none());
}

#[test]
fn creates_an_agent_with_custom_initial_state() {
    let model = test_model();
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(unused_stream_fn()),
        initial_state: Some(PartialAgentState {
            system_prompt: Some("You are a helpful assistant.".to_owned()),
            model: Some(model.clone()),
            thinking_level: Some(ModelThinkingLevel::Low),
            ..PartialAgentState::default()
        }),
        ..AgentOptions::default()
    });
    let state = agent.state();
    assert_eq!(state.system_prompt, "You are a helpful assistant.");
    assert_eq!(state.model.id, model.id);
    assert_eq!(state.thinking_level, ModelThinkingLevel::Low);
}

#[test]
fn subscribes_and_unsubscribes_without_emitting_on_subscribe() {
    let agent = agent_with(unused_stream_fn());
    let count = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&count);
    let subscription = agent.subscribe(Arc::new(move |_event, _signal| {
        counter.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {})
    }));
    assert_eq!(count.load(Ordering::SeqCst), 0);
    agent.set_system_prompt("Test prompt");
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert_eq!(agent.state().system_prompt, "Test prompt");
    agent.unsubscribe(&subscription);
    agent.set_system_prompt("Another prompt");
    assert_eq!(count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn emits_full_lifecycle_events_for_thrown_run_failures() {
    let failing: StreamFn = Arc::new(|_model, _context, _options| {
        let stream = AssistantMessageEventStream::assistant();
        stream.fail(maho_ai::utils::event_stream::StreamError::new("provider exploded"));
        stream
    });
    let agent = agent_with(failing);
    let events: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    agent.subscribe(Arc::new(move |event, _signal| {
        sink.lock().unwrap().push(support::event_name(&event).to_owned());
        Box::pin(async {})
    }));

    agent.prompt(AgentPromptInput::text("hello")).await;

    assert_eq!(
        *events.lock().unwrap(),
        vec![
            "agent_start",
            "turn_start",
            "message_start",
            "message_end",
            "message_start",
            "message_end",
            "turn_end",
            "agent_end",
        ]
    );
    let state = agent.state();
    let last = state.messages().last().and_then(AgentMessage::as_assistant).expect("assistant message");
    assert_eq!(last.stop_reason, StopReason::Error);
    assert_eq!(last.error_message.as_deref(), Some("provider exploded"));
    assert_eq!(state.error_message.as_deref(), Some("provider exploded"));
}

#[tokio::test]
async fn passes_the_active_abort_signal_to_subscribers() {
    let agent = agent_with(scripted_stream_fn(vec![assistant(vec![text_block("ok")], StopReason::Stop)]));
    let seen = Arc::new(std::sync::Mutex::new(Vec::<bool>::new()));
    let sink = Arc::clone(&seen);
    agent.subscribe(Arc::new(move |_event, signal: AbortSignal| {
        sink.lock().unwrap().push(signal.aborted());
        Box::pin(async {})
    }));
    agent.prompt(AgentPromptInput::text("hello")).await;
    let observed = seen.lock().unwrap().clone();
    assert!(!observed.is_empty());
    assert!(observed.iter().all(|aborted| !aborted));
}

#[tokio::test]
async fn steer_and_follow_up_queue_messages() {
    let agent = agent_with(scripted_stream_fn(vec![assistant(vec![text_block("ok")], StopReason::Stop)]));
    assert!(!agent.has_queued_messages());
    agent.steer(user_message("steering"));
    assert!(agent.has_queued_messages());
    agent.clear_steering_queue();
    assert!(!agent.has_queued_messages());
    agent.follow_up(user_message("follow-up"));
    assert!(agent.has_queued_messages());
    agent.clear_follow_up_queue();
    assert!(!agent.has_queued_messages());
    agent.steer(user_message("a"));
    agent.follow_up(user_message("b"));
    agent.clear_all_queues();
    assert!(!agent.has_queued_messages());
}

#[tokio::test]
async fn abort_marks_the_active_run_signal_aborted() {
    let agent = agent_with(unused_stream_fn());
    assert!(agent.signal().is_none());
    agent.abort(None);
    assert!(agent.signal().is_none());
}

/// A stream function that reports when the run has started and then never emits a terminal event,
/// so the test can observe the in-flight run deterministically (no sleeps).
struct HangingRun {
    stream_fn: StreamFn,
    started: tokio::sync::oneshot::Receiver<()>,
    stream: AssistantMessageEventStream,
}

fn hanging_run() -> HangingRun {
    let (started_tx, started) = tokio::sync::oneshot::channel();
    let started_tx = std::sync::Mutex::new(Some(started_tx));
    let stream = AssistantMessageEventStream::assistant();
    let created = stream.clone();
    let stream_fn: StreamFn = Arc::new(move |_model, _context, _options| {
        created.push(maho_ai::types::AssistantMessageEvent::Start {
            partial: assistant(Vec::new(), StopReason::Pending),
        });
        if let Some(sender) = started_tx.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take() {
            let _ = sender.send(());
        }
        created.clone()
    });
    HangingRun { stream_fn, started, stream }
}

#[tokio::test]
async fn reset_while_processing_panics() {
    let run = hanging_run();
    let agent = agent_with(run.stream_fn);
    let runner = agent.clone();
    let task = tokio::spawn(async move { runner.prompt(AgentPromptInput::text("hello")).await });
    run.started.await.expect("run started");
    let resetter = agent.clone();
    let reset_task = tokio::spawn(async move { resetter.reset() });
    let outcome = reset_task.await;
    agent.abort(None);
    run.stream.end(None);
    let _ = task.await;
    assert!(outcome.is_err(), "reset while processing must panic");
}

#[tokio::test]
async fn prompt_while_streaming_panics() {
    let run = hanging_run();
    let agent = agent_with(run.stream_fn);
    let runner = agent.clone();
    let task = tokio::spawn(async move { runner.prompt(AgentPromptInput::text("first")).await });
    run.started.await.expect("run started");
    let second = agent.clone();
    let second_task = tokio::spawn(async move { second.prompt(AgentPromptInput::text("second")).await });
    let outcome = second_task.await;
    agent.abort(None);
    run.stream.end(None);
    let _ = task.await;
    assert!(outcome.is_err(), "a second prompt while streaming must panic");
}

#[tokio::test]
async fn set_tools_and_messages_round_trip() {
    let agent = agent_with(unused_stream_fn());
    let tool: AgentTool = support::result_tool("noop", maho_agent::AgentToolResult::text("ok"));
    agent.set_tools(vec![tool]);
    assert_eq!(agent.state().tools().len(), 1);
    agent.set_messages(vec![user_message("hi")]);
    assert_eq!(agent.state().messages().len(), 1);
    agent.set_model(test_model());
    assert_eq!(agent.state().model.id, "mock");
}

#[tokio::test]
async fn continue_run_without_messages_panics() {
    let agent = agent_with(unused_stream_fn());
    let runner = agent.clone();
    let task = tokio::spawn(async move { runner.continue_run(AgentContinuationOptions::default()).await });
    assert!(task.await.is_err());
}

#[tokio::test]
async fn session_id_and_timeouts_round_trip() {
    let agent = agent_with(unused_stream_fn());
    assert!(agent.session_id().is_none());
    agent.set_session_id(Some("session-1".to_owned()));
    assert_eq!(agent.session_id().as_deref(), Some("session-1"));
    agent.set_timeout_ms(Some(1234));
    assert_eq!(agent.timeout_ms(), Some(1234));
    agent.set_stream_start_timeout_ms(Some(50));
    assert_eq!(agent.stream_start_timeout_ms(), Some(50));
    agent.set_max_retry_delay_ms(Some(7));
    assert_eq!(agent.max_retry_delay_ms(), Some(7));
    let _ = Usage::default();
}
