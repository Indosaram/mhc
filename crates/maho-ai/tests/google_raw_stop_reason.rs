//! Port of senpi packages/ai/test/google-raw-stop-reason.test.ts.

mod google_fixtures;

use google_fixtures::server::{sse_body, RecordingServer};
use google_fixtures::{builtin_model, context, user_text};
use maho_ai::api::google_generative_ai::stream as stream_google_generative_ai;
use maho_ai::api::google_vertex::stream as stream_google_vertex;
use maho_ai::types::{
    AssistantMessageEventStream, ContentBlock, Model, ProviderRequestOptions, StopReason, StreamOptions,
};
use maho_ai::utils::pi_user_agent::get_pi_user_agent;
use serde_json::{json, Value};

fn stop_reason_chunk(finish_reason: &str, include_function_call: bool) -> Value {
    let mut candidate = json!({ "finishReason": finish_reason });
    if include_function_call {
        candidate["content"] = json!({
            "parts": [{
                "functionCall": { "id": "call-1", "name": "echo", "args": { "value": "truncated" } },
            }],
        });
    }
    json!({
        "responseId": "google-response-id",
        "candidates": [candidate],
        "usageMetadata": { "promptTokenCount": 1, "candidatesTokenCount": 0, "totalTokenCount": 1 },
    })
}

fn options(api_key: Option<&str>, headers: Option<Vec<(&str, &str)>>) -> StreamOptions {
    StreamOptions {
        request: ProviderRequestOptions {
            api_key: api_key.map(str::to_owned),
            headers: headers.map(|headers| {
                headers.into_iter().map(|(name, value)| (name.to_owned(), Some(value.to_owned()))).collect()
            }),
            ..ProviderRequestOptions::default()
        },
        ..StreamOptions::default()
    }
}

fn google_model(base_url: &str) -> Model {
    let mut model = builtin_model("google", "gemini-2.5-flash");
    model.base_url = base_url.to_owned();
    model
}

fn vertex_model(base_url: &str) -> Model {
    let mut model = builtin_model("google-vertex", "gemini-3-flash-preview");
    model.base_url = base_url.to_owned();
    model
}

async fn result(stream: AssistantMessageEventStream) -> maho_ai::types::AssistantMessage {
    tokio::time::timeout(std::time::Duration::from_secs(10), stream.result())
        .await
        .expect("bounded wait")
        .expect("result")
}

async fn capture_google_headers(headers: Option<Vec<(&str, &str)>>) -> Vec<(String, String)> {
    let server = RecordingServer::start(sse_body(&[stop_reason_chunk("STOP", false)]), "text/event-stream").await;
    let message = result(stream_google_generative_ai(
        &google_model(&server.base_url()),
        &context(vec![user_text("hello")]),
        Some(options(Some("test-api-key"), headers)),
    ))
    .await;
    assert_eq!(message.stop_reason, StopReason::Stop, "{:?}", message.error_message);
    server.last_request().headers
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers.iter().find(|(header, _)| header.eq_ignore_ascii_case(name)).map(|(_, value)| value.as_str())
}

#[tokio::test]
async fn preserves_raw_gemini_finish_reasons_for_google_generative_ai_errors() {
    let server = RecordingServer::start(
        sse_body(&[stop_reason_chunk("MALFORMED_FUNCTION_CALL", false)]),
        "text/event-stream",
    )
    .await;
    let message = result(stream_google_generative_ai(
        &google_model(&server.base_url()),
        &context(vec![user_text("hello")]),
        Some(options(Some("test-api-key"), None)),
    ))
    .await;

    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("MALFORMED_FUNCTION_CALL"));
    assert_eq!(message.error_message.as_deref(), Some("Provider stopped with: MALFORMED_FUNCTION_CALL"));
}

#[tokio::test]
async fn preserves_raw_gemini_finish_reasons_for_google_vertex_errors() {
    let server = RecordingServer::start(sse_body(&[stop_reason_chunk("SAFETY", false)]), "text/event-stream").await;
    let message = result(stream_google_vertex(
        &vertex_model(&server.base_url()),
        &context(vec![user_text("hello")]),
        Some(options(Some("test-api-key"), None)),
    ))
    .await;

    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("SAFETY"));
    assert_eq!(message.error_message.as_deref(), Some("Provider stopped with: SAFETY"));
}

#[tokio::test]
async fn preserves_max_tokens_with_a_tool_call_as_length_for_google_generative_ai() {
    let server = RecordingServer::start(sse_body(&[stop_reason_chunk("MAX_TOKENS", true)]), "text/event-stream").await;
    let message = result(stream_google_generative_ai(
        &google_model(&server.base_url()),
        &context(vec![user_text("hello")]),
        Some(options(Some("test-api-key"), None)),
    ))
    .await;

    assert_eq!(message.stop_reason, StopReason::Length);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("MAX_TOKENS"));
    assert!(message.content.iter().any(|block| matches!(block, ContentBlock::ToolCall(_))));
}

#[tokio::test]
async fn preserves_max_tokens_with_a_tool_call_as_length_for_google_vertex() {
    let server = RecordingServer::start(sse_body(&[stop_reason_chunk("MAX_TOKENS", true)]), "text/event-stream").await;
    let message = result(stream_google_vertex(
        &vertex_model(&server.base_url()),
        &context(vec![user_text("hello")]),
        Some(options(Some("test-api-key"), None)),
    ))
    .await;

    assert_eq!(message.stop_reason, StopReason::Length);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("MAX_TOKENS"));
    assert!(message.content.iter().any(|block| matches!(block, ContentBlock::ToolCall(_))));
}

#[tokio::test]
async fn maps_stop_with_a_tool_call_to_tool_use_for_google_generative_ai() {
    let server = RecordingServer::start(sse_body(&[stop_reason_chunk("STOP", true)]), "text/event-stream").await;
    let message = result(stream_google_generative_ai(
        &google_model(&server.base_url()),
        &context(vec![user_text("hello")]),
        Some(options(Some("test-api-key"), None)),
    ))
    .await;

    assert_eq!(message.stop_reason, StopReason::ToolUse);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("STOP"));
    assert!(message.content.iter().any(|block| matches!(block, ContentBlock::ToolCall(_))));
}

#[tokio::test]
async fn maps_stop_with_a_tool_call_to_tool_use_for_google_vertex() {
    let server = RecordingServer::start(sse_body(&[stop_reason_chunk("STOP", true)]), "text/event-stream").await;
    let message = result(stream_google_vertex(
        &vertex_model(&server.base_url()),
        &context(vec![user_text("hello")]),
        Some(options(Some("test-api-key"), None)),
    ))
    .await;

    assert_eq!(message.stop_reason, StopReason::ToolUse);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("STOP"));
    assert!(message.content.iter().any(|block| matches!(block, ContentBlock::ToolCall(_))));
}

#[tokio::test]
async fn uses_pis_user_agent_by_default() {
    let headers = capture_google_headers(None).await;
    assert_eq!(header(&headers, "User-Agent"), Some(get_pi_user_agent().as_str()));
}

#[tokio::test]
async fn lets_explicit_headers_override_the_default_user_agent() {
    let headers = capture_google_headers(Some(vec![("User-Agent", "custom-agent")])).await;
    assert_eq!(header(&headers, "User-Agent"), Some("custom-agent"));
}
