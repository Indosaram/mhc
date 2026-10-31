//! Port of senpi packages/ai/test/pi-messages-producer-capture.test.ts. The TS suite serves the
//! pi-messages SSE from a local `http` server; `FixtureServer` serves the same bytes here.

use serde_json::{Value, json};

use crate::api::pi_messages::stream as stream_pi_messages;
use crate::types::{Model, StreamOptions};
use crate::utils::assistant_capture::AssistantRecord;
use crate::utils::capture_test_records::{
    alias_partial, assert_matches_object, block_json, capture, capture_json, delta_captures,
    drain_records, first_event, public_bytes, public_events, record_kinds,
    subscribe_capture_for_test, terminal_message, zero_timestamps,
};
use crate::utils::capture_test_server::{
    FixtureServer, fixture_context, minted_identity_keys, sse_data,
};

const ARGUMENT_DELTAS: [&str; 3] = ["{ \"text\": ", "", "\"\\u00e9\" }"];

async fn capture_records() -> Vec<AssistantRecord> {
    let usage = json!({
        "input": 3, "output": 4, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 7,
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 },
    });
    let mut events = vec![
        json!({ "type": "start" }),
        json!({ "type": "text_start", "contentIndex": 0 }),
        json!({ "type": "text_delta", "contentIndex": 0, "delta": "hi" }),
        json!({ "type": "text_end", "contentIndex": 0, "content": "hi", "contentSignature": "tsig" }),
        json!({ "type": "toolcall_start", "contentIndex": 1, "id": "call_1", "toolName": "echo" }),
    ];
    for delta in ARGUMENT_DELTAS {
        events.push(json!({ "type": "toolcall_delta", "contentIndex": 1, "delta": delta }));
    }
    events.push(json!({
        "type": "toolcall_end", "contentIndex": 1,
        "toolCall": { "type": "toolCall", "id": "call_1", "name": "echo", "arguments": { "text": "é" } },
    }));
    events.push(
        json!({ "type": "done", "reason": "toolUse", "usage": usage, "responseId": "resp_pi" }),
    );
    let server = FixtureServer::start("text/event-stream", sse_data(&events, false)).await;
    let model: Model = serde_json::from_value(json!({
        "id": "auto", "name": "fixture", "api": "pi-messages", "provider": "radius",
        "baseUrl": format!("{}/v1", server.origin), "reasoning": false, "input": ["text"],
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
        "contextWindow": 128_000, "maxTokens": 16_384,
    }))
    .expect("model");
    let mut options = StreamOptions::default();
    options.request.api_key = Some("fixture".into());
    drain_records(&stream_pi_messages(
        &model,
        &fixture_context(),
        Some(options),
    ))
    .await
}

// Pi messages producer capture #4272

#[tokio::test]
async fn keeps_the_public_bytes_recorded_before_capture() {
    let _subscription = subscribe_capture_for_test();
    let mut events = public_events(&capture_records().await);
    // TS partials alias the producer's one output; Rust partials are push-time copies.
    let terminal = terminal_message(&events.last().expect("terminal").value).clone();
    for event in &mut events {
        alias_partial(&mut event.value, &terminal);
    }
    let mut snapshot: Value =
        serde_json::from_str(include_str!("capture_fixtures/pi_messages.json")).expect("snapshot");
    zero_timestamps(&mut snapshot);
    assert_eq!(public_bytes(&events), snapshot);
}

#[tokio::test]
async fn captures_the_closure_held_argument_text_as_the_emitted_deltas_with_the_stores_parse_frontier()
 {
    let _subscription = subscribe_capture_for_test();
    let records = capture_records().await;
    assert_eq!(
        record_kinds(&records),
        [
            "start",
            "text_start:0",
            "text_delta:0",
            "text_end:0",
            "toolcall_start:1",
            "toolcall_delta:1",
            "toolcall_delta:1",
            "toolcall_delta:1",
            "toolcall_end:1",
            "done",
        ]
    );
    let events = public_events(&records);
    assert_eq!(
        block_json(capture(first_event(&events, "text_start", Some(0)))),
        json!({ "type": "text", "text": "" })
    );
    assert_eq!(
        block_json(capture(first_event(&events, "text_end", Some(0)))),
        json!({ "type": "text", "text": "hi", "textSignature": "tsig" })
    );
    assert_matches_object(
        &capture_json(capture(first_event(&events, "toolcall_start", Some(1)))),
        &json!({
            "block": { "type": "toolCall", "id": "call_1", "name": "echo", "arguments": {} },
            "rawArgumentPrefix": "",
            "parsedThrough": 0,
            "coveredCodeUnits": 0,
        }),
    );
    assert_eq!(
        delta_captures(&events, "toolcall_delta"),
        vec![
            (ARGUMENT_DELTAS[0].to_owned(), Some(0), Some(10)),
            (String::new(), Some(10), Some(10)),
            (ARGUMENT_DELTAS[2].to_owned(), Some(10), Some(20)),
        ]
    );
    assert_matches_object(
        &block_json(capture(first_event(&events, "toolcall_end", Some(1)))),
        &json!({ "arguments": { "text": "é" } }),
    );
    let done = capture(first_event(&events, "done", None))
        .and_then(|capture| capture.message.as_deref())
        .expect("done capture message");
    assert_matches_object(
        &serde_json::to_value(done).expect("done json"),
        &json!({ "stopReason": "toolUse", "responseId": "resp_pi" }),
    );
    assert!(minted_identity_keys(Some(done)).is_empty());
}
