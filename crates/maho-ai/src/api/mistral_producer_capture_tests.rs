//! Port of senpi packages/ai/test/mistral-producer-capture.test.ts. The TS suite injects a `fetch`
//! answering with fixed SSE; here a local server serves the same bytes to the real `stream()`.

use serde_json::{Value, json};

use crate::api::mistral_conversations::stream as stream_mistral;
use crate::types::{Model, StreamOptions};
use crate::utils::assistant_capture::AssistantRecord;
use crate::utils::capture_test_records::{
    alias_partial, assert_matches_object, block_json, capture, capture_json, delta_captures,
    drain_records, first_event, public_bytes, public_events, record_kinds,
    subscribe_capture_for_test, terminal_message,
};
use crate::utils::capture_test_server::{
    FixtureServer, fixture_context, minted_identity_keys, sse_data,
};

const STRING_ARGUMENTS: [&str; 2] = [r#"{ "text": "#, r#""a\tb" }"#];

fn catalog_model() -> Model {
    crate::models_generated::get_builtin_model("mistral", "devstral-medium-latest")
        .expect("devstral-medium-latest")
        .clone()
}

fn chunk(model_id: &str, delta: Value, finish_reason: Option<&str>) -> Value {
    json!({
        "id": "mistral-response", "model": model_id,
        "choices": [{ "index": 0, "delta": delta, "finish_reason": finish_reason }]
    })
}

async fn capture_records() -> Vec<AssistantRecord> {
    let mut model = catalog_model();
    let id = model.id.clone();
    let mut chunks = vec![
        chunk(&id, json!({ "content": "Hel" }), None),
        chunk(&id, json!({ "content": "lo" }), None),
    ];
    for args in STRING_ARGUMENTS {
        chunks.push(chunk(
            &id,
            json!({ "tool_calls": [{ "id": "call_1", "index": 0, "function": { "name": "echo", "arguments": args } }] }),
            None,
        ));
    }
    chunks.push(chunk(
        &id,
        json!({ "tool_calls": [{ "id": "call_2", "index": 1, "function": { "name": "count", "arguments": { "n": 2, "unit": "x y" } } }] }),
        None,
    ));
    let mut last = chunk(&id, json!({}), Some("tool_calls"));
    last["usage"] = json!({ "prompt_tokens": 3, "completion_tokens": 4, "total_tokens": 7 });
    chunks.push(last);
    let server = FixtureServer::start("text/event-stream", sse_data(&chunks, true)).await;
    model.base_url = server.origin.clone();
    let mut options = StreamOptions::default();
    options.request.api_key = Some("fixture".into());
    drain_records(&stream_mistral(&model, &fixture_context(), Some(options))).await
}

// Mistral producer capture #4272

#[tokio::test]
async fn keeps_the_public_bytes_recorded_before_capture() {
    let _subscription = subscribe_capture_for_test();
    let mut events = public_events(&capture_records().await);
    // TS partials alias the producer's one `output`; Rust partials are push-time copies.
    let terminal = terminal_message(&events.last().expect("terminal").value).clone();
    for event in &mut events {
        alias_partial(&mut event.value, &terminal);
    }
    let expected: Value =
        serde_json::from_str(include_str!("capture_fixtures/mistral.json")).expect("snapshot");
    assert_eq!(public_bytes(&events), expected);
}

#[tokio::test]
async fn freezes_empty_tool_seeds_and_keeps_the_spelling_the_adapter_emitted() {
    let _subscription = subscribe_capture_for_test();
    let records = capture_records().await;
    assert_eq!(
        record_kinds(&records),
        [
            "start",
            "text_start:0",
            "text_delta:0",
            "text_delta:0",
            "text_end:0",
            "toolcall_start:1",
            "toolcall_delta:1",
            "toolcall_delta:1",
            "toolcall_start:2",
            "toolcall_delta:2",
            "toolcall_end:1",
            "toolcall_end:2",
            "done",
        ]
    );
    let events = public_events(&records);
    for content_index in [1, 2] {
        assert_matches_object(
            &capture_json(capture(first_event(
                &events,
                "toolcall_start",
                Some(content_index),
            ))),
            &json!({
                "block": { "type": "toolCall", "arguments": {} },
                "rawArgumentPrefix": "",
                "parsedThrough": 0,
                "coveredCodeUnits": 0,
            }),
        );
    }
    // Object arguments reach the stream as the adapter's own serialization, which is what readers
    // saw.
    assert_eq!(
        delta_captures(&events, "toolcall_delta"),
        [
            (STRING_ARGUMENTS[0].to_string(), Some(0), Some(10)),
            (STRING_ARGUMENTS[1].to_string(), Some(10), Some(18)),
            (r#"{"n":2,"unit":"x y"}"#.to_string(), Some(0), Some(20)),
        ]
    );
    assert_eq!(
        delta_captures(&events, "text_delta"),
        [
            ("Hel".to_string(), Some(0), None),
            ("lo".to_string(), Some(3), None),
        ]
    );
    assert_matches_object(
        &block_json(capture(first_event(&events, "toolcall_end", Some(1)))),
        &json!({ "arguments": { "text": "a\tb" } }),
    );
    assert_matches_object(
        &block_json(capture(first_event(&events, "toolcall_end", Some(2)))),
        &json!({ "arguments": { "n": 2, "unit": "x y" } }),
    );
    let done =
        capture(first_event(&events, "done", None)).and_then(|capture| capture.message.as_deref());
    assert_eq!(
        done.map(|message| message.stop_reason),
        Some(crate::types::StopReason::ToolUse)
    );
    assert_eq!(minted_identity_keys(done), Vec::<String>::new());
}
