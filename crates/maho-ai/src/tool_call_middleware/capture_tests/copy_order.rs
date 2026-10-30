//! Port of senpi packages/ai/test/tool-call-middleware/capture-copy-order.test.ts.

use serde_json::json;

use crate::types::{AssistantMessageEvent, ContentBlock, DoneReason, StopReason, ThinkingContent};
use crate::utils::assistant_capture::{AssistantProgress, BlockMetadata};
use crate::utils::capture_test_records::{
    AliasedTurn, block_json, capture, drain_records, first_event, public_events, record_kinds,
    subscribe_capture_for_test,
};
use crate::utils::event_stream::AssistantMessageEventStream;

use super::downgrade::{create_assistant_message, invoke_recovery};
use super::relay::relay;

#[derive(Clone, Copy)]
enum Kind {
    ToolCall,
    Thinking,
}

/// One copied block at 0 whose control the source emits before the block's start event.
async fn control_before_start(kind: Kind) -> AssistantMessageEventStream {
    let mut turn = AliasedTurn::new(true);
    let mut output = create_assistant_message(json!([]), "pending");
    turn.push(AssistantMessageEvent::Start {
        partial: output.clone(),
    });
    match kind {
        Kind::ToolCall => {
            let call: crate::types::ToolCall =
                serde_json::from_value(json!({ "id": "native", "name": "Bash", "arguments": {} }))
                    .expect("call");
            output.content.push(ContentBlock::ToolCall(call.clone()));
            turn.progress(AssistantProgress::MessageToolArgumentsReset {
                content_index: 0,
                raw_argument_prefix: String::new(),
                parsed_through: 0,
            });
            turn.push(AssistantMessageEvent::ToolcallStart {
                content_index: 0,
                partial: output.clone(),
            });
            turn.push(AssistantMessageEvent::ToolcallEnd {
                content_index: 0,
                tool_call: call,
                partial: output.clone(),
            });
            output.stop_reason = StopReason::ToolUse;
        }
        Kind::Thinking => {
            output.content.push(ContentBlock::Thinking(ThinkingContent {
                thinking: "plan".into(),
                ..ThinkingContent::default()
            }));
            turn.progress(AssistantProgress::metadata(BlockMetadata {
                content_index: 0,
                thinking_signature_delta: Some("s".into()),
                ..BlockMetadata::default()
            }));
            turn.push(AssistantMessageEvent::ThinkingStart {
                content_index: 0,
                partial: output.clone(),
            });
            turn.push(AssistantMessageEvent::ThinkingEnd {
                content_index: 0,
                content: "plan".into(),
                partial: output.clone(),
            });
            output.stop_reason = StopReason::Stop;
        }
    }
    let reason = if output.stop_reason == StopReason::ToolUse {
        DoneReason::ToolUse
    } else {
        DoneReason::Stop
    };
    turn.push(AssistantMessageEvent::Done {
        reason,
        message: output.clone(),
    });
    turn.finish(&output, Some(output.clone())).await.0
}

// recovery copies keep a block's records in one order #4272

#[tokio::test]
async fn relays_a_tool_call_control_held_for_its_block_after_that_blocks_start() {
    let _subscription = subscribe_capture_for_test();
    let records = drain_records(&invoke_recovery(control_before_start(Kind::ToolCall).await)).await;
    assert_eq!(
        record_kinds(&records),
        [
            "start",
            "toolcall_start:0",
            "message_tool_arguments_reset:0",
            "toolcall_end:0",
            "done",
        ]
    );
}

#[tokio::test]
async fn relays_a_thinking_control_held_for_its_block_after_that_blocks_start() {
    let _subscription = subscribe_capture_for_test();
    let records = drain_records(&invoke_recovery(control_before_start(Kind::Thinking).await)).await;
    assert_eq!(
        record_kinds(&records),
        [
            "start",
            "thinking_start:0",
            "message_block_metadata:0",
            "thinking_end:0",
            "done",
        ]
    );
}

#[tokio::test]
async fn relays_the_sources_record_to_one_copy_only_when_a_step_pushes_two_events_of_its_type() {
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
        json!([{ "type": "thinking", "thinking": "" }, { "type": "thinking", "thinking": "other" }]),
        "pending",
    );
    outer.push(AssistantMessageEvent::Start {
        partial: outer_message.clone(),
    });
    relay(&outer, &source_start, || {
        outer.push(AssistantMessageEvent::ThinkingStart {
            content_index: 0,
            partial: outer_message.clone(),
        });
        outer.push(AssistantMessageEvent::ThinkingStart {
            content_index: 1,
            partial: outer_message.clone(),
        });
    });
    outer.end(Some(outer_message));
    let events = public_events(&drain_records(&outer).await);
    assert_eq!(
        block_json(capture(first_event(&events, "thinking_start", Some(0)))),
        json!({ "type": "thinking", "thinking": "" })
    );
    // The second push is no copy of the source, and its own block was read late: no record, never
    // the source's.
    assert!(capture(first_event(&events, "thinking_start", Some(1))).is_none());
}
