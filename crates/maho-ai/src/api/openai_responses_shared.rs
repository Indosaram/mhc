//! Port of senpi packages/ai/src/api/openai-responses-shared.ts.
//!
//! Shared conversion and stream processing for the OpenAI Responses wire APIs: message/tool
//! conversion into the Responses input shape, and the state machine that turns `ResponseStreamEvent`s
//! into `AssistantMessageEvent`s.
//!
//! Two deviations from the TS source, neither of them wire-visible: `sealContextProvenance` attaches
//! a non-enumerable provenance object in senpi, which a `serde_json` value cannot carry (and which is
//! never serialized to the provider payload), so the option is accepted and has no effect; and the
//! streaming scratch buffers (`partialJson`, `customInput`) live in this module's slot table rather
//! than on the content block, so a persisted block is identical without senpi's delete pass.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::pin::Pin;
use std::time::Duration;

use bytes::Bytes;
use futures::{Stream, StreamExt};
use indexmap::IndexMap;
use serde_json::{json, Map, Value};

use crate::api::constrained_sampling::{
    append_grammar_tool_input_json_delta, get_grammar_tool_input, get_json_schema_tool_parameters,
    resolve_grammar_constrained_sampling, resolve_json_schema_strict_sampling, GrammarToolInputJsonBuffer,
};
use crate::api::responses_completion_grace::format_responses_completion_stall;
use crate::api::transform_messages::{transform_messages, TransformMessagesOptions};
use crate::models::calculate_cost;
use crate::types::{
    AssistantMessage, AssistantMessageEvent, ContentBlock, Context, DoneReason, GrammarFormat, InputModality,
    Message, Model, ProviderNativeContent, StopReason, TextContent, TextPhase, TextSignatureV1, ThinkingContent,
    Tool, ToolCall, Usage, UserContent,
};
use crate::utils::event_stream::AssistantMessageEventStream;
use crate::utils::hash::short_hash;
use crate::utils::json_parse::parse_streaming_json;
use crate::utils::sanitize_unicode::sanitize_surrogates;

pub use crate::api::responses_completion_grace::RESPONSES_COMPLETION_GRACE_MS;

/// `CUSTOM_TOOL_CALL_ITEM_ID_SENTINEL`.
pub const CUSTOM_TOOL_CALL_ITEM_ID_SENTINEL: &str = "custom";

const MAX_NATIVE_IMAGE_BASE64_CHARS: usize = 24 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponsesDeferredToolsMode {
    AdditionalTools,
    ToolSearch,
}

/// `ConvertResponsesToolsOptions`.
#[derive(Debug, Clone, Default)]
pub struct ConvertResponsesToolsOptions {
    pub strict: Option<bool>,
    pub supports_strict_mode: Option<bool>,
    pub supports_openai_grammar_tools: Option<bool>,
    pub defer_loading: Option<bool>,
}

/// `ConvertResponsesMessagesOptions`.
#[derive(Debug, Clone, Default)]
pub struct ConvertResponsesMessagesOptions {
    pub include_system_prompt: Option<bool>,
    pub preserve_thinking: Option<bool>,
    pub preserve_text_signatures: Option<bool>,
    pub grammar_tool_input_properties: BTreeMap<String, String>,
    pub deferred_tools: IndexMap<String, Tool>,
    pub deferred_tools_mode: Option<ResponsesDeferredToolsMode>,
    pub tool_options: ConvertResponsesToolsOptions,
    pub seal_context_provenance: Option<bool>,
}

/// `OpenAIResponsesStreamOptions`.
pub type ApplyServiceTierPricing<'a> = &'a (dyn Fn(&mut Usage, Option<&str>) + Send + Sync);

pub struct ResponsesStreamOptions<'a> {
    pub service_tier: Option<&'a str>,
    pub grammar_tool_input_properties: &'a BTreeMap<String, String>,
    pub apply_service_tier_pricing: Option<ApplyServiceTierPricing<'a>>,
}

impl Default for ResponsesStreamOptions<'_> {
    fn default() -> Self {
        static EMPTY: std::sync::LazyLock<BTreeMap<String, String>> = std::sync::LazyLock::new(BTreeMap::new);
        Self { service_tier: None, grammar_tool_input_properties: &EMPTY, apply_service_tier_pricing: None }
    }
}

/// A thrown `Error` from the shared processor; the caller formats it with the api's error prefix.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ResponsesStreamError {
    pub message: String,
}

impl ResponsesStreamError {
    fn new(message: impl Into<String>) -> Self {
        Self { message: message.into() }
    }
}

/// `encodeTextSignatureV1`.
fn encode_text_signature_v1(id: Option<&str>, phase: Option<TextPhase>) -> String {
    match id {
        Some(id) => {
            let payload = TextSignatureV1 { v: 1, id: id.to_owned(), phase };
            serde_json::to_string(&payload).unwrap_or_else(|_| String::from("{\"v\":1}"))
        }
        None => String::from("{\"v\":1}"),
    }
}

fn text_phase(value: Option<&str>) -> Option<TextPhase> {
    match value {
        Some("commentary") => Some(TextPhase::Commentary),
        Some("final_answer") => Some(TextPhase::FinalAnswer),
        _ => None,
    }
}

/// `parseTextSignature`.
fn parse_text_signature(signature: Option<&str>) -> Option<(String, Option<TextPhase>)> {
    let signature = signature.filter(|signature| !signature.is_empty())?;
    if signature.starts_with('{')
        && let Ok(Value::Object(parsed)) = serde_json::from_str::<Value>(signature)
        && parsed.get("v").and_then(Value::as_u64) == Some(1)
        && let Some(id) = parsed.get("id").and_then(Value::as_str)
    {
        return Some((id.to_owned(), text_phase(parsed.get("phase").and_then(Value::as_str))));
    }
    Some((signature.to_owned(), None))
}

/// `parseReasoningSignature`.
fn parse_reasoning_signature(signature: Option<&str>) -> Option<Value> {
    let signature = signature.filter(|signature| !signature.is_empty())?;
    let parsed: Value = serde_json::from_str(signature).ok()?;
    match parsed.get("type").and_then(Value::as_str) {
        Some("reasoning") => Some(parsed),
        _ => None,
    }
}

/// `convertToolResultOutput`.
fn convert_tool_result_output(model: &Model, content: &[ContentBlock]) -> Value {
    let text_result = content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let images: Vec<&crate::types::ImageContent> = content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Image(image) => Some(image),
            _ => None,
        })
        .collect();
    let has_text = !text_result.is_empty();

    if images.is_empty() || !model.input.contains(&InputModality::Image) {
        let text = if has_text {
            text_result
        } else if images.is_empty() {
            "(no tool output)".to_owned()
        } else {
            "(see attached image)".to_owned()
        };
        return Value::String(sanitize_surrogates(&text));
    }

    let mut output: Vec<Value> = Vec::new();
    if has_text {
        output.push(json!({ "type": "input_text", "text": sanitize_surrogates(&text_result) }));
    }
    for image in images {
        output.push(json!({
            "type": "input_image",
            "detail": "auto",
            "image_url": format!("data:{};base64,{}", image.mime_type, image.data),
        }));
    }
    Value::Array(output)
}

fn is_freeform_tool(tool: &Tool) -> bool {
    tool.freeform.is_some()
}

fn is_freeform_tool_name(tool_name: &str, tools: Option<&Vec<Tool>>) -> bool {
    tools.is_some_and(|tools| tools.iter().any(|tool| tool.name == tool_name && is_freeform_tool(tool)))
}

fn get_freeform_tool_input(arguments: &Map<String, Value>) -> String {
    match arguments.get("input") {
        Some(Value::String(input)) => input.clone(),
        _ => serde_json::to_string(arguments).unwrap_or_else(|_| String::from("{}")),
    }
}

/// JS `String.prototype.split("|")` destructured into its first two parts.
fn split_call_id(id: &str) -> (String, Option<String>) {
    let mut parts = id.split('|');
    let call_id = parts.next().unwrap_or_default().to_owned();
    (call_id, parts.next().map(str::to_owned))
}

/// `normalizeIdPart`.
fn normalize_id_part(part: &str) -> String {
    let sanitized: String = part
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' || character == '-' { character } else { '_' }
        })
        .collect();
    let normalized: String =
        if sanitized.chars().count() > 64 { sanitized.chars().take(64).collect() } else { sanitized };
    normalized.trim_end_matches('_').to_owned()
}

/// `buildForeignResponsesItemId`.
fn build_foreign_responses_item_id(item_id: &str) -> String {
    let normalized = format!("fc_{}", short_hash(item_id));
    if normalized.chars().count() > 64 { normalized.chars().take(64).collect() } else { normalized }
}

/// `convertResponsesMessages`.
pub fn convert_responses_messages(
    model: &Model,
    context: &Context,
    allowed_tool_call_providers: &BTreeSet<String>,
    options: &ConvertResponsesMessagesOptions,
) -> Vec<Value> {
    let mut messages: Vec<Value> = Vec::new();
    let mut loaded_tool_names: BTreeSet<String> = BTreeSet::new();

    let normalize_tool_call_id = |id: &str, _target: &Model, source: &AssistantMessage| -> String {
        if !id.contains('|') {
            return normalize_id_part(id);
        }
        let (call_id, item_id) = split_call_id(id);
        let Some(item_id) = item_id else { return normalize_id_part(id) };
        let normalized_call_id = normalize_id_part(&call_id);
        if item_id == CUSTOM_TOOL_CALL_ITEM_ID_SENTINEL {
            return format!("{normalized_call_id}|{CUSTOM_TOOL_CALL_ITEM_ID_SENTINEL}");
        }
        if !allowed_tool_call_providers.contains(&model.provider) {
            return normalize_id_part(id);
        }
        let is_foreign_tool_call = source.provider != model.provider || source.api != model.api;
        let mut normalized_item_id =
            if is_foreign_tool_call { build_foreign_responses_item_id(&item_id) } else { normalize_id_part(&item_id) };
        if !normalized_item_id.starts_with("fc_") {
            normalized_item_id = normalize_id_part(&format!("fc_{normalized_item_id}"));
        }
        format!("{normalized_call_id}|{normalized_item_id}")
    };

    let transformed_messages = transform_messages(
        &context.messages,
        model,
        Some(&normalize_tool_call_id),
        &TransformMessagesOptions {
            preserve_thinking: options.preserve_thinking,
            preserve_text_signatures: options.preserve_text_signatures,
            ..TransformMessagesOptions::default()
        },
    );

    let include_system_prompt = options.include_system_prompt.unwrap_or(true);
    if include_system_prompt
        && let Some(system_prompt) = context.system_prompt.as_ref()
    {
        let compat = model.compat.as_ref().map(|compat| compat.openai_responses());
        let supports_developer_role =
            compat.as_ref().and_then(|compat| compat.supports_developer_role).unwrap_or(true);
        let role = if model.reasoning && supports_developer_role { "developer" } else { "system" };
        messages.push(json!({ "role": role, "content": sanitize_surrogates(system_prompt) }));
    }

    let mut msg_index: usize = 0;
    for msg in &transformed_messages {
        match msg {
            Message::ConfigurationUpdate(update) => {
                if model.id != "gpt-6-astra"
                    || !(model.provider == "openai" || model.provider == "chatgpt-subscription")
                {
                    continue;
                }
                let next = json!({ "type": "configuration_update", "reasoning": { "effort": update.effort } });
                match messages.last() {
                    Some(Value::Object(previous))
                        if previous.get("type").and_then(Value::as_str) == Some("configuration_update") =>
                    {
                        *messages.last_mut().expect("just checked") = next;
                    }
                    _ => messages.push(next),
                }
            }
            Message::User(user) => match &user.content {
                UserContent::Text(text) => {
                    messages.push(json!({
                        "role": "user",
                        "content": [{ "type": "input_text", "text": sanitize_surrogates(text) }],
                    }));
                }
                UserContent::Blocks(blocks) => {
                    let content: Vec<Value> = blocks
                        .iter()
                        .map(|item| match item {
                            ContentBlock::Text(text) => {
                                json!({ "type": "input_text", "text": sanitize_surrogates(&text.text) })
                            }
                            ContentBlock::Image(image) => json!({
                                "type": "input_image",
                                "detail": "auto",
                                "image_url": format!("data:{};base64,{}", image.mime_type, image.data),
                            }),
                            _ => json!({ "type": "input_image", "detail": "auto", "image_url": "data:;base64," }),
                        })
                        .collect();
                    if content.is_empty() {
                        continue;
                    }
                    messages.push(json!({ "role": "user", "content": content }));
                }
            },
            Message::Assistant(assistant) => {
                let mut output: Vec<Value> = Vec::new();
                let is_same_provider_and_api = assistant.provider == model.provider && assistant.api == model.api;
                let is_same_model = is_same_provider_and_api && assistant.model == model.id;
                let is_different_model = is_same_provider_and_api && assistant.model != model.id;
                let mut text_block_index: usize = 0;

                let mut push_assistant_text = |text: &str, text_signature: Option<&str>, output: &mut Vec<Value>| {
                    let parsed_signature = parse_text_signature(text_signature);
                    let fallback_message_id = if text_block_index == 0 {
                        format!("msg_pi_{msg_index}")
                    } else {
                        format!("msg_pi_{msg_index}_{text_block_index}")
                    };
                    text_block_index += 1;
                    let msg_id = match parsed_signature.as_ref().map(|(id, _)| id.clone()) {
                        None => fallback_message_id,
                        Some(id) if id.chars().count() > 64 => format!("msg_{}", short_hash(&id)),
                        Some(id) => id,
                    };
                    let mut item = Map::new();
                    item.insert("type".into(), json!("message"));
                    item.insert("role".into(), json!("assistant"));
                    item.insert(
                        "content".into(),
                        json!([{ "type": "output_text", "text": sanitize_surrogates(text), "annotations": [] }]),
                    );
                    item.insert("status".into(), json!("completed"));
                    item.insert("id".into(), json!(msg_id));
                    if let Some(phase) = parsed_signature.as_ref().and_then(|(_, phase)| *phase) {
                        item.insert(
                            "phase".into(),
                            json!(match phase {
                                TextPhase::Commentary => "commentary",
                                TextPhase::FinalAnswer => "final_answer",
                            }),
                        );
                    }
                    output.push(Value::Object(item));
                };

                for block in &assistant.content {
                    match block {
                        ContentBlock::Thinking(thinking) => {
                            if let Some(reasoning_item) =
                                parse_reasoning_signature(thinking.thinking_signature.as_deref())
                            {
                                output.push(reasoning_item);
                            } else if thinking.thinking_signature.is_some() && !thinking.thinking.trim().is_empty()
                            {
                                push_assistant_text(&thinking.thinking, None, &mut output);
                            }
                        }
                        ContentBlock::ProviderNative(_) | ContentBlock::Image(_) => {}
                        ContentBlock::Text(text) => {
                            push_assistant_text(&text.text, text.text_signature.as_deref(), &mut output);
                        }
                        ContentBlock::ToolCall(tool_call) => {
                            let (call_id, item_id_raw) = split_call_id(&tool_call.id);
                            let custom_input_property =
                                options.grammar_tool_input_properties.get(&tool_call.name).cloned();
                            let is_persisted_freeform =
                                item_id_raw.as_deref() == Some(CUSTOM_TOOL_CALL_ITEM_ID_SENTINEL);
                            let is_freeform = is_freeform_tool_name(&tool_call.name, context.tools.as_ref())
                                || is_persisted_freeform;
                            let mut item_id: Option<String> =
                                if is_persisted_freeform { None } else { item_id_raw };

                            let item_id_starts_with_fc = item_id.as_deref().is_some_and(|id| id.starts_with("fc_"));
                            if (is_different_model && item_id_starts_with_fc)
                                || (!is_freeform && custom_input_property.is_none() && !item_id_starts_with_fc)
                            {
                                item_id = None;
                            }

                            let can_replay_namespace =
                                is_same_model || options.deferred_tools.contains_key(&tool_call.name);
                            let namespace = if can_replay_namespace { tool_call.namespace.clone() } else { None };

                            if let Some(property) = custom_input_property {
                                let input =
                                    get_grammar_tool_input(&tool_call.name, &tool_call.arguments, &property)
                                        .map(|input| sanitize_surrogates(&input))
                                        .unwrap_or_default();
                                let mut item = Map::new();
                                item.insert("type".into(), json!("custom_tool_call"));
                                if let Some(id) = item_id.as_ref() {
                                    item.insert("id".into(), json!(id));
                                }
                                item.insert("call_id".into(), json!(call_id));
                                item.insert("name".into(), json!(tool_call.name));
                                item.insert("input".into(), json!(input));
                                if let Some(namespace) = namespace.as_ref() {
                                    item.insert("namespace".into(), json!(namespace));
                                }
                                output.push(Value::Object(item));
                            } else if is_freeform {
                                let mut item = Map::new();
                                item.insert("type".into(), json!("custom_tool_call"));
                                item.insert("call_id".into(), json!(call_id));
                                item.insert("name".into(), json!(tool_call.name));
                                item.insert("input".into(), json!(get_freeform_tool_input(&tool_call.arguments)));
                                if let Some(namespace) = namespace.as_ref() {
                                    item.insert("namespace".into(), json!(namespace));
                                }
                                output.push(Value::Object(item));
                            } else {
                                let mut item = Map::new();
                                item.insert("type".into(), json!("function_call"));
                                if item_id.as_deref().is_some_and(|id| id.starts_with("fc_")) {
                                    item.insert("id".into(), json!(item_id));
                                }
                                item.insert("call_id".into(), json!(call_id));
                                item.insert("name".into(), json!(tool_call.name));
                                item.insert(
                                    "arguments".into(),
                                    json!(serde_json::to_string(&tool_call.arguments)
                                        .unwrap_or_else(|_| String::from("{}"))),
                                );
                                if let Some(namespace) = namespace.as_ref() {
                                    item.insert("namespace".into(), json!(namespace));
                                }
                                output.push(Value::Object(item));
                            }
                        }
                    }
                }
                if output.is_empty() {
                    continue;
                }
                messages.extend(output);
            }
            Message::ToolResult(result) => {
                let (call_id, item_id_raw) = split_call_id(&result.tool_call_id);
                let output = convert_tool_result_output(model, &result.content);
                let custom_input_property =
                    options.grammar_tool_input_properties.get(&result.tool_name).cloned();
                let is_persisted_freeform = item_id_raw.as_deref() == Some(CUSTOM_TOOL_CALL_ITEM_ID_SENTINEL);

                if custom_input_property.is_some() {
                    messages.push(json!({
                        "type": "custom_tool_call_output",
                        "call_id": call_id,
                        "output": output,
                    }));
                } else if is_freeform_tool_name(&result.tool_name, context.tools.as_ref()) || is_persisted_freeform
                {
                    messages.push(json!({
                        "type": "custom_tool_call_output",
                        "call_id": call_id,
                        "name": result.tool_name,
                        "output": output,
                    }));
                } else {
                    messages.push(json!({
                        "type": "function_call_output",
                        "call_id": call_id,
                        "output": output,
                    }));
                }

                let mut deferred_tools: Vec<Tool> = Vec::new();
                for name in result.added_tool_names.iter().flatten() {
                    let Some(tool) = options.deferred_tools.get(name) else { continue };
                    if loaded_tool_names.contains(name) {
                        continue;
                    }
                    loaded_tool_names.insert(name.clone());
                    deferred_tools.push(tool.clone());
                }
                if !deferred_tools.is_empty()
                    && options.deferred_tools_mode == Some(ResponsesDeferredToolsMode::AdditionalTools)
                {
                    let tools =
                        convert_responses_tools(&deferred_tools, &options.tool_options).unwrap_or_default();
                    messages.push(json!({
                        "type": "additional_tools",
                        "role": "developer",
                        "tools": tools,
                    }));
                } else if !deferred_tools.is_empty()
                    && options.deferred_tools_mode == Some(ResponsesDeferredToolsMode::ToolSearch)
                {
                    let names: Vec<String> = deferred_tools.iter().map(|tool| tool.name.clone()).collect();
                    let search_call_id = format!(
                        "pi_tool_load_{}",
                        short_hash(&format!("{}:{}", result.tool_call_id, names.join(",")))
                    );
                    messages.push(json!({
                        "type": "tool_search_call",
                        "call_id": search_call_id,
                        "execution": "client",
                        "status": "completed",
                        "arguments": { "query": names.join(" "), "limit": names.len() },
                    }));
                    let tool_options =
                        ConvertResponsesToolsOptions { defer_loading: Some(true), ..options.tool_options.clone() };
                    let tools = convert_responses_tools(&deferred_tools, &tool_options).unwrap_or_default();
                    messages.push(json!({
                        "type": "tool_search_output",
                        "call_id": search_call_id,
                        "execution": "client",
                        "status": "completed",
                        "tools": tools,
                    }));
                }
            }
        }
        msg_index += 1;
    }

    messages
}

/// `convertResponsesTools`.
pub fn convert_responses_tools(
    tools: &[Tool],
    options: &ConvertResponsesToolsOptions,
) -> Result<Vec<Value>, String> {
    let default_strict = options.strict.unwrap_or(false);
    let supports_strict_mode = options.supports_strict_mode.unwrap_or(true);
    let supports_openai_grammar_tools = options.supports_openai_grammar_tools.unwrap_or(false);
    let defer_loading = options.defer_loading.unwrap_or(false);

    let mut converted = Vec::with_capacity(tools.len());
    for tool in tools {
        if let Some(grammar) = resolve_grammar_constrained_sampling(tool, supports_openai_grammar_tools)? {
            let mut item = Map::new();
            item.insert("type".into(), json!("custom"));
            item.insert("name".into(), json!(tool.name));
            item.insert("description".into(), json!(tool.description));
            item.insert(
                "format".into(),
                json!({
                    "type": "grammar",
                    "syntax": match grammar.format {
                        GrammarFormat::OpenaiLark => "lark",
                        GrammarFormat::OpenaiRegex => "regex",
                    },
                    "definition": grammar.definition,
                }),
            );
            if defer_loading {
                item.insert("defer_loading".into(), json!(true));
            }
            converted.push(Value::Object(item));
            continue;
        }
        if let Some(freeform) = tool.freeform.as_ref() {
            let mut item = Map::new();
            item.insert("type".into(), json!("custom"));
            item.insert("name".into(), json!(tool.name));
            item.insert("description".into(), json!(tool.description));
            item.insert("format".into(), serde_json::to_value(freeform).unwrap_or(Value::Null));
            if defer_loading {
                item.insert("defer_loading".into(), json!(true));
            }
            converted.push(Value::Object(item));
            continue;
        }

        let constrained_strict = resolve_json_schema_strict_sampling(tool, supports_strict_mode)?;
        let strict = constrained_strict.unwrap_or(default_strict);
        let mut item = Map::new();
        item.insert("type".into(), json!("function"));
        item.insert("name".into(), json!(tool.name));
        item.insert("description".into(), json!(tool.description));
        item.insert(
            "parameters".into(),
            get_json_schema_tool_parameters(tool, Some(strict)).map_err(|error| error.0)?,
        );
        if defer_loading {
            item.insert("defer_loading".into(), json!(true));
        }
        if supports_strict_mode {
            item.insert("strict".into(), json!(strict));
        }
        converted.push(Value::Object(item));
    }
    Ok(converted)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlotKind {
    Thinking,
    Text,
    ToolCall,
    ProviderNative,
}

struct Slot {
    kind: SlotKind,
    content_index: usize,
    partial_json: Option<String>,
    custom_input: Option<CustomToolInput>,
}

struct CustomToolInput {
    property: String,
    json_buffer: GrammarToolInputJsonBuffer,
}

impl Slot {
    fn new(kind: SlotKind, content_index: usize) -> Self {
        Self { kind, content_index, partial_json: None, custom_input: None }
    }
}

struct ResponsesEvent {
    value: Value,
}

impl crate::api::responses_completion_grace::TypedEvent for ResponsesEvent {
    fn event_type(&self) -> &str {
        self.value.get("type").and_then(Value::as_str).unwrap_or_default()
    }
}

fn field_str<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn field_usize(value: &Value, key: &str) -> Option<usize> {
    value.get(key).and_then(Value::as_u64).map(|value| value as usize)
}

fn field_u64(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// JS truthiness of a string-valued JSON property.
fn js_truthy_str(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(text)) if !text.is_empty() => Some(text.clone()),
        _ => None,
    }
}

/// `${value}` for a JSON value, with JS's `undefined`/`null` rendering.
fn js_template_string(value: Option<&Value>) -> String {
    match value {
        None => String::from("undefined"),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Null) => String::from("null"),
        Some(other) => serde_json::to_string(other).unwrap_or_else(|_| String::from("undefined")),
    }
}

fn is_valid_base64(value: &str) -> bool {
    if value.is_empty() || !value.chars().count().is_multiple_of(4) {
        return false;
    }
    let mut padding = 0usize;
    let mut seen_padding = false;
    for character in value.chars() {
        match character {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '+' | '/' => {
                if seen_padding {
                    return false;
                }
            }
            '=' => {
                seen_padding = true;
                padding += 1;
                if padding > 2 {
                    return false;
                }
            }
            _ => return false,
        }
    }
    true
}

/// `readNativeImageGenerationCall`.
fn read_native_image_generation_call(value: &Value) -> Option<Value> {
    let object = value.as_object()?;
    let status = object.get("status").and_then(Value::as_str)?;
    if object.get("type").and_then(Value::as_str) != Some("image_generation_call") {
        return None;
    }
    let mut item = Map::new();
    item.insert("type".into(), json!("image_generation_call"));
    if let Some(id) = object.get("id").and_then(Value::as_str) {
        item.insert("id".into(), json!(id));
    }
    item.insert("status".into(), json!(status));
    match object.get("result") {
        Some(Value::String(result)) => {
            item.insert("result".into(), json!(result));
        }
        Some(Value::Null) => {
            item.insert("result".into(), Value::Null);
        }
        _ => {}
    }
    if let Some(revised_prompt) = object.get("revised_prompt").and_then(Value::as_str) {
        item.insert("revised_prompt".into(), json!(revised_prompt));
    }
    Some(Value::Object(item))
}

/// `reconcileNativeImageGenerationCall`.
fn reconcile_native_image_generation_call(item: &Value) -> Value {
    let status = field_str(item, "status").unwrap_or_default();
    let id = field_str(item, "id");
    let mut reconciled = Map::new();
    reconciled.insert("type".into(), json!("image_generation_call"));
    if let Some(id) = id {
        reconciled.insert("id".into(), json!(id));
    }
    if status != "completed" {
        reconciled.insert("status".into(), json!(status));
        return Value::Object(reconciled);
    }
    match field_str(item, "result") {
        Some(result) if is_valid_base64(result) => {
            reconciled.insert("status".into(), json!("completed"));
            reconciled.insert("result".into(), json!(result));
            if let Some(revised_prompt) = field_str(item, "revised_prompt")
                && !revised_prompt.trim().is_empty()
            {
                reconciled.insert("revised_prompt".into(), json!(revised_prompt));
            }
        }
        _ => {
            reconciled.insert("status".into(), json!("malformed"));
        }
    }
    Value::Object(reconciled)
}

struct MappedStop {
    stop_reason: StopReason,
    error_message: Option<String>,
}

/// `mapStopReason`.
fn map_stop_reason(status: Option<&str>, incomplete_reason: Option<&str>) -> Result<MappedStop, ResponsesStreamError> {
    let Some(status) = status else {
        return Ok(MappedStop { stop_reason: StopReason::Stop, error_message: None });
    };
    match status {
        "completed" => Ok(MappedStop { stop_reason: StopReason::Stop, error_message: None }),
        "incomplete" => {
            if incomplete_reason == Some("max_output_tokens") {
                return Ok(MappedStop { stop_reason: StopReason::Length, error_message: None });
            }
            let error_message = match incomplete_reason.filter(|reason| !reason.is_empty()) {
                Some(reason) => format!("Response incomplete: {reason}"),
                None => String::from("Response incomplete without a provider reason"),
            };
            Ok(MappedStop { stop_reason: StopReason::Error, error_message: Some(error_message) })
        }
        "failed" | "cancelled" => Ok(MappedStop { stop_reason: StopReason::Error, error_message: None }),
        "in_progress" | "queued" => Ok(MappedStop { stop_reason: StopReason::Stop, error_message: None }),
        other => Err(ResponsesStreamError::new(format!("Unhandled stop reason: {other}"))),
    }
}

/// One SSE frame's `data:` payloads, or `None` for a keep-alive / `[DONE]` frame.
fn parse_sse_event(raw: &str) -> Result<Option<ResponsesEvent>, ResponsesStreamError> {
    let data = raw
        .split("\r\n")
        .flat_map(|line| line.split('\n'))
        .flat_map(|line| line.split('\r'))
        .filter(|line| line.starts_with("data:"))
        .map(|line| line[5..].trim_start())
        .collect::<Vec<_>>()
        .join("\n");
    let data = data.trim();
    if data.is_empty() || data == "[DONE]" {
        return Ok(None);
    }
    let value: Value = serde_json::from_str(data)
        .map_err(|error| ResponsesStreamError::new(error.to_string()))?;
    Ok(Some(ResponsesEvent { value }))
}

fn find_sse_boundary(buffer: &str) -> Option<(usize, usize)> {
    let pattern = "\r\n\r\n|\r\n\r|\r\n\n|\r\r\n|\n\r\n|\r\r|\n\r|\n\n";
    let regex = fancy_regex::Regex::new(pattern).ok()?;
    let found = regex.find(buffer).ok()??;
    Some((found.start(), found.end() - found.start()))
}

struct StreamState {
    output_slots: HashMap<usize, Slot>,
    reasoning_blocks_by_id: HashMap<String, usize>,
    native_image_chars_by_output_index: HashMap<usize, usize>,
    finalized_native_image_output_indexes: HashSet<usize>,
    partial_json_blocks: HashSet<usize>,
    native_image_base64_chars: usize,
    saw_terminal_response_event: bool,
    open_items: usize,
    saw_item_done: bool,
}

impl StreamState {
    fn new() -> Self {
        Self {
            output_slots: HashMap::new(),
            reasoning_blocks_by_id: HashMap::new(),
            native_image_chars_by_output_index: HashMap::new(),
            finalized_native_image_output_indexes: HashSet::new(),
            partial_json_blocks: HashSet::new(),
            native_image_base64_chars: 0,
            saw_terminal_response_event: false,
            open_items: 0,
            saw_item_done: false,
        }
    }
}

/// `processResponsesStream` over an already-parsed event sequence.
pub fn process_responses_events(
    events: impl IntoIterator<Item = Value>,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    model: &Model,
    options: &ResponsesStreamOptions<'_>,
) -> Result<(), ResponsesStreamError> {
    let mut state = StreamState::new();
    for value in events {
        handle_event(ResponsesEvent { value }, output, stream, model, options, &mut state)?;
    }
    finish(&state, output)
}

/// `processResponsesStream`.
pub async fn process_responses_stream(
    body: Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    model: &Model,
    options: &ResponsesStreamOptions<'_>,
) -> Result<(), ResponsesStreamError> {
    let mut state = StreamState::new();

    let mut buffer = String::new();
    let mut body = body;
    while let Some(chunk) = read_chunk(&mut body, &state).await? {
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        while let Some((index, length)) = find_sse_boundary(&buffer) {
            let raw: String = buffer[..index].to_owned();
            buffer = buffer[index + length..].to_owned();
            let Some(event) = parse_sse_event(&raw)? else { continue };
            handle_event(event, output, stream, model, options, &mut state)?;
        }
    }
    finish(&state, output)
}

fn finish(state: &StreamState, output: &AssistantMessage) -> Result<(), ResponsesStreamError> {
    let has_finalized_tool_call = output.content.iter().enumerate().any(|(index, block)| {
        matches!(block, ContentBlock::ToolCall(_)) && !state.partial_json_blocks.contains(&index)
    });
    if !state.saw_terminal_response_event && !has_finalized_tool_call {
        return Err(ResponsesStreamError::new("OpenAI Responses stream ended before a terminal response event"));
    }
    Ok(())
}

/// The next body chunk, bounded by `withResponsesCompletionGrace`'s deadline once every output item
/// has been finalized.
async fn read_chunk(
    body: &mut Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    state: &StreamState,
) -> Result<Option<Bytes>, ResponsesStreamError> {
    let bounded = state.saw_item_done && state.open_items == 0 && RESPONSES_COMPLETION_GRACE_MS > 0;
    if !bounded {
        return match body.next().await {
            None => Ok(None),
            Some(chunk) => chunk.map(Some).map_err(|error| ResponsesStreamError::new(error.to_string())),
        };
    }
    match tokio::time::timeout(Duration::from_millis(RESPONSES_COMPLETION_GRACE_MS), body.next()).await {
        Ok(None) => Ok(None),
        Ok(Some(chunk)) => chunk.map(Some).map_err(|error| ResponsesStreamError::new(error.to_string())),
        Err(_) => Err(ResponsesStreamError::new(format_responses_completion_stall(
            RESPONSES_COMPLETION_GRACE_MS,
        ))),
    }
}

fn get_slot(state: &StreamState, output_index: usize, kind: SlotKind) -> Option<&Slot> {
    state.output_slots.get(&output_index).filter(|slot| slot.kind == kind)
}

/// `getCustomToolCallInput`.
fn get_custom_tool_call_input(output: &AssistantMessage, state: &StreamState, output_index: usize) -> String {
    let Some(property) = state.output_slots.get(&output_index).and_then(|slot| slot.custom_input.as_ref())
    else {
        return String::new();
    };
    let property = property.property.clone();
    let Some(ContentBlock::ToolCall(block)) = state
        .output_slots
        .get(&output_index)
        .map(|slot| slot.content_index)
        .and_then(|content_index| output.content.get(content_index))
    else {
        return String::new();
    };
    match block.arguments.get(&property) {
        Some(Value::String(value)) => value.clone(),
        _ => String::new(),
    }
}

/// `appendCustomToolCallInput`.
fn append_custom_tool_call_input(
    output: &mut AssistantMessage,
    state: &mut StreamState,
    output_index: usize,
    next_input: &str,
    close: bool,
) -> Result<Option<String>, ResponsesStreamError> {
    let content_index = state.output_slots.get(&output_index).map(|slot| slot.content_index);
    let (property, delta) = {
        let Some(slot) = state.output_slots.get_mut(&output_index) else { return Ok(None) };
        let Some(custom_input) = slot.custom_input.as_mut() else { return Ok(None) };
        let property = custom_input.property.clone();
        let delta = append_grammar_tool_input_json_delta(
            &mut custom_input.json_buffer,
            &property,
            next_input,
            close,
        )
        .map_err(ResponsesStreamError::new)?;
        (property, delta)
    };
    if let Some(content_index) = content_index
        && let Some(ContentBlock::ToolCall(block)) = output.content.get_mut(content_index)
    {
        let mut arguments = Map::new();
        arguments.insert(property, json!(next_input));
        block.arguments = arguments;
    }
    Ok(delta)
}

fn push_tool_call_delta(
    slot: &Slot,
    delta: Option<String>,
    output: &AssistantMessage,
    stream: &AssistantMessageEventStream,
) {
    let Some(delta) = delta else { return };
    stream.push(AssistantMessageEvent::ToolcallDelta {
        content_index: slot.content_index,
        delta,
        partial: output.clone(),
    });
}

fn create_slot(
    output_index: usize,
    item: &Value,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    options: &ResponsesStreamOptions<'_>,
    state: &mut StreamState,
) -> Result<(), ResponsesStreamError> {
    let item_type = field_str(item, "type").unwrap_or_default().to_owned();
    match item_type.as_str() {
        "reasoning" => {
            output.content.push(ContentBlock::Thinking(ThinkingContent::default()));
            let content_index = output.content.len() - 1;
            state.output_slots.insert(output_index, Slot::new(SlotKind::Thinking, content_index));
            stream.push(AssistantMessageEvent::ThinkingStart { content_index, partial: output.clone() });
        }
        "message" => {
            if field_str(item, "phase") == Some("final_answer") {
                output.stop_reason = StopReason::Stop;
            }
            output.content.push(ContentBlock::Text(TextContent::default()));
            let content_index = output.content.len() - 1;
            state.output_slots.insert(output_index, Slot::new(SlotKind::Text, content_index));
            stream.push(AssistantMessageEvent::TextStart { content_index, partial: output.clone() });
        }
        "function_call" => {
            let call_id = field_str(item, "call_id").unwrap_or_default();
            let item_id = field_str(item, "id").unwrap_or_default();
            let tool_call = ToolCall {
                id: format!("{call_id}|{item_id}"),
                name: field_str(item, "name").unwrap_or_default().to_owned(),
                arguments: Map::new(),
                incomplete: None,
                error_message: None,
                thought_signature: None,
                namespace: field_str(item, "namespace").map(str::to_owned),
            };
            output.content.push(ContentBlock::ToolCall(tool_call));
            let content_index = output.content.len() - 1;
            let mut slot = Slot::new(SlotKind::ToolCall, content_index);
            slot.partial_json = Some(field_str(item, "arguments").unwrap_or_default().to_owned());
            state.output_slots.insert(output_index, slot);
            state.partial_json_blocks.insert(content_index);
            stream.push(AssistantMessageEvent::ToolcallStart { content_index, partial: output.clone() });
        }
        "custom_tool_call" => {
            let input_property = options
                .grammar_tool_input_properties
                .get(field_str(item, "name").unwrap_or_default())
                .cloned()
                .unwrap_or_else(|| String::from("input"));
            let input = field_str(item, "input").unwrap_or_default().to_owned();
            let call_id = field_str(item, "call_id").unwrap_or_default();
            let item_id = field_str(item, "id").unwrap_or(CUSTOM_TOOL_CALL_ITEM_ID_SENTINEL);
            let mut arguments = Map::new();
            arguments.insert(input_property.clone(), json!(input));
            let tool_call = ToolCall {
                id: format!("{call_id}|{item_id}"),
                name: field_str(item, "name").unwrap_or_default().to_owned(),
                arguments,
                incomplete: None,
                error_message: None,
                thought_signature: None,
                namespace: field_str(item, "namespace").map(str::to_owned),
            };
            output.content.push(ContentBlock::ToolCall(tool_call));
            let content_index = output.content.len() - 1;
            let mut slot = Slot::new(SlotKind::ToolCall, content_index);
            slot.custom_input = Some(CustomToolInput {
                property: input_property,
                json_buffer: GrammarToolInputJsonBuffer::default(),
            });
            state.output_slots.insert(output_index, slot);
            stream.push(AssistantMessageEvent::ToolcallStart { content_index, partial: output.clone() });
        }
        _ => {
            let image_item = read_native_image_generation_call(item);
            let raw = match image_item.as_ref() {
                Some(image_item) => reconcile_native_image_generation_call(image_item),
                None => item.clone(),
            };
            let content_index = output.content.len();
            if let Some(image_item) = image_item.as_ref() {
                reconcile_native_image_slot(output_index, content_index, image_item, output, state)?;
            }
            output.content.push(ContentBlock::ProviderNative(ProviderNativeContent { subtype: item_type, raw }));
            state.output_slots.insert(output_index, Slot::new(SlotKind::ProviderNative, content_index));
        }
    }
    Ok(())
}

fn reconcile_native_image_slot(
    output_index: usize,
    content_index: usize,
    item: &Value,
    output: &mut AssistantMessage,
    state: &mut StreamState,
) -> Result<(), ResponsesStreamError> {
    let reconciled = reconcile_native_image_generation_call(item);
    let previous_chars = state.native_image_chars_by_output_index.get(&output_index).copied().unwrap_or(0);
    let next_chars = field_str(&reconciled, "result").map(str::len).unwrap_or(0);
    let next_total = state.native_image_base64_chars.saturating_sub(previous_chars) + next_chars;
    if next_total > MAX_NATIVE_IMAGE_BASE64_CHARS {
        scrub_native_image_results(output, state);
        return Err(ResponsesStreamError::new("Native image generation results exceed the 24 MiB base64 limit"));
    }
    state.native_image_base64_chars = next_total;
    if next_chars > 0 {
        state.native_image_chars_by_output_index.insert(output_index, next_chars);
    } else {
        state.native_image_chars_by_output_index.remove(&output_index);
    }
    if let Some(ContentBlock::ProviderNative(block)) = output.content.get_mut(content_index) {
        block.subtype = String::from("image_generation_call");
        block.raw = reconciled;
    }
    Ok(())
}

fn scrub_native_image_results(output: &mut AssistantMessage, state: &mut StreamState) {
    for block in output.content.iter_mut() {
        let ContentBlock::ProviderNative(native) = block else { continue };
        if native.subtype != "image_generation_call" {
            continue;
        }
        let Some(item) = read_native_image_generation_call(&native.raw) else { continue };
        if field_str(&item, "result").is_none() {
            continue;
        }
        let mut raw = Map::new();
        raw.insert("type".into(), json!("image_generation_call"));
        if let Some(id) = field_str(&item, "id") {
            raw.insert("id".into(), json!(id));
        }
        raw.insert("status".into(), json!("malformed"));
        native.raw = Value::Object(raw);
    }
    state.native_image_base64_chars = 0;
    state.native_image_chars_by_output_index.clear();
}

fn handle_event(
    event: ResponsesEvent,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    model: &Model,
    options: &ResponsesStreamOptions<'_>,
    state: &mut StreamState,
) -> Result<(), ResponsesStreamError> {
    let event = event.value;
    let event_type = field_str(&event, "type").unwrap_or_default().to_owned();
    let output_index = field_usize(&event, "output_index").unwrap_or(0);

    let apply_message_phase_stop_reason = |item: &Value, output: &mut AssistantMessage| {
        if field_str(item, "type") == Some("message") && field_str(item, "phase") == Some("final_answer") {
            output.stop_reason = StopReason::Stop;
        }
    };

    match event_type.as_str() {
        "response.created" => {
            if let Some(id) = event.get("response").and_then(|response| field_str(response, "id")) {
                output.response_id = Some(id.to_owned());
            }
        }
        "response.output_item.added" => {
            let item = event.get("item").cloned().unwrap_or(Value::Null);
            create_slot(output_index, &item, output, stream, options, state)?;
            state.open_items += 1;
        }
        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
            let delta = field_str(&event, "delta").unwrap_or_default().to_owned();
            if let Some(content_index) = get_slot(state, output_index, SlotKind::Thinking).map(|slot| slot.content_index)
            {
                if let Some(ContentBlock::Thinking(block)) = output.content.get_mut(content_index) {
                    block.thinking.push_str(&delta);
                }
                stream.push(AssistantMessageEvent::ThinkingDelta {
                    content_index,
                    delta,
                    partial: output.clone(),
                });
            }
        }
        "response.reasoning_summary_part.done" => {
            if let Some(content_index) = get_slot(state, output_index, SlotKind::Thinking).map(|slot| slot.content_index)
            {
                if let Some(ContentBlock::Thinking(block)) = output.content.get_mut(content_index) {
                    block.thinking.push_str("\n\n");
                }
                stream.push(AssistantMessageEvent::ThinkingDelta {
                    content_index,
                    delta: String::from("\n\n"),
                    partial: output.clone(),
                });
            }
        }
        "response.output_text.delta" | "response.refusal.delta" => {
            let delta = field_str(&event, "delta").unwrap_or_default().to_owned();
            if let Some(content_index) = get_slot(state, output_index, SlotKind::Text).map(|slot| slot.content_index) {
                if let Some(ContentBlock::Text(block)) = output.content.get_mut(content_index) {
                    block.text.push_str(&delta);
                }
                stream.push(AssistantMessageEvent::TextDelta {
                    content_index,
                    delta,
                    partial: output.clone(),
                });
            }
        }
        "response.function_call_arguments.delta" => {
            let delta = field_str(&event, "delta").unwrap_or_default().to_owned();
            let Some(slot) = state.output_slots.get(&output_index) else { return Ok(()) };
            if slot.kind != SlotKind::ToolCall || slot.partial_json.is_none() {
                return Ok(());
            }
            let content_index = slot.content_index;
            let partial_json = {
                let slot = state.output_slots.get_mut(&output_index).expect("checked");
                let partial_json = slot.partial_json.get_or_insert_with(String::new);
                partial_json.push_str(&delta);
                partial_json.clone()
            };
            if let Some(ContentBlock::ToolCall(block)) = output.content.get_mut(content_index) {
                block.arguments =
                    parse_streaming_json(Some(&partial_json)).as_object().cloned().unwrap_or_default();
            }
            let slot = state.output_slots.get(&output_index).expect("checked");
            push_tool_call_delta(slot, Some(delta), output, stream);
        }
        "response.function_call_arguments.done" => {
            let arguments = field_str(&event, "arguments").unwrap_or_default().to_owned();
            let Some(slot) = state.output_slots.get(&output_index) else { return Ok(()) };
            if slot.kind != SlotKind::ToolCall || slot.partial_json.is_none() {
                return Ok(());
            }
            let content_index = slot.content_index;
            let previous_partial_json = slot.partial_json.clone().unwrap_or_default();
            if let Some(slot) = state.output_slots.get_mut(&output_index) {
                slot.partial_json = Some(arguments.clone());
            }
            if let Some(ContentBlock::ToolCall(block)) = output.content.get_mut(content_index) {
                block.arguments =
                    parse_streaming_json(Some(&arguments)).as_object().cloned().unwrap_or_default();
            }
            if let Some(delta) = arguments.strip_prefix(previous_partial_json.as_str())
                && !delta.is_empty()
            {
                let slot = state.output_slots.get(&output_index).expect("checked");
                push_tool_call_delta(slot, Some(delta.to_owned()), output, stream);
            }
        }
        "response.custom_tool_call_input.delta" => {
            let delta = field_str(&event, "delta").unwrap_or_default().to_owned();
            let Some(slot) = state.output_slots.get(&output_index) else { return Ok(()) };
            if slot.kind != SlotKind::ToolCall || slot.custom_input.is_none() {
                return Ok(());
            }
            let content_index = slot.content_index;
            let next_input = format!("{}{delta}", get_custom_tool_call_input(output, state, output_index));
            let delta = append_custom_tool_call_input(output, state, output_index, &next_input, false)?;
            let slot = Slot { kind: SlotKind::ToolCall, content_index, partial_json: None, custom_input: None };
            push_tool_call_delta(&slot, delta, output, stream);
        }
        "response.custom_tool_call_input.done" => {
            let input = field_str(&event, "input").unwrap_or_default().to_owned();
            let Some(slot) = state.output_slots.get(&output_index) else { return Ok(()) };
            if slot.kind != SlotKind::ToolCall || slot.custom_input.is_none() {
                return Ok(());
            }
            let content_index = slot.content_index;
            let delta = append_custom_tool_call_input(output, state, output_index, &input, true)?;
            let slot = Slot { kind: SlotKind::ToolCall, content_index, partial_json: None, custom_input: None };
            push_tool_call_delta(&slot, delta, output, stream);
        }
        "response.output_item.done" => {
            let item = event.get("item").cloned().unwrap_or(Value::Null);
            apply_message_phase_stop_reason(&item, output);
            let item_type = field_str(&item, "type").unwrap_or_default().to_owned();
            let image_item = read_native_image_generation_call(&item);

            if !state.output_slots.contains_key(&output_index) {
                create_slot(output_index, &item, output, stream, options, state)?;
            }
            let kind = state.output_slots.get(&output_index).map(|slot| slot.kind);
            let content_index = state.output_slots.get(&output_index).map(|slot| slot.content_index);

            if let (Some(image_item), Some(SlotKind::ProviderNative), Some(content_index)) =
                (image_item.as_ref(), kind, content_index)
            {
                reconcile_native_image_slot(output_index, content_index, image_item, output, state)?;
                state.finalized_native_image_output_indexes.insert(output_index);
                state.output_slots.remove(&output_index);
            } else if item_type == "reasoning" && kind == Some(SlotKind::Thinking) {
                let content_index = content_index.expect("kind implies a slot");
                let joined = |key: &str| {
                    item.get(key)
                        .and_then(Value::as_array)
                        .map(|parts| {
                            parts
                                .iter()
                                .map(|part| field_str(part, "text").unwrap_or_default().to_owned())
                                .collect::<Vec<_>>()
                                .join("\n\n")
                        })
                        .unwrap_or_default()
                };
                let summary_text = joined("summary");
                let content_text = joined("content");
                let thinking = if !summary_text.is_empty() {
                    summary_text
                } else if !content_text.is_empty() {
                    content_text
                } else {
                    match output.content.get(content_index) {
                        Some(ContentBlock::Thinking(block)) => block.thinking.clone(),
                        _ => String::new(),
                    }
                };
                let signature = serde_json::to_string(&item).unwrap_or_default();
                let item_id = field_str(&item, "id").unwrap_or_default().to_owned();
                if let Some(ContentBlock::Thinking(block)) = output.content.get_mut(content_index) {
                    block.thinking = thinking.clone();
                    block.thinking_signature = Some(signature);
                }
                state.reasoning_blocks_by_id.insert(item_id, content_index);
                stream.push(AssistantMessageEvent::ThinkingEnd {
                    content_index,
                    content: thinking,
                    partial: output.clone(),
                });
                state.output_slots.remove(&output_index);
            } else if item_type == "message" && kind == Some(SlotKind::Text) {
                let content_index = content_index.expect("kind implies a slot");
                let text = item
                    .get("content")
                    .and_then(Value::as_array)
                    .map(|content| {
                        content
                            .iter()
                            .map(|part| {
                                if field_str(part, "type") == Some("output_text") {
                                    field_str(part, "text").unwrap_or_default().to_owned()
                                } else {
                                    field_str(part, "refusal").unwrap_or_default().to_owned()
                                }
                            })
                            .collect::<Vec<_>>()
                            .join("")
                    })
                    .unwrap_or_default();
                let signature =
                    encode_text_signature_v1(field_str(&item, "id"), text_phase(field_str(&item, "phase")));
                if let Some(ContentBlock::Text(block)) = output.content.get_mut(content_index) {
                    block.text = text.clone();
                    block.text_signature = Some(signature);
                }
                stream.push(AssistantMessageEvent::TextEnd {
                    content_index,
                    content: text,
                    partial: output.clone(),
                });
                state.output_slots.remove(&output_index);
            } else if item_type == "function_call"
                && kind == Some(SlotKind::ToolCall)
                && state.output_slots.get(&output_index).is_some_and(|slot| slot.partial_json.is_some())
            {
                let content_index = content_index.expect("kind implies a slot");
                let slot_partial_json = state
                    .output_slots
                    .get(&output_index)
                    .and_then(|slot| slot.partial_json.clone())
                    .unwrap_or_default();
                let arguments = field_str(&item, "arguments")
                    .filter(|arguments| !arguments.is_empty())
                    .map(str::to_owned)
                    .unwrap_or_else(|| {
                        if slot_partial_json.is_empty() { String::from("{}") } else { slot_partial_json }
                    });
                let parsed = parse_streaming_json(Some(&arguments));
                let namespace = field_str(&item, "namespace").map(str::to_owned);
                if let Some(ContentBlock::ToolCall(block)) = output.content.get_mut(content_index) {
                    block.arguments = parsed.as_object().cloned().unwrap_or_default();
                    if let Some(namespace) = namespace {
                        block.namespace = Some(namespace);
                    }
                }
                state.partial_json_blocks.remove(&content_index);
                let tool_call = match output.content.get(content_index) {
                    Some(ContentBlock::ToolCall(tool_call)) => tool_call.clone(),
                    _ => ToolCall::default(),
                };
                stream.push(AssistantMessageEvent::ToolcallEnd {
                    content_index,
                    tool_call,
                    partial: output.clone(),
                });
                state.output_slots.remove(&output_index);
            } else if item_type == "custom_tool_call"
                && kind == Some(SlotKind::ToolCall)
                && state.output_slots.get(&output_index).is_some_and(|slot| slot.custom_input.is_some())
            {
                let content_index = content_index.expect("kind implies a slot");
                let next_input = field_str(&item, "input")
                    .map(str::to_owned)
                    .unwrap_or_else(|| get_custom_tool_call_input(output, state, output_index));
                let delta = append_custom_tool_call_input(output, state, output_index, &next_input, true)?;
                let namespace = field_str(&item, "namespace").map(str::to_owned);
                if let Some(ContentBlock::ToolCall(block)) = output.content.get_mut(content_index)
                    && let Some(namespace) = namespace
                {
                    block.namespace = Some(namespace);
                }
                if let Some(slot) = state.output_slots.get_mut(&output_index) {
                    slot.custom_input = None;
                }
                let slot = Slot { kind: SlotKind::ToolCall, content_index, partial_json: None, custom_input: None };
                push_tool_call_delta(&slot, delta, output, stream);
                let tool_call = match output.content.get(content_index) {
                    Some(ContentBlock::ToolCall(tool_call)) => tool_call.clone(),
                    _ => ToolCall::default(),
                };
                stream.push(AssistantMessageEvent::ToolcallEnd {
                    content_index,
                    tool_call,
                    partial: output.clone(),
                });
                state.output_slots.remove(&output_index);
            } else if item_type == "custom_tool_call" && kind == Some(SlotKind::ToolCall) {
                let content_index = content_index.expect("kind implies a slot");
                let input = field_str(&item, "input").unwrap_or_default().to_owned();
                if let Some(ContentBlock::ToolCall(block)) = output.content.get_mut(content_index) {
                    let mut arguments = Map::new();
                    arguments.insert("input".into(), json!(input));
                    block.arguments = arguments;
                }
                state.partial_json_blocks.remove(&content_index);
                let tool_call = match output.content.get(content_index) {
                    Some(ContentBlock::ToolCall(tool_call)) => tool_call.clone(),
                    _ => ToolCall::default(),
                };
                stream.push(AssistantMessageEvent::ToolcallEnd {
                    content_index,
                    tool_call,
                    partial: output.clone(),
                });
                state.output_slots.remove(&output_index);
            } else if kind == Some(SlotKind::ProviderNative) {
                let content_index = content_index.expect("kind implies a slot");
                if let Some(ContentBlock::ProviderNative(block)) = output.content.get_mut(content_index) {
                    block.subtype = item_type;
                    block.raw = item;
                }
                state.output_slots.remove(&output_index);
            }
            state.open_items = state.open_items.saturating_sub(1);
            state.saw_item_done = true;
        }
        "response.completed" | "response.incomplete" => {
            let response = event.get("response").cloned().unwrap_or(Value::Null);
            finalize_response(&response, output, model, options, state)?;
        }
        "error" => {
            return Err(ResponsesStreamError::new(format!(
                "Error Code {}: {}",
                js_template_string(event.get("code")),
                js_template_string(event.get("message"))
            )));
        }
        "response.failed" => {
            state.saw_terminal_response_event = true;
            let response = event.get("response").cloned().unwrap_or(Value::Null);
            output.raw_stop_reason = field_str(&response, "status").map(str::to_owned);
            let error = response.get("error").filter(|error| error.is_object());
            let details = response.get("incomplete_details");
            let message = match error {
                Some(error) => format!(
                    "{}: {}",
                    js_truthy_str(error.get("code")).unwrap_or_else(|| String::from("unknown")),
                    js_truthy_str(error.get("message")).unwrap_or_else(|| String::from("no message")),
                ),
                None => match details.and_then(|details| js_truthy_str(details.get("reason"))) {
                    Some(reason) => format!("incomplete: {reason}"),
                    None => String::from("Unknown error (no error details in response)"),
                },
            };
            return Err(ResponsesStreamError::new(message));
        }
        _ => {}
    }
    Ok(())
}

fn finalize_response(
    response: &Value,
    output: &mut AssistantMessage,
    model: &Model,
    options: &ResponsesStreamOptions<'_>,
    state: &mut StreamState,
) -> Result<(), ResponsesStreamError> {
    state.saw_terminal_response_event = true;
    let response_output = response.get("output").and_then(Value::as_array).cloned().unwrap_or_default();
    backfill_reasoning_signatures(&response_output, output, state);
    backfill_native_image_generation_calls(&response_output, output, state)?;

    if let Some(id) = field_str(response, "id") {
        output.response_id = Some(id.to_owned());
    }
    if let Some(usage) = response.get("usage").filter(|usage| usage.is_object()) {
        let input_details = usage.get("input_tokens_details");
        let cached_tokens = input_details.map(|details| field_u64(details, "cached_tokens")).unwrap_or(0);
        let cache_write_tokens =
            input_details.map(|details| field_u64(details, "cache_write_tokens")).unwrap_or(0);
        output.usage = Usage {
            input: field_u64(usage, "input_tokens").saturating_sub(cached_tokens).saturating_sub(cache_write_tokens),
            output: field_u64(usage, "output_tokens"),
            cache_read: cached_tokens,
            cache_write: cache_write_tokens,
            cache_write_1h: None,
            reasoning: Some(
                usage
                    .get("output_tokens_details")
                    .map(|details| field_u64(details, "reasoning_tokens"))
                    .unwrap_or(0),
            ),
            total_tokens: field_u64(usage, "total_tokens"),
            cost: Default::default(),
        };
    }
    calculate_cost(model, &mut output.usage);
    if let Some(apply) = options.apply_service_tier_pricing {
        apply(&mut output.usage, field_str(response, "service_tier").or(options.service_tier));
    }

    let status = field_str(response, "status").map(str::to_owned);
    let incomplete_reason = response
        .get("incomplete_details")
        .and_then(|details| js_truthy_str(details.get("reason")));
    output.raw_stop_reason = match incomplete_reason.as_ref() {
        Some(reason) => Some(format!(
            "{}.{}",
            status.clone().unwrap_or_else(|| String::from("undefined")),
            reason
        )),
        None => status.clone(),
    };
    let mapped = map_stop_reason(status.as_deref(), incomplete_reason.as_deref())?;
    output.stop_reason = mapped.stop_reason;
    output.error_message = mapped.error_message;
    if output.stop_reason == StopReason::Stop
        && output.content.iter().any(|block| matches!(block, ContentBlock::ToolCall(_)))
    {
        output.stop_reason = StopReason::ToolUse;
    }
    Ok(())
}

fn backfill_reasoning_signatures(
    response_output: &[Value],
    output: &mut AssistantMessage,
    state: &StreamState,
) {
    for item in response_output {
        if field_str(item, "type") != Some("reasoning") {
            continue;
        }
        let Some(encrypted_content) = js_truthy_str(item.get("encrypted_content")) else { continue };
        let Some(item_id) = field_str(item, "id") else { continue };
        let Some(content_index) = state.reasoning_blocks_by_id.get(item_id) else { continue };
        let Some(ContentBlock::Thinking(block)) = output.content.get(*content_index) else { continue };
        let Some(signature) = block.thinking_signature.as_deref() else { continue };
        let Some(stored_item) = parse_reasoning_signature(Some(signature)) else { continue };
        if js_truthy_str(stored_item.get("encrypted_content")).is_some() {
            continue;
        }
        let Some(stored) = stored_item.as_object() else { continue };
        let mut merged = stored.clone();
        merged.insert("encrypted_content".into(), json!(encrypted_content));
        if let Some(ContentBlock::Thinking(block)) = output.content.get_mut(*content_index) {
            block.thinking_signature = serde_json::to_string(&Value::Object(merged)).ok();
        }
    }
}

fn backfill_native_image_generation_calls(
    response_output: &[Value],
    output: &mut AssistantMessage,
    state: &mut StreamState,
) -> Result<(), ResponsesStreamError> {
    for (output_index, output_item) in response_output.iter().enumerate() {
        if state.finalized_native_image_output_indexes.contains(&output_index) {
            continue;
        }
        let Some(image_item) = read_native_image_generation_call(output_item) else { continue };
        match state.output_slots.get(&output_index).filter(|slot| slot.kind == SlotKind::ProviderNative) {
            Some(slot) => {
                let content_index = slot.content_index;
                reconcile_native_image_slot(output_index, content_index, &image_item, output, state)?;
            }
            None => {
                let content_index = output.content.len();
                let raw = reconcile_native_image_generation_call(&image_item);
                reconcile_native_image_slot(output_index, content_index, &image_item, output, state)?;
                output.content.push(ContentBlock::ProviderNative(ProviderNativeContent {
                    subtype: String::from("image_generation_call"),
                    raw,
                }));
                state.output_slots.insert(output_index, Slot::new(SlotKind::ProviderNative, content_index));
            }
        }
        state.output_slots.remove(&output_index);
    }
    Ok(())
}

/// `getDoneReason`.
pub fn get_done_reason(stop_reason: StopReason) -> DoneReason {
    match stop_reason {
        StopReason::Length => DoneReason::Length,
        StopReason::ToolUse => DoneReason::ToolUse,
        _ => DoneReason::Stop,
    }
}
