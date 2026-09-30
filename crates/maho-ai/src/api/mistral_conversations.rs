//! Port of senpi packages/ai/src/api/mistral-conversations.ts.

use serde_json::{json, Map, Value};

use crate::api::constrained_sampling::{get_json_schema_tool_parameters, resolve_json_schema_strict_sampling};
use crate::api::simple_options::{apply_extra_body, build_base_options, MISTRAL_RESERVED_BODY_KEYS};
use crate::api::transform_messages::{transform_messages, TransformMessagesOptions};
use crate::models::{calculate_cost, clamp_thinking_level};
use crate::types::{
    AssistantMessage, AssistantMessageEvent, ContentBlock, Context, DoneReason, ErrorReason, Message, Model,
    ModelThinkingLevel, SimpleStreamOptions, StopReason, StreamOptions, Tool, Usage,
};
use crate::utils::event_stream::{create_assistant_message_event_stream, AssistantMessageEventStream};
use crate::utils::hash::short_hash;
use crate::utils::headers::headers_to_record;
use crate::utils::json_parse::parse_streaming_json;
use crate::utils::pi_user_agent::get_pi_user_agent;
use crate::utils::sanitize_unicode::sanitize_surrogates;

const MISTRAL_TOOL_CALL_ID_LENGTH: usize = 9;
const MAX_MISTRAL_ERROR_BODY_CHARS: usize = 4000;
const DEFAULT_TIMEOUT_MS: u64 = 60_000;

fn option_value(options: &StreamOptions, key: &str) -> Option<Value> {
    options.extra.get(key).cloned().filter(|value| !value.is_null())
}

fn create_output(model: &Model) -> AssistantMessage {
    AssistantMessage {
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::Pending,
        stop_details: None,
        deferred: None,
        error_message: None,
        abort_source: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: crate::utils::diagnostics::now_ms(),
    }
}

struct MistralToolCallIdNormalizer {
    id_map: std::collections::HashMap<String, String>,
    reverse_map: std::collections::HashMap<String, String>,
}

impl MistralToolCallIdNormalizer {
    fn new() -> Self {
        Self { id_map: std::collections::HashMap::new(), reverse_map: std::collections::HashMap::new() }
    }

    fn normalize(&mut self, id: &str) -> String {
        if let Some(existing) = self.id_map.get(id) {
            return existing.clone();
        }

        let mut attempt = 0;
        loop {
            let candidate = derive_mistral_tool_call_id(id, attempt);
            match self.reverse_map.get(&candidate) {
                Some(owner) if owner != id => attempt += 1,
                _ => {
                    self.id_map.insert(id.to_owned(), candidate.clone());
                    self.reverse_map.insert(candidate.clone(), id.to_owned());
                    return candidate;
                }
            }
        }
    }
}

fn derive_mistral_tool_call_id(id: &str, attempt: usize) -> String {
    let normalized: String = id.chars().filter(|character| character.is_ascii_alphanumeric()).collect();
    if attempt == 0 && normalized.len() == MISTRAL_TOOL_CALL_ID_LENGTH {
        return normalized;
    }
    let seed_base = if normalized.is_empty() { id } else { &normalized };
    let seed = if attempt == 0 { seed_base.to_owned() } else { format!("{seed_base}:{attempt}") };
    short_hash(&seed)
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .take(MISTRAL_TOOL_CALL_ID_LENGTH)
        .collect()
}

fn format_mistral_error(error: &MistralError) -> String {
    match error {
        MistralError::Http(http) => {
            let body_text = http.body.trim();
            if !body_text.is_empty() {
                format!(
                    "Mistral API error ({}): {}",
                    http.status_code,
                    truncate_error_text(body_text, MAX_MISTRAL_ERROR_BODY_CHARS)
                )
            } else {
                format!("Mistral API error ({}): {}", http.status_code, http.message)
            }
        }
        MistralError::Message(message) => message.clone(),
    }
}

fn truncate_error_text(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_owned();
    }
    format!("{}... [truncated {} chars]", &text[..max_chars], text.len() - max_chars)
}

enum MistralError {
    Http(MistralHttpError),
    Message(String),
}

#[derive(Debug, Clone)]
struct MistralHttpError {
    status_code: u16,
    body: String,
    message: String,
}

fn build_mistral_headers(model: &Model, api_key: &str, options: &StreamOptions) -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    let set = |headers: &mut reqwest::header::HeaderMap, name: &'static str, value: String| {
        if let Ok(value) = reqwest::header::HeaderValue::from_str(&value) {
            headers.insert(reqwest::header::HeaderName::from_static(name), value);
        }
    };
    set(&mut headers, "user-agent", get_pi_user_agent());
    set(&mut headers, "accept", "text/event-stream".to_owned());
    set(&mut headers, "authorization", format!("Bearer {api_key}"));
    set(&mut headers, "content-type", "application/json".to_owned());
    apply_mistral_model_headers(&mut headers, model.headers.as_ref());
    apply_mistral_provider_headers(&mut headers, options.request.headers.as_ref());

    let has_explicit_affinity = has_mistral_model_header_override(model.headers.as_ref(), "x-affinity")
        || has_mistral_provider_header_override(options.request.headers.as_ref(), "x-affinity");
    if should_use_prompt_caching(options)
        && !has_explicit_affinity
        && let Some(session_id) = &options.session_id
    {
        set(&mut headers, "x-affinity", session_id.clone());
    }

    headers
}

fn apply_mistral_model_headers(
    headers: &mut reqwest::header::HeaderMap,
    overrides: Option<&std::collections::BTreeMap<String, String>>,
) {
    let Some(overrides) = overrides else { return };
    for (name, value) in overrides {
        let Ok(name) = reqwest::header::HeaderName::from_bytes(name.as_bytes()) else { continue };
        if let Ok(value) = reqwest::header::HeaderValue::from_str(value) {
            headers.insert(name, value);
        }
    }
}

fn apply_mistral_provider_headers(
    headers: &mut reqwest::header::HeaderMap,
    overrides: Option<&crate::types::ProviderHeaders>,
) {
    let Some(overrides) = overrides else { return };
    for (name, value) in overrides {
        let Ok(name) = reqwest::header::HeaderName::from_bytes(name.as_bytes()) else { continue };
        match value {
            None => {
                headers.remove(name);
            }
            Some(value) => {
                if let Ok(value) = reqwest::header::HeaderValue::from_str(value) {
                    headers.insert(name, value);
                }
            }
        }
    }
}

fn has_mistral_model_header_override(
    overrides: Option<&std::collections::BTreeMap<String, String>>,
    target: &str,
) -> bool {
    overrides.is_some_and(|overrides| overrides.keys().any(|name| name.to_lowercase() == target))
}

fn has_mistral_provider_header_override(
    overrides: Option<&crate::types::ProviderHeaders>,
    target: &str,
) -> bool {
    overrides.is_some_and(|overrides| overrides.keys().any(|name| name.to_lowercase() == target))
}

fn to_mistral_wire_payload(payload: &Value) -> Value {
    let mut wire = payload.clone();
    let Some(object) = wire.as_object_mut() else { return wire };
    for (source, target) in [
        ("topP", "top_p"),
        ("maxTokens", "max_tokens"),
        ("randomSeed", "random_seed"),
        ("responseFormat", "response_format"),
        ("toolChoice", "tool_choice"),
        ("presencePenalty", "presence_penalty"),
        ("frequencyPenalty", "frequency_penalty"),
        ("parallelToolCalls", "parallel_tool_calls"),
        ("reasoningEffort", "reasoning_effort"),
        ("promptMode", "prompt_mode"),
        ("promptCacheKey", "prompt_cache_key"),
        ("safePrompt", "safe_prompt"),
    ] {
        remap_mistral_property(object, source, target);
    }
    if let Some(Value::Array(messages)) = object.get("messages").cloned().as_ref() {
        let mapped: Vec<Value> = messages.iter().map(to_mistral_wire_message).collect();
        object.insert("messages".into(), Value::Array(mapped));
    }

    if let Some(Value::Object(response_format)) = object.get("response_format").cloned().as_ref() {
        let mut wire_response_format = response_format.clone();
        remap_mistral_property(&mut wire_response_format, "jsonSchema", "json_schema");
        if let Some(Value::Object(json_schema)) = wire_response_format.get("json_schema").cloned().as_ref() {
            let mut wire_json_schema = json_schema.clone();
            remap_mistral_property(&mut wire_json_schema, "schemaDefinition", "schema");
            wire_response_format.insert("json_schema".into(), Value::Object(wire_json_schema));
        }
        object.insert("response_format".into(), Value::Object(wire_response_format));
    }

    wire
}

fn to_mistral_wire_message(message: &Value) -> Value {
    let mut wire = message.clone();
    let Some(object) = wire.as_object_mut() else { return wire };
    remap_mistral_property(object, "toolCalls", "tool_calls");
    remap_mistral_property(object, "toolCallId", "tool_call_id");
    if let Some(Value::Array(content)) = object.get("content").cloned().as_ref() {
        let mapped: Vec<Value> = content.iter().map(to_mistral_wire_content_chunk).collect();
        object.insert("content".into(), Value::Array(mapped));
    }
    wire
}

fn to_mistral_wire_content_chunk(chunk: &Value) -> Value {
    let mut wire = chunk.clone();
    let Some(object) = wire.as_object_mut() else { return wire };
    for (source, target) in [
        ("imageUrl", "image_url"),
        ("documentUrl", "document_url"),
        ("documentName", "document_name"),
        ("fileId", "file_id"),
        ("referenceIds", "reference_ids"),
        ("inputAudio", "input_audio"),
    ] {
        remap_mistral_property(object, source, target);
    }
    wire
}

fn remap_mistral_property(record: &mut Map<String, Value>, source: &str, target: &str) {
    let Some(value) = record.remove(source) else { return };
    record.insert(target.to_owned(), value);
}

fn to_function_tools(tools: &[Tool]) -> Result<Vec<Value>, String> {
    let mut result = Vec::with_capacity(tools.len());
    for tool in tools {
        let strict = resolve_json_schema_strict_sampling(tool, true)?;
        let parameters = get_json_schema_tool_parameters(tool, strict).map_err(|error| error.to_string())?;
        result.push(json!({
            "type": "function",
            "function": {
                "name": tool.name,
                "description": tool.description,
                "parameters": strip_symbol_keys(&parameters),
                "strict": strict.unwrap_or(false),
            },
        }));
    }
    Ok(result)
}

fn strip_symbol_keys(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(strip_symbol_keys).collect()),
        Value::Object(map) => Value::Object(map.iter().map(|(key, value)| (key.clone(), strip_symbol_keys(value))).collect()),
        other => other.clone(),
    }
}

fn to_chat_messages(messages: &[Message], supports_images: bool) -> Vec<Value> {
    let mut result: Vec<Value> = Vec::new();

    for message in messages {
        match message {
            Message::ConfigurationUpdate(_) => continue,
            Message::User(user) => {
                if let crate::types::UserContent::Text(text) = &user.content {
                    result.push(json!({ "role": "user", "content": sanitize_surrogates(text) }));
                    continue;
                }
                let crate::types::UserContent::Blocks(blocks) = &user.content else { continue };
                let had_images = blocks.iter().any(|block| block.type_name() == "image");
                let content: Vec<Value> = blocks
                    .iter()
                    .filter(|block| block.type_name() == "text" || supports_images)
                    .map(|block| match block {
                        ContentBlock::Text(text) => {
                            json!({ "type": "text", "text": sanitize_surrogates(&text.text) })
                        }
                        ContentBlock::Image(image) => {
                            json!({ "type": "image_url", "imageUrl": format!("data:{};base64,{}", image.mime_type, image.data) })
                        }
                        other => serde_json::to_value(other).unwrap_or(Value::Null),
                    })
                    .collect();
                if !content.is_empty() {
                    result.push(json!({ "role": "user", "content": content }));
                    continue;
                }
                if had_images && !supports_images {
                    result.push(json!({ "role": "user", "content": "(image omitted: model does not support images)" }));
                }
                continue;
            }
            Message::Assistant(assistant) => {
                let mut content_parts: Vec<Value> = Vec::new();
                let mut tool_calls: Vec<Value> = Vec::new();

                for block in &assistant.content {
                    match block {
                        ContentBlock::Text(text) if !text.text.trim().is_empty() => {
                            content_parts.push(json!({ "type": "text", "text": sanitize_surrogates(&text.text) }));
                        }
                        ContentBlock::Thinking(thinking) if !thinking.thinking.trim().is_empty() => {
                            content_parts.push(json!({
                                "type": "thinking",
                                "thinking": [{ "type": "text", "text": sanitize_surrogates(&thinking.thinking) }],
                            }));
                        }
                        ContentBlock::ToolCall(tool_call) => {
                            tool_calls.push(json!({
                                "id": tool_call.id,
                                "type": "function",
                                "function": {
                                    "name": tool_call.name,
                                    "arguments": Value::Object(tool_call.arguments.clone()).to_string(),
                                },
                                "index": 0,
                            }));
                        }
                        _ => {}
                    }
                }

                let mut assistant_message = Map::new();
                assistant_message.insert("role".into(), Value::from("assistant"));
                assistant_message.insert("prefix".into(), Value::from(false));
                if !content_parts.is_empty() {
                    assistant_message.insert("content".into(), Value::Array(content_parts));
                }
                if !tool_calls.is_empty() {
                    assistant_message.insert("toolCalls".into(), Value::Array(tool_calls));
                }
                if assistant_message.contains_key("content") || assistant_message.contains_key("toolCalls") {
                    result.push(Value::Object(assistant_message));
                }
                continue;
            }
            Message::ToolResult(result_message) => {
                let text_result: String = result_message
                    .content
                    .iter()
                    .filter_map(|part| match part {
                        ContentBlock::Text(text) => Some(sanitize_surrogates(&text.text)),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let has_images = result_message.content.iter().any(|part| part.type_name() == "image");
                let mut tool_content =
                    vec![json!({ "type": "text", "text": build_tool_result_text(&text_result, has_images, supports_images, result_message.is_error) })];
                if supports_images {
                    for part in &result_message.content {
                        if let ContentBlock::Image(image) = part {
                            tool_content.push(json!({
                                "type": "image_url",
                                "imageUrl": format!("data:{};base64,{}", image.mime_type, image.data),
                            }));
                        }
                    }
                }
                result.push(json!({
                    "role": "tool",
                    "toolCallId": result_message.tool_call_id,
                    "name": result_message.tool_name,
                    "content": tool_content,
                }));
            }
        }
    }

    result
}

fn build_tool_result_text(text: &str, has_images: bool, supports_images: bool, is_error: bool) -> String {
    let trimmed = text.trim();
    let error_prefix = if is_error { "[tool error] " } else { "" };

    if !trimmed.is_empty() {
        let image_suffix =
            if has_images && !supports_images { "\n[tool image omitted: model does not support images]" } else { "" };
        return format!("{error_prefix}{trimmed}{image_suffix}");
    }

    if has_images {
        if supports_images {
            return if is_error { "[tool error] (see attached image)".into() } else { "(see attached image)".into() };
        }
        return if is_error {
            "[tool error] (image omitted: model does not support images)".into()
        } else {
            "(image omitted: model does not support images)".into()
        };
    }

    if is_error {
        "[tool error] (no tool output)".into()
    } else {
        "(no tool output)".into()
    }
}

fn uses_reasoning_effort(model: &Model) -> bool {
    model.id == "mistral-small-2603"
        || model.id == "mistral-small-latest"
        || model.id.starts_with("mistral-medium-")
        || model.id == "zai-glm-5-2"
}

fn uses_prompt_mode_reasoning(model: &Model) -> bool {
    model.reasoning && !uses_reasoning_effort(model)
}

fn map_reasoning_effort(model: &Model, level: ModelThinkingLevel) -> String {
    model
        .thinking_level_map
        .as_ref()
        .and_then(|map| map.get(&level))
        .cloned()
        .flatten()
        .unwrap_or_else(|| "high".to_owned())
}

fn map_tool_choice(choice: &Value) -> Value {
    match choice {
        Value::String(text) => Value::String(text.clone()),
        other => other.clone(),
    }
}

fn map_chat_stop_reason(reason: Option<&str>) -> (StopReason, Option<String>) {
    let Some(reason) = reason else { return (StopReason::Stop, None) };
    match reason {
        "stop" => (StopReason::Stop, None),
        "length" | "model_length" => (StopReason::Length, None),
        "tool_calls" => (StopReason::ToolUse, None),
        "error" => (StopReason::Error, Some("Provider stopped with: error".to_owned())),
        other => (StopReason::Error, Some(format!("Provider stopped with: {other}"))),
    }
}

fn build_chat_payload(model: &Model, context: &Context, messages: &[Message], options: &StreamOptions) -> Value {
    let mut payload = Map::new();
    payload.insert("model".into(), Value::from(model.id.clone()));
    payload.insert("stream".into(), Value::from(true));
    payload.insert(
        "messages".into(),
        Value::Array(to_chat_messages(messages, model.input.contains(&crate::types::InputModality::Image))),
    );

    if let Some(tools) = &context.tools
        && !tools.is_empty()
        && let Ok(function_tools) = to_function_tools(tools)
    {
        payload.insert("tools".into(), Value::Array(function_tools));
    }
    if let Some(temperature) = options.temperature {
        payload.insert("temperature".into(), json!(temperature));
    }
    if let Some(max_tokens) = options.max_tokens {
        payload.insert("maxTokens".into(), json!(max_tokens));
    }
    if let Some(tool_choice) = option_value(options, "toolChoice") {
        payload.insert("toolChoice".into(), map_tool_choice(&tool_choice));
    }
    if let Some(prompt_mode) = option_value(options, "promptMode") {
        payload.insert("promptMode".into(), prompt_mode);
    }
    if let Some(reasoning_effort) = option_value(options, "reasoningEffort") {
        payload.insert("reasoningEffort".into(), reasoning_effort);
    }
    if should_use_prompt_caching(options)
        && let Some(session_id) = &options.session_id
    {
        payload.insert("promptCacheKey".into(), Value::from(session_id.clone()));
    }

    if let Some(system_prompt) = &context.system_prompt
        && let Some(Value::Array(messages)) = payload.get_mut("messages")
    {
        messages.insert(0, json!({ "role": "system", "content": sanitize_surrogates(system_prompt) }));
    }

    let mut payload = Value::Object(payload);
    if let Some(object) = payload.as_object_mut() {
        apply_extra_body(object, options.extra_body.as_ref(), &MISTRAL_RESERVED_BODY_KEYS);
    }
    payload
}

fn should_use_prompt_caching(options: &StreamOptions) -> bool {
    options.cache_retention != Some(crate::types::CacheRetention::None) && options.session_id.is_some()
}

fn get_mistral_cached_prompt_tokens(usage: &Value, prompt_tokens: u64) -> u64 {
    let raw = usage
        .get("promptTokensDetails")
        .and_then(|details| details.get("cachedTokens"))
        .or_else(|| usage.get("prompt_tokens_details").and_then(|details| details.get("cached_tokens")))
        .or_else(|| usage.get("promptTokenDetails").and_then(|details| details.get("cachedTokens")))
        .or_else(|| usage.get("prompt_token_details").and_then(|details| details.get("cached_tokens")))
        .or_else(|| usage.get("numCachedTokens"))
        .or_else(|| usage.get("num_cached_tokens"));
    let cached = raw.and_then(Value::as_f64).filter(|value| value.is_finite()).unwrap_or(0.0);
    prompt_tokens.min(cached.max(0.0) as u64)
}

struct StreamState {
    output: AssistantMessage,
    current_block: Option<usize>,
    tool_blocks_by_key: std::collections::HashMap<String, usize>,
    partial_args: std::collections::HashMap<usize, String>,
}

fn finish_current_block(state: &mut StreamState, sink: &AssistantMessageEventStream, index: Option<usize>) {
    let Some(index) = index else { return };
    let partial = state.output.clone();
    match state.output.content.get(index) {
        Some(ContentBlock::Text(text)) => {
            sink.push(AssistantMessageEvent::TextEnd {
                content_index: index,
                content: text.text.clone(),
                partial,
            });
        }
        Some(ContentBlock::Thinking(thinking)) => {
            sink.push(AssistantMessageEvent::ThinkingEnd {
                content_index: index,
                content: thinking.thinking.clone(),
                partial,
            });
        }
        _ => {}
    }
}

fn consume_chunk(state: &mut StreamState, model: &Model, chunk: &Value, sink: &AssistantMessageEventStream) {
    if state.output.response_id.is_none()
        && let Some(id) = chunk.get("id").and_then(Value::as_str)
        && !id.is_empty()
    {
        {
            {
                state.output.response_id = Some(id.to_owned());
            }
        }
    }

    if let Some(usage) = chunk.get("usage").filter(|usage| !usage.is_null()) {
        let prompt_tokens = usage.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0);
        let cached_prompt_tokens = get_mistral_cached_prompt_tokens(usage, prompt_tokens);

        state.output.usage.input = prompt_tokens.saturating_sub(cached_prompt_tokens);
        state.output.usage.output = usage.get("completion_tokens").and_then(Value::as_u64).unwrap_or(0);
        state.output.usage.cache_read = cached_prompt_tokens;
        state.output.usage.cache_write = 0;
        state.output.usage.total_tokens = usage.get("total_tokens").and_then(Value::as_u64).unwrap_or(
            state.output.usage.input
                + state.output.usage.output
                + state.output.usage.cache_read
                + state.output.usage.cache_write,
        );
        calculate_cost(model, &mut state.output.usage);
    }

    let Some(choice) = chunk.get("choices").and_then(Value::as_array).and_then(|choices| choices.first()) else {
        return;
    };

    if let Some(finish_reason) = choice.get("finish_reason")
        && !finish_reason.is_null()
    {
        {
            let reason = finish_reason.as_str();
            state.output.raw_stop_reason = reason.map(str::to_owned);
            let (stop_reason, error_message) = map_chat_stop_reason(reason);
            state.output.stop_reason = stop_reason;
            if let Some(error_message) = error_message {
                state.output.error_message = Some(error_message);
            }
        }
    }

    let Some(delta) = choice.get("delta") else { return };

    if let Some(content) = delta.get("content")
        && !content.is_null()
    {
        {
            let items: Vec<Value> = match content {
                Value::String(text) => vec![Value::String(text.clone())],
                Value::Array(items) => items.clone(),
                _ => Vec::new(),
            };
            for item in items {
                match &item {
                    Value::String(text) => {
                        push_text_delta(state, sink, &sanitize_surrogates(text));
                    }
                    Value::Object(object) => {
                        if object.get("type").and_then(Value::as_str) == Some("thinking") {
                            let delta_text: String = object
                                .get("thinking")
                                .and_then(Value::as_array)
                                .map(|parts| {
                                    parts
                                        .iter()
                                        .filter_map(|part| part.get("text").and_then(Value::as_str))
                                        .filter(|text| !text.is_empty())
                                        .collect::<Vec<_>>()
                                        .join("")
                                })
                                .unwrap_or_default();
                            let thinking_delta = sanitize_surrogates(&delta_text);
                            if thinking_delta.is_empty() {
                                continue;
                            }
                            push_thinking_delta(state, sink, &thinking_delta);
                        } else if object.get("type").and_then(Value::as_str) == Some("text") {
                            let text_delta =
                                sanitize_surrogates(object.get("text").and_then(Value::as_str).unwrap_or_default());
                            push_text_delta(state, sink, &text_delta);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    let tool_calls = delta.get("tool_calls").and_then(Value::as_array).cloned().unwrap_or_default();
    for tool_call in &tool_calls {
        if let Some(index) = state.current_block.take() {
            finish_current_block(state, sink, Some(index));
        }
        let id = tool_call.get("id").and_then(Value::as_str).filter(|id| *id != "null");
        let call_id = id.map(str::to_owned).unwrap_or_else(|| {
            let index = tool_call.get("index").and_then(Value::as_u64).unwrap_or(0);
            derive_mistral_tool_call_id(&format!("toolcall:{index}"), 0)
        });
        let key = tool_call
            .get("index")
            .map(|index| index.to_string())
            .unwrap_or_else(|| call_id.clone());

        let existing = state.tool_blocks_by_key.get(&key).copied();
        if existing.is_none() {
            let function = tool_call.get("function").cloned().unwrap_or(Value::Null);
            state.output.content.push(ContentBlock::ToolCall(crate::types::ToolCall {
                id: call_id,
                name: function.get("name").and_then(Value::as_str).unwrap_or_default().to_owned(),
                arguments: Map::new(),
                incomplete: None,
                error_message: None,
                thought_signature: None,
                namespace: None,
            }));
            let index = state.output.content.len() - 1;
            state.tool_blocks_by_key.insert(key.clone(), index);
            state.partial_args.insert(index, String::new());
            sink.push(AssistantMessageEvent::ToolcallStart { content_index: index, partial: state.output.clone() });
        }

        let index = *state.tool_blocks_by_key.get(&key).expect("tool block index");
        let args_delta = match tool_call.get("function").and_then(|function| function.get("arguments")) {
            Some(Value::String(text)) => text.clone(),
            Some(other) => other.to_string(),
            None => "{}".to_owned(),
        };
        let accumulated = state.partial_args.entry(index).or_default();
        accumulated.push_str(&args_delta);
        let parsed = parse_streaming_json(Some(accumulated));
        if let Some(ContentBlock::ToolCall(call)) = state.output.content.get_mut(index) {
            call.arguments = parsed.as_object().cloned().unwrap_or_default();
        }
        sink.push(AssistantMessageEvent::ToolcallDelta {
            content_index: index,
            delta: args_delta,
            partial: state.output.clone(),
        });
    }
}

fn push_text_delta(state: &mut StreamState, sink: &AssistantMessageEventStream, delta: &str) {
    let is_text = matches!(state.current_block.and_then(|index| state.output.content.get(index)), Some(ContentBlock::Text(_)));
    if !is_text {
        let previous = state.current_block.take();
        finish_current_block(state, sink, previous);
        state.output.content.push(ContentBlock::text(""));
        state.current_block = Some(state.output.content.len() - 1);
        sink.push(AssistantMessageEvent::TextStart {
            content_index: state.output.content.len() - 1,
            partial: state.output.clone(),
        });
    }
    let index = state.current_block.expect("text block");
    if let Some(ContentBlock::Text(text)) = state.output.content.get_mut(index) {
        text.text.push_str(delta);
    }
    sink.push(AssistantMessageEvent::TextDelta {
        content_index: index,
        delta: delta.to_owned(),
        partial: state.output.clone(),
    });
}

fn push_thinking_delta(state: &mut StreamState, sink: &AssistantMessageEventStream, delta: &str) {
    let is_thinking =
        matches!(state.current_block.and_then(|index| state.output.content.get(index)), Some(ContentBlock::Thinking(_)));
    if !is_thinking {
        let previous = state.current_block.take();
        finish_current_block(state, sink, previous);
        state.output.content.push(ContentBlock::Thinking(crate::types::ThinkingContent {
            thinking: String::new(),
            ..crate::types::ThinkingContent::default()
        }));
        state.current_block = Some(state.output.content.len() - 1);
        sink.push(AssistantMessageEvent::ThinkingStart {
            content_index: state.output.content.len() - 1,
            partial: state.output.clone(),
        });
    }
    let index = state.current_block.expect("thinking block");
    if let Some(ContentBlock::Thinking(thinking)) = state.output.content.get_mut(index) {
        thinking.thinking.push_str(delta);
    }
    sink.push(AssistantMessageEvent::ThinkingDelta {
        content_index: index,
        delta: delta.to_owned(),
        partial: state.output.clone(),
    });
}

pub fn stream(model: &Model, context: &Context, options: Option<StreamOptions>) -> AssistantMessageEventStream {
    let stream = create_assistant_message_event_stream();
    let model = model.clone();
    let context = context.clone();
    let options = options.unwrap_or_default();
    let sink = stream.clone();
    tokio::spawn(async move {
        run_stream(model, context, options, sink).await;
    });
    stream
}

async fn run_stream(model: Model, context: Context, options: StreamOptions, sink: AssistantMessageEventStream) {
    let mut output = create_output(&model);

    let outcome = drive(&model, &context, &options, &mut output, &sink).await;
    if let Err(error) = outcome {
        output.stop_reason = if options.request.signal.as_ref().is_some_and(|signal| signal.aborted()) {
            StopReason::Aborted
        } else {
            StopReason::Error
        };
        output.error_message = Some(format_mistral_error(&error));
        sink.push(AssistantMessageEvent::Error {
            reason: if output.stop_reason == StopReason::Aborted { ErrorReason::Aborted } else { ErrorReason::Error },
            error: output,
        });
    }
}

async fn drive(
    model: &Model,
    context: &Context,
    options: &StreamOptions,
    output: &mut AssistantMessage,
    sink: &AssistantMessageEventStream,
) -> Result<(), MistralError> {
    let Some(api_key) = options.request.api_key.clone().filter(|key| !key.is_empty()) else {
        return Err(MistralError::Message(format!("No API key for provider: {}", model.provider)));
    };

    let preserve_thinking =
        option_value(options, "promptMode").is_some() || option_value(options, "reasoningEffort").is_some();
    let transformed_messages = {
        let normalizer = std::cell::RefCell::new(MistralToolCallIdNormalizer::new());
        let normalize = |id: &str, _model: &Model, _assistant: &AssistantMessage| -> String {
            normalizer.borrow_mut().normalize(id)
        };
        transform_messages(
            &context.messages,
            model,
            Some(&normalize),
            &TransformMessagesOptions {
                preserve_thinking: Some(preserve_thinking),
                ..TransformMessagesOptions::default()
            },
        )
    };

    let mut payload = build_chat_payload(model, context, &transformed_messages, options);
    if let Some(on_payload) = &options.request.on_payload
        && let Some(next) = on_payload(&payload, model, None)
    {
        payload = next;
    }

    let base_url = model.base_url.trim_end_matches('/');
    let url = format!("{base_url}/v1/chat/completions");
    let headers = build_mistral_headers(model, &api_key, options);
    let client = options.request.fetch.clone().unwrap_or_default();
    let body = to_mistral_wire_payload(&payload).to_string();

    let timeout_ms = options.request.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
    let request = client.post(&url).headers(headers).body(body);
    let response = tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), request.send())
        .await
        .map_err(|_| MistralError::Message("Request was aborted".into()))?
        .map_err(|error| MistralError::Message(error.to_string()))?;

    if let Some(on_response) = &options.request.on_response {
        on_response(
            &crate::types::ProviderResponse {
                status: response.status().as_u16(),
                headers: headers_to_record(response.headers()),
            },
            model,
        );
    }

    if !response.status().is_success() {
        let status_code = response.status().as_u16();
        let message = response.status().canonical_reason().unwrap_or_default().to_owned();
        let body = response.text().await.map_err(|error| MistralError::Message(error.to_string()))?;
        return Err(MistralError::Http(MistralHttpError { status_code, body, message }));
    }

    sink.push(AssistantMessageEvent::Start { partial: output.clone() });

    let mut state = StreamState {
        output: output.clone(),
        current_block: None,
        tool_blocks_by_key: std::collections::HashMap::new(),
        partial_args: std::collections::HashMap::new(),
    };

    let mut body = response.bytes_stream();
    use futures::StreamExt;
    let mut buffer = String::new();
    let mut saw_done = false;
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|error| MistralError::Message(error.to_string()))?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        while let Some((index, length)) = find_mistral_event_boundary(&buffer) {
            let raw: String = buffer[..index].to_owned();
            buffer = buffer[index + length..].to_owned();
            match parse_mistral_event(&raw)? {
                MistralEvent::Done => {
                    saw_done = true;
                    break;
                }
                MistralEvent::Chunk(chunk) => consume_chunk(&mut state, model, &chunk, sink),
                MistralEvent::Empty => {}
            }
        }
        if saw_done {
            break;
        }
    }

    if !saw_done
        && !buffer.trim().is_empty()
        && let MistralEvent::Chunk(chunk) = parse_mistral_event(&buffer)?
    {
        consume_chunk(&mut state, model, &chunk, sink);
    }

    if let Some(index) = state.current_block.take() {
        finish_current_block(&mut state, sink, Some(index));
    }
    let tool_indices: Vec<usize> = state.tool_blocks_by_key.values().copied().collect();
    for index in tool_indices {
        let Some(ContentBlock::ToolCall(call)) = state.output.content.get(index).cloned() else { continue };
        state.partial_args.remove(&index);
        sink.push(AssistantMessageEvent::ToolcallEnd {
            content_index: index,
            tool_call: call,
            partial: state.output.clone(),
        });
    }

    *output = state.output.clone();

    if options.request.signal.as_ref().is_some_and(|signal| signal.aborted()) {
        return Err(MistralError::Message("Request was aborted".into()));
    }
    if output.stop_reason == StopReason::Pending {
        return Err(MistralError::Message("Mistral stream ended without a finish reason".into()));
    }
    if output.stop_reason == StopReason::Aborted || output.stop_reason == StopReason::Error {
        return Err(MistralError::Message(
            output.error_message.clone().unwrap_or_else(|| "An unknown error occurred".to_owned()),
        ));
    }

    sink.push(AssistantMessageEvent::Done {
        reason: match output.stop_reason {
            StopReason::Length => DoneReason::Length,
            StopReason::ToolUse => DoneReason::ToolUse,
            StopReason::Deferred => DoneReason::Deferred,
            _ => DoneReason::Stop,
        },
        message: output.clone(),
    });
    Ok(())
}

enum MistralEvent {
    Chunk(Value),
    Done,
    Empty,
}

fn find_mistral_event_boundary(buffer: &str) -> Option<(usize, usize)> {
    let pattern = "\r\n\r\n|\r\n\r|\r\n\n|\r\r\n|\n\r\n|\r\r|\n\r|\n\n";
    let regex = fancy_regex::Regex::new(pattern).ok()?;
    let found = regex.find(buffer).ok()??;
    Some((found.start(), found.end() - found.start()))
}

fn parse_mistral_event(raw: &str) -> Result<MistralEvent, MistralError> {
    let data = raw
        .split("\r\n")
        .flat_map(|line| line.split('\n'))
        .flat_map(|line| line.split('\r'))
        .filter(|line| line.starts_with("data:"))
        .map(|line| line[5..].trim_start())
        .collect::<Vec<_>>()
        .join("\n");
    let data = data.trim();
    if data.is_empty() {
        return Ok(MistralEvent::Empty);
    }
    if data == "[DONE]" {
        return Ok(MistralEvent::Done);
    }

    let parsed: Value =
        serde_json::from_str(data).map_err(|error| MistralError::Message(error.to_string()))?;
    if !parsed.is_object() || !parsed.get("choices").is_some_and(Value::is_array) {
        return Err(MistralError::Message("Invalid Mistral streaming event".into()));
    }
    Ok(MistralEvent::Chunk(parsed))
}

pub fn stream_simple(
    model: &Model,
    context: &Context,
    options: Option<SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    let simple = options.unwrap_or_default();
    let Some(api_key) = simple.stream.request.api_key.clone().filter(|key| !key.is_empty()) else {
        let stream = create_assistant_message_event_stream();
        let message = crate::utils::lazy::setup_error_message(model, &format!("No API key for provider: {}", model.provider));
        stream.push(AssistantMessageEvent::Error { reason: ErrorReason::Error, error: message.clone() });
        stream.end(Some(message));
        return stream;
    };

    let mut stream_options = match build_base_options(model, context, Some(&simple), Some(&api_key)) {
        Ok(stream_options) => stream_options,
        Err(error) => {
            let stream = create_assistant_message_event_stream();
            let message = crate::utils::lazy::setup_error_message(model, &error.to_string());
            stream.push(AssistantMessageEvent::Error { reason: ErrorReason::Error, error: message.clone() });
            stream.end(Some(message));
            return stream;
        }
    };
    if let Some(tool_choice) = simple.tool_choice {
        stream_options.extra.insert("toolChoice".into(), serde_json::to_value(tool_choice).unwrap_or(Value::Null));
    }

    let clamped_reasoning = simple.reasoning.map(|reasoning| clamp_thinking_level(model, ModelThinkingLevel::from(reasoning)));
    let reasoning = clamped_reasoning.filter(|level| *level != ModelThinkingLevel::Off);
    let should_use_reasoning = model.reasoning && reasoning.is_some();

    if should_use_reasoning {
        if uses_prompt_mode_reasoning(model) {
            stream_options.extra.insert("promptMode".into(), Value::from("reasoning"));
        } else if uses_reasoning_effort(model) {
            let level = reasoning.unwrap_or(ModelThinkingLevel::High);
            stream_options
                .extra
                .insert("reasoningEffort".into(), Value::from(map_reasoning_effort(model, level)));
        }
    }

    stream(model, context, Some(stream_options))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn any_model() -> Model {
        crate::models_generated::get_builtin_provider_models("anthropic").expect("models")[0].clone()
    }

    fn model() -> Model {
        let mut model = any_model();
        model.provider = "mistral".into();
        model.api = "mistral-conversations".into();
        model.base_url = "http://127.0.0.1:1".into();
        model
    }

    #[test]
    fn derives_mistral_tool_call_ids_from_short_hash() {
        let id = derive_mistral_tool_call_id("call_abc", 0);
        assert_eq!(id.len(), MISTRAL_TOOL_CALL_ID_LENGTH);
        assert!(id.chars().all(|character| character.is_ascii_alphanumeric()));
        assert_eq!(derive_mistral_tool_call_id("abcdefghi", 0), "abcdefghi");
        assert_ne!(derive_mistral_tool_call_id("call_abc", 0), derive_mistral_tool_call_id("call_abc", 1));
    }

    #[test]
    fn maps_chat_stop_reasons_like_the_ts_switch() {
        assert_eq!(map_chat_stop_reason(None), (StopReason::Stop, None));
        assert_eq!(map_chat_stop_reason(Some("model_length")), (StopReason::Length, None));
        assert_eq!(map_chat_stop_reason(Some("tool_calls")), (StopReason::ToolUse, None));
        assert_eq!(
            map_chat_stop_reason(Some("content_filter")),
            (StopReason::Error, Some("Provider stopped with: content_filter".into()))
        );
    }

    #[test]
    fn wire_payload_renames_camel_case_keys_and_nested_content() {
        let payload = json!({
            "model": "m",
            "stream": true,
            "maxTokens": 5,
            "messages": [
                { "role": "tool", "toolCallId": "c", "content": [{ "type": "image_url", "imageUrl": "data:x" }] }
            ],
        });
        let wire = to_mistral_wire_payload(&payload);
        assert_eq!(wire.get("max_tokens"), Some(&json!(5)));
        assert!(wire.get("maxTokens").is_none());
        let message = &wire["messages"][0];
        assert_eq!(message.get("tool_call_id"), Some(&json!("c")));
        assert_eq!(message["content"][0].get("image_url"), Some(&json!("data:x")));
    }

    #[test]
    fn chat_messages_flatten_tool_results_and_keep_image_data_urls() {
        let mut content = vec![ContentBlock::text("out")];
        content.push(ContentBlock::Image(crate::types::ImageContent {
            data: "AA==".into(),
            mime_type: "image/png".into(),
        }));
        let messages = vec![Message::ToolResult(crate::types::ToolResultMessage {
            tool_call_id: "c".into(),
            tool_name: "t".into(),
            content,
            details: None,
            usage: None,
            added_tool_names: None,
            is_error: true,
            timestamp: 0,
        })];
        let mapped = to_chat_messages(&messages, true);
        assert_eq!(mapped[0]["role"], json!("tool"));
        assert_eq!(mapped[0]["content"][0]["text"], json!("[tool error] out"));
        assert_eq!(mapped[0]["content"][1]["imageUrl"], json!("data:image/png;base64,AA=="));
    }

    #[test]
    fn tool_result_text_matches_the_ts_branches() {
        assert_eq!(build_tool_result_text("", false, false, false), "(no tool output)");
        assert_eq!(build_tool_result_text("", true, false, false), "(image omitted: model does not support images)");
        assert_eq!(build_tool_result_text("", true, true, true), "[tool error] (see attached image)");
        assert_eq!(build_tool_result_text("x", true, false, false), "x\n[tool image omitted: model does not support images]");
    }

    #[test]
    fn event_boundaries_follow_the_ts_regex_order() {
        assert_eq!(find_mistral_event_boundary("data: {}\n\nrest"), Some((8, 2)));
        assert_eq!(find_mistral_event_boundary("data: {}\r\n\r\nrest"), Some((8, 4)));
        assert_eq!(find_mistral_event_boundary("no boundary here"), None);
    }

    #[test]
    fn parses_data_lines_and_rejects_malformed_chunks() {
        assert!(matches!(parse_mistral_event("data: [DONE]"), Ok(MistralEvent::Done)));
        assert!(matches!(parse_mistral_event("data:\n"), Ok(MistralEvent::Empty)));
        assert!(matches!(parse_mistral_event("data: {\"choices\":[]}"), Ok(MistralEvent::Chunk(_))));
        assert!(parse_mistral_event("data: {\"nope\":1}").is_err());
    }

    #[test]
    fn streaming_text_and_thinking_deltas_emit_start_and_end_events() {
        let stream = create_assistant_message_event_stream();
        let mut state = StreamState {
            output: create_output(&model()),
            current_block: None,
            tool_blocks_by_key: std::collections::HashMap::new(),
            partial_args: std::collections::HashMap::new(),
        };
        let chunk = json!({
            "id": "r1",
            "choices": [{ "delta": { "content": [
                { "type": "thinking", "thinking": [{ "text": "hmm" }] },
                { "type": "text", "text": "hi" }
            ] } }]
        });
        consume_chunk(&mut state, &model(), &chunk, &stream);
        let events = stream.queue();
        assert!(matches!(events[0], AssistantMessageEvent::ThinkingStart { .. }));
        assert!(matches!(events[1], AssistantMessageEvent::ThinkingDelta { .. }));
        assert!(matches!(events[2], AssistantMessageEvent::ThinkingEnd { .. }));
        assert!(matches!(events[3], AssistantMessageEvent::TextStart { .. }));
        assert!(matches!(events[4], AssistantMessageEvent::TextDelta { .. }));
        assert_eq!(state.output.response_id.as_deref(), Some("r1"));
        assert_eq!(state.output.content.len(), 2);
    }

    #[test]
    fn tool_call_deltas_accumulate_arguments_and_finish() {
        let stream = create_assistant_message_event_stream();
        let mut state = StreamState {
            output: create_output(&model()),
            current_block: None,
            tool_blocks_by_key: std::collections::HashMap::new(),
            partial_args: std::collections::HashMap::new(),
        };
        let chunk = json!({
            "choices": [{ "delta": { "tool_calls": [
                { "id": "call", "index": 0, "function": { "name": "t", "arguments": "{\"a\":" } }
            ] } }]
        });
        consume_chunk(&mut state, &model(), &chunk, &stream);
        let chunk = json!({
            "choices": [{ "delta": { "tool_calls": [
                { "index": 0, "function": { "name": "t", "arguments": "1}" } }
            ] } }]
        });
        consume_chunk(&mut state, &model(), &chunk, &stream);
        let ContentBlock::ToolCall(call) = &state.output.content[0] else { panic!("tool call") };
        assert_eq!(call.arguments.get("a"), Some(&json!(1)));
        assert_eq!(state.partial_args.get(&0).map(String::as_str), Some("{\"a\":1}"));
    }

    #[test]
    fn cached_prompt_tokens_read_every_supported_spelling() {
        assert_eq!(get_mistral_cached_prompt_tokens(&json!({ "prompt_tokens_details": { "cached_tokens": 7 } }), 10), 7);
        assert_eq!(get_mistral_cached_prompt_tokens(&json!({ "num_cached_tokens": 4 }), 10), 4);
        assert_eq!(get_mistral_cached_prompt_tokens(&json!({ "num_cached_tokens": 40 }), 10), 10);
        assert_eq!(get_mistral_cached_prompt_tokens(&json!({}), 10), 0);
    }

    #[test]
    fn reasoning_effort_uses_the_model_map_or_high() {
        let mut model = model();
        assert_eq!(map_reasoning_effort(&model, ModelThinkingLevel::Low), "high");
        model.thinking_level_map =
            Some([(ModelThinkingLevel::Low, Some("none".to_owned()))].into_iter().collect());
        assert_eq!(map_reasoning_effort(&model, ModelThinkingLevel::Low), "none");
    }
}
