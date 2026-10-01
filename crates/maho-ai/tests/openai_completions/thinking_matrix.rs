use std::sync::{Arc, Mutex};

use maho_ai::api::openai_completions::{stream, stream_simple};
use maho_ai::types::{ModelThinkingLevel, SimpleStreamOptions, ThinkingLevel};
use serde_json::{json, Value};

use super::harness::*;

fn payload_capture_model(model: &maho_ai::types::Model) -> maho_ai::types::Model {
    let mut capture = model.clone();
    capture.base_url = "http://127.0.0.1:9".to_owned();
    capture
}

fn capture_sink() -> (Arc<Mutex<Option<Value>>>, maho_ai::types::OnPayload) {
    let sink: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let writer = sink.clone();
    let hook: maho_ai::types::OnPayload = Arc::new(move |payload: &Value, _model, _meta| {
        *writer.lock().expect("payload lock") = Some(payload.clone());
        Some(payload.clone())
    });
    (sink, hook)
}

async fn capture_payload(model: &maho_ai::types::Model, reasoning: Option<ThinkingLevel>) -> Value {
    let (sink, hook) = capture_sink();
    let mut options: SimpleStreamOptions = simple_options("fake-key");
    options.reasoning = reasoning;
    options.stream.request.on_payload = Some(hook);
    let _ = stream_simple(&payload_capture_model(model), &context(vec![user_message("Hello")], None), Some(options)).result().await;
    sink.lock().expect("payload lock").clone().expect("payload captured before request failure")
}

async fn capture_direct_payload(model: &maho_ai::types::Model, reasoning_effort: ModelThinkingLevel) -> Value {
    let (sink, hook) = capture_sink();
    let mut options = options("fake-key");
    options.reasoning_effort = thinking_level(reasoning_effort);
    options.stream.request.on_payload = Some(hook);
    let _ = stream(&payload_capture_model(model), &context(vec![user_message("Hello")], None), Some(options)).result().await;
    sink.lock().expect("payload lock").clone().expect("payload captured before request failure")
}

fn thinking_level(level: ModelThinkingLevel) -> Option<ModelThinkingLevel> {
    Some(level)
}

fn custom_model(id: &str, provider: &str, compat: Option<Value>, thinking_level_map: Option<Value>) -> maho_ai::types::Model {
    let mut overrides = vec![
        ("id", json!(id)),
        ("name", json!(id)),
        ("provider", json!(provider)),
        ("baseUrl", json!("http://localhost:8000/v1")),
        ("reasoning", json!(true)),
    ];
    if let Some(compat) = compat {
        overrides.push(("compat", compat));
    }
    if let Some(map) = thinking_level_map {
        overrides.push(("thinkingLevelMap", map));
    }
    model(&overrides)
}

fn has_field(payload: &Value, field: &str) -> bool {
    payload.get(field).is_some_and(|value| !value.is_null())
}

#[tokio::test]
async fn infers_the_gpt_6_astra_ladder_for_map_less_custom_models() {
    let astra = model(&[
        ("id", json!("gpt-6-astra")),
        ("name", json!("GPT-6 Astra")),
        ("provider", json!("quotio-openai")),
        ("baseUrl", json!("http://localhost:8000/v1")),
        ("reasoning", json!(true)),
        ("contextWindow", json!(1050000)),
        ("maxTokens", json!(128000)),
    ]);

    assert_eq!(
        capture_payload(&astra, Some(ThinkingLevel::Minimal)).await.get("reasoning_effort").and_then(Value::as_str),
        Some("low")
    );
    assert_eq!(
        capture_payload(&astra, None).await.get("reasoning_effort").and_then(Value::as_str),
        Some("low")
    );
    assert_eq!(
        capture_direct_payload(&astra, ModelThinkingLevel::Xhigh).await.get("reasoning_effort").and_then(Value::as_str),
        Some("xhigh")
    );
    assert_eq!(
        capture_direct_payload(&astra, ModelThinkingLevel::Max).await.get("reasoning_effort").and_then(Value::as_str),
        Some("max")
    );
}

#[tokio::test]
async fn uses_deep_seek_s_two_tier_ladder_on_alibaba_token_plan() {
    let deepseek = catalog_model("alibaba-token-plan", "deepseek-v3.2");

    let payload = capture_payload(&deepseek, Some(ThinkingLevel::Minimal)).await;

    assert_eq!(payload.get("thinking"), Some(&json!({ "type": "enabled" })));
    assert_eq!(payload.get("reasoning_effort").and_then(Value::as_str), Some("high"));
}

#[tokio::test]
async fn uses_deep_seek_s_max_tier_on_alibaba_token_plan() {
    let deepseek = catalog_model("alibaba-token-plan", "deepseek-v3.2");

    let payload = capture_payload(&deepseek, Some(ThinkingLevel::Xhigh)).await;

    assert_eq!(payload.get("thinking"), Some(&json!({ "type": "enabled" })));
    assert_eq!(payload.get("reasoning_effort").and_then(Value::as_str), Some("max"));
}

#[tokio::test]
async fn uses_openrouter_deep_seek_s_high_only_ladder() {
    let deepseek = catalog_model("openrouter", "deepseek/deepseek-r1");

    let payload = capture_payload(&deepseek, Some(ThinkingLevel::Minimal)).await;

    assert_eq!(payload.get("reasoning"), Some(&json!({ "effort": "high" })));
}

#[tokio::test]
async fn uses_openrouter_mi_mo_s_minimal_to_low_mapping() {
    let mimo = catalog_model("openrouter", "xiaomi/mimo-v2.5");

    let payload = capture_payload(&mimo, Some(ThinkingLevel::Minimal)).await;

    assert_eq!(payload.get("reasoning"), Some(&json!({ "effort": "low" })));
}

#[tokio::test]
async fn uses_openrouter_kimi_k3_s_minimal_to_low_mapping() {
    let kimi = catalog_model("openrouter", "moonshotai/kimi-k3");

    let payload = capture_payload(&kimi, Some(ThinkingLevel::Minimal)).await;

    assert_eq!(payload.get("reasoning"), Some(&json!({ "effort": "low" })));
}

#[tokio::test]
async fn uses_the_default_glm_5_2_max_tier() {
    let glm = catalog_model("alibaba-token-plan", "glm-5.2");

    let payload = capture_payload(&glm, Some(ThinkingLevel::Max)).await;

    assert_eq!(payload.get("reasoning_effort").and_then(Value::as_str), Some("max"));
}

#[tokio::test]
async fn does_not_send_a_null_mapped_effort_for_zai() {
    let model = custom_model(
        "explicit-null-zai",
        "local",
        Some(json!({ "thinkingFormat": "zai", "supportsReasoningEffort": true })),
        Some(json!({ "high": null })),
    );

    let payload = capture_direct_payload(&model, ModelThinkingLevel::High).await;

    assert_eq!(
        payload.get("thinking").and_then(Value::as_object).and_then(|thinking| thinking.get("type")).and_then(Value::as_str),
        Some("enabled")
    );
    assert!(!has_field(&payload, "reasoning_effort"));
}

#[tokio::test]
async fn does_not_send_a_null_mapped_effort_for_deepseek() {
    let model = custom_model(
        "explicit-null-deepseek",
        "local",
        Some(json!({ "thinkingFormat": "deepseek", "supportsReasoningEffort": true })),
        Some(json!({ "high": null })),
    );

    let payload = capture_direct_payload(&model, ModelThinkingLevel::High).await;

    assert_eq!(
        payload.get("thinking").and_then(Value::as_object).and_then(|thinking| thinking.get("type")).and_then(Value::as_str),
        Some("enabled")
    );
    assert!(!has_field(&payload, "reasoning_effort"));
}

#[tokio::test]
async fn does_not_send_a_null_mapped_effort_for_openrouter() {
    let model = custom_model(
        "explicit-null-openrouter",
        "local",
        Some(json!({ "thinkingFormat": "openrouter", "supportsReasoningEffort": true })),
        Some(json!({ "high": null })),
    );

    let payload = capture_direct_payload(&model, ModelThinkingLevel::High).await;

    assert!(!has_field(&payload, "reasoning"));
}

#[tokio::test]
async fn does_not_send_a_null_mapped_effort_for_together() {
    let model = custom_model(
        "explicit-null-together",
        "local",
        Some(json!({ "thinkingFormat": "together", "supportsReasoningEffort": true })),
        Some(json!({ "high": null })),
    );

    let payload = capture_direct_payload(&model, ModelThinkingLevel::High).await;

    assert_eq!(payload.get("reasoning"), Some(&json!({ "enabled": true })));
    assert!(!has_field(&payload, "reasoning_effort"));
}

#[tokio::test]
async fn does_not_send_a_null_mapped_effort_for_string_thinking() {
    let model = custom_model(
        "explicit-null-string-thinking",
        "local",
        Some(json!({ "thinkingFormat": "string-thinking", "supportsReasoningEffort": true })),
        Some(json!({ "high": null })),
    );

    let payload = capture_direct_payload(&model, ModelThinkingLevel::High).await;

    assert!(!has_field(&payload, "thinking"));
}

#[tokio::test]
async fn does_not_send_a_null_mapped_effort_for_openai() {
    let model = custom_model(
        "explicit-null-openai",
        "local",
        Some(json!({ "thinkingFormat": "openai", "supportsReasoningEffort": true })),
        Some(json!({ "high": null })),
    );

    let payload = capture_direct_payload(&model, ModelThinkingLevel::High).await;

    assert!(!has_field(&payload, "reasoning_effort"));
}

#[tokio::test]
async fn preserves_map_less_gpt_5_6_sol_s_existing_effort_behavior() {
    let sol = custom_model(
        "gpt-5.6-sol",
        "quotio-openai",
        Some(json!({ "thinkingFormat": "openai", "supportsReasoningEffort": true })),
        None,
    );

    assert_eq!(
        capture_direct_payload(&sol, ModelThinkingLevel::Minimal).await.get("reasoning_effort").and_then(Value::as_str),
        Some("minimal")
    );
    assert_eq!(
        capture_direct_payload(&sol, ModelThinkingLevel::Off).await.get("reasoning_effort").and_then(Value::as_str),
        Some("off")
    );
}

#[tokio::test]
async fn uses_ollama_s_none_off_sentinel_and_clamps_max_to_its_highest_supported_wire_tier() {
    let ollama = model(&[
        ("id", json!("qwen3")),
        ("name", json!("Qwen3")),
        ("provider", json!("ollama")),
        ("baseUrl", json!("http://localhost:11434/v1")),
        ("reasoning", json!(true)),
        ("contextWindow", json!(32768)),
        ("maxTokens", json!(8192)),
    ]);

    assert_eq!(
        capture_payload(&ollama, None).await.get("reasoning_effort").and_then(Value::as_str),
        Some("none")
    );
    assert_eq!(
        capture_payload(&ollama, Some(ThinkingLevel::Max)).await.get("reasoning_effort").and_then(Value::as_str),
        Some("high")
    );
}

#[tokio::test]
async fn does_not_send_openrouter_s_none_sentinel_for_mandatory_kimi_k3_thinking() {
    let kimi = catalog_model("openrouter", "moonshotai/kimi-k3");

    let payload = capture_payload(&kimi, None).await;

    assert_eq!(payload.get("reasoning"), None);
}
