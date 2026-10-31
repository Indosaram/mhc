//! Port of senpi packages/ai/test/google-producer-capture.test.ts. The TS suite mocks
//! `@google/genai` to yield fixed chunks; here a local server serves the same chunks as SSE to the
//! real google and vertex `stream()`.

use serde_json::{Value, json};

use crate::types::{Model, StreamOptions};
use crate::utils::assistant_capture::{AssistantProgress, AssistantRecord};
use crate::utils::capture_test_records::{
    alias_partial, assert_matches_object, block_json, capture, capture_json, delta_captures,
    drain_records, first_event, progress_records, public_bytes, public_events, record_kinds,
    subscribe_capture_for_test, terminal_message,
};
use crate::utils::capture_test_server::{
    FixtureServer, fixture_context, minted_identity_keys, sse_data,
};
use crate::utils::event_stream::AssistantMessageEventStream;

pub(super) const ADAPTERS: [&str; 2] = ["google", "vertex"];

pub(super) fn adapter_model(name: &str) -> Model {
    let (provider, id) = match name {
        "google" => ("google", "gemini-2.5-flash"),
        _ => ("google-vertex", "gemini-3-flash-preview"),
    };
    crate::models_generated::get_builtin_model(provider, id)
        .unwrap_or_else(|| panic!("{provider}/{id}"))
        .clone()
}

/// Opens `name`'s stream against a server answering with `chunks`.
pub(super) async fn open(
    name: &str,
    chunks: &[Value],
) -> (AssistantMessageEventStream, FixtureServer) {
    let server = FixtureServer::start("text/event-stream", sse_data(chunks, false)).await;
    let mut model = adapter_model(name);
    model.base_url = server.origin.clone();
    let mut options = StreamOptions::default();
    options.request.api_key = Some("x".into());
    let stream = match name {
        "google" => {
            crate::api::google_generative_ai::stream(&model, &fixture_context(), Some(options))
        }
        _ => crate::api::google_vertex::stream(&model, &fixture_context(), Some(options)),
    };
    (stream, server)
}

fn args() -> Value {
    json!({ "text": "a  b", "n": 1 })
}
fn executable_code() -> Value {
    json!({ "executableCode": { "language": "PYTHON", "code": "print(1)" } })
}
fn grounding_metadata() -> Value {
    json!({ "webSearchQueries": ["q"] })
}
fn url_context_metadata() -> Value {
    json!({ "urlMetadata": [{ "retrievedUrl": "https://docs.example.com" }] })
}

fn parts(value: Vec<Value>) -> Value {
    json!({ "candidates": [{ "content": { "parts": value } }] })
}

async fn capture_records(name: &str) -> Vec<AssistantRecord> {
    let mut first = parts(vec![
        json!({ "text": "plan", "thought": true, "thoughtSignature": "c2lnMQ==" }),
    ]);
    first["responseId"] = json!("resp_google");
    let chunks = vec![
        first,
        parts(vec![json!({ "text": " more", "thought": true })]),
        parts(vec![
            json!({ "text": "answer", "thoughtSignature": "dHh0MQ==" }),
        ]),
        parts(vec![
            json!({ "functionCall": { "id": "call_1", "name": "echo", "args": args() } }),
        ]),
        parts(vec![executable_code()]),
        json!({
            "candidates": [{ "content": { "parts": [] }, "groundingMetadata": grounding_metadata(),
                "urlContextMetadata": url_context_metadata(), "finishReason": "STOP" }],
            "usageMetadata": { "promptTokenCount": 3, "candidatesTokenCount": 4, "totalTokenCount": 7 },
        }),
    ];
    let (stream, _server) = open(name, &chunks).await;
    drain_records(&stream).await
}

fn native(content_index: usize, subtype: &str, raw: Value) -> [Value; 2] {
    let block = json!({ "type": "providerNative", "subtype": subtype, "raw": raw });
    [
        json!({ "type": "message_block_state", "contentIndex": content_index, "phase": "insert", "block": block }),
        json!({ "type": "message_block_state", "contentIndex": content_index, "phase": "end", "block": block }),
    ]
}

fn snapshot(name: &str) -> Value {
    let text = match name {
        "google" => include_str!("capture_fixtures/google_0.json"),
        _ => include_str!("capture_fixtures/google_1.json"),
    };
    let mut value: Value = serde_json::from_str(text).expect("snapshot");
    crate::utils::capture_test_records::zero_timestamps(&mut value);
    value
}

// %s producer capture #4272

#[tokio::test]
async fn keeps_the_public_bytes_recorded_before_capture() {
    for name in ADAPTERS {
        let _subscription = subscribe_capture_for_test();
        let mut events = public_events(&capture_records(name).await);
        // TS partials alias the producer's one output; Rust partials are push-time copies.
        let terminal = terminal_message(&events.last().expect("terminal").value).clone();
        for event in &mut events {
            alias_partial(&mut event.value, &terminal);
        }
        assert_eq!(public_bytes(&events), snapshot(name), "{name}");
    }
}

#[tokio::test]
async fn records_signature_changes_and_event_less_native_blocks_in_order() {
    for name in ADAPTERS {
        let _subscription = subscribe_capture_for_test();
        let records = capture_records(name).await;
        assert_eq!(
            record_kinds(&records),
            [
                "start",
                "thinking_start:0",
                "message_block_metadata:0",
                "thinking_delta:0",
                "thinking_delta:0",
                "thinking_end:0",
                "text_start:1",
                "message_block_metadata:1",
                "text_delta:1",
                "text_end:1",
                "toolcall_start:2",
                "toolcall_delta:2",
                "toolcall_end:2",
                "message_block_state:3:insert",
                "message_block_state:3:end",
                "message_block_state:4:insert",
                "message_block_state:4:end",
                "message_block_state:5:insert",
                "message_block_state:5:end",
                "done",
            ],
            "{name}"
        );
        let mut expected = vec![
            json!({ "type": "message_block_metadata", "contentIndex": 0, "thinkingSignature": "c2lnMQ==" }),
            json!({ "type": "message_block_metadata", "contentIndex": 1, "textSignature": "dHh0MQ==" }),
        ];
        expected.extend(native(3, "executableCode", executable_code()));
        expected.extend(native(4, "groundingMetadata", grounding_metadata()));
        expected.extend(native(5, "urlContextMetadata", url_context_metadata()));
        assert_eq!(progress_records(&records), expected, "{name}");
        // `Object.isFrozen(record.block)` and `states[0].block === states[1].block`: a block complete
        // at insertion shares one immutable `Arc` between its two transitions.
        let states: Vec<_> = records
            .iter()
            .filter_map(|record| match record {
                AssistantRecord::Progress(progress) => match progress.as_ref() {
                    AssistantProgress::MessageBlockState { block, .. } => Some(block),
                    _ => None,
                },
                AssistantRecord::Event(_) => None,
            })
            .collect();
        assert!(
            std::sync::Arc::ptr_eq(states[0], states[1]),
            "{name}: insert and end share one block"
        );
        let events = public_events(&records);
        assert_eq!(
            block_json(capture(first_event(&events, "thinking_start", Some(0)))),
            json!({ "type": "thinking", "thinking": "" }),
            "{name}"
        );
        assert_eq!(
            block_json(capture(first_event(&events, "text_end", Some(1)))),
            json!({ "type": "text", "text": "answer", "textSignature": "dHh0MQ==" }),
            "{name}"
        );
    }
}

#[tokio::test]
async fn keeps_the_complete_parsed_seed_and_the_emitted_spelling_without_a_parse() {
    for name in ADAPTERS {
        let _subscription = subscribe_capture_for_test();
        let events = public_events(&capture_records(name).await);
        assert_matches_object(
            &capture_json(capture(first_event(&events, "toolcall_start", Some(2)))),
            &json!({
                "block": { "type": "toolCall", "id": "call_1", "name": "echo", "arguments": args() },
                "rawArgumentPrefix": "",
                "parsedThrough": 0,
                "coveredCodeUnits": 0,
            }),
        );
        assert_eq!(
            delta_captures(&events, "toolcall_delta"),
            vec![(r#"{"text":"a  b","n":1}"#.to_owned(), Some(0), Some(0))],
            "{name}"
        );
        let done = capture(first_event(&events, "done", None))
            .and_then(|capture| capture.message.as_deref())
            .expect("done capture message");
        assert_eq!(
            serde_json::to_value(&done.stop_reason).expect("stop"),
            json!("toolUse")
        );
        assert!(minted_identity_keys(Some(done)).is_empty(), "{name}");
    }
}
