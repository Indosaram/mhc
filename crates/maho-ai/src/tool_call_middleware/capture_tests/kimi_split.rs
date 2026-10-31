//! Port of senpi packages/ai/test/tool-call-middleware/capture-kimi-split.test.ts.

use serde_json::{Value, json};

use crate::tool_call_middleware::protocols::kimi_xtml::thinking_recovery_stream::wrap_stream_with_kimi_thinking_recovery;
use crate::types::{
    AssistantMessageEvent, ContentBlock, DoneReason, StopReason, TextContent, ThinkingContent,
    ToolCall,
};
use crate::utils::assistant_capture::{AssistantProgress, BlockMetadata, CapturedEvent};
use crate::utils::capture_test_records::{
    AliasedTurn, block_types, capture, carrying_records, drain_records, event_index, event_type,
    first_event, progress_records, public_events, records_on_foreign_blocks,
    subscribe_capture_for_test, terminal_message,
};
use crate::utils::event_stream::AssistantMessageEventStream;

use super::relay::create_assistant_message;

const LEAKED_THINKING: &str = "plan<|open|>response answer";
const CALL_ARGUMENTS: &str = r#"{"city":"Seoul"}"#;

fn weather_call(arguments: Value) -> ToolCall {
    ToolCall {
        id: "c1".into(),
        name: "get_weather".into(),
        arguments: arguments.as_object().cloned().unwrap_or_default(),
        incomplete: None,
        error_message: None,
        thought_signature: None,
        namespace: None,
    }
}

/// Thinking at 0, signed text at 1, a native call at 2; `late_thinking_end` ends the thinking
/// after the text starts.
async fn thinking_text_call(
    thinking_text: &str,
    late_thinking_end: bool,
) -> (AssistantMessageEventStream, Vec<AssistantMessageEvent>) {
    let mut turn = AliasedTurn::new(true);
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
        thinking: thinking_text.into(),
        ..ThinkingContent::default()
    });
    turn.push(AssistantMessageEvent::ThinkingDelta {
        content_index: 0,
        delta: thinking_text.into(),
        partial: output.clone(),
    });
    let end_thinking = |turn: &mut AliasedTurn, output: &crate::types::AssistantMessage| {
        turn.push(AssistantMessageEvent::ThinkingEnd {
            content_index: 0,
            content: thinking_text.into(),
            partial: output.clone(),
        })
    };
    if !late_thinking_end {
        end_thinking(&mut turn, &output);
    }
    output.content.push(ContentBlock::text(""));
    turn.push(AssistantMessageEvent::TextStart {
        content_index: 1,
        partial: output.clone(),
    });
    if late_thinking_end {
        end_thinking(&mut turn, &output);
    }
    output.content[1] = ContentBlock::text("final");
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
    turn.progress(AssistantProgress::metadata(BlockMetadata {
        content_index: 1,
        text_signature: Some("t1".into()),
        ..BlockMetadata::default()
    }));
    turn.push(AssistantMessageEvent::TextEnd {
        content_index: 1,
        content: "final".into(),
        partial: output.clone(),
    });
    output
        .content
        .push(ContentBlock::ToolCall(weather_call(json!({}))));
    turn.push(AssistantMessageEvent::ToolcallStart {
        content_index: 2,
        partial: output.clone(),
    });
    let call = weather_call(json!({ "city": "Seoul" }));
    output.content[2] = ContentBlock::ToolCall(call.clone());
    turn.push(AssistantMessageEvent::ToolcallDelta {
        content_index: 2,
        delta: CALL_ARGUMENTS.into(),
        partial: output.clone(),
    });
    turn.progress(AssistantProgress::MessageToolArgumentsReset {
        content_index: 2,
        raw_argument_prefix: CALL_ARGUMENTS.into(),
        parsed_through: CALL_ARGUMENTS.len(),
    });
    turn.push(AssistantMessageEvent::ToolcallEnd {
        content_index: 2,
        tool_call: call,
        partial: output.clone(),
    });
    output.stop_reason = StopReason::ToolUse;
    turn.push(AssistantMessageEvent::Done {
        reason: DoneReason::ToolUse,
        message: output.clone(),
    });
    turn.finish(&output, Some(output.clone())).await
}

fn captured_indexes(events: &[CapturedEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| {
            let index = event_index(&event.value)?;
            capture(event).map(|_| format!("{}:{index}", event_type(&event.value)))
        })
        .collect()
}

fn progress_indexes(records: &[crate::utils::assistant_capture::AssistantRecord]) -> Vec<u64> {
    progress_records(records)
        .iter()
        .map(|progress| progress["contentIndex"].as_u64().expect("contentIndex"))
        .collect()
}

// Live events keep their source indices and only the terminal is rewritten, so a block that a
// split moves keeps its public events but loses every record: lack, never wrong.

#[tokio::test]
async fn forwards_no_record_for_a_text_block_or_a_tool_call_that_the_split_moves() {
    let _subscription = subscribe_capture_for_test();
    let (source, pushed) = thinking_text_call(LEAKED_THINKING, false).await;
    let records = drain_records(&wrap_stream_with_kimi_thinking_recovery(source)).await;
    let events = public_events(&records);
    let done = terminal_message(&first_event(&events, "done", None).value);
    // Thinking 0 recovers to thinking + text, so source 1 lands at 2 and source 2 at 3.
    assert_eq!(block_types(done), ["thinking", "text", "text", "toolCall"]);
    assert_eq!(
        captured_indexes(&events),
        ["thinking_start:0", "thinking_delta:0", "thinking_end:0"]
    );
    assert_eq!(progress_records(&records), Vec::<Value>::new());
    assert_eq!(
        records_on_foreign_blocks(&carrying_records(&records), done),
        Vec::<String>::new()
    );
    // Public bytes are unchanged: the same events cross, with only their records dropped.
    for (event, source_event) in events[..events.len() - 1].iter().zip(&pushed) {
        assert_eq!(&event.value, source_event);
    }
}

#[tokio::test]
async fn keeps_every_record_of_the_later_blocks_when_the_thinking_before_them_does_not_split() {
    let _subscription = subscribe_capture_for_test();
    let (source, _) = thinking_text_call("plan", false).await;
    let records = drain_records(&wrap_stream_with_kimi_thinking_recovery(source)).await;
    let events = public_events(&records);
    assert_eq!(
        captured_indexes(&events),
        [
            "thinking_start:0",
            "thinking_delta:0",
            "thinking_end:0",
            "text_start:1",
            "text_delta:1",
            "text_end:1",
            "toolcall_start:2",
            "toolcall_delta:2",
            "toolcall_end:2",
        ]
    );
    assert_eq!(progress_indexes(&records), [1, 2]);
    assert_eq!(
        records_on_foreign_blocks(
            &records,
            terminal_message(&first_event(&events, "done", None).value)
        ),
        Vec::<String>::new()
    );
}

#[tokio::test]
async fn drops_a_blocks_records_for_good_once_it_started_while_an_earlier_thinking_block_was_open()
{
    let _subscription = subscribe_capture_for_test();
    let (source, _) = thinking_text_call("plan", true).await;
    let records = drain_records(&wrap_stream_with_kimi_thinking_recovery(source)).await;
    // Its start had no record, so its later deltas, end and controls carry none either.
    assert_eq!(
        captured_indexes(&public_events(&records)),
        [
            "thinking_start:0",
            "thinking_delta:0",
            "thinking_end:0",
            "toolcall_start:2",
            "toolcall_delta:2",
            "toolcall_end:2",
        ]
    );
    assert_eq!(progress_indexes(&records), [2]);
}
