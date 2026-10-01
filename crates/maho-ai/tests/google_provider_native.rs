//! Port of senpi packages/ai/test/google.provider-native.test.ts.

mod google_fixtures;

use google_fixtures::server::{sse_body, RecordingServer};
use google_fixtures::{assistant, builtin_model, context, text, user_text};
use maho_ai::api::google_generative_ai::stream as stream_google;
use maho_ai::api::google_shared::{convert_messages, ConvertMessagesOptions};
use maho_ai::api::google_vertex::stream as stream_vertex;
use maho_ai::types::{ContentBlock, ProviderNativeContent, ProviderRequestOptions, StopReason, StreamOptions};
use serde_json::{json, Value};

fn provider_native_stream_chunks() -> Vec<Value> {
    vec![
        json!({
            "responseId": "resp_google_native",
            "candidates": [{
                "content": {
                    "parts": [
                        { "text": "done" },
                        { "executableCode": { "language": "python", "code": "print(1)" } },
                        { "codeExecutionResult": { "outcome": "OUTCOME_OK", "output": "1\n" } },
                    ],
                },
                "groundingMetadata": {
                    "webSearchQueries": ["gemini code execution"],
                    "groundingChunks": [{ "web": { "uri": "https://example.com", "title": "Example" } }],
                },
                "urlContextMetadata": {
                    "urlMetadata": [{ "retrievedUrl": "https://docs.example.com" }],
                },
            }],
        }),
        json!({
            "candidates": [{
                "content": { "parts": [] },
                "groundingMetadata": { "webSearchQueries": ["gemini code execution"] },
                "urlContextMetadata": { "urlMetadata": [{ "retrievedUrl": "https://docs.example.com" }] },
                "finishReason": "STOP",
            }],
            "usageMetadata": {
                "promptTokenCount": 3,
                "candidatesTokenCount": 2,
                "totalTokenCount": 5,
            },
        }),
    ]
}

fn expected_provider_native_blocks() -> Vec<ProviderNativeContent> {
    vec![
        ProviderNativeContent {
            subtype: "executableCode".into(),
            raw: json!({ "executableCode": { "language": "python", "code": "print(1)" } }),
        },
        ProviderNativeContent {
            subtype: "codeExecutionResult".into(),
            raw: json!({ "codeExecutionResult": { "outcome": "OUTCOME_OK", "output": "1\n" } }),
        },
        ProviderNativeContent {
            subtype: "groundingMetadata".into(),
            raw: json!({
                "webSearchQueries": ["gemini code execution"],
                "groundingChunks": [{ "web": { "uri": "https://example.com", "title": "Example" } }],
            }),
        },
        ProviderNativeContent {
            subtype: "urlContextMetadata".into(),
            raw: json!({ "urlMetadata": [{ "retrievedUrl": "https://docs.example.com" }] }),
        },
    ]
}

fn options(api_key: &str) -> StreamOptions {
    StreamOptions {
        request: ProviderRequestOptions { api_key: Some(api_key.into()), ..ProviderRequestOptions::default() },
        ..StreamOptions::default()
    }
}

#[tokio::test]
async fn surfaces_executable_code_and_candidate_metadata_once_for_google_and_vertex() {
    let server = RecordingServer::start(sse_body(&provider_native_stream_chunks()), "text/event-stream").await;

    let mut google_model = builtin_model("google", "gemini-2.5-flash");
    google_model.base_url = server.base_url();
    let google_message = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        stream_google(&google_model, &context(vec![user_text("hello")]), Some(options("x"))).result(),
    )
    .await
    .expect("bounded wait")
    .expect("result");

    let mut vertex_model = builtin_model("google-vertex", "gemini-3-flash-preview");
    vertex_model.base_url = server.base_url();
    let vertex_message = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        stream_vertex(&vertex_model, &context(vec![user_text("hello")]), Some(options("x"))).result(),
    )
    .await
    .expect("bounded wait")
    .expect("result");

    for message in [&google_message, &vertex_message] {
        assert_eq!(message.stop_reason, StopReason::Stop, "{:?}", message.error_message);
        let provider_native: Vec<ProviderNativeContent> = message
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ProviderNative(block) => Some(block.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(provider_native, expected_provider_native_blocks());
    }
}

#[test]
fn skips_provider_native_blocks_when_converting_assistant_replay_messages() {
    let model = builtin_model("google", "gemini-2.5-flash");
    let previous = assistant(
        &model,
        vec![
            text("kept"),
            ContentBlock::ProviderNative(ProviderNativeContent {
                subtype: "executableCode".into(),
                raw: json!({ "executableCode": { "code": "print(1)" } }),
            }),
            ContentBlock::ProviderNative(ProviderNativeContent {
                subtype: "groundingMetadata".into(),
                raw: json!({ "webSearchQueries": ["gemini code execution"] }),
            }),
        ],
        StopReason::Stop,
    );

    let replay = convert_messages(
        &model,
        &context(vec![user_text("hello"), previous]),
        ConvertMessagesOptions::default(),
    );

    let assistant_replay = replay.iter().find(|item| item.role == "model").expect("a model turn");
    assert_eq!(assistant_replay.parts, vec![json!({ "text": "kept" }).as_object().cloned().expect("object")]);
}
