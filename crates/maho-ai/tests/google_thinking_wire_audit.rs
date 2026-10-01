//! The Google halves of senpi packages/ai/test/mistral-google-thinking-matrix.test.ts.
//!
//! The TS file audits the adapter x thinking-level matrix for Mistral and Google. The Mistral
//! suites belong to todo 12-misc (its `mistral-conversations` port); this file ports the two Google
//! describe blocks and records the Mistral ones as excluded.

mod google_fixtures;

use google_fixtures::{builtin_model, context, user_text};
use maho_ai::api::google_generative_ai::stream_simple as stream_simple_google;
use maho_ai::api::google_vertex::stream_simple as stream_simple_vertex;
use maho_ai::types::{
    Model, ProviderRequestOptions, SimpleStreamOptions, StreamOptions, ThinkingLevel,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

fn test_context() -> maho_ai::types::Context {
    context(vec![user_text("Hello")])
}

fn options(reasoning: Option<ThinkingLevel>, captured: &Arc<Mutex<Option<Value>>>) -> SimpleStreamOptions {
    let captured = Arc::clone(captured);
    SimpleStreamOptions {
        stream: StreamOptions {
            request: ProviderRequestOptions {
                api_key: Some("fake-key".into()),
                on_payload: Some(Arc::new(move |payload: &Value, _model, _metadata| {
                    *captured.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(payload.clone());
                    Some(payload.clone())
                })),
                ..ProviderRequestOptions::default()
            },
            ..StreamOptions::default()
        },
        reasoning,
        ..SimpleStreamOptions::default()
    }
}

async fn capture_google_payload(model: &Model, reasoning: Option<ThinkingLevel>) -> Value {
    let captured = Arc::new(Mutex::new(None));
    let mut model = model.clone();
    model.base_url = "http://127.0.0.1:9".into();
    let stream = stream_simple_google(&model, &test_context(), Some(options(reasoning, &captured)));
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), stream.result())
        .await
        .expect("bounded wait")
        .expect("result");
    assert!(result.error_message.is_some(), "the request must fail after the payload was captured");
    captured
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .expect("Expected Google payload to be captured before request failure")
}

async fn capture_vertex_payload(model: &Model, reasoning: Option<ThinkingLevel>) -> Value {
    let captured = Arc::new(Mutex::new(None));
    let mut model = model.clone();
    model.base_url = "http://127.0.0.1:9".into();
    let stream = stream_simple_vertex(&model, &test_context(), Some(options(reasoning, &captured)));
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), stream.result())
        .await
        .expect("bounded wait")
        .expect("result");
    assert!(result.error_message.is_some(), "the request must fail after the payload was captured");
    captured
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .expect("Expected Google payload to be captured before request failure")
}

fn thinking_config(payload: &Value) -> Value {
    payload["config"]["thinkingConfig"].clone()
}

#[tokio::test]
async fn google_generative_ai_explicit_runtime_off_disables_thinking_via_thinking_budget_0_for_gemini_2_5() {
    let model = builtin_model("google", "gemini-2.5-flash");
    let payload = capture_google_payload(&model, None).await;
    assert_eq!(thinking_config(&payload), json!({ "thinkingBudget": 0 }));
}

#[tokio::test]
async fn google_generative_ai_explicit_runtime_off_floors_gemini_3_flash_at_minimal_without_include_thoughts() {
    let model = builtin_model("google", "gemini-3-flash-preview");
    let payload = capture_google_payload(&model, None).await;
    assert_eq!(thinking_config(&payload), json!({ "thinkingLevel": "MINIMAL" }));
}

#[tokio::test]
async fn google_generative_ai_explicit_runtime_off_floors_gemini_3_1_pro_at_low_without_include_thoughts() {
    let model = builtin_model("google", "gemini-3.1-pro-preview");
    let payload = capture_google_payload(&model, None).await;
    assert_eq!(thinking_config(&payload), json!({ "thinkingLevel": "LOW" }));
}

#[tokio::test]
async fn google_generative_ai_maps_medium_to_thinking_level_medium_for_gemini_3_flash() {
    let model = builtin_model("google", "gemini-3-flash-preview");
    let payload = capture_google_payload(&model, Some(ThinkingLevel::Medium)).await;
    assert_eq!(thinking_config(&payload), json!({ "includeThoughts": true, "thinkingLevel": "MEDIUM" }));
}

#[tokio::test]
async fn google_generative_ai_maps_low_to_thinking_budget_2048_for_gemini_2_5_flash() {
    let model = builtin_model("google", "gemini-2.5-flash");
    let payload = capture_google_payload(&model, Some(ThinkingLevel::Low)).await;
    assert_eq!(thinking_config(&payload), json!({ "includeThoughts": true, "thinkingBudget": 2048 }));
}

#[tokio::test]
async fn google_generative_ai_clamps_unsupported_xhigh_down_to_the_high_budget_for_gemini_2_5_flash() {
    let model = builtin_model("google", "gemini-2.5-flash");
    let payload = capture_google_payload(&model, Some(ThinkingLevel::Xhigh)).await;
    assert_eq!(thinking_config(&payload), json!({ "includeThoughts": true, "thinkingBudget": 24576 }));
}

#[tokio::test]
async fn google_vertex_explicit_runtime_off_disables_thinking_via_thinking_budget_0_for_gemini_2_5() {
    let model = builtin_model("google-vertex", "gemini-2.5-flash");
    let payload = capture_vertex_payload(&model, None).await;
    assert_eq!(thinking_config(&payload), json!({ "thinkingBudget": 0 }));
}

#[tokio::test]
async fn google_vertex_explicit_runtime_off_floors_gemini_3_flash_at_minimal_without_include_thoughts() {
    let model = builtin_model("google-vertex", "gemini-3-flash-preview");
    let payload = capture_vertex_payload(&model, None).await;
    assert_eq!(thinking_config(&payload), json!({ "thinkingLevel": "MINIMAL" }));
}

#[tokio::test]
async fn google_vertex_explicit_runtime_off_floors_gemini_3_1_pro_at_low_without_include_thoughts() {
    let model = builtin_model("google-vertex", "gemini-3.1-pro-preview");
    let payload = capture_vertex_payload(&model, None).await;
    assert_eq!(thinking_config(&payload), json!({ "thinkingLevel": "LOW" }));
}

#[tokio::test]
async fn google_vertex_maps_high_to_thinking_level_high_for_gemini_3_1_pro() {
    let model = builtin_model("google-vertex", "gemini-3.1-pro-preview");
    let payload = capture_vertex_payload(&model, Some(ThinkingLevel::High)).await;
    assert_eq!(thinking_config(&payload), json!({ "includeThoughts": true, "thinkingLevel": "HIGH" }));
}
