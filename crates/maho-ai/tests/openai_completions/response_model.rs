use maho_ai::types::StopReason;
use serde_json::json;

use super::harness::*;

fn open_router_auto() -> maho_ai::types::Model {
    model(&[
        ("id", json!("openrouter/auto")),
        ("name", json!("OpenRouter Auto")),
        ("provider", json!("openrouter")),
        ("baseUrl", json!("https://openrouter.ai/api/v1")),
        ("contextWindow", json!(200000)),
        ("maxTokens", json!(8192)),
    ])
}

#[tokio::test]
async fn surfaces_routed_chunk_model_on_response_model_without_changing_model() {
    let transport = ScriptedTransport::success([
        json!({
            "id": "chatcmpl-1",
            "model": "anthropic/claude-3",
            "choices": [{ "index": 0, "delta": { "content": "hi" } }],
        }),
        json!({
            "id": "chatcmpl-1",
            "model": "anthropic/claude-3",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 5 },
        }),
    ]);

    let message = finish(&run(
        &open_router_auto(),
        &context(vec![user_message("hi")], None),
        options("test"),
        transport,
    ))
    .await;

    assert_eq!(message.model, "openrouter/auto");
    assert_eq!(message.response_model.as_deref(), Some("anthropic/claude-3"));
    assert_eq!(message.provider, "openrouter");
    assert_eq!(message.stop_reason, StopReason::Stop);
}

#[tokio::test]
async fn leaves_response_model_undefined_when_chunks_echo_the_requested_id() {
    let transport = ScriptedTransport::success([
        json!({
            "id": "chatcmpl-2",
            "model": "openrouter/auto",
            "choices": [{ "index": 0, "delta": { "content": "hi" } }],
        }),
        json!({
            "id": "chatcmpl-2",
            "model": "openrouter/auto",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1 },
        }),
    ]);

    let message = finish(&run(
        &open_router_auto(),
        &context(vec![user_message("hi")], None),
        options("test"),
        transport,
    ))
    .await;

    assert_eq!(message.model, "openrouter/auto");
    assert_eq!(message.response_model, None);
}

#[tokio::test]
async fn ignores_empty_or_missing_chunk_model() {
    let transport = ScriptedTransport::success([
        json!({ "id": "chatcmpl-3", "choices": [{ "index": 0, "delta": { "content": "hi" } }] }),
        json!({ "id": "chatcmpl-3", "model": "", "choices": [{ "index": 0, "delta": { "content": "!" } }] }),
        json!({
            "id": "chatcmpl-3",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 2 },
        }),
    ]);

    let message = finish(&run(
        &open_router_auto(),
        &context(vec![user_message("hi")], None),
        options("test"),
        transport,
    ))
    .await;

    assert_eq!(message.model, "openrouter/auto");
    assert_eq!(message.response_model, None);
}
