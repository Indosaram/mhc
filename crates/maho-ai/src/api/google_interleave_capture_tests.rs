//! Port of senpi packages/ai/test/google-interleave-capture.test.ts.

use serde_json::{Value, json};

use crate::utils::assistant_capture::AssistantRecord;
use crate::utils::capture_test_records::{
    alias_partial, block_json, capture, delta_captures, drain_records, event_type, first_event,
    progress_records, public_bytes, public_events, record_kinds, records_on_foreign_blocks,
    subscribe_capture_for_test, terminal_message, zero_timestamps,
};

use super::google_producer_capture_tests::{ADAPTERS, open};

fn code() -> Value {
    json!({ "executableCode": { "language": "PYTHON", "code": "print(1)" } })
}
fn result() -> Value {
    json!({ "codeExecutionResult": { "outcome": "OUTCOME_OK", "output": "1\n" } })
}
fn finish() -> Value {
    json!({
        "candidates": [{ "content": { "parts": [] }, "finishReason": "STOP" }],
        "usageMetadata": { "promptTokenCount": 3, "candidatesTokenCount": 4, "totalTokenCount": 7 },
    })
}
fn thinking_end() -> Value {
    json!({ "type": "thinking", "thinking": "plan more", "thinkingSignature": "c2lnMQ==" })
}
fn text_end() -> Value {
    json!({ "type": "text", "text": "answer more", "textSignature": "dHh0MQ==" })
}

fn chunk(parts: Vec<Value>, metadata: Value) -> Value {
    let mut candidate = json!({ "content": { "parts": parts } });
    if let Value::Object(metadata) = metadata {
        candidate
            .as_object_mut()
            .expect("candidate")
            .extend(metadata);
    }
    json!({ "candidates": [candidate] })
}

struct Scenario {
    name: &'static str,
    chunks: Vec<Value>,
    kinds: Vec<&'static str>,
    thinking_offsets: Vec<usize>,
    text_offsets: Vec<usize>,
    ends: Vec<Value>,
    signatures: Vec<Value>,
}

fn scenarios() -> Vec<Scenario> {
    let grounding_metadata = json!({ "webSearchQueries": ["q"] });
    let url_context_metadata =
        json!({ "urlMetadata": [{ "retrievedUrl": "https://docs.example.com" }] });
    vec![
        Scenario {
            name: "thinking, then text, each interleaved",
            chunks: vec![
                chunk(vec![json!({ "text": "plan", "thought": true })], json!({})),
                chunk(
                    vec![code()],
                    json!({ "groundingMetadata": grounding_metadata }),
                ),
                chunk(
                    vec![
                        json!({ "text": " more", "thought": true, "thoughtSignature": "c2lnMQ==" }),
                    ],
                    json!({}),
                ),
                chunk(vec![json!({ "text": "answer" })], json!({})),
                chunk(
                    vec![result()],
                    json!({ "urlContextMetadata": url_context_metadata }),
                ),
                chunk(
                    vec![json!({ "text": " more", "thoughtSignature": "dHh0MQ==" })],
                    json!({}),
                ),
                finish(),
            ],
            kinds: vec![
                "start",
                "thinking_start:0",
                "thinking_delta:0",
                "message_block_state:1:insert",
                "message_block_state:1:end",
                "message_block_state:2:insert",
                "message_block_state:2:end",
                "message_block_metadata:0",
                "thinking_delta:0",
                "thinking_end:0",
                "text_start:3",
                "text_delta:3",
                "message_block_state:4:insert",
                "message_block_state:4:end",
                "message_block_state:5:insert",
                "message_block_state:5:end",
                "message_block_metadata:3",
                "text_delta:3",
                "text_end:3",
                "done",
            ],
            thinking_offsets: vec![0, 4],
            text_offsets: vec![0, 6],
            ends: vec![thinking_end(), text_end()],
            signatures: vec![
                json!({ "type": "message_block_metadata", "contentIndex": 0, "thinkingSignature": "c2lnMQ==" }),
                json!({ "type": "message_block_metadata", "contentIndex": 3, "textSignature": "dHh0MQ==" }),
            ],
        },
        Scenario {
            name: "text interleaved, closed by a function call",
            chunks: vec![
                chunk(vec![json!({ "text": "answer" })], json!({})),
                chunk(
                    vec![code()],
                    json!({ "groundingMetadata": grounding_metadata, "urlContextMetadata": url_context_metadata }),
                ),
                chunk(
                    vec![json!({ "text": " more", "thoughtSignature": "dHh0MQ==" })],
                    json!({}),
                ),
                chunk(
                    vec![
                        json!({ "functionCall": { "id": "call_1", "name": "echo", "args": { "text": "x" } } }),
                    ],
                    json!({}),
                ),
                finish(),
            ],
            kinds: vec![
                "start",
                "text_start:0",
                "text_delta:0",
                "message_block_state:1:insert",
                "message_block_state:1:end",
                "message_block_state:2:insert",
                "message_block_state:2:end",
                "message_block_state:3:insert",
                "message_block_state:3:end",
                "message_block_metadata:0",
                "text_delta:0",
                "text_end:0",
                "toolcall_start:4",
                "toolcall_delta:4",
                "toolcall_end:4",
                "done",
            ],
            thinking_offsets: vec![],
            text_offsets: vec![0, 6],
            ends: vec![text_end()],
            signatures: vec![
                json!({ "type": "message_block_metadata", "contentIndex": 0, "textSignature": "dHh0MQ==" }),
            ],
        },
    ]
}

/// Snapshot keys sort alphabetically: per adapter, "text interleaved..." before "thinking...".
fn snapshot(adapter: usize, scenario: usize) -> Value {
    let text = match adapter * 2 + (1 - scenario) {
        0 => include_str!("capture_fixtures/google_interleave_0.json"),
        1 => include_str!("capture_fixtures/google_interleave_1.json"),
        2 => include_str!("capture_fixtures/google_interleave_2.json"),
        _ => include_str!("capture_fixtures/google_interleave_3.json"),
    };
    let mut value: Value = serde_json::from_str(text).expect("snapshot");
    zero_timestamps(&mut value);
    value
}

/// `internalKinds`: `recordKinds` with each event at its capture's internal index.
fn internal_kinds(records: &[AssistantRecord]) -> Vec<String> {
    record_kinds(records)
        .into_iter()
        .zip(records)
        .map(|(kind, record)| match record {
            AssistantRecord::Event(event) => {
                match capture(event).and_then(|capture| capture.content_index) {
                    Some(index) => format!("{}:{index}", event_type(&event.value)),
                    None => kind,
                }
            }
            AssistantRecord::Progress(_) => kind,
        })
        .collect()
}

// %s capture while native parts interleave #4272

/// `it.each(Object.entries(scenarios))("keeps the open block's own index: %s")`, per adapter.
#[tokio::test]
async fn keeps_the_open_blocks_own_index() {
    for (adapter_index, adapter) in ADAPTERS.into_iter().enumerate() {
        for (scenario_index, scenario) in scenarios().into_iter().enumerate() {
            let _subscription = subscribe_capture_for_test();
            let case = format!("{adapter}: {}", scenario.name);
            let (stream, _server) = open(adapter, &scenario.chunks).await;
            let records = drain_records(&stream).await;
            let mut events = public_events(&records);
            let done = terminal_message(&first_event(&events, "done", None).value).clone();
            // TS partials alias the producer's one output; Rust partials are push-time copies.
            let mut aliased = events.clone();
            for event in &mut aliased {
                alias_partial(&mut event.value, &done);
            }
            assert_eq!(
                public_bytes(&aliased),
                snapshot(adapter_index, scenario_index),
                "{case}"
            );
            assert_eq!(internal_kinds(&records), scenario.kinds, "{case}");
            assert_eq!(
                records_on_foreign_blocks(&records, &done),
                Vec::<String>::new(),
                "{case}"
            );
            let offsets = |kind: &str| -> Vec<usize> {
                delta_captures(&events, kind)
                    .into_iter()
                    .map(|(_, offset, _)| offset.expect("delta offset"))
                    .collect()
            };
            assert_eq!(
                (offsets("thinking_delta"), offsets("text_delta")),
                (
                    scenario.thinking_offsets.clone(),
                    scenario.text_offsets.clone()
                ),
                "{case}"
            );
            events.retain(|event| {
                matches!(
                    event_type(&event.value).as_str(),
                    "text_end" | "thinking_end"
                )
            });
            let ends: Vec<Value> = events
                .iter()
                .map(|event| block_json(capture(event)))
                .collect();
            assert_eq!(ends, scenario.ends, "{case}");
            let signatures: Vec<Value> = progress_records(&records)
                .into_iter()
                .filter(|record| record["type"] == "message_block_metadata")
                .collect();
            assert_eq!(signatures, scenario.signatures, "{case}");
        }
    }
}
