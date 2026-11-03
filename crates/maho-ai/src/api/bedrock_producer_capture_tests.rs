//! Port of senpi packages/ai/test/bedrock-producer-capture.test.ts. The TS suite mocks the SDK
//! client to yield decoded wire items; here a local server serves the same items as AWS
//! event-stream frames to the real `stream()`.

use serde_json::{Value, json};

use crate::api::bedrock_converse_stream::stream as stream_bedrock;
use crate::types::{Model, StreamOptions};
use crate::utils::capture_test_records::{
    assert_matches_object, block_json, capture, capture_json, delta_captures, drain_records,
    first_event, progress_records, public_bytes, public_events, record_kinds,
    subscribe_capture_for_test,
};
use crate::utils::capture_test_server::{
    FixtureServer, aws_event_frame, fixture_context, minted_identity_keys,
};

const RAW_PARTS: [&str; 3] = [r#"{ "text""#, "", r#": "\u00e9 \"x\"" }"#];

fn model(base_url: &str) -> Model {
    serde_json::from_value(json!({
        "id": "global.openai.gpt-5.6-terra", "name": "fixture", "api": "bedrock-converse-stream",
        "provider": "amazon-bedrock", "baseUrl": base_url, "reasoning": true, "input": ["text"],
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
        "contextWindow": 200000, "maxTokens": 8192
    }))
    .expect("bedrock model")
}

fn reasoning(content_block_index: u64, reasoning_content: Value) -> (&'static str, Value) {
    (
        "contentBlockDelta",
        json!({ "contentBlockIndex": content_block_index, "delta": { "reasoningContent": reasoning_content } }),
    )
}

/// `capture()`: the wire items, served to the real stream. `redactedContent` bytes travel base64
/// encoded on the wire (the SDK hands TS the decoded `Uint8Array`).
async fn capture_records() -> Vec<crate::utils::assistant_capture::AssistantRecord> {
    let mut items = vec![
        ("messageStart", json!({ "role": "assistant" })),
        reasoning(0, json!({ "text": "think " })),
        reasoning(0, json!({ "text": "more" })),
        reasoning(0, json!({ "signature": "sig-a" })),
        reasoning(0, json!({ "signature": "sig-b" })),
        ("contentBlockStop", json!({ "contentBlockIndex": 0 })),
        (
            "contentBlockStart",
            json!({ "contentBlockIndex": 1, "start": { "toolUse": { "toolUseId": "call_1", "name": "echo" } } }),
        ),
    ];
    for input in RAW_PARTS {
        items.push((
            "contentBlockDelta",
            json!({ "contentBlockIndex": 1, "delta": { "toolUse": { "input": input } } }),
        ));
    }
    items.extend([
        ("contentBlockStop", json!({ "contentBlockIndex": 1 })),
        reasoning(2, json!({ "signature": "early" })),
        reasoning(2, json!({ "redactedContent": "AQID" })),
        reasoning(2, json!({ "redactedContent": "BA==" })),
        ("contentBlockStop", json!({ "contentBlockIndex": 2 })),
        ("messageStop", json!({ "stopReason": "tool_use" })),
        (
            "metadata",
            json!({ "usage": { "inputTokens": 3, "outputTokens": 5, "totalTokens": 8 } }),
        ),
    ]);
    let body = items
        .iter()
        .flat_map(|(event_type, payload)| aws_event_frame(event_type, payload))
        .collect();
    let server = FixtureServer::start("application/vnd.amazon.eventstream", body).await;
    let mut options = StreamOptions::default();
    options.request.api_key = Some("fixture".into());
    let stream = stream_bedrock(&model(&server.origin), &fixture_context(), Some(options));
    drain_records(&stream).await
}

// Bedrock producer capture #4272

#[tokio::test]
async fn keeps_the_public_bytes_recorded_before_capture() {
    let _subscription = subscribe_capture_for_test();
    let mut events = public_events(&capture_records().await);
    // TS events alias the producer's one `output` object, so every snapshot partial is its final
    // state; Rust partials are push-time copies and are aliased the same way before comparing.
    let terminal = crate::utils::capture_test_records::terminal_message(
        &events.last().expect("terminal").value,
    )
    .clone();
    for event in &mut events {
        crate::utils::capture_test_records::alias_partial(&mut event.value, &terminal);
    }
    let expected: Value =
        serde_json::from_str(include_str!("capture_fixtures/bedrock.json")).expect("snapshot");
    assert_eq!(public_bytes(&events), expected);
}

#[tokio::test]
async fn records_silent_signature_growth_and_mid_block_redaction_in_order() {
    let _subscription = subscribe_capture_for_test();
    let records = capture_records().await;
    assert_eq!(
        record_kinds(&records),
        [
            "start",
            "thinking_start:0",
            "thinking_delta:0",
            "thinking_delta:0",
            "message_block_metadata:0",
            "message_block_metadata:0",
            "thinking_end:0",
            "toolcall_start:1",
            "toolcall_delta:1",
            "toolcall_delta:1",
            "toolcall_delta:1",
            "toolcall_end:1",
            "thinking_start:2",
            "message_block_metadata:2",
            "message_block_metadata:2",
            "thinking_delta:2",
            "thinking_end:2",
            "done",
        ]
    );
    assert_eq!(
        progress_records(&records),
        [
            json!({ "type": "message_block_metadata", "contentIndex": 0, "thinkingSignatureDelta": "sig-a" }),
            json!({ "type": "message_block_metadata", "contentIndex": 0, "thinkingSignatureDelta": "sig-b" }),
            json!({ "type": "message_block_metadata", "contentIndex": 2, "thinkingSignatureDelta": "early" }),
            json!({ "type": "message_block_metadata", "contentIndex": 2, "thinkingSignature": "", "redacted": true }),
        ]
    );
    let events = public_events(&records);
    assert_matches_object(
        &capture_json(capture(first_event(&events, "thinking_start", Some(0)))),
        &json!({
            "block": { "type": "thinking", "thinking": "", "thinkingSignature": "" },
            "coveredCodeUnits": 0,
        }),
    );
    assert_eq!(
        block_json(capture(first_event(&events, "thinking_end", Some(0)))),
        json!({ "type": "thinking", "thinking": "think more", "thinkingSignature": "sig-asig-b" })
    );
    assert_eq!(
        block_json(capture(first_event(&events, "thinking_end", Some(2)))),
        json!({
            "type": "thinking", "thinking": "[Reasoning redacted]",
            "thinkingSignature": "AQIDBA==", "redacted": true
        })
    );
}

#[tokio::test]
async fn freezes_the_empty_tool_seed_and_advances_the_frontier_with_each_provider_parse() {
    let _subscription = subscribe_capture_for_test();
    let events = public_events(&capture_records().await);
    assert_matches_object(
        &capture_json(capture(first_event(&events, "toolcall_start", Some(1)))),
        &json!({
            "block": { "type": "toolCall", "id": "call_1", "name": "echo", "arguments": {} },
            "rawArgumentPrefix": "",
            "parsedThrough": 0,
            "coveredCodeUnits": 0,
        }),
    );
    let raw_length = RAW_PARTS.concat().encode_utf16().count();
    assert_eq!(
        delta_captures(&events, "toolcall_delta"),
        [
            (RAW_PARTS[0].to_string(), Some(0), Some(8)),
            (String::new(), Some(8), Some(8)),
            (RAW_PARTS[2].to_string(), Some(8), Some(raw_length)),
        ]
    );
    assert_matches_object(
        &block_json(capture(first_event(&events, "toolcall_end", Some(1)))),
        &json!({ "arguments": { "text": "é \"x\"" } }),
    );
    let done =
        capture(first_event(&events, "done", None)).and_then(|capture| capture.message.as_deref());
    assert_eq!(
        done.map(|message| message.stop_reason),
        Some(crate::types::StopReason::ToolUse)
    );
    assert_eq!(minted_identity_keys(done), Vec::<String>::new());
}
