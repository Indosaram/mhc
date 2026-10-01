use std::sync::{Arc, Mutex};

use maho_ai::types::{StopReason};
use serde_json::{json, Value};

use super::harness::*;

fn base_model(overrides: &[(&str, Value)]) -> maho_ai::types::Model {
    let mut model = catalog_model("openai", "gpt-4o-mini");
    model.compat = None;
    let mut value = serde_json::to_value(&model).expect("model json");
    for (key, override_value) in overrides {
        value[key] = override_value.clone();
    }
    serde_json::from_value(value).expect("model")
}

fn capture_options(sink: &Arc<Mutex<Option<Value>>>) -> maho_ai::types::SimpleStreamOptions {
    let writer = sink.clone();
    let mut options = simple_options("test");
    options.stream.request.on_payload = Some(Arc::new(move |payload: &Value, _model, _meta| {
        *writer.lock().expect("payload lock") = Some(payload.clone());
        None
    }));
    options
}

async fn capture(
    model: &maho_ai::types::Model,
    context: &maho_ai::types::Context,
    max_tokens: Option<u64>,
) -> Value {
    let sink: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let transport = ScriptedTransport::success([json!({
        "choices": [{ "delta": {}, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": 1, "completion_tokens": 1 },
    })]);
    let mut options = capture_options(&sink);
    options.stream.max_tokens = max_tokens;
    let message = finish(&run_simple(model, context, options, Some(transport.clone()))).await;
    assert_eq!(message.stop_reason, StopReason::Stop, "request should reach the scripted transport");
    sink.lock().expect("payload lock").clone().unwrap_or_else(|| transport.last_request().body)
}

#[tokio::test]
async fn omits_tools_field_when_context_tools_is_an_empty_array() {
    let model = base_model(&[]);
    let payload = capture(&model, &context(vec![user_message("hi")], Some(Vec::new())), None).await;

    assert!(!payload.as_object().expect("payload object").contains_key("tools"));
}

#[tokio::test]
async fn omits_tools_field_when_context_tools_is_undefined() {
    let model = base_model(&[]);
    let payload = capture(&model, &context(vec![user_message("hi")], None), None).await;

    assert!(!payload.as_object().expect("payload object").contains_key("tools"));
}

#[tokio::test]
async fn sends_default_max_tokens() {
    let model = base_model(&[]);
    let payload = capture(&model, &context(vec![user_message("hi")], None), None).await;

    assert_eq!(payload.get("max_tokens"), None);
    assert_eq!(payload.get("max_completion_tokens").and_then(Value::as_u64), Some(model.max_tokens));
}

#[tokio::test]
async fn sends_explicit_max_tokens() {
    let model = base_model(&[]);
    let payload = capture(&model, &context(vec![user_message("hi")], None), Some(1234)).await;

    assert_eq!(payload.get("max_tokens"), None);
    assert_eq!(payload.get("max_completion_tokens").and_then(Value::as_u64), Some(1234));
}

#[tokio::test]
async fn clamps_default_max_tokens_to_remaining_context() {
    let model = base_model(&[("contextWindow", json!(10000)), ("maxTokens", json!(8000))]);
    let payload = capture(&model, &context(vec![user_message(&"x".repeat(8000))], None), None).await;

    assert_eq!(payload.get("max_tokens"), None);
    assert_eq!(payload.get("max_completion_tokens").and_then(Value::as_u64), Some(3904));
}

#[tokio::test]
async fn clamps_explicit_max_tokens_to_remaining_context() {
    let model = base_model(&[("contextWindow", json!(10000)), ("maxTokens", json!(8000))]);
    let payload = capture(&model, &context(vec![user_message(&"x".repeat(8000))], None), Some(7000)).await;

    assert_eq!(payload.get("max_tokens"), None);
    assert_eq!(payload.get("max_completion_tokens").and_then(Value::as_u64), Some(3904));
}

#[tokio::test]
async fn still_emits_tools_empty_for_anthropic_lite_llm_proxy_when_conversation_has_tool_history() {
    let model = base_model(&[]);
    let mut call = assistant(vec![tool_call("t1", "noop", json!({}))], StopReason::ToolUse);
    if let maho_ai::types::Message::Assistant(assistant) = &mut call {
        assistant.api = "openai-completions".to_owned();
        assistant.provider = "openai".to_owned();
        assistant.model = "gpt-4o-mini".to_owned();
    }
    let messages = context(
        vec![user_message("use the tool"), call, tool_result("t1", vec![text_block("done")])],
        Some(Vec::new()),
    );

    let payload = capture(&model, &messages, None).await;

    let tools = payload.get("tools").and_then(Value::as_array).expect("tools array");
    assert!(tools.is_empty());
}
