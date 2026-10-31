//! Port of senpi packages/ai/test/tool-call-middleware/capture-relay.test.ts.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};

use crate::tool_call_middleware::context_transformer::get_protocol;
use crate::tool_call_middleware::protocols::kimi_xtml::thinking_recovery_stream::wrap_stream_with_kimi_thinking_recovery;
use crate::tool_call_middleware::stream_wrapper::wrap_stream_with_tool_call_middleware;
use crate::tool_call_middleware::types::ToolCallFormat;
use crate::types::{
    AssistantMessage, AssistantMessageEvent, ContentBlock, DoneReason, StopReason, TextContent,
    ThinkingContent, Tool,
};
use crate::utils::assistant_capture::{
    AssistantProgress, BlockMetadata, Captured, assistant_records, get_assistant_capture,
};
use crate::utils::capture_test_records::{
    AliasedTurn, DEADLINE, assert_matches_object, block_json, block_types, capture, capture_json,
    delta_captures, drain_records, first_event, progress_records, public_bytes, public_events,
    record_kinds, records_on_foreign_blocks, subscribe_capture_for_test, terminal_message,
};
use crate::utils::event_stream::AssistantMessageEventStream;

const LEAKED_THINKING: &str = "plan<|open|>response answer";
const HERMES_TEXT: &str =
    r#"Before <tool_call>{"name":"get_weather","arguments":{"city":"Seoul"}}</tool_call> after"#;

/// stream-wrapper-fixtures.ts `createAssistantMessage`.
pub(super) fn create_assistant_message(content: Value, stop_reason: &str) -> AssistantMessage {
    crate::utils::capture_test_records::assistant_message(
        "openai-completions",
        "openai",
        "test-model",
        content,
        stop_reason,
    )
}

pub(super) fn weather_tool() -> Tool {
    Tool {
        name: "get_weather".into(),
        description: "Get weather for a location".into(),
        parameters: json!({
            "type": "object",
            "properties": { "city": { "type": "string" } },
            "required": ["city"]
        }),
        output_schema: None,
        freeform: None,
        constrained_sampling: None,
    }
}

fn thinking(text: &str) -> ContentBlock {
    ContentBlock::Thinking(ThinkingContent {
        thinking: text.into(),
        ..ThinkingContent::default()
    })
}

fn text(text: &str) -> ContentBlock {
    ContentBlock::text(text)
}

fn signature(content_index: usize, delta: &str) -> AssistantProgress {
    AssistantProgress::metadata(BlockMetadata {
        content_index,
        thinking_signature_delta: Some(delta.into()),
        ..BlockMetadata::default()
    })
}

fn text_signature(content_index: usize, value: &str) -> AssistantProgress {
    AssistantProgress::metadata(BlockMetadata {
        content_index,
        text_signature: Some(value.into()),
        ..BlockMetadata::default()
    })
}

fn set_thinking_signature(output: &mut AssistantMessage, index: usize, value: &str) {
    if let ContentBlock::Thinking(thinking) = &mut output.content[index] {
        thinking.thinking_signature = Some(value.into());
    }
}

/// `signedThinkingThenText`.
async fn signed_thinking_then_text(
    body: &str,
) -> (AssistantMessageEventStream, Vec<AssistantMessageEvent>) {
    let mut turn = AliasedTurn::new(true);
    let mut output = create_assistant_message(json!([]), "pending");
    turn.push(AssistantMessageEvent::Start {
        partial: output.clone(),
    });
    output.content.push(thinking(""));
    turn.push(AssistantMessageEvent::ThinkingStart {
        content_index: 0,
        partial: output.clone(),
    });
    output.content[0] = thinking("plan");
    turn.push(AssistantMessageEvent::ThinkingDelta {
        content_index: 0,
        delta: "plan".into(),
        partial: output.clone(),
    });
    set_thinking_signature(&mut output, 0, "sig");
    turn.progress(signature(0, "sig"));
    turn.push(AssistantMessageEvent::ThinkingEnd {
        content_index: 0,
        content: "plan".into(),
        partial: output.clone(),
    });
    if !body.is_empty() {
        output.content.push(text(""));
        turn.push(AssistantMessageEvent::TextStart {
            content_index: 1,
            partial: output.clone(),
        });
        output.content[1] = text(body);
        turn.push(AssistantMessageEvent::TextDelta {
            content_index: 1,
            delta: body.into(),
            partial: output.clone(),
        });
        turn.push(AssistantMessageEvent::TextEnd {
            content_index: 1,
            content: body.into(),
            partial: output.clone(),
        });
    }
    output.stop_reason = StopReason::Stop;
    turn.push(AssistantMessageEvent::Done {
        reason: DoneReason::Stop,
        message: output.clone(),
    });
    turn.finish(&output, Some(output.clone())).await
}

/// `leakedKimiThinking`.
async fn leaked_kimi_thinking() -> (AssistantMessageEventStream, Vec<AssistantMessageEvent>) {
    let mut turn = AliasedTurn::new(true);
    let mut output = create_assistant_message(json!([]), "pending");
    turn.push(AssistantMessageEvent::Start {
        partial: output.clone(),
    });
    output.content.push(thinking(""));
    turn.push(AssistantMessageEvent::ThinkingStart {
        content_index: 0,
        partial: output.clone(),
    });
    output.content[0] = thinking(LEAKED_THINKING);
    turn.push(AssistantMessageEvent::ThinkingDelta {
        content_index: 0,
        delta: LEAKED_THINKING.into(),
        partial: output.clone(),
    });
    set_thinking_signature(&mut output, 0, "sig");
    turn.progress(signature(0, "sig"));
    turn.push(AssistantMessageEvent::ThinkingEnd {
        content_index: 0,
        content: LEAKED_THINKING.into(),
        partial: output.clone(),
    });
    output.stop_reason = StopReason::Stop;
    turn.push(AssistantMessageEvent::Done {
        reason: DoneReason::Stop,
        message: output.clone(),
    });
    turn.finish(&output, Some(output.clone())).await
}

fn without_diagnostic_timestamps(mut message: AssistantMessage) -> AssistantMessage {
    for diagnostic in message.diagnostics.iter_mut().flatten() {
        diagnostic.timestamp = 0;
    }
    message
}

// Kimi thinking recovery relays capture records #4272

#[tokio::test]
async fn kimi_keeps_the_public_bytes_recorded_before_capture() {
    let _subscription = subscribe_capture_for_test();
    let (source, _) = leaked_kimi_thinking().await;
    let records = drain_records(&wrap_stream_with_kimi_thinking_recovery(source)).await;
    let expected: Value =
        serde_json::from_str(include_str!("relay_kimi_public_bytes.json")).expect("snapshot");
    assert_eq!(public_bytes(&public_events(&records)), expected);
}

#[tokio::test]
async fn kimi_passes_every_source_record_through_and_relays_the_rewritten_terminal() {
    let _subscription = subscribe_capture_for_test();
    let (source, pushed) = leaked_kimi_thinking().await;
    let records = drain_records(&wrap_stream_with_kimi_thinking_recovery(source)).await;
    assert_eq!(
        record_kinds(&records),
        [
            "start",
            "thinking_start:0",
            "thinking_delta:0",
            "message_block_metadata:0",
            "thinking_end:0",
            "done",
        ]
    );
    assert_eq!(
        progress_records(&records),
        [
            json!({ "type": "message_block_metadata", "contentIndex": 0, "thinkingSignatureDelta": "sig" })
        ]
    );
    let events = public_events(&records);
    for (event, source_event) in events[..events.len() - 1].iter().zip(&pushed) {
        assert_eq!(&event.value, source_event);
        assert!(capture(event).is_some());
    }
    let done = first_event(&events, "done", None);
    let message = terminal_message(&done.value);
    assert_eq!(block_types(message), ["thinking", "text"]);
    let terminal = capture(done).expect("terminal capture");
    assert_eq!(
        terminal
            .message
            .as_deref()
            .cloned()
            .map(without_diagnostic_timestamps),
        Some(without_diagnostic_timestamps(message.clone()))
    );
    assert_eq!(terminal.usage, message.usage);
    assert_eq!(
        terminal
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("diagnostics"))
            .map(|diagnostics| diagnostics.as_array().map(Vec::len)),
        Some(message.diagnostics.as_ref().map(Vec::len))
    );
    assert_eq!(
        records_on_foreign_blocks(&records, message),
        Vec::<String>::new()
    );
}

/// `kimiThinkingThenSignedText`.
async fn kimi_thinking_then_signed_text(
    thinking_text: &str,
    end_thinking: bool,
    strays: bool,
) -> AssistantMessageEventStream {
    let mut turn = AliasedTurn::new(true);
    let mut output = create_assistant_message(json!([]), "pending");
    if strays {
        turn.progress(signature(0, "early"));
    }
    turn.push(AssistantMessageEvent::Start {
        partial: output.clone(),
    });
    output.content.push(thinking(""));
    turn.push(AssistantMessageEvent::ThinkingStart {
        content_index: 0,
        partial: output.clone(),
    });
    output.content[0] = thinking(thinking_text);
    turn.push(AssistantMessageEvent::ThinkingDelta {
        content_index: 0,
        delta: thinking_text.into(),
        partial: output.clone(),
    });
    set_thinking_signature(&mut output, 0, "sig");
    turn.progress(signature(0, "sig"));
    if end_thinking {
        turn.push(AssistantMessageEvent::ThinkingEnd {
            content_index: 0,
            content: thinking_text.into(),
            partial: output.clone(),
        });
    }
    output.content.push(text(""));
    turn.push(AssistantMessageEvent::TextStart {
        content_index: 1,
        partial: output.clone(),
    });
    output.content[1] = text("final");
    turn.push(AssistantMessageEvent::TextDelta {
        content_index: 1,
        delta: "final".into(),
        partial: output.clone(),
    });
    output.content[1] = ContentBlock::Text(TextContent {
        text: "final".into(),
        text_signature: Some("t1".into()),
        ..TextContent::default()
    });
    turn.progress(text_signature(1, "t1"));
    if strays {
        turn.progress(text_signature(2, "nowhere"));
    }
    turn.push(AssistantMessageEvent::TextEnd {
        content_index: 1,
        content: "final".into(),
        partial: output.clone(),
    });
    output.stop_reason = StopReason::Stop;
    turn.push(AssistantMessageEvent::Done {
        reason: DoneReason::Stop,
        message: output.clone(),
    });
    turn.finish(&output, Some(output.clone())).await.0
}

fn thinking_signature_record() -> Value {
    json!({ "type": "message_block_metadata", "contentIndex": 0, "thinkingSignatureDelta": "sig" })
}

fn text_signature_record() -> Value {
    json!({ "type": "message_block_metadata", "contentIndex": 1, "textSignature": "t1" })
}

// Kimi thinking recovery drops controls it cannot place #4272

#[tokio::test]
async fn kimi_keeps_a_later_blocks_control_at_its_source_index_when_no_earlier_thinking_block_splits()
 {
    let _subscription = subscribe_capture_for_test();
    let source = kimi_thinking_then_signed_text("plan", true, false).await;
    let records = drain_records(&wrap_stream_with_kimi_thinking_recovery(source)).await;
    let events = public_events(&records);
    assert_eq!(
        block_types(terminal_message(&first_event(&events, "done", None).value)),
        ["thinking", "text"]
    );
    assert_eq!(
        progress_records(&records),
        [thinking_signature_record(), text_signature_record()]
    );
}

#[tokio::test]
async fn kimi_drops_a_control_that_arrives_before_start_or_names_a_block_the_turn_never_has() {
    let _subscription = subscribe_capture_for_test();
    let source = kimi_thinking_then_signed_text("plan", true, true).await;
    let records = drain_records(&wrap_stream_with_kimi_thinking_recovery(source)).await;
    assert_eq!(
        progress_records(&records),
        [thinking_signature_record(), text_signature_record()]
    );
}

#[tokio::test]
async fn kimi_drops_a_later_blocks_control_when_the_thinking_before_it_splits() {
    let _subscription = subscribe_capture_for_test();
    let source = kimi_thinking_then_signed_text(LEAKED_THINKING, true, false).await;
    let records = drain_records(&wrap_stream_with_kimi_thinking_recovery(source)).await;
    let events = public_events(&records);
    let done = terminal_message(&first_event(&events, "done", None).value);
    assert_eq!(
        serde_json::to_value(&done.content).expect("content"),
        json!([
            { "type": "thinking", "thinking": "plan", "thinkingSignature": "sig" },
            { "type": "text", "text": " answer" },
            { "type": "text", "text": "final", "textSignature": "t1" },
        ])
    );
    assert_eq!(progress_records(&records), [thinking_signature_record()]);
}

#[tokio::test]
async fn kimi_drops_a_later_blocks_control_while_an_earlier_thinking_block_is_still_open() {
    let _subscription = subscribe_capture_for_test();
    let source = kimi_thinking_then_signed_text("plan", false, false).await;
    let records = drain_records(&wrap_stream_with_kimi_thinking_recovery(source)).await;
    assert_eq!(progress_records(&records), [thinking_signature_record()]);
}

// legacy text tool middleware captures its own blocks #4272

#[tokio::test]
async fn legacy_keeps_the_public_bytes_recorded_before_capture() {
    let _subscription = subscribe_capture_for_test();
    let (source, _) = signed_thinking_then_text(HERMES_TEXT).await;
    let wrapped = wrap_stream_with_tool_call_middleware(
        source,
        get_protocol(ToolCallFormat::Hermes),
        vec![weather_tool()],
    );
    let expected: Value =
        serde_json::from_str(include_str!("relay_legacy_public_bytes.json")).expect("snapshot");
    // The TS wrapper pushes `partial: this.message`, one object it keeps mutating; Rust partials
    // are push-time copies, so each is aliased to that object's final state, the last partial.
    let mut events = public_events(&drain_records(&wrapped).await);
    let last_partial = crate::utils::event_stream::extract_message(&events[events.len() - 2].value);
    for event in &mut events {
        crate::utils::capture_test_records::alias_partial(&mut event.value, &last_partial);
    }
    assert_eq!(public_bytes(&events), expected);
}

#[tokio::test]
async fn legacy_captures_the_blocks_it_projects_and_forwards_no_source_control_for_blocks_it_rebuilds()
 {
    let _subscription = subscribe_capture_for_test();
    let (source, _) = signed_thinking_then_text(HERMES_TEXT).await;
    let wrapped = wrap_stream_with_tool_call_middleware(
        source,
        get_protocol(ToolCallFormat::Hermes),
        vec![weather_tool()],
    );
    let records = drain_records(&wrapped).await;
    assert_eq!(
        record_kinds(&records),
        [
            "start",
            "thinking_start:0",
            "thinking_delta:0",
            "thinking_end:0",
            "text_start:1",
            "text_delta:1",
            "toolcall_start:2",
            "toolcall_delta:2",
            "toolcall_end:2",
            "text_delta:3",
            "text_end:3",
            "done",
        ]
    );
    assert_eq!(progress_records(&records), Vec::<Value>::new());
    let events = public_events(&records);
    let done = first_event(&events, "done", None);
    assert_eq!(
        records_on_foreign_blocks(&records, terminal_message(&done.value)),
        Vec::<String>::new()
    );
    assert_eq!(
        block_json(capture(first_event(&events, "thinking_start", Some(0)))),
        json!({ "type": "thinking", "thinking": "" })
    );
    assert_eq!(
        block_json(capture(first_event(&events, "thinking_end", Some(0)))),
        json!({ "type": "thinking", "thinking": "plan" })
    );
    assert!(capture(first_event(&events, "text_start", Some(1))).is_none());
    assert_eq!(
        delta_captures(&events, "text_delta"),
        [
            ("Before ".to_string(), Some(0), None),
            (" after".to_string(), Some(0), None),
        ]
    );
    assert_matches_object(
        &capture_json(capture(first_event(&events, "toolcall_start", Some(2)))),
        &json!({
            "block": { "type": "toolCall", "name": "get_weather", "arguments": {} },
            "rawArgumentPrefix": "",
            "coveredCodeUnits": 0,
        }),
    );
    assert_matches_object(
        &block_json(capture(first_event(&events, "toolcall_end", Some(2)))),
        &json!({ "arguments": { "city": "Seoul" } }),
    );
    assert_eq!(
        capture(done)
            .and_then(|capture| capture.message.as_deref())
            .map(block_types),
        Some(vec!["thinking", "text", "toolCall", "text"])
    );
}

// recovery event stream keeps records #4272

#[tokio::test]
async fn carries_a_forwarded_records_capture_onto_its_snapshot_and_keeps_controls_and_cancellation()
{
    let _subscription = subscribe_capture_for_test();
    let (source, _) = signed_thinking_then_text("").await;
    let source_records = drain_records(&source).await;
    let source_events = public_events(&source_records);
    let source_start = source_events.first().expect("start").clone();
    let source_done = source_events.last().expect("done").clone();
    assert!(matches!(
        source_start.value,
        AssistantMessageEvent::Start { .. }
    ));
    let outer = AssistantMessageEventStream::with_capture_blocks(true);
    let cancelled = Arc::new(AtomicUsize::new(0));
    let counter = cancelled.clone();
    outer.set_cancellation_handler(Box::new(move || {
        Box::pin(async move {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }));
    let reader = assistant_records(&outer);
    outer.push_captured(source_start.clone(), Default::default());
    crate::utils::assistant_capture::push_assistant_progress(&outer, signature(0, "x"));
    let done_message = terminal_message(&source_done.value).clone();
    outer.push_captured(source_done.clone(), Default::default());
    outer.end(Some(done_message));
    let first = tokio::time::timeout(DEADLINE, reader.next())
        .await
        .expect("first deadline")
        .expect("first")
        .expect("first record");
    let second = tokio::time::timeout(DEADLINE, reader.next())
        .await
        .expect("second deadline")
        .expect("second")
        .expect("second record");
    let crate::utils::assistant_capture::Record::Event(first) = first else {
        panic!("first record is not an event");
    };
    assert_eq!(first.value, source_start.value);
    assert!(Arc::ptr_eq(
        first.capture.as_ref().expect("first capture"),
        source_start.capture.as_ref().expect("source capture")
    ));
    assert_eq!(
        progress_records(&[second]),
        [
            json!({ "type": "message_block_metadata", "contentIndex": 0, "thinkingSignatureDelta": "x" })
        ]
    );
    // The stream already ended, so `return()` finds the source closed: TS counts one call because
    // its handler runs on return regardless; the Rust stream runs it once on cancel.
    tokio::time::timeout(DEADLINE, outer.cancel())
        .await
        .expect("cancel deadline")
        .expect("cancel");
    assert_eq!(cancelled.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn relays_a_clean_sources_facts_to_its_copy_and_nothing_from_a_source_captured_at_a_drifted_index()
 {
    let _subscription = subscribe_capture_for_test();
    let source = AssistantMessageEventStream::with_capture_blocks(true);
    let source_message = create_assistant_message(
        json!([{ "type": "thinking", "thinking": "" }, { "type": "thinking", "thinking": "" }]),
        "pending",
    );
    source.push(AssistantMessageEvent::Start {
        partial: source_message.clone(),
    });
    source.push(AssistantMessageEvent::ThinkingStart {
        content_index: 0,
        partial: source_message.clone(),
    });
    // Captured for block 0 while its public index names block 1: two blocks could own it.
    source.push_with_hint(
        AssistantMessageEvent::ThinkingStart {
            content_index: 1,
            partial: source_message.clone(),
        },
        crate::utils::assistant_capture::CaptureHint::content_index(0),
    );
    source.end(None);
    let source_events = public_events(&drain_records(&source).await);
    let clean = source_events[1].clone();
    let drifted = source_events[2].clone();
    assert_eq!(
        get_assistant_capture(&drifted).and_then(|capture| capture.content_index),
        Some(0)
    );
    let outer = AssistantMessageEventStream::with_capture_blocks(true);
    let outer_message = create_assistant_message(
        json!([{ "type": "thinking", "thinking": "late" }, { "type": "thinking", "thinking": "late" }]),
        "pending",
    );
    outer.push(AssistantMessageEvent::Start {
        partial: outer_message.clone(),
    });
    relay(&outer, &clean, || {
        outer.push(AssistantMessageEvent::ThinkingStart {
            content_index: 0,
            partial: outer_message.clone(),
        })
    });
    relay(&outer, &drifted, || {
        outer.push(AssistantMessageEvent::ThinkingStart {
            content_index: 1,
            partial: outer_message.clone(),
        })
    });
    outer.end(Some(outer_message));
    let events = public_events(&drain_records(&outer).await);
    assert_eq!(
        block_json(capture(first_event(&events, "thinking_start", Some(0)))),
        json!({ "type": "thinking", "thinking": "" })
    );
    assert!(capture(first_event(&events, "thinking_start", Some(1))).is_none());
}

#[tokio::test]
async fn carries_nothing_from_a_relaying_step_that_pushed_no_copy_onto_a_later_push() {
    let _subscription = subscribe_capture_for_test();
    let source = AssistantMessageEventStream::with_capture_blocks(true);
    let source_message =
        create_assistant_message(json!([{ "type": "thinking", "thinking": "" }]), "pending");
    source.push(AssistantMessageEvent::Start {
        partial: source_message.clone(),
    });
    source.push(AssistantMessageEvent::ThinkingStart {
        content_index: 0,
        partial: source_message,
    });
    source.end(None);
    let source_start = public_events(&drain_records(&source).await)[1].clone();
    let outer = AssistantMessageEventStream::with_capture_blocks(true);
    let outer_message = create_assistant_message(
        json!([{ "type": "thinking", "thinking": "own" }]),
        "pending",
    );
    outer.push(AssistantMessageEvent::Start {
        partial: outer_message.clone(),
    });
    relay(&outer, &source_start, || ());
    outer.push(AssistantMessageEvent::ThinkingStart {
        content_index: 0,
        partial: outer_message.clone(),
    });
    outer.end(Some(outer_message));
    let events = public_events(&drain_records(&outer).await);
    assert_eq!(
        block_json(capture(first_event(&events, "thinking_start", Some(0)))),
        json!({ "type": "thinking", "thinking": "own" })
    );
}

/// `outer.relaying(source, project)`.
pub(super) fn relay<V>(
    outer: &AssistantMessageEventStream,
    source: &Captured<AssistantMessageEvent>,
    project: impl FnOnce() -> V,
) -> V {
    outer.relaying(
        std::mem::discriminant(&source.value),
        source.capture.clone(),
        project,
    )
}
