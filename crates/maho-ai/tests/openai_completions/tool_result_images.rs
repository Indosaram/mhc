use maho_ai::api::openai_completions::{
    convert_messages, ConvertCompletionsMessagesOptions, ResolvedOpenAICompletionsCompat,
};
use maho_ai::types::{
    ContentBlock, ImageContent, InputModality, MaxTokensField, SessionAffinityFormat, StopReason, TextContent,
    ThinkingFormat, ToolResultMessage,
};
use serde_json::{json, Value};

use super::harness::*;

fn tool_result_images_compat() -> ResolvedOpenAICompletionsCompat {
    ResolvedOpenAICompletionsCompat {
        supports_store: true,
        supports_developer_role: true,
        supports_reasoning_effort: true,
        supports_usage_in_streaming: true,
        supports_finish_reason: true,
        max_tokens_field: MaxTokensField::MaxCompletionTokens,
        requires_tool_result_name: false,
        requires_assistant_after_tool_result: false,
        requires_thinking_as_text: false,
        requires_reasoning_content_on_assistant_messages: false,
        thinking_format: ThinkingFormat::Openai,
        supports_disabled_thinking: true,
        open_router_routing: Default::default(),
        vercel_gateway_routing: Default::default(),
        chat_template_kwargs: Default::default(),
        chat_template_args: Some(Default::default()),
        zai_tool_stream: false,
        supports_thinking_token_budget: Some(false),
        thinking_token_budget_field: None,
        supports_strict_mode: true,
        tool_schema_flavor: None,
        tool_call_format: None,
        supports_openai_grammar_tools: false,
        cache_control_format: Some(maho_ai::types::CacheControlFormat::Anthropic),
        send_session_affinity_headers: false,
        deferred_tools_mode: None,
        session_affinity_format: SessionAffinityFormat::Openai,
        supports_prompt_cache_key: Some(false),
        supports_max_output_tokens: true,
        vllm_priority: None,
        venice_parameters: None,
        supports_long_cache_retention: true,
    }
}

fn image_model() -> maho_ai::types::Model {
    model(&[
        ("id", json!("gpt-4o-mini")),
        ("name", json!("GPT-4o mini")),
        ("provider", json!("openai")),
        ("input", json!(["text", "image"])),
    ])
}

fn image_tool_result(tool_call_id: &str, tool_name: &str, timestamp: i64) -> maho_ai::types::Message {
    maho_ai::types::Message::ToolResult(ToolResultMessage {
        tool_call_id: tool_call_id.to_owned(),
        tool_name: tool_name.to_owned(),
        content: vec![
            ContentBlock::Text(TextContent { text: "Read image file [image/png]".to_owned(), ..TextContent::default() }),
            ContentBlock::Image(ImageContent { data: "ZmFrZQ==".to_owned(), mime_type: "image/png".to_owned() }),
        ],
        details: None,
        usage: None,
        added_tool_names: None,
        is_error: false,
        timestamp,
    })
}

fn empty_tool_result(tool_call_id: &str, tool_name: &str, timestamp: i64) -> maho_ai::types::Message {
    maho_ai::types::Message::ToolResult(ToolResultMessage {
        tool_call_id: tool_call_id.to_owned(),
        tool_name: tool_name.to_owned(),
        content: vec![text_block("")],
        details: None,
        usage: None,
        added_tool_names: None,
        is_error: false,
        timestamp,
    })
}

fn assistant_with_tool_calls(calls: Vec<(&str, &str, Value)>, provider: &str, model_id: &str) -> maho_ai::types::Message {
    let content = calls
        .into_iter()
        .map(|(id, name, arguments)| tool_call(id, name, arguments))
        .collect();
    let mut message = assistant(content, StopReason::ToolUse);
    if let maho_ai::types::Message::Assistant(assistant) = &mut message {
        assistant.provider = provider.to_owned();
        assistant.model = model_id.to_owned();
    }
    message
}

#[test]
fn batches_tool_result_images_after_consecutive_tool_results() {
    let model = image_model();
    let messages = context(
        vec![
            user_message("Read the images"),
            assistant_with_tool_calls(
                vec![
                    ("tool-1", "read", json!({ "path": "img-1.png" })),
                    ("tool-2", "read", json!({ "path": "img-2.png" })),
                ],
                &model.provider,
                &model.id,
            ),
            image_tool_result("tool-1", "read", 2),
            image_tool_result("tool-2", "read", 3),
        ],
        None,
    );

    let converted = convert_messages(
        &model,
        &messages,
        &tool_result_images_compat(),
        &ConvertCompletionsMessagesOptions::default(),
    )
    .expect("convert messages");

    let roles: Vec<&str> = converted.iter().filter_map(|message| message.get("role").and_then(Value::as_str)).collect();
    assert_eq!(roles, vec!["user", "assistant", "tool", "tool", "user"]);

    let image_message = converted.last().expect("image message");
    assert_eq!(image_message.get("role").and_then(Value::as_str), Some("user"));
    let content = image_message.get("content").and_then(Value::as_array).expect("image content array");
    let image_parts = content
        .iter()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("image_url"))
        .count();
    assert_eq!(image_parts, 2);
    let _ = InputModality::Image;
}

#[test]
fn uses_no_tool_output_placeholder_for_empty_tool_results_without_images() {
    let model = image_model();
    let messages = context(
        vec![
            user_message("Run the command"),
            assistant_with_tool_calls(vec![("tool-1", "bash", json!({ "command": "true" }))], &model.provider, &model.id),
            empty_tool_result("tool-1", "bash", 2),
        ],
        None,
    );

    let converted = convert_messages(
        &model,
        &messages,
        &tool_result_images_compat(),
        &ConvertCompletionsMessagesOptions::default(),
    )
    .expect("convert messages");

    let tool_message = converted
        .iter()
        .find(|message| message.get("role").and_then(Value::as_str) == Some("tool"))
        .expect("tool message");
    let content = tool_message.get("content").and_then(Value::as_str).expect("tool content");
    assert_eq!(content, "(no tool output)");
    assert!(!content.contains("see attached image"));
}
