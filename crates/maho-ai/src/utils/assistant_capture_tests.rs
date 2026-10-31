//! Ports of senpi packages/ai/test/capture-cost.test.ts, seed-frontier-capture.test.ts and the
//! capture cases of provider-error-redaction.test.ts. Uncloneable-payload failure isolation
//! (capture-cost "drops only the failing block's records") has no Rust analogue: every content
//! block is plain data and cloning cannot fail; the seed-frontier rejection below exercises the
//! same `drop_capture` isolation path instead.

use super::*;
use crate::types::{DoneReason, ErrorReason, StopReason, TextContent};
use crate::utils::event_stream::AssistantMessageEventStream;
use serde_json::json;

fn message() -> AssistantMessage {
    serde_json::from_value(json!({
        "role": "assistant", "api": "fixture", "provider": "fixture", "model": "fixture",
        "timestamp": 1, "content": [], "stopReason": "pending",
        "usage": { "input": 1, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 1,
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 } }
    }))
    .expect("message fixture")
}

fn finish(stream: &AssistantMessageEventStream, partial: &mut AssistantMessage) {
    partial.stop_reason = StopReason::Stop;
    stream.push(AssistantMessageEvent::Done {
        reason: DoneReason::Stop,
        message: partial.clone(),
    });
    stream.end(Some(partial.clone()));
}

fn kind(record: &AssistantRecord) -> &'static str {
    match record {
        Record::Progress(progress) => match progress.as_ref() {
            AssistantProgress::MessageBlockState { .. } => "message_block_state",
            AssistantProgress::MessageBlockMetadata(_) => "message_block_metadata",
            AssistantProgress::MessageToolArgumentsReset { .. } => "message_tool_arguments_reset",
        },
        Record::Event(event) => match event.value {
            AssistantMessageEvent::Start { .. } => "start",
            AssistantMessageEvent::TextStart { .. } => "text_start",
            AssistantMessageEvent::TextDelta { .. } => "text_delta",
            AssistantMessageEvent::TextEnd { .. } => "text_end",
            AssistantMessageEvent::ThinkingStart { .. } => "thinking_start",
            AssistantMessageEvent::ThinkingDelta { .. } => "thinking_delta",
            AssistantMessageEvent::ThinkingEnd { .. } => "thinking_end",
            AssistantMessageEvent::ToolcallStart { .. } => "toolcall_start",
            AssistantMessageEvent::ToolcallDelta { .. } => "toolcall_delta",
            AssistantMessageEvent::ToolcallEnd { .. } => "toolcall_end",
            AssistantMessageEvent::Done { .. } => "done",
            AssistantMessageEvent::Error { .. } => "error",
        },
    }
}

fn capture_of(record: &AssistantRecord) -> Option<&AssistantCapture> {
    match record {
        Record::Event(event) => get_assistant_capture(event),
        Record::Progress(_) => None,
    }
}

fn set_text(partial: &mut AssistantMessage, index: usize, text: &str) {
    partial.content[index] = ContentBlock::Text(TextContent {
        text: text.into(),
        ..TextContent::default()
    });
}

#[tokio::test]
async fn builds_no_capture_object_or_progress_record_for_a_stream_with_no_internal_reader() {
    let stream = AssistantMessageEventStream::with_capture_blocks(true);
    let mut partial = message();
    stream.push(AssistantMessageEvent::Start {
        partial: partial.clone(),
    });
    partial.content.push(ContentBlock::text(""));
    stream.push(AssistantMessageEvent::TextStart {
        content_index: 0,
        partial: partial.clone(),
    });
    let mut text = String::new();
    for _ in 0..200 {
        text.push('x');
        set_text(&mut partial, 0, &text);
        stream.push(AssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: "x".into(),
            partial: partial.clone(),
        });
    }
    stream.push(AssistantMessageEvent::TextEnd {
        content_index: 0,
        content: text,
        partial: partial.clone(),
    });
    let native = ContentBlock::ProviderNative(crate::types::ProviderNativeContent {
        subtype: "fixture".into(),
        raw: json!({ "payload": "x".repeat(4096) }),
    });
    partial.content.push(native.clone());
    for phase in [BlockPhase::Insert, BlockPhase::End] {
        push_assistant_progress(&stream, AssistantProgress::block_state(1, phase, &native));
    }
    finish(&stream, &mut partial);
    // Attached only after every push: a reader that arrives late never turns capture on.
    let records = assistant_records(&stream).collect().await.expect("records");
    assert!(!records.iter().any(is_assistant_progress));
    assert!(records.iter().all(|record| capture_of(record).is_none()));
    let kinds: Vec<&str> = records.iter().map(kind).collect();
    let mut expected = vec!["start", "text_start"];
    expected.extend(std::iter::repeat_n("text_delta", 200));
    expected.extend(["text_end", "done"]);
    assert_eq!(kinds, expected);
}

#[tokio::test]
async fn keeps_a_subscribed_delta_to_offsets_and_copies_usage_only_when_it_changes() {
    let stream = AssistantMessageEventStream::with_capture_blocks(true);
    let reader = assistant_records(&stream);
    let mut partial = message();
    stream.push(AssistantMessageEvent::Start {
        partial: partial.clone(),
    });
    partial.content.push(ContentBlock::text(""));
    stream.push(AssistantMessageEvent::TextStart {
        content_index: 0,
        partial: partial.clone(),
    });
    let mut text = String::new();
    for index in 0..1000 {
        if index == 500 {
            partial.usage.output = 7;
        }
        text.push_str("xy");
        stream.push(AssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: "xy".into(),
            partial: partial.clone(),
        });
    }
    set_text(&mut partial, 0, &text);
    stream.push(AssistantMessageEvent::TextEnd {
        content_index: 0,
        content: text,
        partial: partial.clone(),
    });
    finish(&stream, &mut partial);
    let records = reader.collect().await.expect("records");
    let deltas: Vec<&AssistantCapture> = records
        .iter()
        .filter(|record| kind(record) == "text_delta")
        .map(|record| capture_of(record).expect("delta capture"))
        .collect();
    assert_eq!(
        deltas
            .iter()
            .map(|capture| capture.offset)
            .collect::<Vec<_>>(),
        (0..1000).map(|index| Some(index * 2)).collect::<Vec<_>>()
    );
    assert_eq!((deltas[499].usage.output, deltas[500].usage.output), (0, 7));
    assert!(deltas.iter().all(|capture| capture.metadata.is_none()));
}

#[tokio::test]
async fn shares_one_frozen_copy_between_a_native_insert_and_its_unchanged_end_never_a_stale_one() {
    let stream = AssistantMessageEventStream::with_capture_blocks(true);
    let reader = assistant_records(&stream);
    let mut partial = message();
    stream.push(AssistantMessageEvent::Start {
        partial: partial.clone(),
    });
    let image = ContentBlock::ProviderNative(crate::types::ProviderNativeContent {
        subtype: "image".into(),
        raw: json!({ "result": "x".repeat(1 << 20) }),
    });
    for phase in [BlockPhase::Insert, BlockPhase::End] {
        push_assistant_progress(&stream, AssistantProgress::block_state(0, phase, &image));
    }
    let mut search = crate::types::ProviderNativeContent {
        subtype: "search".into(),
        raw: json!({ "status": "in_progress" }),
    };
    push_assistant_progress(
        &stream,
        AssistantProgress::block_state(
            1,
            BlockPhase::Insert,
            &ContentBlock::ProviderNative(search.clone()),
        ),
    );
    search.raw = json!({ "status": "completed" });
    push_assistant_progress(
        &stream,
        AssistantProgress::block_state(1, BlockPhase::End, &ContentBlock::ProviderNative(search)),
    );
    finish(&stream, &mut partial);
    let states: Vec<Arc<ContentBlock>> = reader
        .collect()
        .await
        .expect("records")
        .into_iter()
        .filter_map(|record| match record {
            Record::Progress(progress) => match progress.as_ref() {
                AssistantProgress::MessageBlockState { block, .. } => Some(block.clone()),
                _ => None,
            },
            Record::Event(_) => None,
        })
        .collect();
    assert_eq!(states.len(), 4);
    assert!(Arc::ptr_eq(&states[0], &states[1]));
    let raw = |block: &ContentBlock| match block {
        ContentBlock::ProviderNative(native) => native.raw.clone(),
        _ => Value::Null,
    };
    assert_eq!(raw(&states[2]), json!({ "status": "in_progress" }));
    assert_eq!(raw(&states[3]), json!({ "status": "completed" }));
}

// The capture subscription is process-wide, so these ports turn capture on per stream with an
// internal reader attached before the first push instead of `subscribeAssistantCapture`.
fn start_at(offset: i64) -> (Option<AssistantCapture>, Option<AssistantCapture>) {
    let mut partial = message();
    partial.content.push(ContentBlock::text("Hé"));
    let stream = AssistantMessageEventStream::with_capture_blocks(true);
    let reader = assistant_records(&stream);
    stream.push_with_hint(
        AssistantMessageEvent::TextStart {
            content_index: 0,
            partial: partial.clone(),
        },
        CaptureHint::offset(offset),
    );
    stream.push(AssistantMessageEvent::TextDelta {
        content_index: 0,
        delta: String::new(),
        partial: partial.clone(),
    });
    stream.end(Some(partial));
    let records = futures::executor::block_on(reader.collect()).expect("records");
    (
        capture_of(&records[0]).cloned(),
        capture_of(&records[1]).cloned(),
    )
}

#[test]
fn distinguishes_identical_replay_and_continuation_seeds_before_any_delta_is_read() {
    let (replay_start, replay_delta) = start_at(0);
    let (continuation_start, continuation_delta) = start_at(2);
    let replay_start = replay_start.expect("replay start");
    assert_eq!(
        (replay_start.covered_code_units, replay_start.next_offset),
        (Some(2), Some(0))
    );
    let continuation_start = continuation_start.expect("continuation start");
    assert_eq!(
        (
            continuation_start.covered_code_units,
            continuation_start.next_offset
        ),
        (Some(2), Some(2))
    );
    assert_eq!(replay_delta.and_then(|capture| capture.offset), Some(0));
    assert_eq!(
        continuation_delta.and_then(|capture| capture.offset),
        Some(2)
    );
}

#[test]
fn rejects_unsupported_producer_frontiers_without_disrupting_the_legacy_stream() {
    for offset in [-1, 1, 3] {
        let (start, delta) = start_at(offset);
        assert!(start.is_none(), "{offset}");
        assert!(delta.is_none(), "{offset}");
    }
}

#[tokio::test]
async fn relays_the_producer_frontier_rather_than_the_later_partial_length() {
    for offset in [0usize, 2] {
        let mut partial = message();
        partial.content.push(ContentBlock::text("Hé"));
        let source = AssistantMessageEventStream::with_capture_blocks(true);
        let source_reader = assistant_records(&source);
        source.push_with_hint(
            AssistantMessageEvent::TextStart {
                content_index: 0,
                partial: partial.clone(),
            },
            CaptureHint::offset(offset as i64),
        );
        source.end(Some(partial));
        let Some(Record::Event(start)) = source_reader.next().await.expect("record") else {
            panic!("expected the start event");
        };
        let mut later = message();
        later.content.push(ContentBlock::text("later bytes"));
        let relay = AssistantMessageEventStream::with_capture_blocks(true);
        let relay_reader = assistant_records(&relay);
        relay.push_with_hint(
            AssistantMessageEvent::TextStart {
                content_index: 0,
                partial: later.clone(),
            },
            CaptureHint {
                carried: Some(start.capture.clone()),
                ..CaptureHint::default()
            },
        );
        relay.end(Some(later));
        let records = relay_reader.collect().await.expect("records");
        let capture = capture_of(&records[0]).expect("relayed capture");
        assert_eq!(capture.block.as_deref(), Some(&ContentBlock::text("Hé")));
        assert_eq!(
            (capture.covered_code_units, capture.next_offset),
            (Some(2), Some(offset))
        );
    }
}

#[tokio::test]
async fn redacts_streamed_provider_errors_before_capture_freezes_their_persisted_bytes() {
    let stream = AssistantMessageEventStream::with_capture_blocks(true);
    let records = assistant_records(&stream);
    let partial = crate::utils::provider_error::tests::failed_message(
        &crate::utils::provider_error::tests::model_not_found(),
        StopReason::Error,
    );
    stream.push(AssistantMessageEvent::Start {
        partial: partial.clone(),
    });
    stream.push(AssistantMessageEvent::Error {
        reason: ErrorReason::Error,
        error: partial,
    });
    stream.end(None);
    let records = records.collect().await.expect("records");
    assert_eq!(records.len(), 2);
    for record in &records {
        let captured = capture_of(record)
            .and_then(|capture| capture.message.clone())
            .expect("capture message");
        let text = captured.error_message.clone().expect("error text");
        assert!(text.contains("[REDACTED]"));
        assert!(!text.contains("zzfixture4283valueQ"));
        assert_eq!(
            captured.error_cause,
            Some(crate::types::AssistantErrorCause::ModelConfig {
                model: "faux/faux-1".into()
            })
        );
    }
}

#[tokio::test]
async fn rejects_post_done_pushes_before_capture_or_message_mutation() {
    let stream = AssistantMessageEventStream::with_capture_blocks(true);
    let records = assistant_records(&stream);
    let mut done_message = message();
    done_message.stop_reason = StopReason::Stop;
    stream.push(AssistantMessageEvent::Done {
        reason: DoneReason::Stop,
        message: done_message.clone(),
    });
    stream.push(AssistantMessageEvent::Start {
        partial: crate::utils::provider_error::tests::failed_message(
            &crate::utils::provider_error::tests::model_not_found(),
            StopReason::Error,
        ),
    });
    assert_eq!(stream.result().await, Ok(done_message));
    let observed = records.collect().await.expect("records");
    assert_eq!(observed.iter().map(kind).collect::<Vec<_>>(), ["done"]);
}

#[test]
fn a_held_subscription_is_released_exactly_once() {
    // A counter of its own: the process-global one would turn capture on for streams other tests
    // push in parallel.
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let mut subscription = AssistantCaptureSubscription::on(&COUNTER);
    let second = AssistantCaptureSubscription::on(&COUNTER);
    assert_eq!(COUNTER.load(Ordering::SeqCst), 2);
    subscription.release();
    subscription.release();
    assert_eq!(COUNTER.load(Ordering::SeqCst), 1);
    drop(subscription);
    assert_eq!(COUNTER.load(Ordering::SeqCst), 1);
    drop(second);
    assert_eq!(COUNTER.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn the_public_iterator_never_yields_progress_records() {
    let stream = AssistantMessageEventStream::with_capture_blocks(true);
    let _reader = assistant_records(&stream);
    let mut partial = message();
    stream.push(AssistantMessageEvent::Start {
        partial: partial.clone(),
    });
    push_assistant_complete_block(&stream, 0, &ContentBlock::text("native"));
    finish(&stream, &mut partial);
    let events = stream.collect().await.expect("events");
    assert_eq!(events.len(), 2);
}

#[tokio::test]
async fn assistant_events_hands_progress_to_the_callback_in_order() {
    let stream = AssistantMessageEventStream::with_capture_blocks(true);
    let mut seen: Vec<String> = Vec::new();
    {
        let _demand = assistant_records(&stream);
        let mut partial = message();
        stream.push(AssistantMessageEvent::Start {
            partial: partial.clone(),
        });
        push_assistant_progress(
            &stream,
            AssistantProgress::metadata(BlockMetadata {
                content_index: 0,
                name: Some("late".into()),
                ..BlockMetadata::default()
            }),
        );
        finish(&stream, &mut partial);
    }
    let mut events = assistant_events(&stream, |progress| {
        seen.push(format!("progress:{}", progress.content_index()))
    });
    let mut kinds = Vec::new();
    while let Some(event) = events.next().await.expect("event") {
        kinds.push(kind(&Record::Event(event)));
    }
    drop(events);
    assert_eq!(kinds, ["start", "done"]);
    assert_eq!(seen, ["progress:0"]);
    assert_eq!(
        serde_json::to_value(AssistantProgress::metadata(BlockMetadata {
            content_index: 2,
            redacted: Some(true),
            ..BlockMetadata::default()
        }))
        .expect("serialize"),
        json!({ "type": "message_block_metadata", "contentIndex": 2, "redacted": true })
    );
}
