//! Ports of senpi packages/agent/test/internal-channel-lifecycle.test.ts,
//! internal-channel-capability.test.ts, internal-events.test.ts and internal-events-entry.test.ts.

use super::*;

use std::sync::atomic::{AtomicUsize, Ordering};

use maho_ai::types::{
    AssistantMessage, AssistantMessageEvent, ContentBlock, DoneReason, ProviderNativeContent,
    StopReason, ThinkingContent,
};
use maho_ai::utils::assistant_capture::{
    AssistantProgress, BlockMetadata, BlockPhase, get_assistant_capture, push_assistant_progress,
};
use maho_ai::utils::event_stream::AssistantMessageEventStream;

use crate::agent::{Agent, AgentOptions, AgentPromptInput, PartialAgentState};
use crate::types::StreamFn;

fn message() -> AssistantMessage {
    let mut message = crate::assistant_terminal_state::create_terminal_failure_assistant_message(
        &crate::agent::default_model(),
        StopReason::Stop,
        &crate::assistant_terminal_state::AgentStreamError::Other {
            message: String::new(),
        },
        None,
    );
    message.content.clear();
    message.error_message = None;
    message.timestamp = 1;
    message
}

fn recorder() -> (
    Arc<Mutex<Vec<OrderedAgentRecord>>>,
    Arc<dyn Fn(OrderedAgentRecord) -> BoxFuture<'static, ()> + Send + Sync>,
) {
    let records: Arc<Mutex<Vec<OrderedAgentRecord>>> = Arc::default();
    let sink = records.clone();
    (
        records,
        Arc::new(move |record| {
            sink.lock().expect("records").push(record);
            Box::pin(async {})
        }),
    )
}

/// Whether a capture-enabled stream created now captures its first event.
fn captured() -> bool {
    let stream = AssistantMessageEventStream::with_capture_blocks(true);
    stream.push(AssistantMessageEvent::Start { partial: message() });
    let records = maho_ai::utils::assistant_capture::assistant_records(&stream);
    let first = futures::executor::block_on(records.next()).expect("record");
    matches!(first, Some(maho_ai::utils::assistant_capture::AssistantRecord::Event(event)) if get_assistant_capture(&event).is_some())
}

fn stream_fn_of(stream: AssistantMessageEventStream) -> StreamFn {
    Arc::new(move |_, _, _| stream.clone())
}

fn progress_of(records: &[OrderedAgentRecord]) -> Vec<AssistantProgress> {
    records
        .iter()
        .filter_map(|record| match &record.record {
            InternalEvent::Progress(progress) => Some((**progress).clone()),
            InternalEvent::Event(_) => None,
        })
        .collect()
}

fn sequences_are_dense(records: &[OrderedAgentRecord]) -> bool {
    records
        .iter()
        .enumerate()
        .all(|(index, record)| record.sequence == index as u64 + 1)
}

// internal-channel-lifecycle.test.ts. The capture subscription counter is process-global, as in
// TS; the assertions below only hold while no other test in this binary holds a subscription, so
// they check the transitions this channel owns rather than absolute capture state.
#[test]
fn holds_capture_only_between_the_first_subscription_and_last_release() {
    let channel = create_internal_agent_channel();
    assert!(internal_loop_sink(Some(&channel)).is_none());
    assert!(
        dispatch_internal_agent(
            Some(&channel),
            InternalEvent::Event(Captured::bare(AgentEvent::AgentStart))
        )
        .is_none()
    );
    let first = channel.subscribe(Arc::new(|_| Box::pin(async {})));
    let second = channel.subscribe(Arc::new(|_| Box::pin(async {})));
    assert!(captured());
    assert!(channel.state().release_capture.is_some());
    first();
    assert!(captured());
    assert!(channel.state().release_capture.is_some());
    second();
    assert!(channel.state().release_capture.is_none());
    assert!(internal_loop_sink(Some(&channel)).is_none());
    first();
    second();
}

#[test]
fn gives_a_run_started_without_listeners_no_progress_sink_even_after_a_late_subscription() {
    let channel = create_internal_agent_channel();
    let run_config = internal_loop_sink(Some(&channel));
    let release = channel.subscribe(Arc::new(|_| Box::pin(async {})));
    assert!(run_config.is_none());
    assert!(internal_loop_sink(Some(&channel)).is_some());
    release();
}

// internal-channel-capability.test.ts
#[tokio::test]
async fn cannot_attach_a_fresh_channel_to_an_existing_agent() {
    let failing = AssistantMessageEventStream::assistant();
    failing.fail(maho_ai::utils::event_stream::StreamError::new(
        "fixture provider failed",
    ));
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(stream_fn_of(failing)),
        ..AgentOptions::default()
    });
    let channel = create_internal_agent_channel();
    let (records, listener) = recorder();
    let release = channel.subscribe(listener);
    agent.prompt(AgentPromptInput::text("go")).await;
    release();
    assert!(records.lock().expect("records").is_empty());
}

#[tokio::test]
async fn delivers_ordered_records_only_to_its_construction_channel() {
    let failing = AssistantMessageEventStream::assistant();
    failing.fail(maho_ai::utils::event_stream::StreamError::new(
        "fixture provider failed",
    ));
    let channel = create_internal_agent_channel();
    let agent = Agent::new(AgentOptions {
        internal_channel: Some(channel.clone()),
        stream_fn: Some(stream_fn_of(failing)),
        ..AgentOptions::default()
    });
    let (records, listener) = recorder();
    let release = channel.subscribe(listener);
    agent.prompt(AgentPromptInput::text("go")).await;
    release();
    let records = records.lock().expect("records");
    assert!(!records.is_empty());
    assert!(sequences_are_dense(&records));
}

// internal-events.test.ts
#[tokio::test]
async fn delivers_native_only_progress_before_any_following_public_event() {
    let source = AssistantMessageEventStream::with_capture_blocks(true);
    let channel = create_internal_agent_channel();
    let agent = Agent::new(AgentOptions {
        internal_channel: Some(channel.clone()),
        stream_fn: Some(stream_fn_of(source.clone())),
        ..AgentOptions::default()
    });
    let (records, listener) = recorder();
    let release = channel.subscribe(listener);
    let public: Arc<Mutex<Vec<AgentEvent>>> = Arc::default();
    let public_sink = public.clone();
    agent.subscribe(Arc::new(move |event, _| {
        public_sink.lock().expect("public").push(event);
        Box::pin(async {})
    }));
    let mut partial = message();
    source.push(AssistantMessageEvent::Start {
        partial: partial.clone(),
    });
    let native = |value: i64| {
        ContentBlock::ProviderNative(ProviderNativeContent {
            subtype: "fixture".into(),
            raw: serde_json::json!({ "value": value }),
        })
    };
    partial.content.push(native(1));
    push_assistant_progress(
        &source,
        AssistantProgress::block_state(0, BlockPhase::Insert, &native(1)),
    );
    push_assistant_progress(
        &source,
        AssistantProgress::block_state(0, BlockPhase::End, &native(2)),
    );
    partial.content[0] = native(2);
    source.push(AssistantMessageEvent::Done {
        reason: DoneReason::Stop,
        message: partial.clone(),
    });
    source.end(Some(partial));
    agent.prompt(AgentPromptInput::text("fixture")).await;
    release();

    let records = records.lock().expect("records");
    let progress = progress_of(&records);
    assert_eq!(progress.len(), 2);
    assert!(
        matches!(&progress[0], AssistantProgress::MessageBlockState { phase: BlockPhase::Insert, block, .. } if **block == native(1))
    );
    assert!(
        matches!(&progress[1], AssistantProgress::MessageBlockState { phase: BlockPhase::End, block, .. } if **block == native(2))
    );
    let public = public.lock().expect("public");
    assert!(
        !public
            .iter()
            .any(|event| matches!(event, AgentEvent::MessageUpdate { .. }))
    );
    let start = records
        .iter()
        .find_map(|record| match &record.record {
            InternalEvent::Event(event)
                if matches!(&event.value, AgentEvent::MessageStart { message, .. } if message.role() == "assistant") =>
            {
                Some(event.clone())
            }
            _ => None,
        })
        .expect("assistant message_start reached the internal listener");
    assert_eq!(
        get_assistant_capture(&start)
            .and_then(|capture| capture.message.as_ref())
            .map(|message| message.content.len()),
        Some(0)
    );
    assert!(sequences_are_dense(&records));
}

#[tokio::test]
async fn stops_delivering_to_a_removed_internal_listener_while_a_kept_one_still_receives() {
    let source = AssistantMessageEventStream::assistant();
    source.push(AssistantMessageEvent::Done {
        reason: DoneReason::Stop,
        message: message(),
    });
    let channel = create_internal_agent_channel();
    let agent = Agent::new(AgentOptions {
        internal_channel: Some(channel.clone()),
        stream_fn: Some(stream_fn_of(source)),
        ..AgentOptions::default()
    });
    let removed = Arc::new(AtomicUsize::new(0));
    let kept = Arc::new(AtomicUsize::new(0));
    let removed_count = removed.clone();
    channel.subscribe(Arc::new(move |_| {
        removed_count.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {})
    }))();
    let kept_count = kept.clone();
    let unsubscribe = channel.subscribe(Arc::new(move |_| {
        kept_count.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {})
    }));
    agent.prompt(AgentPromptInput::text("fixture")).await;
    unsubscribe();
    assert!(kept.load(Ordering::SeqCst) > 0);
    assert_eq!(removed.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn orders_agent_thinking_timing_after_the_event_it_amends() {
    let source = AssistantMessageEventStream::with_capture_blocks(true);
    let channel = create_internal_agent_channel();
    let agent = Agent::new(AgentOptions {
        internal_channel: Some(channel.clone()),
        stream_fn: Some(stream_fn_of(source.clone())),
        ..AgentOptions::default()
    });
    let (records, listener) = recorder();
    let unsubscribe = channel.subscribe(listener);
    let mut partial = message();
    source.push(AssistantMessageEvent::Start {
        partial: partial.clone(),
    });
    partial
        .content
        .push(ContentBlock::Thinking(ThinkingContent::default()));
    source.push(AssistantMessageEvent::ThinkingStart {
        content_index: 0,
        partial: partial.clone(),
    });
    source.push(AssistantMessageEvent::ThinkingEnd {
        content_index: 0,
        content: String::new(),
        partial: partial.clone(),
    });
    source.push(AssistantMessageEvent::Done {
        reason: DoneReason::Stop,
        message: partial.clone(),
    });
    source.end(Some(partial));
    agent.prompt(AgentPromptInput::text("fixture")).await;
    unsubscribe();
    // The provider captured each block before the agent stamped its timing, so the metadata must
    // follow it.
    let order: Vec<&str> = records
        .lock()
        .expect("records")
        .iter()
        .filter_map(|record| match &record.record {
            InternalEvent::Progress(progress) => match &**progress {
                AssistantProgress::MessageBlockMetadata(BlockMetadata {
                    started_at: Some(_),
                    ended_at: Some(_),
                    ..
                }) => Some("startedAt+endedAt"),
                AssistantProgress::MessageBlockMetadata(BlockMetadata {
                    started_at: Some(_),
                    ..
                }) => Some("startedAt"),
                _ => Some(""),
            },
            InternalEvent::Event(event) => match &event.value {
                AgentEvent::MessageUpdate {
                    assistant_message_event: AssistantMessageEvent::ThinkingStart { .. },
                    ..
                } => Some("thinking_start"),
                AgentEvent::MessageUpdate {
                    assistant_message_event: AssistantMessageEvent::ThinkingEnd { .. },
                    ..
                } => Some("thinking_end"),
                _ => None,
            },
        })
        .collect();
    assert_eq!(
        order,
        vec![
            "thinking_start",
            "startedAt",
            "thinking_end",
            "startedAt+endedAt"
        ]
    );
}

// #4272: progress records are not provider liveness; the idle bound measures silence between
// public events as before.
#[tokio::test(start_paused = true)]
async fn does_not_let_internal_progress_extend_the_provider_idle_bound() {
    let source = AssistantMessageEventStream::with_capture_blocks(true);
    let channel = create_internal_agent_channel();
    let agent = Agent::new(AgentOptions {
        internal_channel: Some(channel.clone()),
        stream_fn: Some(stream_fn_of(source.clone())),
        timeout_ms: Some(100),
        initial_state: Some(PartialAgentState::default()),
        ..AgentOptions::default()
    });
    let ends: Arc<Mutex<Vec<AssistantMessage>>> = Arc::default();
    let ends_sink = ends.clone();
    agent.subscribe(Arc::new(move |event, _| {
        if let AgentEvent::MessageEnd { message } = &event
            && let Some(assistant) = message.as_assistant()
        {
            ends_sink.lock().expect("ends").push(assistant.clone());
        }
        Box::pin(async {})
    }));
    let (records, listener) = recorder();
    let unsubscribe = channel.subscribe(listener);
    source.push(AssistantMessageEvent::Start { partial: message() });
    let feeder = {
        let source = source.clone();
        tokio::spawn(async move {
            for _ in 0..3 {
                tokio::time::sleep(std::time::Duration::from_millis(40)).await;
                push_assistant_progress(
                    &source,
                    AssistantProgress::metadata(BlockMetadata {
                        content_index: 0,
                        thinking_signature_delta: Some("x".into()),
                        ..BlockMetadata::default()
                    }),
                );
            }
        })
    };
    agent.prompt(AgentPromptInput::text("fixture")).await;
    feeder.await.expect("feeder");
    unsubscribe();
    assert_eq!(progress_of(&records.lock().expect("records")).len(), 2);
    let ends = ends.lock().expect("ends");
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0].stop_reason, StopReason::Error);
    assert_eq!(
        ends[0].error_message.as_deref(),
        Some("Idle timeout waiting for provider stream after 100ms")
    );
}

// internal-events-entry.test.ts: the crate root exposes the channel constructor and record types
// only; `dispatch_internal_agent`/`internal_loop_sink` are `pub(crate)` (compile-time boundary),
// and channel ownership uses no process-global registry.
#[test]
fn keeps_channel_ownership_out_of_global_registries() {
    let source = include_str!("internal_events.rs");
    let code: String = source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect();
    assert!(!code.contains("static "));
    assert!(!code.contains("thread_local!"));
    let _: fn() -> crate::InternalAgentChannel = crate::create_internal_agent_channel;
}
