use serde_json::{json, Value};

use super::harness::*;

fn vllm_model(compat: Option<Value>) -> maho_ai::types::Model {
    let mut overrides = vec![
        ("id", json!("gpt-4o-mini")),
        ("name", json!("GPT-4o mini")),
        ("provider", json!("openai")),
    ];
    if let Some(compat) = compat {
        overrides.push(("compat", compat));
    }
    model(&overrides)
}

async fn capture_request(model: &maho_ai::types::Model) -> CapturedRequest {
    let transport = ScriptedTransport::success([json!({
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": 1, "completion_tokens": 1 },
    })]);
    let _ = finish(&run(
        model,
        &context(vec![user_message("hi")], None),
        options("test-key"),
        transport.clone(),
    ))
    .await;
    transport.last_request()
}

#[tokio::test]
async fn sends_compat_vllm_priority_as_the_top_level_priority_request_field() {
    let payload = capture_request(&vllm_model(Some(json!({ "vllmPriority": 10 })))).await;

    assert_eq!(payload.get("priority").and_then(Value::as_i64), Some(10));
}

#[tokio::test]
async fn omits_priority_when_vllm_priority_is_not_set() {
    let payload = capture_request(&vllm_model(None)).await;

    assert_eq!(payload.get("priority"), None);
}
