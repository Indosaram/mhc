//! The thinking-level payload cases of senpi packages/ai/test/google-thinking-level-map.test.ts
//! that pin the shared `usesGoogleThinkingLevel` / `getDisabledGoogleThinkingConfig` behaviour:
//! Gemini 3.x and generation 4+ take a discrete level (never a budget), and thinking-off clamps to
//! the lowest level the model supports. Each case runs on both Google adapters (`it.each`).

use crate::google_fixtures;

use google_fixtures::{context, model_with, user_text};
use maho_ai::api::google_generative_ai::stream_simple as stream_simple_google;
use maho_ai::api::google_vertex::stream_simple as stream_simple_vertex;
use maho_ai::types::{
    Model, ModelThinkingLevel, ProviderRequestOptions, SimpleStreamOptions, StreamOptions,
    ThinkingLevel,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

const ADAPTERS: [(&str, &str); 2] = [
    ("google-generative-ai", "test-google"),
    ("google-vertex", "test-vertex"),
];

fn options(
    reasoning: Option<ThinkingLevel>,
    captured: &Arc<Mutex<Option<Value>>>,
) -> SimpleStreamOptions {
    let captured = Arc::clone(captured);
    SimpleStreamOptions {
        stream: StreamOptions {
            request: ProviderRequestOptions {
                api_key: Some("test".into()),
                on_payload: Some(Arc::new(move |payload: &Value, _model, _metadata| {
                    *captured
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(payload.clone());
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

/// `capture(...)`: build the payload, then fail the request against an unroutable base URL.
async fn thinking_config(model: &Model, reasoning: Option<ThinkingLevel>) -> Value {
    let captured = Arc::new(Mutex::new(None));
    let mut model = model.clone();
    model.base_url = "http://127.0.0.1:9".into();
    let context = context(vec![user_text("Hello")]);
    let options = Some(options(reasoning, &captured));
    let stream = if model.api == "google-vertex" {
        stream_simple_vertex(&model, &context, options)
    } else {
        stream_simple_google(&model, &context, options)
    };
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), stream.result())
        .await
        .expect("bounded wait")
        .expect("result");
    assert!(
        result.error_message.is_some(),
        "the request must fail after the payload was captured"
    );
    let payload = captured
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .unwrap_or_else(|| panic!("{} payload was not captured", model.api));
    payload["config"]["thinkingConfig"].clone()
}

/// `{ off: null, minimal: null, low, medium, high, xhigh: null, max: null }`, the low-to-high
/// ladder the Gemini 3.1 Pro, 3.8 Flash and gemini-4-argon cases share.
fn low_to_high_model(api: &str, provider: &str, id: &str) -> Model {
    let mut model = model_with(api, provider, id);
    model.reasoning = true;
    model.thinking_level_map = Some(
        [
            (ModelThinkingLevel::Off, None),
            (ModelThinkingLevel::Minimal, None),
            (ModelThinkingLevel::Low, Some("low".to_owned())),
            (ModelThinkingLevel::Medium, Some("medium".to_owned())),
            (ModelThinkingLevel::High, Some("high".to_owned())),
            (ModelThinkingLevel::Xhigh, None),
            (ModelThinkingLevel::Max, None),
        ]
        .into_iter()
        .collect(),
    );
    model
}

/// Custom models.json and discovery rows usually carry no thinkingLevelMap.
fn map_less_model(api: &str, provider: &str, id: &str) -> Model {
    let mut model = model_with(api, provider, id);
    model.reasoning = true;
    model.thinking_level_map = None;
    model
}

// Regression test for https://github.com/earendil-works/pi/issues/9455
#[tokio::test]
async fn uses_the_lowest_supported_level_when_reasoning_is_omitted() {
    for (api, provider) in ADAPTERS {
        let model = low_to_high_model(api, provider, "gemini-3.8-flash");
        assert_eq!(
            thinking_config(&model, None).await,
            json!({ "thinkingLevel": "LOW" }),
            "{api}"
        );
    }
}

#[tokio::test]
async fn preserves_native_medium_effort_for_gemini_3_1_pro() {
    for (api, provider) in ADAPTERS {
        let model = low_to_high_model(api, provider, "gemini-3.1-pro-preview");
        assert_eq!(
            thinking_config(&model, Some(ThinkingLevel::Medium)).await,
            json!({ "includeThoughts": true, "thinkingLevel": "MEDIUM" }),
            "{api}"
        );
    }
}

// gemini-4-argon rejects thinking_budget with HTTP 400; it only accepts LOW/MEDIUM/HIGH levels (senpi#3292).
#[tokio::test]
async fn sends_a_thinking_level_never_a_budget_for_gemini_4_argon() {
    for (api, provider) in ADAPTERS {
        let model = low_to_high_model(api, provider, "gemini-4-argon");
        assert_eq!(
            thinking_config(&model, Some(ThinkingLevel::High)).await,
            json!({ "includeThoughts": true, "thinkingLevel": "HIGH" }),
            "{api}"
        );
    }
}

#[tokio::test]
async fn clamps_disabled_thinking_to_low_for_gemini_4_argon() {
    for (api, provider) in ADAPTERS {
        let model = low_to_high_model(api, provider, "gemini-4-argon");
        assert_eq!(
            thinking_config(&model, None).await,
            json!({ "thinkingLevel": "LOW" }),
            "{api}"
        );
    }
}

#[tokio::test]
async fn raises_minimal_to_low_for_a_map_less_gemini_4_argon() {
    for (api, provider) in ADAPTERS {
        let model = map_less_model(api, provider, "gemini-4-argon");
        assert_eq!(
            thinking_config(&model, Some(ThinkingLevel::Minimal)).await,
            json!({ "includeThoughts": true, "thinkingLevel": "LOW" }),
            "{api}"
        );
    }
}

#[tokio::test]
async fn clamps_disabled_thinking_to_low_not_a_zero_budget_for_a_map_less_gemini_4_argon() {
    for (api, provider) in ADAPTERS {
        let model = map_less_model(api, provider, "gemini-4-argon");
        assert_eq!(
            thinking_config(&model, None).await,
            json!({ "thinkingLevel": "LOW" }),
            "{api}"
        );
    }
}
