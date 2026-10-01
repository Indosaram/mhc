//! Shared builders for the ported `google*.test.ts` suites (todo 11-google).
//!
//! The TS suites build their models inline (`makeGemini3Model`, `makeModel`, `googleModel`, …) and
//! read models out of the generated catalog with `getModel`. The Rust suites do the same: the
//! catalog-backed base is cloned and the fields the case cares about are overridden.

#![allow(dead_code)]

pub mod server;

use maho_ai::api::google_shared::ConvertMessagesOptions;
use maho_ai::types::{
    AssistantMessage, ContentBlock, Context, ImageContent, InputModality, Message, Model, ProviderEnv, StopReason,
    TextContent, ThinkingContent, Tool, ToolCall, ToolResultMessage, Usage, UserContent, UserMessage,
};
use serde_json::Value;

/// `getModel(provider, id)`.
pub fn builtin_model(provider: &str, id: &str) -> Model {
    maho_ai::models_generated::get_builtin_model(provider, id)
        .unwrap_or_else(|error| panic!("{provider}/{id}: {error}"))
        .clone()
}

/// A catalog-backed base for a model id the catalog may not carry (`gemini-3-pro-preview`).
/// The TS suites build these models inline, so the base only supplies the cost/context fields and
/// every identity field the case cares about is overridden.
pub fn model_with(api: &str, provider: &str, id: &str) -> Model {
    let base = if api == "google-vertex" { "gemini-3-flash-preview" } else { "gemini-2.5-flash" };
    let catalog_provider = if api == "google-vertex" { "google-vertex" } else { "google" };
    let mut model = builtin_model(catalog_provider, base);
    model.id = id.to_owned();
    model.name = id.to_owned();
    model.api = api.to_owned();
    model.provider = provider.to_owned();
    model
}

/// `makeGemini3Model(api, provider, id)` — text-only, no thinking-level map.
pub fn text_model(api: &str, provider: &str, id: &str) -> Model {
    let mut model = model_with(api, provider, id);
    model.input = vec![InputModality::Text];
    model.base_url = "https://example.com".into();
    model
}

/// `makeModel("google-generative-ai", "google", id)` in the image-routing suite.
pub fn image_model(api: &str, provider: &str, id: &str) -> Model {
    let mut model = text_model(api, provider, id);
    model.input = vec![InputModality::Text, InputModality::Image];
    model
}

pub fn context(messages: Vec<Message>) -> Context {
    Context { system_prompt: None, messages, tools: None }
}

pub fn user_text(text: &str) -> Message {
    Message::User(UserMessage { content: UserContent::Text(text.to_owned()), timestamp: 0 })
}

pub fn text(text: &str) -> ContentBlock {
    ContentBlock::Text(TextContent { text: text.to_owned(), ..TextContent::default() })
}

pub fn text_with_signature(text: &str, signature: &str) -> ContentBlock {
    ContentBlock::Text(TextContent {
        text: text.to_owned(),
        text_signature: Some(signature.to_owned()),
        ..TextContent::default()
    })
}

pub fn thinking(text: &str) -> ContentBlock {
    ContentBlock::Thinking(ThinkingContent { thinking: text.to_owned(), ..ThinkingContent::default() })
}

pub fn thinking_with_signature(text: &str, signature: &str) -> ContentBlock {
    ContentBlock::Thinking(ThinkingContent {
        thinking: text.to_owned(),
        thinking_signature: Some(signature.to_owned()),
        ..ThinkingContent::default()
    })
}

pub fn tool_call(id: &str, name: &str, arguments: Value) -> ContentBlock {
    ContentBlock::ToolCall(ToolCall {
        id: id.to_owned(),
        name: name.to_owned(),
        arguments: arguments.as_object().cloned().unwrap_or_default(),
        ..ToolCall::default()
    })
}

pub fn tool_call_with_signature(id: &str, name: &str, arguments: Value, signature: &str) -> ContentBlock {
    ContentBlock::ToolCall(ToolCall {
        id: id.to_owned(),
        name: name.to_owned(),
        arguments: arguments.as_object().cloned().unwrap_or_default(),
        thought_signature: Some(signature.to_owned()),
        ..ToolCall::default()
    })
}

pub fn image(data: &str, mime_type: &str) -> ContentBlock {
    ContentBlock::Image(ImageContent { data: data.to_owned(), mime_type: mime_type.to_owned() })
}

/// An assistant turn the adapters replay (`role: "assistant"` with the provider metadata attached).
pub fn assistant(model: &Model, content: Vec<ContentBlock>, stop_reason: StopReason) -> Message {
    Message::Assistant(Box::new(AssistantMessage {
        content,
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason,
        stop_details: None,
        deferred: None,
        error_message: None,
        abort_source: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 0,
    }))
}

pub fn tool_result(tool_call_id: &str, tool_name: &str, content: Vec<ContentBlock>, is_error: bool) -> Message {
    Message::ToolResult(ToolResultMessage {
        tool_call_id: tool_call_id.to_owned(),
        tool_name: tool_name.to_owned(),
        content,
        details: None,
        usage: None,
        added_tool_names: None,
        is_error,
        timestamp: 0,
    })
}

pub fn tool(name: &str, description: &str, parameters: Value) -> Tool {
    Tool {
        name: name.to_owned(),
        description: description.to_owned(),
        parameters,
        freeform: None,
        constrained_sampling: None,
    }
}

/// The adapter-level options `convertMessages(model, context, options?)` carries.
pub fn preserve_thinking(value: bool) -> ConvertMessagesOptions {
    ConvertMessagesOptions { preserve_thinking: Some(value) }
}

pub fn env(entries: &[(&str, &str)]) -> ProviderEnv {
    entries.iter().map(|(key, value)| ((*key).to_owned(), (*value).to_owned())).collect()
}
