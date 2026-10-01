use maho_ai::types::{CacheRetention, ProviderHeaders, StopReason};
use serde_json::{json, Value};

use super::harness::*;

fn create_model(overrides: &[(&str, Value)]) -> maho_ai::types::Model {
    let mut model = catalog_model("openai", "gpt-4o-mini");
    model.compat = None;
    let mut value = serde_json::to_value(&model).expect("model json");
    for (key, override_value) in overrides {
        value[key] = override_value.clone();
    }
    serde_json::from_value(value).expect("model")
}

struct Capture {
    payload: Value,
    headers: std::collections::BTreeMap<String, String>,
    message: maho_ai::types::AssistantMessage,
}

async fn capture_request(
    cache_retention: Option<CacheRetention>,
    session_id: Option<&str>,
    headers: Option<ProviderHeaders>,
    model: &maho_ai::types::Model,
    usage: Option<Value>,
) -> Capture {
    let usage_chunk = json!({
        "choices": [{ "delta": {}, "finish_reason": "stop" }],
        "usage": usage.unwrap_or_else(|| json!({ "prompt_tokens": 1, "completion_tokens": 1 })),
    });
    let transport = ScriptedTransport::success([usage_chunk]);
    let mut context = context(vec![user_message("hi")], None);
    context.system_prompt = Some("sys".to_owned());
    let mut options = options("test-key");
    options.stream.cache_retention = cache_retention;
    options.stream.session_id = session_id.map(str::to_owned);
    options.stream.request.headers = headers;
    let message = finish(&run(model, &context, options, transport.clone())).await;
    let request = transport.last_request();
    Capture { payload: request.body.clone(), headers: request.headers.clone(), message }
}

fn default_model() -> maho_ai::types::Model {
    create_model(&[])
}

#[tokio::test]
async fn parses_flat_cached_tokens_from_kimi_usage_as_cache_read_tokens() {
    let capture = capture_request(
        None,
        None,
        None,
        &default_model(),
        Some(json!({
            "prompt_tokens": 1000,
            "completion_tokens": 10,
            "total_tokens": 1010,
            "cached_tokens": 400,
        })),
    )
    .await;

    assert_eq!(capture.message.usage.cache_read, 400);
    assert_eq!(capture.message.usage.input, 600);
    assert_eq!(capture.message.stop_reason, StopReason::Stop);
}

#[tokio::test]
async fn sets_a_clamped_prompt_cache_key_for_moonshot_requests_with_short_retention() {
    let session_id = "moonshot-session-".repeat(5);
    let model = create_model(&[
        ("provider", json!("moonshotai")),
        ("baseUrl", json!("https://api.moonshot.ai/v1")),
    ]);

    let capture = capture_request(
        Some(CacheRetention::Short),
        Some(&session_id),
        None,
        &model,
        None,
    )
    .await;

    assert_eq!(capture.payload.get("prompt_cache_key").and_then(Value::as_str), Some(&session_id[..64]));
}

#[tokio::test]
async fn does_not_set_prompt_cache_key_for_unknown_openai_compatible_providers() {
    let model = create_model(&[
        ("provider", json!("custom")),
        ("baseUrl", json!("https://proxy.example.com/v1")),
    ]);

    let capture =
        capture_request(Some(CacheRetention::Short), Some("custom-session"), None, &model, None).await;

    assert_eq!(capture.payload.get("prompt_cache_key"), None);
}

#[tokio::test]
async fn sets_prompt_cache_key_for_direct_openai_requests_when_caching_is_enabled() {
    let capture = capture_request(None, Some("session-123"), None, &default_model(), None).await;

    assert_eq!(capture.payload.get("prompt_cache_key").and_then(Value::as_str), Some("session-123"));
    assert_eq!(capture.payload.get("prompt_cache_retention"), None);
}

#[tokio::test]
async fn sets_prompt_cache_retention_to_24h_for_direct_openai_requests_when_cache_retention_is_long() {
    let capture =
        capture_request(Some(CacheRetention::Long), Some("session-456"), None, &default_model(), None).await;

    assert_eq!(capture.payload.get("prompt_cache_key").and_then(Value::as_str), Some("session-456"));
    assert_eq!(capture.payload.get("prompt_cache_retention").and_then(Value::as_str), Some("24h"));
}

#[tokio::test]
async fn clamps_prompt_cache_key_to_openai_64_character_limit() {
    let session_id = "x".repeat(67);
    let capture = capture_request(None, Some(&session_id), None, &default_model(), None).await;

    assert_eq!(capture.payload.get("prompt_cache_key").and_then(Value::as_str), Some(&"x".repeat(64)[..]));
}

#[tokio::test]
async fn omits_prompt_cache_fields_when_cache_retention_is_none() {
    let capture =
        capture_request(Some(CacheRetention::None), Some("session-789"), None, &default_model(), None).await;

    assert_eq!(capture.payload.get("prompt_cache_key"), None);
    assert_eq!(capture.payload.get("prompt_cache_retention"), None);
}

#[tokio::test]
async fn omits_prompt_cache_fields_for_non_openai_base_urls_without_compatible_long_retention() {
    let model = create_model(&[
        ("baseUrl", json!("https://proxy.example.com/v1")),
        ("compat", json!({ "supportsLongCacheRetention": false })),
    ]);

    let capture =
        capture_request(Some(CacheRetention::Long), Some("session-proxy"), None, &model, None).await;

    assert_eq!(capture.payload.get("prompt_cache_key"), None);
    assert_eq!(capture.payload.get("prompt_cache_retention"), None);
}

#[tokio::test]
async fn uses_pi_cache_retention_for_direct_openai_requests() {
    let transport = ScriptedTransport::success([json!({
        "choices": [{ "delta": {}, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": 1, "completion_tokens": 1 },
    })]);
    let mut context = context(vec![user_message("hi")], None);
    context.system_prompt = Some("sys".to_owned());
    let mut options = options("test-key");
    options.stream.session_id = Some("session-env".to_owned());
    let mut env = maho_ai::types::ProviderEnv::new();
    env.insert("PI_CACHE_RETENTION".to_owned(), "long".to_owned());
    options.stream.request.env = Some(env);
    let _ = finish(&run(&default_model(), &context, options, transport.clone())).await;
    let payload = transport.last_request().body;

    assert_eq!(payload.get("prompt_cache_key").and_then(Value::as_str), Some("session-env"));
    assert_eq!(payload.get("prompt_cache_retention").and_then(Value::as_str), Some("24h"));
}

#[tokio::test]
async fn sends_known_session_affinity_headers_when_compat_enables_them() {
    let model = create_model(&[
        ("baseUrl", json!("https://proxy.example.com/v1")),
        ("compat", json!({ "sendSessionAffinityHeaders": true })),
    ]);

    let capture =
        capture_request(None, Some("session-affinity"), None, &model, None).await;

    assert_eq!(capture.headers.get("session_id").map(String::as_str), Some("session-affinity"));
    assert_eq!(capture.headers.get("x-client-request-id").map(String::as_str), Some("session-affinity"));
    assert_eq!(capture.headers.get("x-session-affinity").map(String::as_str), Some("session-affinity"));
}

#[tokio::test]
async fn sends_fireworks_session_affinity_for_glm_5p2() {
    let model = catalog_model("fireworks", "accounts/fireworks/models/glm-5p2");

    let capture = capture_request(None, Some("fireworks-session"), None, &model, None).await;

    assert_eq!(capture.headers.get("x-session-affinity").map(String::as_str), Some("fireworks-session"));
}

#[tokio::test]
async fn sends_fireworks_session_affinity_for_glm_5p2_fast() {
    let model = catalog_model("fireworks", "accounts/fireworks/routers/glm-5p2-fast");

    let capture = capture_request(None, Some("fireworks-session"), None, &model, None).await;

    assert_eq!(capture.headers.get("x-session-affinity").map(String::as_str), Some("fireworks-session"));
}

#[tokio::test]
async fn uses_openai_no_session_format_when_configured() {
    let model = create_model(&[(
        "compat",
        json!({ "sendSessionAffinityHeaders": true, "sessionAffinityFormat": "openai-nosession" }),
    )]);

    let capture =
        capture_request(None, Some("session-nosession"), None, &model, None).await;

    assert_eq!(capture.payload.get("session_id"), None);
    assert_eq!(capture.payload.get("prompt_cache_key").and_then(Value::as_str), Some("session-nosession"));
    assert_eq!(capture.headers.get("session_id"), None);
    assert_eq!(capture.headers.get("x-client-request-id").map(String::as_str), Some("session-nosession"));
    assert_eq!(capture.headers.get("x-session-affinity").map(String::as_str), Some("session-nosession"));
    assert_eq!(capture.headers.get("x-session-id"), None);
}

#[tokio::test]
async fn uses_openrouter_session_affinity_header_and_body_field_when_configured() {
    let model = create_model(&[
        ("baseUrl", json!("https://proxy.example.com/v1")),
        ("compat", json!({ "sendSessionAffinityHeaders": true, "sessionAffinityFormat": "openrouter" })),
    ]);

    let capture =
        capture_request(None, Some("session-proxy"), None, &model, None).await;

    assert_eq!(capture.payload.get("session_id").and_then(Value::as_str), Some("session-proxy"));
    assert_eq!(capture.payload.get("prompt_cache_key"), None);
    assert_eq!(capture.headers.get("x-session-id").map(String::as_str), Some("session-proxy"));
    assert_eq!(capture.headers.get("session_id"), None);
    assert_eq!(capture.headers.get("x-client-request-id"), None);
    assert_eq!(capture.headers.get("x-session-affinity"), None);
}

#[tokio::test]
async fn auto_detects_openrouter_session_affinity_header_and_body_field_for_openrouter_endpoints() {
    let model = create_model(&[
        ("provider", json!("openrouter")),
        ("baseUrl", json!("https://openrouter.ai/api/v1")),
    ]);

    let capture =
        capture_request(None, Some("session-openrouter"), None, &model, None).await;

    assert_eq!(capture.payload.get("session_id").and_then(Value::as_str), Some("session-openrouter"));
    assert_eq!(capture.payload.get("prompt_cache_key"), None);
    assert_eq!(capture.headers.get("x-session-id").map(String::as_str), Some("session-openrouter"));
    assert_eq!(capture.headers.get("session_id"), None);
    assert_eq!(capture.headers.get("x-client-request-id"), None);
    assert_eq!(capture.headers.get("x-session-affinity"), None);
}

#[tokio::test]
async fn sends_openrouter_session_affinity_header_by_default_for_built_in_openrouter_models() {
    let model = catalog_model("openrouter", "auto");

    let capture =
        capture_request(None, Some("session-openrouter"), None, &model, None).await;

    assert_eq!(capture.payload.get("session_id").and_then(Value::as_str), Some("session-openrouter"));
    assert_eq!(capture.payload.get("prompt_cache_key"), None);
    assert_eq!(capture.headers.get("x-session-id").map(String::as_str), Some("session-openrouter"));
    assert_eq!(capture.headers.get("session_id"), None);
    assert_eq!(capture.headers.get("x-client-request-id"), None);
    assert_eq!(capture.headers.get("x-session-affinity"), None);
}

#[tokio::test]
async fn omits_openrouter_session_affinity_data_when_explicitly_disabled() {
    let model = create_model(&[
        ("provider", json!("openrouter")),
        ("baseUrl", json!("https://openrouter.ai/api/v1")),
        ("compat", json!({ "sendSessionAffinityHeaders": false })),
    ]);

    let capture =
        capture_request(None, Some("session-openrouter"), None, &model, None).await;

    assert_eq!(capture.payload.get("session_id"), None);
    assert_eq!(capture.payload.get("prompt_cache_key"), None);
    assert_eq!(capture.headers.get("x-session-id"), None);
}

#[tokio::test]
async fn omits_session_affinity_headers_when_cache_retention_is_none() {
    let model = create_model(&[
        ("baseUrl", json!("https://proxy.example.com/v1")),
        ("compat", json!({ "sendSessionAffinityHeaders": true })),
    ]);

    let capture =
        capture_request(Some(CacheRetention::None), Some("session-affinity"), None, &model, None).await;

    assert_eq!(capture.headers.get("session_id"), None);
    assert_eq!(capture.headers.get("x-client-request-id"), None);
    assert_eq!(capture.headers.get("x-session-affinity"), None);
}

#[tokio::test]
async fn lets_explicit_headers_override_generated_session_affinity_headers() {
    let model = create_model(&[
        ("baseUrl", json!("https://proxy.example.com/v1")),
        ("compat", json!({ "sendSessionAffinityHeaders": true })),
    ]);
    let mut headers = ProviderHeaders::new();
    headers.insert("session_id".to_owned(), Some("override-session".to_owned()));
    headers.insert("x-client-request-id".to_owned(), Some("override-request".to_owned()));
    headers.insert("x-session-affinity".to_owned(), Some("override-affinity".to_owned()));

    let capture =
        capture_request(None, Some("session-affinity"), Some(headers), &model, None).await;

    assert_eq!(capture.headers.get("session_id").map(String::as_str), Some("override-session"));
    assert_eq!(capture.headers.get("x-client-request-id").map(String::as_str), Some("override-request"));
    assert_eq!(capture.headers.get("x-session-affinity").map(String::as_str), Some("override-affinity"));
}
