//! Recorded-stream replay equality for `api/anthropic-messages.ts` (plan todo 11 QA), plus the
//! retry classification of a 529 overloaded response.
//!
//! `tools/golden/ai-replay.mjs` drives the pinned senpi `stream()` against each recorded fixture and
//! writes the resulting event JSON; these tests serve the same fixture bytes to the Rust `stream()`
//! and require the same event sequence.

mod anthropic_common;
mod support;

use anthropic_common::{harness_haiku, CaptureServer};
use maho_ai::api::anthropic_messages;
use maho_ai::types::{
    AssistantMessageEvent, Context, InputModality, Message, Model, ModelCost, StreamOptions, UserContent, UserMessage,
};
use maho_ai::utils::retry_profile::failure::{normalize_anthropic_retry_failure, RetryErrorShape};
use maho_ai::utils::retry_profile::types::RetryFailureKind;
use serde_json::Value;
use std::path::PathBuf;
use support::mock_server::{Fixture, MockFixtureServer};

fn case_path(case: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tools/golden/cases").join(format!("{case}.json"))
}

fn load_case(case: &str) -> Value {
    let path = case_path(case);
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {path:?}: {error}")))
        .expect("replay case JSON")
}

fn sse_fixture_bytes(spec: &Value) -> Vec<u8> {
    let mut body = String::new();
    for event in spec["fixture"]["events"].as_array().expect("fixture.events") {
        if let Some(name) = event["event"].as_str() {
            body.push_str(&format!("event: {name}\n"));
        }
        body.push_str(&format!("data: {}\n\n", event["data"].as_str().expect("fixture event data")));
    }
    body.into_bytes()
}

/// Builds the `Model` the golden generator used: the case's model object with `api`/`baseUrl`
/// applied on top (`runCase` in `tools/golden/ai-replay.mjs`).
fn case_model(spec: &Value, base_url: &str) -> Model {
    let model = &spec["model"];
    let mut built = Model {
        id: model["id"].as_str().unwrap_or_default().to_owned(),
        name: model["name"].as_str().unwrap_or_default().to_owned(),
        api: spec["api"].as_str().expect("case api").to_owned(),
        provider: model["provider"].as_str().expect("case model provider").to_owned(),
        base_url: base_url.to_owned(),
        reasoning: model["reasoning"].as_bool().unwrap_or(false),
        thinking_level_map: None,
        input: model["input"]
            .as_array()
            .map(|modalities| {
                modalities
                    .iter()
                    .filter_map(|modality| match modality.as_str() {
                        Some("text") => Some(InputModality::Text),
                        Some("image") => Some(InputModality::Image),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default(),
        cost: ModelCost {
            input: model["cost"]["input"].as_f64().unwrap_or_default(),
            output: model["cost"]["output"].as_f64().unwrap_or_default(),
            cache_read: model["cost"]["cacheRead"].as_f64().unwrap_or_default(),
            cache_write: model["cost"]["cacheWrite"].as_f64().unwrap_or_default(),
            tiers: None,
        },
        context_window: model["contextWindow"].as_u64().unwrap_or_default(),
        max_tokens: model["maxTokens"].as_u64().unwrap_or_default(),
        sampling_params: None,
        headers: None,
        cache_retention: None,
        upstream_model_id: None,
        service_tier: None,
        recover_text_tool_calls: None,
        compat: None,
    };
    built.thinking_level_map = None;
    built
}

fn case_context(spec: &Value) -> Context {
    let messages = spec["context"]["messages"]
        .as_array()
        .expect("context messages")
        .iter()
        .map(|message| {
            let content = message["content"].as_str().expect("a string user message");
            Message::User(UserMessage { content: UserContent::Text(content.to_owned()), timestamp: 0 })
        })
        .collect();
    Context { system_prompt: None, messages, tools: None }
}

fn case_options(spec: &Value) -> StreamOptions {
    let mut options = StreamOptions::default();
    options.request.api_key = spec["options"]["apiKey"].as_str().map(str::to_owned);
    options.request.max_retries = spec["options"]["maxRetries"].as_u64().map(|value| value as u32);
    options
}

async fn replay(case: &str) -> Vec<AssistantMessageEvent> {
    let spec = load_case(case);
    let server = MockFixtureServer::start(Fixture {
        status: axum::http::StatusCode::OK,
        headers: vec![(
            axum::http::HeaderName::from_static("content-type"),
            axum::http::HeaderValue::from_static("text/event-stream"),
        )],
        body: sse_fixture_bytes(&spec),
        close_after_bytes: None,
    })
    .await;
    let model = case_model(&spec, server.base_url());
    let context = case_context(&spec);
    let options = case_options(&spec);
    let stream = anthropic_messages::stream(&model, &context, Some(options));
    let events = tokio::time::timeout(std::time::Duration::from_secs(10), stream.collect())
        .await
        .expect("replay round trip within the bound")
        .expect("stream collected");
    server.shutdown();
    reapply_shared_message_containers(events)
}

/// Reproduces the aliasing senpi's replay harness records.
///
/// senpi yields events that all reference one live `AssistantMessage`, and `normalizeEvent` copies
/// the event object shallowly, so a nested array or object the implementation mutates in place
/// (the content blocks and `usage`) shows its final state in every earlier event, while a scalar it
/// assigns keeps the value it had at yield time. Serializing the live message back into each
/// message-bearing event restores that, exactly as `tools/golden/ai-replay.mjs` recorded it.
fn reapply_shared_message_containers(events: Vec<AssistantMessageEvent>) -> Vec<AssistantMessageEvent> {
    let raw: Vec<Value> = events.iter().map(|event| serde_json::to_value(event).expect("event JSON")).collect();
    let terminal = raw
        .iter()
        .rev()
        .find_map(|event| {
            event
                .get("message")
                .or_else(|| event.get("error"))
                .filter(|value| value.get("role").is_some())
                .cloned()
        })
        .expect("a terminal message");
    let mut normalized = raw;
    for event in &mut normalized {
        let Some(object) = event.as_object_mut() else { continue };
        for key in ["partial", "message", "error"] {
            let Some(message) = object.get_mut(key).and_then(Value::as_object_mut) else { continue };
            if !message.contains_key("role") {
                continue;
            }
            for container in ["content", "usage"] {
                if let Some(value) = terminal.get(container) {
                    message.insert(container.to_owned(), value.clone());
                }
            }
            message.insert("timestamp".to_owned(), Value::from(0));
        }
    }
    serde_json::from_value(Value::Array(normalized)).expect("normalized events match the Rust event types")
}

#[tokio::test]
async fn replays_the_recorded_thinking_and_tool_use_stream() {
    let events = replay("ai-replay-anthropic-thinking-tool").await;
    if std::env::var_os("MAHO_AI_EVIDENCE_DUMP").is_some() {
        println!("{}", serde_json::to_string_pretty(&events).expect("event JSON"));
    }
    support::replay::assert_matches_golden("ai-replay-anthropic-thinking-tool", &events);
}

#[tokio::test]
async fn replays_the_recorded_basic_stream() {
    let events = replay("ai-replay-anthropic-basic").await;
    support::replay::assert_matches_golden("ai-replay-anthropic-basic", &events);
}

const OVERLOADED_BODY: &str =
    r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"},"request_id":"req_replay_529"}"#;

#[tokio::test]
async fn a_529_overloaded_response_is_retried_as_a_transient_http_failure() {
    let server = CaptureServer::start_response(
        axum::http::StatusCode::from_u16(529).expect("529"),
        vec![(axum::http::HeaderName::from_static("content-type"), axum::http::HeaderValue::from_static("application/json"))],
        OVERLOADED_BODY.as_bytes().to_vec(),
    )
    .await;

    let mut model = harness_haiku().clone();
    model.base_url = server.base_url();
    let context = Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserContent::Text("Hello".into()),
            timestamp: 0,
        })],
        tools: None,
    };
    let mut options = StreamOptions::default();
    options.request.api_key = Some("fake-key".into());
    options.request.max_retries = Some(1);
    options.request.fetch = Some(server.client());

    let stream = anthropic_messages::stream(&model, &context, Some(options));
    let events = tokio::time::timeout(std::time::Duration::from_secs(30), stream.collect())
        .await
        .expect("the retried stream settles within the bound")
        .expect("stream collected");

    let attempts = server.request_count();
    server.shutdown();

    assert_eq!(attempts, 2, "a 529 is retried once with maxRetries=1, exactly as senpi classifies it");
    let last = events.last().expect("terminal event");
    assert!(matches!(last, AssistantMessageEvent::Error { .. }), "the exhausted retry ends in an error event");
}

#[test]
fn the_529_failure_normalizes_to_senpis_retry_profile_class() {
    let error = RetryErrorShape {
        is_error: true,
        name: "Error".into(),
        class_name: Some("APIError".into()),
        message: "529 Overloaded".into(),
        status: Some(529),
        headers: None,
        error: serde_json::from_str(OVERLOADED_BODY).ok(),
    };
    let failure = normalize_anthropic_retry_failure(&error, None);
    assert_eq!(failure.origin, "anthropic-messages");
    assert_eq!(failure.kind, RetryFailureKind::HttpStatus);
    assert_eq!(failure.status_code, Some(529));
    assert_eq!(failure.provider_codes.as_deref(), Some(["overloaded_error".to_owned()].as_slice()));
}
