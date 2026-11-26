//! Direct cases from senpi packages/ai/test/openai-responses-siwc.test.ts.

use super::{context, options, wire_client};
use maho_ai::api::openai_responses::{stream, warm_openai_responses_prompt_cache};
use maho_ai::models_generated::get_builtin_model;
use maho_ai::types::{
    CacheRetention, Model, ModelCompat, SimpleStreamOptions, StopReason, StreamOptions, Transport,
};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

const UNSUPPORTED: &[&str] = &[
    "background",
    "conversation",
    "max_output_tokens",
    "max_tool_calls",
    "metadata",
    "moderation",
    "multi_agent",
    "prompt",
    "prompt_cache_retention",
    "safety_identifier",
    "temperature",
    "top_logprobs",
    "top_p",
    "truncation",
    "user",
    "previous_response_id",
];

fn target(siwc: Option<bool>) -> Model {
    let mut model = get_builtin_model("openai", "gpt-6-luna")
        .expect("Responses model")
        .clone();
    model.reasoning = false;
    let compat = model
        .compat
        .get_or_insert_with(|| ModelCompat(Default::default()));
    compat
        .0
        .insert("supportsDeveloperRole".into(), json!(false));
    if let Some(siwc) = siwc {
        compat.0.insert("siwc".into(), json!(siwc));
    }
    model
}

async fn capture(siwc: Option<bool>, configure: impl FnOnce(&mut StreamOptions)) -> Vec<u8> {
    let (client, wire) = wire_client("api.openai.com", 1).await;
    let mut settings = options(client);
    settings.request.api_key = Some("synthetic-siwc-token".into());
    settings.cache_retention = Some(CacheRetention::None);
    configure(&mut settings);
    let model = target(siwc);
    let message = stream(&model, &context(), Some(settings))
        .result()
        .await
        .expect("terminal");
    assert_eq!(
        message.stop_reason,
        StopReason::Stop,
        "{:?}",
        message.error_message
    );
    let mut requests = tokio::time::timeout(Duration::from_secs(5), wire)
        .await
        .expect("wire deadline")
        .expect("fixture request");
    let request = requests.pop().expect("one request");
    assert_eq!(request.line, "POST /v1/responses HTTP/1.1");
    request.body
}

#[tokio::test]
async fn removes_unsupported_fields_and_hosted_tools_after_the_payload_hook() {
    let body: Value = serde_json::from_slice(
        &capture(Some(true), |settings| {
            settings.request.on_payload = Some(Arc::new(|payload, _, _| {
                let mut payload = payload.as_object().expect("payload").clone();
                payload.extend(
                    UNSUPPORTED
                        .iter()
                        .map(|key| ((*key).into(), json!("unsupported"))),
                );
                payload.insert("store".into(), json!(true));
                payload.insert("stream".into(), json!(false));
                payload.insert("tools".into(), json!([
                {"type": "web_search"}, {"type": "image_generation"}, {"type": "file_search"},
                {"type": "code_interpreter"}, {"type": "computer"}, {"type": "mcp"},
                {"type": "tool_search"}, {"type": "programmatic_tool_calling"}
            ]));
                payload.insert("tool_choice".into(), json!({"type": "image_generation"}));
                Some(Value::Object(payload))
            }));
        })
        .await,
    )
    .expect("wire JSON");
    assert_eq!(body["store"], false);
    assert_eq!(body["stream"], true);
    assert!(UNSUPPORTED.iter().all(|field| body.get(field).is_none()));
    assert_eq!(body["tools"], json!([{"type": "web_search"}]));
    assert!(body.get("tool_choice").is_none());
    assert_eq!(body["input"][0]["role"], "developer");
    assert_eq!(body["input"][1]["role"], "user");
}

#[tokio::test]
async fn uses_http_with_full_history_when_websocket_is_requested() {
    let body: Value = serde_json::from_slice(
        &capture(Some(true), |settings| {
            settings.transport = Some(Transport::Websocket);
            settings.extra_body = json!({"previous_response_id": "synthetic-previous"})
                .as_object()
                .cloned();
        })
        .await,
    )
    .expect("wire JSON");
    assert!(body.get("previous_response_id").is_none());
    assert_eq!(body["input"].as_array().expect("message history").len(), 2);
}

#[tokio::test]
async fn rewrites_system_items_from_payload_hooks_without_dropping_history() {
    let body: Value = serde_json::from_slice(
        &capture(Some(true), |settings| {
            settings.request.on_payload = Some(Arc::new(|payload, _, _| {
                let mut updated = payload.clone();
                updated["input"]
                    .as_array_mut()
                    .expect("history")
                    .push(json!({"type": "message", "role": "system", "content": "hook-marker"}));
                Some(updated)
            }));
        })
        .await,
    )
    .expect("wire JSON");
    assert_eq!(body["input"].as_array().expect("message history").len(), 3);
    assert_eq!(
        body["input"][0],
        json!({"role": "developer", "content": "fixture instructions"})
    );
    assert_eq!(body["input"][1]["role"], "user");
    assert_eq!(
        body["input"][2],
        json!({"type": "message", "role": "developer", "content": "hook-marker"})
    );
}

#[tokio::test]
async fn preserves_local_custom_and_namespace_tools_but_strips_nested_hosted_tools() {
    let body: Value = serde_json::from_slice(
        &capture(Some(true), |settings| {
            settings.request.on_payload = Some(Arc::new(|payload, _, _| {
                let mut updated = payload.clone();
                updated["tools"] = json!([
                    {"type": "custom", "name": "local", "format": {"type": "text"}},
                    {"type": "namespace", "name": "fixture", "tools": [
                        {"type": "function", "name": "read"}, {"type": "file_search"}
                    ]}
                ]);
                Some(updated)
            }));
        })
        .await,
    )
    .expect("wire JSON");
    assert_eq!(
        body["tools"],
        json!([{"type": "namespace", "name": "fixture", "tools": [
            {"type": "function", "name": "read"}
        ]}])
    );
    assert_eq!(
        body["input"][0],
        json!({
            "type": "additional_tools", "role": "developer",
            "tools": [{"type": "custom", "name": "local", "format": {"type": "text"}}]
        })
    );
    assert_eq!(body["input"][1]["role"], "developer");
    assert_eq!(body["input"][2]["role"], "user");
}

#[tokio::test]
async fn explicitly_disabled_siwc_preserves_native_legacy_responses_body_bytes() {
    let configure = |settings: &mut StreamOptions| {
        settings.max_tokens = Some(128);
        settings.temperature = Some(0.5);
        settings.extra_body =
            json!({"metadata": {"fixture": true}, "previous_response_id": "synthetic-old"})
                .as_object()
                .cloned();
    };
    let disabled = capture(Some(false), configure).await;
    assert_eq!(disabled, capture(None, configure).await);
    let body: Value = serde_json::from_slice(&disabled).expect("wire JSON");
    assert_eq!(body["temperature"], 0.5);
    assert_eq!(body["input"][0]["role"], "system");
    assert_eq!(body["metadata"], json!({"fixture": true}));
}

#[tokio::test]
async fn refuses_prompt_cache_prewarm_before_sending_a_siwc_request() {
    let (client, wire) = wire_client("api.openai.com", 0).await;
    let mut settings = options(client);
    settings.request.api_key = Some("synthetic-siwc-token".into());
    let outcome = warm_openai_responses_prompt_cache(
        &target(Some(true)),
        &context(),
        SimpleStreamOptions {
            stream: settings,
            ..Default::default()
        },
    )
    .await;
    assert!(outcome.expect_err("prewarm refused").contains("SIWC"));
    assert!(wire.await.expect("loopback fixture").is_empty());
}
