use maho_ai::types::{CacheRetention, ContentBlock, Message, StopReason};
use serde_json::{json, Value};

use super::harness::*;

async fn capture_payload(
    model: &maho_ai::types::Model,
    cache_retention: Option<CacheRetention>,
    messages: Option<Vec<Message>>,
) -> CapturedRequest {
    let read = tool("read", json!({ "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] }));
    let context = context(
        messages.unwrap_or_else(|| vec![user_message("Hello")]),
        Some(vec![read]),
    );
    let mut context = context;
    context.system_prompt = Some("System prompt".to_owned());
    let mut options = options("test-key");
    options.stream.cache_retention = cache_retention;
    let transport = ScriptedTransport::success([json!({
        "id": "chatcmpl-test",
        "choices": [{ "delta": {}, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": 1, "completion_tokens": 1 },
    })]);
    let _ = finish(&run(model, &context, options, transport.clone())).await;
    transport.last_request()
}

fn instruction_message(request: &CapturedRequest) -> Option<&Value> {
    request
        .messages()
        .iter()
        .find(|message| matches!(message.get("role").and_then(Value::as_str), Some("system" | "developer")))
}

fn expect_anthropic_cache_markers(request: &CapturedRequest) {
    let instruction = instruction_message(request).expect("instruction message");
    let content = instruction.get("content").and_then(Value::as_array).expect("instruction content array");
    assert_eq!(content[0].get("cache_control"), Some(&json!({ "type": "ephemeral" })));

    assert_eq!(request.tools().len(), 1);
    assert_eq!(request.tools()[0].get("cache_control"), Some(&json!({ "type": "ephemeral" })));

    let last = request.messages().last().expect("last message");
    assert_eq!(last.get("role").and_then(Value::as_str), Some("user"));
    let content = last.get("content").and_then(Value::as_array).expect("last content array");
    assert_eq!(content[0].get("cache_control"), Some(&json!({ "type": "ephemeral" })));
}

fn open_router_model(id: &str) -> maho_ai::types::Model {
    model(&[
        ("id", json!(id)),
        ("name", json!(id)),
        ("provider", json!("openrouter")),
        ("baseUrl", json!("https://openrouter.ai/api/v1")),
        ("reasoning", json!(true)),
        ("maxTokens", json!(32000)),
    ])
}

fn custom_qwen() -> maho_ai::types::Model {
    model(&[
        ("id", json!("custom-qwen")),
        ("name", json!("Custom Qwen")),
        ("provider", json!("openrouter")),
        ("baseUrl", json!("https://example.com/v1")),
        ("reasoning", json!(true)),
        ("maxTokens", json!(32000)),
        ("compat", json!({ "cacheControlFormat": "anthropic" })),
    ])
}

#[tokio::test]
async fn applies_anthropic_style_cache_markers_when_model_compat_enables_them() {
    let params = capture_payload(&custom_qwen(), None, None).await;

    expect_anthropic_cache_markers(&params);
}

#[tokio::test]
async fn applies_anthropic_style_cache_markers_for_allowlisted_openrouter_model_anthropic() {
    let params = capture_payload(&open_router_model("anthropic/claude"), None, None).await;

    expect_anthropic_cache_markers(&params);
}

#[tokio::test]
async fn applies_anthropic_style_cache_markers_for_allowlisted_openrouter_model_batch_anthropic() {
    let params = capture_payload(&open_router_model("~anthropic/claude"), None, None).await;

    expect_anthropic_cache_markers(&params);
}

#[tokio::test]
async fn applies_anthropic_style_cache_markers_for_allowlisted_openrouter_model_qwen() {
    let params = capture_payload(&open_router_model("qwen/qwen3-235b-a22b"), None, None).await;

    expect_anthropic_cache_markers(&params);
}

#[tokio::test]
async fn applies_anthropic_style_cache_markers_for_allowlisted_openrouter_model_google() {
    let params = capture_payload(&open_router_model("google/gemini-2.5-pro"), None, None).await;

    expect_anthropic_cache_markers(&params);
}

#[tokio::test]
async fn does_not_apply_cache_markers_to_openrouter_models_outside_the_prefix_allowlist() {
    let params = capture_payload(&open_router_model("meta-llama/llama-3.3-70b-instruct"), None, None).await;
    let instruction = instruction_message(&params).expect("instruction message");

    assert!(!instruction.get("content").is_some_and(Value::is_array));
    assert_eq!(params.tools()[0].get("cache_control"), None);
    assert!(params.messages().last().and_then(|message| message.get("content")).is_some_and(Value::is_string));
}

#[tokio::test]
async fn moves_the_conversation_cache_marker_to_a_tool_result() {
    let model = open_router_model("anthropic/claude:batch");
    let messages = vec![
        user_message("Read the file"),
        assistant(vec![tool_call("call_1", "read", json!({ "path": "README.md" }))], StopReason::ToolUse),
        tool_result("call_1", vec![text_block("file contents")]),
    ];
    let params = capture_payload(&model, None, Some(messages)).await;

    let user = params.messages().iter().find(|message| message.get("role").and_then(Value::as_str) == Some("user"));
    assert_eq!(user.and_then(|message| message.get("content")), Some(&json!("Read the file")));

    let tool_message = params.messages().last().expect("last message");
    assert_eq!(tool_message.get("role").and_then(Value::as_str), Some("tool"));
    let content = tool_message.get("content").and_then(Value::as_array).expect("tool content array");
    assert_eq!(content[0].get("cache_control"), Some(&json!({ "type": "ephemeral" })));
}

#[tokio::test]
async fn omits_anthropic_style_cache_markers_when_cache_retention_is_none() {
    let params = capture_payload(&custom_qwen(), Some(CacheRetention::None), None).await;
    let instruction = instruction_message(&params).expect("instruction message");

    assert!(!instruction.get("content").is_some_and(Value::is_array));
    assert_eq!(params.tools()[0].get("cache_control"), None);
    assert!(params.messages().last().and_then(|message| message.get("content")).is_some_and(Value::is_string));
    let _ = ContentBlock::Text(maho_ai::types::TextContent::default());
}
