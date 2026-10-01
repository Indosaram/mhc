//! Port of senpi packages/ai/test/google-thinking-level-map.test.ts.

mod google_fixtures;

use google_fixtures::model_with;
use maho_ai::api::google_generative_ai::stream_simple as stream_simple_google;
use maho_ai::api::google_vertex::stream_simple as stream_simple_vertex;
use maho_ai::types::{Context, Model, ModelThinkingLevel, ProviderRequestOptions, SimpleStreamOptions, StreamOptions, ThinkingBudgets};
use maho_ai::types::UserContent;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

fn context() -> Context {
    Context {
        system_prompt: None,
        messages: vec![maho_ai::types::Message::User(maho_ai::types::UserMessage {
            content: UserContent::Text("Hello".into()),
            timestamp: 0,
        })],
        tools: None,
    }
}

fn thinking_model(api: &str, provider: &str, id: &str, map: &[(ModelThinkingLevel, Option<&str>)]) -> Model {
    let mut model = model_with(api, provider, id);
    model.reasoning = true;
    model.thinking_level_map = Some(
        map.iter().map(|(level, value)| (*level, value.map(str::to_owned))).collect(),
    );
    model
}

fn options(reasoning: ModelThinkingLevel, budgets: Option<ThinkingBudgets>, captured: &Arc<Mutex<Option<Value>>>) -> SimpleStreamOptions {
    let captured = Arc::clone(captured);
    SimpleStreamOptions {
        stream: StreamOptions {
            request: ProviderRequestOptions {
                api_key: Some("test".into()),
                on_payload: Some(Arc::new(move |payload: &Value, _model, _metadata| {
                    *captured.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(payload.clone());
                    Some(payload.clone())
                })),
                ..ProviderRequestOptions::default()
            },
            ..StreamOptions::default()
        },
        reasoning: reasoning_level(reasoning),
        thinking_budgets: budgets,
        ..SimpleStreamOptions::default()
    }
}

fn reasoning_level(level: ModelThinkingLevel) -> Option<maho_ai::types::ThinkingLevel> {
    match level {
        ModelThinkingLevel::Off => None,
        ModelThinkingLevel::Minimal => Some(maho_ai::types::ThinkingLevel::Minimal),
        ModelThinkingLevel::Low => Some(maho_ai::types::ThinkingLevel::Low),
        ModelThinkingLevel::Medium => Some(maho_ai::types::ThinkingLevel::Medium),
        ModelThinkingLevel::High => Some(maho_ai::types::ThinkingLevel::High),
        ModelThinkingLevel::Xhigh => Some(maho_ai::types::ThinkingLevel::Xhigh),
        ModelThinkingLevel::Max => Some(maho_ai::types::ThinkingLevel::Max),
    }
}

async fn capture_google_payload(
    model: &Model,
    reasoning: ModelThinkingLevel,
    budgets: Option<ThinkingBudgets>,
) -> Value {
    let captured = Arc::new(Mutex::new(None));
    let mut model = model.clone();
    model.base_url = "http://127.0.0.1:9".into();
    let stream = stream_simple_google(&model, &context(), Some(options(reasoning, budgets, &captured)));
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), stream.result())
        .await
        .expect("bounded wait")
        .expect("result");
    assert!(result.error_message.is_some(), "the request must fail after the payload was captured");
    let payload = captured.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
    payload.expect("Google payload was not captured")
}

async fn capture_vertex_payload(
    model: &Model,
    reasoning: ModelThinkingLevel,
    budgets: Option<ThinkingBudgets>,
) -> Value {
    let captured = Arc::new(Mutex::new(None));
    let mut model = model.clone();
    model.base_url = "http://127.0.0.1:9".into();
    let stream = stream_simple_vertex(&model, &context(), Some(options(reasoning, budgets, &captured)));
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), stream.result())
        .await
        .expect("bounded wait")
        .expect("result");
    assert!(result.error_message.is_some(), "the request must fail after the payload was captured");
    let payload = captured.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
    payload.expect("Vertex payload was not captured")
}

#[test]
fn exhaustively_resolves_supported_logical_levels_and_mapping_values() {
    let model = thinking_model("google-generative-ai", "test-google", "gemini-3.7-flash", &[]);
    for (level, expected) in [
        (ModelThinkingLevel::Off, "high"),
        (ModelThinkingLevel::Minimal, "minimal"),
        (ModelThinkingLevel::Low, "low"),
        (ModelThinkingLevel::Medium, "medium"),
        (ModelThinkingLevel::High, "high"),
    ] {
        assert_eq!(
            maho_ai::api::google_shared::resolve_google_thinking_level(&model, level)
                .expect("resolved level")
                .as_str(),
            expected
        );
    }

    for (mapped, expected) in [
        ("minimal", "minimal"),
        ("low", "low"),
        ("medium", "medium"),
        ("high", "high"),
        ("MINIMAL", "minimal"),
        ("LOW", "low"),
        ("MEDIUM", "medium"),
        ("HIGH", "high"),
    ] {
        let mapped_model = thinking_model(
            "google-generative-ai",
            "test-google",
            "gemini-3.7-flash",
            &[
                (ModelThinkingLevel::High, Some(mapped)),
                (ModelThinkingLevel::Xhigh, Some(mapped)),
                (ModelThinkingLevel::Max, Some(mapped)),
            ],
        );
        for level in [ModelThinkingLevel::High, ModelThinkingLevel::Xhigh, ModelThinkingLevel::Max] {
            assert_eq!(
                maho_ai::api::google_shared::resolve_google_thinking_level(&mapped_model, level)
                    .expect("resolved level")
                    .as_str(),
                expected
            );
        }
    }

    let invalid = thinking_model(
        "google-generative-ai",
        "test-google",
        "gemini-3.7-flash",
        &[(ModelThinkingLevel::Xhigh, Some("extreme"))],
    );
    assert_eq!(
        maho_ai::api::google_shared::resolve_google_thinking_level(&invalid, ModelThinkingLevel::Xhigh)
            .expect_err("invalid mapping"),
        "Unsupported Google thinking level mapping for test-google/gemini-3.7-flash: xhigh -> extreme"
    );
    assert_eq!(
        maho_ai::api::google_shared::resolve_google_thinking_level(&model, ModelThinkingLevel::Max)
            .expect_err("missing mapping"),
        "Unsupported Google thinking level mapping for test-google/gemini-3.7-flash: max -> undefined"
    );
}

#[tokio::test]
async fn maps_google_generative_ai_xhigh_to_a_supported_level() {
    let model = thinking_model(
        "google-generative-ai",
        "test-google",
        "gemini-3.7-flash",
        &[(ModelThinkingLevel::Xhigh, Some("high")), (ModelThinkingLevel::Max, Some("high"))],
    );
    let payload = capture_google_payload(&model, ModelThinkingLevel::Xhigh, None).await;
    assert_eq!(payload["config"]["thinkingConfig"], json!({ "includeThoughts": true, "thinkingLevel": "HIGH" }));
}

#[tokio::test]
async fn maps_google_generative_ai_max_to_a_supported_level() {
    let model = thinking_model(
        "google-generative-ai",
        "test-google",
        "gemini-3.7-flash",
        &[(ModelThinkingLevel::Xhigh, Some("high")), (ModelThinkingLevel::Max, Some("high"))],
    );
    let payload = capture_google_payload(&model, ModelThinkingLevel::Max, None).await;
    assert_eq!(payload["config"]["thinkingConfig"], json!({ "includeThoughts": true, "thinkingLevel": "HIGH" }));
}

#[tokio::test]
async fn honors_uppercase_provider_values_for_standard_google_generative_ai_levels() {
    let model = thinking_model(
        "google-generative-ai",
        "test-google",
        "gemini-3.7-flash",
        &[(ModelThinkingLevel::High, Some("LOW"))],
    );
    let payload = capture_google_payload(&model, ModelThinkingLevel::High, None).await;
    assert_eq!(payload["config"]["thinkingConfig"]["thinkingLevel"], json!("LOW"));
}

#[tokio::test]
async fn uses_mapped_google_generative_ai_levels_for_token_budgets() {
    let model = thinking_model(
        "google-generative-ai",
        "test-google",
        "gemini-2.5-flash",
        &[(ModelThinkingLevel::Xhigh, Some("high"))],
    );
    let payload = capture_google_payload(
        &model,
        ModelThinkingLevel::Xhigh,
        Some(ThinkingBudgets { high: Some(1234), ..ThinkingBudgets::default() }),
    )
    .await;
    assert_eq!(payload["config"]["thinkingConfig"], json!({ "includeThoughts": true, "thinkingBudget": 1234 }));
}

#[tokio::test]
async fn maps_google_vertex_extended_levels() {
    let model = thinking_model(
        "google-vertex",
        "test-vertex",
        "gemini-3.7-flash",
        &[(ModelThinkingLevel::Xhigh, Some("high"))],
    );
    let payload = capture_vertex_payload(&model, ModelThinkingLevel::Xhigh, None).await;
    assert_eq!(payload["config"]["thinkingConfig"], json!({ "includeThoughts": true, "thinkingLevel": "HIGH" }));
}

#[tokio::test]
async fn uses_mapped_google_vertex_levels_for_token_budgets() {
    let model = thinking_model(
        "google-vertex",
        "test-vertex",
        "gemini-2.5-flash",
        &[(ModelThinkingLevel::Max, Some("high"))],
    );
    let payload = capture_vertex_payload(
        &model,
        ModelThinkingLevel::Max,
        Some(ThinkingBudgets { high: Some(4321), ..ThinkingBudgets::default() }),
    )
    .await;
    assert_eq!(payload["config"]["thinkingConfig"], json!({ "includeThoughts": true, "thinkingBudget": 4321 }));
}
