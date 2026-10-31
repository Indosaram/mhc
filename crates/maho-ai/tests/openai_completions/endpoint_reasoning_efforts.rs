//! The "models configured from endpoint reasoning efforts" cases of senpi
//! packages/ai/test/endpoint-reasoning-efforts.test.ts: a `thinkingLevelMap` parsed from an
//! endpoint's `reasoning_efforts` ladder is authoritative for the supported levels and for the
//! `reasoning_effort` value sent on the wire.

use std::sync::{Arc, Mutex};

use maho_ai::api::openai_completions::stream_simple;
use maho_ai::endpoint_reasoning_efforts::parse_endpoint_reasoning_efforts;
use maho_ai::models::{clamp_thinking_level, get_supported_thinking_levels};
use maho_ai::types::{Model, ModelThinkingLevel, ThinkingLevel, ThinkingLevelMap};
use serde_json::{Value, json};

use super::harness::*;

fn low_high() -> Option<ThinkingLevelMap> {
    parse_endpoint_reasoning_efforts(
        &json!([{ "value": "low" }, { "value": "High", "default": true }]),
    )
    .and_then(|parsed| parsed.thinking_level_map)
}

fn none_low_high() -> Option<ThinkingLevelMap> {
    parse_endpoint_reasoning_efforts(&json!(["none", "low", "high"]))
        .and_then(|parsed| parsed.thinking_level_map)
}

fn endpoint_model(id: &str, thinking_level_map: Option<ThinkingLevelMap>) -> Model {
    let mut endpoint = model(&[
        ("id", json!(id)),
        ("name", json!(id)),
        ("provider", json!("local-endpoint")),
        ("baseUrl", json!("http://127.0.0.1:9")),
        ("reasoning", json!(true)),
        ("maxTokens", json!(16384)),
        ("compat", json!({ "supportsReasoningEffort": true })),
    ]);
    endpoint.thinking_level_map = thinking_level_map;
    endpoint
}

/// `sentReasoningEffort`: the `reasoning_effort` field of the built payload (`None` when absent).
async fn sent_reasoning_effort(model: &Model, reasoning: Option<ThinkingLevel>) -> Option<Value> {
    let sink: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let writer = Arc::clone(&sink);
    let mut options = simple_options("fake-key");
    options.reasoning = reasoning;
    options.stream.request.on_payload = Some(Arc::new(move |payload: &Value, _model, _meta| {
        *writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(payload.clone());
        Some(payload.clone())
    }));
    let stream = stream_simple(
        model,
        &context(vec![user_message("Hello")], None),
        Some(options),
    );
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), stream.result())
        .await
        .expect("bounded wait");
    let payload = sink
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .expect("payload was never built");
    payload.get("reasoning_effort").cloned()
}

#[test]
fn offers_only_the_advertised_levels_skipping_id_based_inference() {
    assert_eq!(
        get_supported_thinking_levels(&endpoint_model("gpt-5.5", low_high())),
        vec![ModelThinkingLevel::Low, ModelThinkingLevel::High]
    );
    assert_eq!(
        clamp_thinking_level(
            &endpoint_model("effort-model", low_high()),
            ModelThinkingLevel::Medium
        ),
        ModelThinkingLevel::High
    );
}

#[tokio::test]
async fn sends_the_endpoints_original_effort_name_on_the_wire() {
    assert_eq!(
        sent_reasoning_effort(
            &endpoint_model("effort-model", low_high()),
            Some(ThinkingLevel::High)
        )
        .await,
        Some(json!("High"))
    );
}

// review B2: an advertised off sentinel is what "reasoning off" sends, for every model id.
#[tokio::test]
async fn sends_the_advertised_off_value_when_reasoning_is_off() {
    for id in ["effort-model", "gpt-6-astra"] {
        assert_eq!(
            sent_reasoning_effort(&endpoint_model(id, none_low_high()), None).await,
            Some(json!("none")),
            "{id}"
        );
    }
}

#[tokio::test]
async fn keeps_the_map_less_gpt_6_astra_off_to_low_normalization() {
    assert_eq!(
        sent_reasoning_effort(&endpoint_model("gpt-6-astra", None), None).await,
        Some(json!("low"))
    );
}
