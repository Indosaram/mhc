//! Port of senpi packages/ai/test/tool-call-middleware/capture-downgrade.test.ts.

use std::sync::Arc;

use serde_json::{Value, json};

use crate::tool_call_middleware::recovery_native_projection::RecoveryNativeProjection;
use crate::tool_call_middleware::recovery_stream_wrapper::{
    InvokeRecoveryOptions, wrap_stream_with_invoke_recovery,
};
use crate::tool_call_middleware::types::ToolCallFormat;
use crate::types::{
    AssistantMessage, AssistantMessageEvent, ContentBlock, DoneReason, ProviderNativeContent,
    StopReason, TextContent, ThinkingContent, Tool, ToolCall,
};
use crate::utils::assistant_capture::{AssistantProgress, BlockMetadata, BlockPhase};
use crate::utils::capture_test_records::{
    AliasedTurn, assert_matches_object, block_json, block_types, capture, capture_json,
    delta_captures, drain_records, event_index, event_type, first_event, progress_records,
    public_bytes, public_events, record_kinds, records_on_foreign_blocks,
    subscribe_capture_for_test, terminal_message,
};
use crate::utils::event_stream::AssistantMessageEventStream;

pub(super) const INVOKE: &str =
    r#"<invoke name="Bash"><parameter name="command">echo recovered</parameter></invoke>"#;
const NATIVE_ARGUMENTS: &str = r#"{"command":"echo native"}"#;

fn prose() -> String {
    format!("before {INVOKE} after")
}

/// invoke-recovery-stream-fixtures.ts `createAssistantMessage`.
pub(super) fn create_assistant_message(content: Value, stop_reason: &str) -> AssistantMessage {
    crate::utils::capture_test_records::assistant_message(
        "anthropic-messages",
        "anthropic",
        "claude-test",
        content,
        stop_reason,
    )
}

pub(super) fn bash_tool() -> Tool {
    Tool {
        name: "Bash".into(),
        description: "fixture".into(),
        parameters: json!({
            "type": "object",
            "properties": { "command": { "type": "string" } },
            "required": ["command"]
        }),
        output_schema: None,
        freeform: None,
        constrained_sampling: None,
    }
}

pub(super) fn invoke_recovery(source: AssistantMessageEventStream) -> AssistantMessageEventStream {
    wrap_stream_with_invoke_recovery(source, vec![bash_tool()], InvokeRecoveryOptions::default())
}

fn thinking_signature(content_index: usize, delta: &str) -> AssistantProgress {
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

fn search(status: &str) -> ContentBlock {
    ContentBlock::ProviderNative(ProviderNativeContent {
        subtype: "web_search_call".into(),
        raw: json!({ "status": status }),
    })
}

fn native_call(arguments: Value) -> ToolCall {
    ToolCall {
        id: "native".into(),
        name: "Bash".into(),
        arguments: arguments.as_object().cloned().unwrap_or_default(),
        incomplete: None,
        error_message: None,
        thought_signature: None,
        namespace: None,
    }
}

/// `bufferedProviderTurn`.
async fn buffered_provider_turn(capture_blocks: bool) -> AssistantMessageEventStream {
    let prose = prose();
    let mut turn = AliasedTurn::new(capture_blocks);
    let mut output = create_assistant_message(json!([]), "pending");
    turn.push(AssistantMessageEvent::Start {
        partial: output.clone(),
    });
    output
        .content
        .push(ContentBlock::Thinking(ThinkingContent::default()));
    turn.push(AssistantMessageEvent::ThinkingStart {
        content_index: 0,
        partial: output.clone(),
    });
    output.content[0] = ContentBlock::Thinking(ThinkingContent {
        thinking: "plan".into(),
        ..ThinkingContent::default()
    });
    turn.push(AssistantMessageEvent::ThinkingDelta {
        content_index: 0,
        delta: "plan".into(),
        partial: output.clone(),
    });
    output.content[0] = ContentBlock::Thinking(ThinkingContent {
        thinking: "plan".into(),
        thinking_signature: Some("sig".into()),
        ..ThinkingContent::default()
    });
    turn.progress(thinking_signature(0, "sig"));
    turn.push(AssistantMessageEvent::ThinkingEnd {
        content_index: 0,
        content: "plan".into(),
        partial: output.clone(),
    });
    output.content.push(ContentBlock::text(""));
    turn.push(AssistantMessageEvent::TextStart {
        content_index: 1,
        partial: output.clone(),
    });
    output.content[1] = ContentBlock::text(prose.clone());
    turn.push(AssistantMessageEvent::TextDelta {
        content_index: 1,
        delta: prose.clone(),
        partial: output.clone(),
    });
    output.content[1] = ContentBlock::Text(TextContent {
        text: prose.clone(),
        text_signature: Some("t1".into()),
        ..TextContent::default()
    });
    turn.progress(text_signature(1, "t1"));
    turn.push(AssistantMessageEvent::TextEnd {
        content_index: 1,
        content: prose,
        partial: output.clone(),
    });
    output.content.push(search("in_progress"));
    turn.progress(AssistantProgress::block_state(
        2,
        BlockPhase::Insert,
        &search("in_progress"),
    ));
    output.content[2] = search("completed");
    turn.progress(AssistantProgress::block_state(
        2,
        BlockPhase::End,
        &search("completed"),
    ));
    output
        .content
        .push(ContentBlock::ToolCall(native_call(json!({}))));
    turn.push(AssistantMessageEvent::ToolcallStart {
        content_index: 3,
        partial: output.clone(),
    });
    let call = native_call(json!({ "command": "echo native" }));
    output.content[3] = ContentBlock::ToolCall(call.clone());
    turn.push(AssistantMessageEvent::ToolcallDelta {
        content_index: 3,
        delta: NATIVE_ARGUMENTS.into(),
        partial: output.clone(),
    });
    turn.progress(AssistantProgress::MessageToolArgumentsReset {
        content_index: 3,
        raw_argument_prefix: NATIVE_ARGUMENTS.into(),
        parsed_through: NATIVE_ARGUMENTS.len(),
    });
    turn.push(AssistantMessageEvent::ToolcallEnd {
        content_index: 3,
        tool_call: call,
        partial: output.clone(),
    });
    output.stop_reason = StopReason::ToolUse;
    turn.push(AssistantMessageEvent::Done {
        reason: DoneReason::ToolUse,
        message: output.clone(),
    });
    turn.finish(&output, Some(output.clone())).await.0
}

fn search_record(phase: &str, status: &str) -> Value {
    json!({
        "type": "message_block_state", "contentIndex": 4, "phase": phase,
        "block": { "type": "providerNative", "subtype": "web_search_call", "raw": { "status": status } }
    })
}

// invoke recovery capture remap #4272

#[tokio::test]
async fn keeps_the_public_bytes_recorded_before_capture() {
    let _subscription = subscribe_capture_for_test();
    let records = drain_records(&invoke_recovery(buffered_provider_turn(true).await)).await;
    let expected: Value =
        serde_json::from_str(include_str!("downgrade_public_bytes.json")).expect("snapshot");
    assert_eq!(public_bytes(&public_events(&records)), expected);
}

#[tokio::test]
async fn remaps_the_kept_records_to_outer_indices_and_drops_the_ones_the_projection_does_not_follow()
 {
    let _subscription = subscribe_capture_for_test();
    let records = drain_records(&invoke_recovery(buffered_provider_turn(true).await)).await;
    assert_eq!(
        record_kinds(&records),
        [
            "start",
            "thinking_start:0",
            "thinking_delta:0",
            "message_block_metadata:0",
            "thinking_end:0",
            "text_start:1",
            "text_delta:1",
            "toolcall_start:2",
            "toolcall_delta:2",
            "toolcall_end:2",
            "text_delta:3",
            "text_end:3",
            "message_block_state:4:insert",
            "message_block_state:4:end",
            "toolcall_start:5",
            "toolcall_delta:5",
            "message_tool_arguments_reset:5",
            "toolcall_end:5",
            "done",
        ]
    );
    assert_eq!(
        progress_records(&records),
        [
            json!({ "type": "message_block_metadata", "contentIndex": 0, "thinkingSignatureDelta": "sig" }),
            search_record("insert", "in_progress"),
            search_record("end", "completed"),
            json!({
                "type": "message_tool_arguments_reset", "contentIndex": 5,
                "rawArgumentPrefix": NATIVE_ARGUMENTS, "parsedThrough": NATIVE_ARGUMENTS.len()
            }),
        ]
    );
    let events = public_events(&records);
    let done = first_event(&events, "done", None);
    let message = terminal_message(&done.value);
    assert_eq!(
        records_on_foreign_blocks(&records, message),
        Vec::<String>::new()
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| capture(event).is_none())
            .map(|event| event_type(&event.value))
            .collect::<Vec<_>>(),
        Vec::<String>::new()
    );
    assert_eq!(
        capture(done)
            .and_then(|capture| capture.message.as_deref())
            .map(block_types),
        Some(block_types(message))
    );
}

#[tokio::test]
async fn carries_the_sources_emission_time_seed_for_a_lagging_1_1_copy_instead_of_capturing_the_copy()
 {
    let _subscription = subscribe_capture_for_test();
    let events =
        public_events(&drain_records(&invoke_recovery(buffered_provider_turn(true).await)).await);
    let thinking_start = first_event(&events, "thinking_start", Some(0));
    let AssistantMessageEvent::ThinkingStart { partial, .. } = &thinking_start.value else {
        panic!("thinking_start");
    };
    assert_matches_object(
        &serde_json::to_value(&partial.content[0]).expect("block"),
        &json!({ "thinking": "plan" }),
    );
    assert_matches_object(
        &capture_json(capture(thinking_start)),
        &json!({ "block": { "type": "thinking", "thinking": "" }, "coveredCodeUnits": 0 }),
    );
    assert_eq!(
        delta_captures(&events, "thinking_delta"),
        [("plan".to_string(), Some(0), None)]
    );
    assert_eq!(
        block_json(capture(first_event(&events, "thinking_end", Some(0)))),
        json!({ "type": "thinking", "thinking": "plan", "thinkingSignature": "sig" })
    );
    let native_start = first_event(&events, "toolcall_start", Some(5));
    let AssistantMessageEvent::ToolcallStart { partial, .. } = &native_start.value else {
        panic!("toolcall_start");
    };
    assert_matches_object(
        &serde_json::to_value(&partial.content[5]).expect("block"),
        &json!({ "arguments": { "command": "echo native" } }),
    );
    assert_matches_object(
        &capture_json(capture(native_start)),
        &json!({
            "block": { "type": "toolCall", "id": "native", "arguments": {} },
            "rawArgumentPrefix": "",
            "parsedThrough": 0,
            "coveredCodeUnits": 0,
        }),
    );
    let tool_deltas = delta_captures(&events, "toolcall_delta");
    assert_eq!(
        tool_deltas[1],
        (
            NATIVE_ARGUMENTS.to_string(),
            Some(0),
            Some(NATIVE_ARGUMENTS.len())
        )
    );
}

#[tokio::test]
async fn captures_the_blocks_the_projection_builds_itself_at_their_outer_indices() {
    let _subscription = subscribe_capture_for_test();
    let events =
        public_events(&drain_records(&invoke_recovery(buffered_provider_turn(true).await)).await);
    assert_matches_object(
        &block_json(capture(first_event(&events, "text_start", Some(1)))),
        &json!({ "type": "text", "text": "" }),
    );
    assert_eq!(
        delta_captures(&events, "text_delta"),
        [
            ("before ".to_string(), Some(0), None),
            (" after".to_string(), Some(0), None),
        ]
    );
    assert_matches_object(
        &capture_json(capture(first_event(&events, "toolcall_start", Some(2)))),
        &json!({
            "block": { "type": "toolCall", "id": "recovered-antml-0", "arguments": {} },
            "rawArgumentPrefix": "",
        }),
    );
    let recovered_delta = delta_captures(&events, "toolcall_delta")[0].clone();
    assert_eq!(recovered_delta.1, Some(0));
    assert_eq!(
        serde_json::from_str::<Value>(&recovered_delta.0).expect("delta json"),
        json!({ "command": "echo recovered" })
    );
    assert_matches_object(
        &block_json(capture(first_event(&events, "toolcall_end", Some(2)))),
        &json!({ "arguments": { "command": "echo recovered" } }),
    );
}

#[tokio::test]
async fn never_captures_a_lagging_copy_whose_source_carried_no_record() {
    let _subscription = subscribe_capture_for_test();
    let records = drain_records(&invoke_recovery(buffered_provider_turn(false).await)).await;
    let captured: Vec<String> = public_events(&records)
        .iter()
        .map(|event| match event_index(&event.value) {
            Some(index) => format!(
                "{}:{index}:{}",
                event_type(&event.value),
                capture(event).is_some()
            ),
            None => format!("{}:{}", event_type(&event.value), capture(event).is_some()),
        })
        .collect();
    assert_eq!(
        captured,
        [
            "start:true",
            "thinking_start:0:false",
            "thinking_delta:0:false",
            "thinking_end:0:false",
            "text_start:1:true",
            "text_delta:1:true",
            "toolcall_start:2:true",
            "toolcall_delta:2:true",
            "toolcall_end:2:true",
            "text_delta:3:true",
            "text_end:3:true",
            "toolcall_start:5:false",
            "toolcall_delta:5:false",
            "toolcall_end:5:false",
            "done:true",
        ]
    );
    assert_eq!(progress_records(&records), Vec::<Value>::new());
}

#[derive(Default)]
struct StrayControls {
    before_start: Option<AssistantProgress>,
    before_prose: Option<AssistantProgress>,
    after_prose: Option<AssistantProgress>,
}

/// `proseTurnWithStrays`.
async fn prose_turn_with_strays(strays: StrayControls) -> AssistantMessageEventStream {
    let prose = prose();
    let mut turn = AliasedTurn::new(true);
    let mut output = create_assistant_message(json!([]), "pending");
    if let Some(progress) = strays.before_start {
        turn.progress(progress);
    }
    turn.push(AssistantMessageEvent::Start {
        partial: output.clone(),
    });
    output
        .content
        .push(ContentBlock::Thinking(ThinkingContent::default()));
    turn.push(AssistantMessageEvent::ThinkingStart {
        content_index: 0,
        partial: output.clone(),
    });
    output.content[0] = ContentBlock::Thinking(ThinkingContent {
        thinking: "plan".into(),
        ..ThinkingContent::default()
    });
    turn.push(AssistantMessageEvent::ThinkingDelta {
        content_index: 0,
        delta: "plan".into(),
        partial: output.clone(),
    });
    turn.progress(thinking_signature(0, "sig"));
    turn.push(AssistantMessageEvent::ThinkingEnd {
        content_index: 0,
        content: "plan".into(),
        partial: output.clone(),
    });
    if let Some(progress) = strays.before_prose {
        turn.progress(progress);
    }
    output.content.push(ContentBlock::text(""));
    turn.push(AssistantMessageEvent::TextStart {
        content_index: 1,
        partial: output.clone(),
    });
    output.content[1] = ContentBlock::text(prose.clone());
    turn.push(AssistantMessageEvent::TextDelta {
        content_index: 1,
        delta: prose.clone(),
        partial: output.clone(),
    });
    if let Some(progress) = strays.after_prose {
        turn.progress(progress);
    }
    turn.push(AssistantMessageEvent::TextEnd {
        content_index: 1,
        content: prose,
        partial: output.clone(),
    });
    output.stop_reason = StopReason::ToolUse;
    turn.push(AssistantMessageEvent::Done {
        reason: DoneReason::ToolUse,
        message: output.clone(),
    });
    turn.finish(&output, Some(output.clone())).await.0
}

fn kept_thinking_signature() -> Value {
    json!({ "type": "message_block_metadata", "contentIndex": 0, "thinkingSignatureDelta": "sig" })
}

// invoke recovery drops controls it cannot remap #4272

#[tokio::test]
async fn drops_a_control_that_arrives_before_start_or_names_a_source_block_that_never_exists() {
    let _subscription = subscribe_capture_for_test();
    let source = prose_turn_with_strays(StrayControls {
        before_start: Some(thinking_signature(0, "early")),
        // Outer index 2 exists (the recovered call), but no source block 2 does: forwarding it
        // would be a guess.
        after_prose: Some(AssistantProgress::block_state(
            2,
            BlockPhase::Insert,
            &search("completed"),
        )),
        ..StrayControls::default()
    })
    .await;
    let records = drain_records(&invoke_recovery(source)).await;
    assert_eq!(
        block_types(terminal_message(
            &first_event(&public_events(&records), "done", None).value
        )),
        ["thinking", "text", "toolCall", "text"]
    );
    assert_eq!(progress_records(&records), [kept_thinking_signature()]);
}

#[tokio::test]
async fn drops_the_prose_blocks_controls_held_or_late_because_outer_blocks_1_2_and_3_could_each_own_them()
 {
    let _subscription = subscribe_capture_for_test();
    let source = prose_turn_with_strays(StrayControls {
        before_prose: Some(text_signature(1, "held")),
        after_prose: Some(text_signature(1, "late")),
        ..StrayControls::default()
    })
    .await;
    let records = drain_records(&invoke_recovery(source)).await;
    assert_eq!(
        record_kinds(&records),
        [
            "start",
            "thinking_start:0",
            "thinking_delta:0",
            "message_block_metadata:0",
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
    assert_eq!(progress_records(&records), [kept_thinking_signature()]);
}

// recovery native projection remap #4272

#[tokio::test]
async fn relays_a_copied_blocks_controls_to_its_exact_outer_index_and_drops_those_it_cannot_place()
{
    let _subscription = subscribe_capture_for_test();
    let stream = AssistantMessageEventStream::with_capture_blocks(true);
    let mut message = create_assistant_message(json!([]), "pending");
    let mut projection = RecoveryNativeProjection::new(stream.clone(), ToolCallFormat::Antml);
    let control =
        |content_index: usize, delta: &str| Arc::new(thinking_signature(content_index, delta));
    // Declared layout: inner 0 copies to outer 0; inner 1 is prose rebuilt into outer 1..3; inner
    // 2 copies to outer 1 + 3 = 4; inner 3 is never projected.
    projection.relay_progress(control(2, "held"));
    message
        .content
        .push(ContentBlock::Thinking(ThinkingContent {
            thinking: "a".into(),
            ..ThinkingContent::default()
        }));
    assert!(projection.record_projected_block(0, 0));
    projection.relay_progress(control(0, "zero"));
    assert!(projection.start_text(&message, 1, 1));
    message.content.extend([
        ContentBlock::text("before "),
        ContentBlock::ToolCall(ToolCall {
            id: "recovered-antml-0".into(),
            name: "Bash".into(),
            arguments: Default::default(),
            incomplete: None,
            error_message: None,
            thought_signature: None,
            namespace: None,
        }),
        ContentBlock::text(" after"),
    ]);
    projection.extend_text(&message, 1);
    projection.relay_progress(control(1, "prose"));
    message
        .content
        .push(ContentBlock::Thinking(ThinkingContent {
            thinking: "b".into(),
            ..ThinkingContent::default()
        }));
    assert!(projection.record_projected_block(2, 4));
    projection.relay_progress(control(2, "late"));
    projection.relay_progress(control(3, "never"));
    stream.end(Some(message));
    assert_eq!(
        progress_records(&drain_records(&stream).await),
        [
            json!({ "type": "message_block_metadata", "contentIndex": 0, "thinkingSignatureDelta": "zero" }),
            json!({ "type": "message_block_metadata", "contentIndex": 4, "thinkingSignatureDelta": "held" }),
            json!({ "type": "message_block_metadata", "contentIndex": 4, "thinkingSignatureDelta": "late" }),
        ]
    );
}
