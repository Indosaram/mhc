//! Port of senpi packages/ai/src/api/devin-agent/request.ts.
//!
//! Builds the Cascade `GetChatMessage` request from senpi's provider-neutral context. Cascade has no
//! system role: the system prompt travels in the top-level `prompt` field, and history is a flat
//! list of `ChatMessagePrompt` entries whose `source` carries the role. Message ids must be
//! UUID-shaped; they are derived deterministically from the conversation id and the entry index so a
//! retried turn re-sends the same ids instead of forking the server-side transcript, while a native
//! Devin turn is replayed under the id the server minted for it.

use crate::api::cursor_agent::deterministic_id::deterministic_uuid;
use crate::api::devin_agent::r#gen::cascade_pb::{
    chat_tool_choice, CacheControlType, ChatMessagePrompt, ChatMessageRequestType, ChatMessageSource, ChatToolCall,
    ChatToolChoice, ChatToolDefinition, CompletionConfiguration, ConversationalPlannerMode, GetChatMessageRequest,
    ImageData, PromptCacheOptions,
};
use crate::api::devin_agent::metadata::devin_cli_metadata;
use crate::types::{ContentBlock, Context, Message, Model, Tool, UserContent};

/// `DEVIN_DEFAULT_STOP_PATTERNS`: Cascade's own stop vocabulary; the server echoes these as
/// STOP_PATTERN.
pub const DEVIN_DEFAULT_STOP_PATTERNS: [&str; 5] =
    ["<|user|>", "<|bot|>", "<|context_request|>", "<|endoftext|>", "<|end_of_turn|>"];

const DEFAULT_TEMPERATURE: f64 = 0.4;
/// Cascade refuses a temperature of exactly 0 (proto3 drops it) with an opaque invalid_argument.
const MIN_TEMPERATURE: f64 = 0.0001;

/// `DevinModelAssignment`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevinModelAssignment {
    pub model_uid: String,
    pub assignment_jwt: String,
}

/// `DevinChatRequestInput`.
pub struct DevinChatRequestInput<'a> {
    pub model: &'a Model,
    pub context: &'a Context,
    pub api_key: Option<&'a str>,
    pub user_jwt: Option<&'a str>,
    pub cascade_id: &'a str,
    pub assignment: Option<&'a DevinModelAssignment>,
    pub max_tokens: Option<u64>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub stop_sequences: Option<&'a [String]>,
}

/// `buildDevinChatRequest`.
pub fn build_devin_chat_request(input: &DevinChatRequestInput<'_>) -> GetChatMessageRequest {
    let temperature = input.temperature.unwrap_or(DEFAULT_TEMPERATURE).max(MIN_TEMPERATURE);
    let mut stop_patterns: Vec<String> = DEVIN_DEFAULT_STOP_PATTERNS.iter().map(|pattern| (*pattern).to_owned()).collect();
    stop_patterns.extend(input.stop_sequences.unwrap_or_default().iter().cloned());
    let chat_model_uid = match input.assignment {
        Some(assignment) => assignment.model_uid.clone(),
        None => input.model.upstream_model_id.clone().unwrap_or_else(|| input.model.id.clone()),
    };
    GetChatMessageRequest {
        metadata: Some(devin_cli_metadata(input.api_key, input.user_jwt.unwrap_or_default())),
        prompt: input.context.system_prompt.clone().unwrap_or_default(),
        chat_message_prompts: map_history(&input.context.messages, input.cascade_id, input.model),
        use_internal_chat_model: false,
        request_type: ChatMessageRequestType::Cascade as i32,
        configuration: Some(CompletionConfiguration {
            num_completions: 1,
            max_tokens: input.max_tokens.unwrap_or(input.model.max_tokens),
            max_newlines: 200,
            min_log_probability: 0.0,
            temperature,
            first_temperature: temperature,
            top_k: 50,
            top_p: input.top_p.unwrap_or(1.0),
            stop_patterns,
            seed: 0,
            fim_eot_prob_threshold: 1.0,
        }),
        tools: input.context.tools.as_deref().unwrap_or_default().iter().map(tool_definition).collect(),
        disable_parallel_tool_calls: input.model.compat.as_ref().and_then(|compat| compat.devin_agent().supports_parallel_tool_calls) != Some(true),
        tool_choice: Some(ChatToolChoice {
            choice: Some(chat_tool_choice::Choice::OptionName(String::from("auto"))),
        }),
        system_prompt_cache_options: Some(PromptCacheOptions { r#type: CacheControlType::Ephemeral as i32 }),
        chat_model_name: String::new(),
        cascade_id: input.cascade_id.to_owned(),
        prompt_id: String::new(),
        planner_mode: ConversationalPlannerMode::Default as i32,
        chat_model_uid,
        execution_id: uuid::Uuid::new_v4().to_string(),
        model_assignment_jwt: input.assignment.map(|assignment| assignment.assignment_jwt.clone()),
    }
}

/// `buildDevinRouterPrompt`: the router scores the latest user turn alone; the chat request mints
/// the turn's id.
pub fn build_devin_router_prompt(messages: &[Message]) -> Option<ChatMessagePrompt> {
    for message in messages.iter().rev() {
        if let Message::User(user) = message {
            return Some(user_prompt(user, ""));
        }
    }
    None
}

fn tool_definition(tool: &Tool) -> ChatToolDefinition {
    ChatToolDefinition {
        name: tool.name.clone(),
        description: tool.description.clone(),
        json_schema_string: serde_json::to_string(&tool.parameters).unwrap_or_else(|_| String::from("{}")),
        attribution_field_names: Vec::new(),
        server_name: String::new(),
        read_only_hint: None,
        computer_use_config: None,
        is_custom_tool: None,
        strict: false,
    }
}

fn map_history(messages: &[Message], cascade_id: &str, model: &Model) -> Vec<ChatMessagePrompt> {
    let mut prompts: Vec<ChatMessagePrompt> = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        let seed = format!("{cascade_id}\u{0}{index}\u{0}{}", message.role());
        match message {
            Message::User(user) => prompts.push(user_prompt(user, &deterministic_uuid(&seed))),
            Message::Assistant(assistant) => {
                if let Some(prompt) = assistant_prompt(assistant, model, &format!("bot-{}", deterministic_uuid(&seed))) {
                    prompts.push(prompt);
                }
            }
            Message::ToolResult(tool_result) => prompts.push(tool_result_prompt(
                tool_result,
                &deterministic_uuid(&format!("{seed}\u{0}{}", tool_result.tool_call_id)),
            )),
            Message::ConfigurationUpdate(_) => {}
        }
    }
    prompts
}

fn user_prompt(message: &crate::types::UserMessage, message_id: &str) -> ChatMessagePrompt {
    let (text, images) = match &message.content {
        UserContent::Text(text) => (text.clone(), Vec::new()),
        UserContent::Blocks(blocks) => split_content(blocks),
    };
    ChatMessagePrompt {
        message_id: message_id.to_owned(),
        source: ChatMessageSource::User as i32,
        prompt: text,
        num_tokens: 0,
        safe_for_code_telemetry: false,
        tool_calls: Vec::new(),
        tool_call_id: String::new(),
        prompt_cache_options: None,
        tool_result_is_error: false,
        images,
        thinking: String::new(),
        signature: String::new(),
        thinking_redacted: false,
        signature_type: String::new(),
    }
}

fn assistant_prompt(
    message: &crate::types::AssistantMessage,
    model: &Model,
    fallback_id: &str,
) -> Option<ChatMessagePrompt> {
    let native = message.api == model.api && message.provider == model.provider && message.model == model.id;
    let mut text = String::new();
    let mut thinking = String::new();
    let mut signature = String::new();
    let mut tool_calls: Vec<ChatToolCall> = Vec::new();
    for block in &message.content {
        match block {
            ContentBlock::Text(block) => text.push_str(&block.text),
            ContentBlock::Thinking(block) => {
                thinking.push_str(&block.thinking);
                if native && signature.is_empty()
                    && let Some(block_signature) = block.thinking_signature.as_ref()
                {
                    signature = block_signature.clone();
                }
            }
            ContentBlock::ToolCall(block) => tool_calls.push(ChatToolCall {
                id: block.id.clone(),
                name: block.name.clone(),
                arguments_json: serde_json::to_string(&block.arguments).unwrap_or_else(|_| String::from("{}")),
                invalid_json_str: String::new(),
                invalid_json_err: String::new(),
                is_custom_tool_call: false,
            }),
            _ => {}
        }
    }
    if text.is_empty() && thinking.is_empty() && signature.is_empty() && tool_calls.is_empty() {
        return None;
    }
    let message_id = if native { message.response_id.clone().unwrap_or_else(|| fallback_id.to_owned()) } else { fallback_id.to_owned() };
    Some(ChatMessagePrompt {
        message_id,
        source: ChatMessageSource::System as i32,
        prompt: text,
        num_tokens: 0,
        safe_for_code_telemetry: false,
        tool_calls,
        tool_call_id: String::new(),
        prompt_cache_options: None,
        tool_result_is_error: false,
        images: Vec::new(),
        thinking,
        signature,
        thinking_redacted: false,
        signature_type: String::new(),
    })
}

fn tool_result_prompt(message: &crate::types::ToolResultMessage, message_id: &str) -> ChatMessagePrompt {
    let (text, images) = split_content(&message.content);
    ChatMessagePrompt {
        message_id: message_id.to_owned(),
        source: ChatMessageSource::Tool as i32,
        prompt: text,
        num_tokens: 0,
        safe_for_code_telemetry: false,
        tool_calls: Vec::new(),
        tool_call_id: message.tool_call_id.clone(),
        prompt_cache_options: None,
        tool_result_is_error: message.is_error,
        images,
        thinking: String::new(),
        signature: String::new(),
        thinking_redacted: false,
        signature_type: String::new(),
    }
}

fn split_content(content: &[ContentBlock]) -> (String, Vec<ImageData>) {
    let mut text = String::new();
    let mut images: Vec<ImageData> = Vec::new();
    for block in content {
        match block {
            ContentBlock::Text(block) => text.push_str(&block.text),
            ContentBlock::Image(block) => images.push(ImageData {
                base64_data: block.data.clone(),
                mime_type: block.mime_type.clone(),
                caption: String::new(),
            }),
            _ => {}
        }
    }
    (text, images)
}
