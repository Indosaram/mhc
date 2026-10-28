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
use serde_json::{Map, Value, json};

use crate::api::constrained_sampling::{
    GrammarToolInputJsonBuffer, append_grammar_tool_input_json_delta, get_grammar_tool_input,
    get_json_schema_tool_parameters, resolve_grammar_constrained_sampling,
    resolve_json_schema_strict_sampling,
};
use crate::api::responses_completion_grace::format_responses_completion_stall;
use crate::api::transform_messages::{TransformMessagesOptions, transform_messages};
use crate::models::{calculate_cost, supports_configuration_update};
use crate::types::{
    AssistantMessage, AssistantMessageEvent, ContentBlock, Context, DoneReason, GrammarFormat,
    InputModality, Message, Model, ProviderNativeContent, StopReason, TextContent, TextPhase,
    TextSignatureV1, ThinkingContent, Tool, ToolCall, Usage, UserContent,
};
use crate::utils::event_stream::AssistantMessageEventStream;
use crate::utils::hash::short_hash;
use crate::utils::json_parse::parse_streaming_json;
use crate::utils::sanitize_unicode::sanitize_surrogates;

pub use crate::api::responses_completion_grace::RESPONSES_COMPLETION_GRACE_MS;

#[path = "openai_responses_prompt_cache.rs"]
pub mod openai_responses_prompt_cache;

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
    pub system_prompt_cache_breakpoint: Option<bool>,
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
pub type ResolveServiceTier<'a> =
    &'a (dyn Fn(Option<&str>, Option<&str>) -> Option<String> + Send + Sync);

pub struct ResponsesStreamOptions<'a> {
    pub on_provider_stream_event: Option<&'a crate::types::OnProviderStreamEvent>,
    pub async_on_provider_stream_event: Option<&'a crate::types::AsyncOnProviderStreamEvent>,
    pub resolve_service_tier: Option<ResolveServiceTier<'a>>,
    pub service_tier: Option<&'a str>,
    pub grammar_tool_input_properties: &'a BTreeMap<String, String>,
    pub apply_service_tier_pricing: Option<ApplyServiceTierPricing<'a>>,
}

impl Default for ResponsesStreamOptions<'_> {
    fn default() -> Self {
        static EMPTY: std::sync::LazyLock<BTreeMap<String, String>> =
            std::sync::LazyLock::new(BTreeMap::new);
        Self {
            on_provider_stream_event: None,
            async_on_provider_stream_event: None,
            resolve_service_tier: None,
            service_tier: None,
            grammar_tool_input_properties: &EMPTY,
            apply_service_tier_pricing: None,
        }
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
        Self {
            message: message.into(),
        }
    }
}

/// `encodeTextSignatureV1`.
fn encode_text_signature_v1(id: Option<&str>, phase: Option<TextPhase>) -> String {
    match id {
        Some(id) => {
            let payload = TextSignatureV1 {
                v: 1,
                id: id.to_owned(),
                phase,
            };
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
        return Some((
            id.to_owned(),
            text_phase(parsed.get("phase").and_then(Value::as_str)),
        ));
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
    tools.is_some_and(|tools| {
        tools
            .iter()
            .any(|tool| tool.name == tool_name && is_freeform_tool(tool))
    })
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
            if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
                character
            } else {
                '_'
            }
        })
        .collect();
    let normalized: String = if sanitized.chars().count() > 64 {
        sanitized.chars().take(64).collect()
    } else {
        sanitized
    };
    normalized.trim_end_matches('_').to_owned()
}

/// `buildForeignResponsesItemId`.
fn build_foreign_responses_item_id(item_id: &str) -> String {
    let normalized = format!("fc_{}", short_hash(item_id));
    if normalized.chars().count() > 64 {
        normalized.chars().take(64).collect()
    } else {
        normalized
    }
}

/// `convertResponsesMessages`.
pub fn convert_responses_messages(
    model: &Model,
    context: &Context,
    allowed_tool_call_providers: &BTreeSet<String>,
    options: &ConvertResponsesMessagesOptions,
) -> Vec<Value> {
    let resolved_context = crate::types::transcript::resolve_provider_context(
        context,
        model.compat.as_ref().is_some_and(|compat| {
            compat.openai_responses().supports_mid_convo_system_messages == Some(true)
        }),
    );
    let context = &resolved_context;
    let transcript = crate::types::transcript::normalize_context(context);
    let declared_tools = Some(crate::types::transcript::get_declared_tools(
        &transcript.messages,
    ));
    let anchors_additions = crate::types::transcript::resolve_transcript_tools(
        &transcript.messages,
        options.deferred_tools_mode.is_some(),
    )
    .anchors_additions;
    let mut messages: Vec<Value> = Vec::new();
    let mut loaded_tool_names: BTreeSet<String> = BTreeSet::new();

    let normalize_tool_call_id = |id: &str, _target: &Model, source: &AssistantMessage| -> String {
        if !id.contains('|') {
            return normalize_id_part(id);
        }
        let (call_id, item_id) = split_call_id(id);
        let Some(item_id) = item_id else {
            return normalize_id_part(id);
        };
        let normalized_call_id = normalize_id_part(&call_id);
        if item_id == CUSTOM_TOOL_CALL_ITEM_ID_SENTINEL {
            return format!("{normalized_call_id}|{CUSTOM_TOOL_CALL_ITEM_ID_SENTINEL}");
        }
        if !allowed_tool_call_providers.contains(&model.provider) {
            return normalize_id_part(id);
        }
        let is_foreign_tool_call = source.provider != model.provider || source.api != model.api;
        let mut normalized_item_id = if is_foreign_tool_call {
            build_foreign_responses_item_id(&item_id)
        } else {
            normalize_id_part(&item_id)
        };
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
        && let Some(system_prompt) = context
            .system_prompt
            .as_ref()
            .filter(|prompt| !prompt.is_empty())
    {
        let compat = model
            .compat
            .as_ref()
            .map(|compat| compat.openai_responses());
        let supports_developer_role = compat
            .as_ref()
            .and_then(|compat| compat.supports_developer_role)
            .unwrap_or(true);
        let role = if model.reasoning && supports_developer_role {
            "developer"
        } else {
            "system"
        };
        let content = if options.system_prompt_cache_breakpoint == Some(true) {
            json!([{"type":"input_text", "text":sanitize_surrogates(system_prompt),
                "prompt_cache_breakpoint":{"mode":"explicit"}}])
        } else {
            json!(sanitize_surrogates(system_prompt))
        };
        messages.push(json!({ "role": role, "content": content }));
    }

    let mut msg_index: usize = 0;
    for msg in &transformed_messages {
        match msg {
            Message::System(system) => {
                let text = crate::utils::text::render_system_message_update(
                    &system.content,
                    &system.sections,
                );
                if anchors_additions {
                    let additions: Vec<Tool> = system
                        .tools_added
                        .iter()
                        .filter(|tool| loaded_tool_names.insert(tool.name.clone()))
                        .cloned()
                        .collect();
                    if !additions.is_empty() {
                        let tools = convert_responses_tools(&additions, &options.tool_options)
                            .unwrap_or_default();
                        if options.deferred_tools_mode
                            == Some(ResponsesDeferredToolsMode::AdditionalTools)
                        {
                            messages.push(
                                json!({"type":"additional_tools","role":"developer","tools":tools}),
                            );
                        } else {
                            let names: Vec<_> =
                                additions.iter().map(|tool| tool.name.as_str()).collect();
                            let call_id = format!(
                                "pi_tool_load_{}",
                                short_hash(&format!("system:{msg_index}:{}", names.join(",")))
                            );
                            messages.push(json!({"type":"tool_search_call","call_id":call_id,"execution":"client","status":"completed","arguments":{"query":names.join(" "),"limit":names.len()}}));
                            let tool_options = ConvertResponsesToolsOptions {
                                defer_loading: Some(true),
                                ..options.tool_options.clone()
                            };
                            let tools = convert_responses_tools(&additions, &tool_options)
                                .unwrap_or_default();
                            messages.push(json!({"type":"tool_search_output","call_id":call_id,"execution":"client","status":"completed","tools":tools}));
                        }
                    }
                }
                if !text.is_empty() {
                    let developer = model.reasoning
                        && model.compat.as_ref().is_none_or(|compat| {
                            compat.openai_responses().supports_developer_role != Some(false)
                        });
                    messages.push(json!({"role": if developer { "developer" } else { "system" }, "content": sanitize_surrogates(&text)}));
                }
            }
            Message::ConfigurationUpdate(update) => {
                if !supports_configuration_update(model) {
                    continue;
                }
                let next = json!({ "type": "configuration_update", "reasoning": { "effort": update.effort } });
                match messages.last() {
                    Some(Value::Object(previous))
                        if previous.get("type").and_then(Value::as_str)
                            == Some("configuration_update") =>
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
                let is_same_provider_and_api =
                    assistant.provider == model.provider && assistant.api == model.api;
                let is_same_model = is_same_provider_and_api && assistant.model == model.id;
                let is_different_model = is_same_provider_and_api && assistant.model != model.id;
                let mut text_block_index: usize = 0;

                let mut push_assistant_text =
                    |text: &str, text_signature: Option<&str>, output: &mut Vec<Value>| {
                        let parsed_signature = parse_text_signature(text_signature);
                        let fallback_message_id = if text_block_index == 0 {
                            format!("msg_pi_{msg_index}")
                        } else {
                            format!("msg_pi_{msg_index}_{text_block_index}")
                        };
                        text_block_index += 1;
                        let msg_id = match parsed_signature.as_ref().map(|(id, _)| id.clone()) {
                            None => fallback_message_id,
                            Some(id) if id.chars().count() > 64 => {
                                format!("msg_{}", short_hash(&id))
                            }
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
                        if let Some(phase) = parsed_signature.as_ref().and_then(|(_, phase)| *phase)
                        {
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
                            } else if thinking.thinking_signature.is_some()
                                && !thinking.thinking.trim().is_empty()
                            {
                                push_assistant_text(&thinking.thinking, None, &mut output);
                            }
                        }
                        ContentBlock::ProviderNative(block) => {
                            let raw = &block.raw;
                            if is_same_model
                                && block.subtype == "web_search_call"
                                && raw.get("type").and_then(Value::as_str)
                                    == Some("web_search_call")
                                && raw.get("id").and_then(Value::as_str).is_some()
                                && raw.get("action").is_some()
                                && matches!(
                                    raw.get("status").and_then(Value::as_str),
                                    Some("completed" | "failed")
                                )
                            {
                                output.push(raw.clone());
                            }
                        }
                        ContentBlock::Image(_) => {}
                        ContentBlock::Text(text) => {
                            push_assistant_text(
                                &text.text,
                                text.text_signature.as_deref(),
                                &mut output,
                            );
                        }
                        ContentBlock::ToolCall(tool_call) => {
                            let (call_id, item_id_raw) = split_call_id(&tool_call.id);
                            let custom_input_property = options
                                .grammar_tool_input_properties
                                .get(&tool_call.name)
                                .cloned();
                            let is_persisted_freeform =
                                item_id_raw.as_deref() == Some(CUSTOM_TOOL_CALL_ITEM_ID_SENTINEL);
                            let is_freeform =
                                is_freeform_tool_name(&tool_call.name, declared_tools.as_ref())
                                    || is_persisted_freeform;
                            let mut item_id: Option<String> = if is_persisted_freeform {
                                None
                            } else {
                                item_id_raw
                            };

                            let item_id_prefix = if custom_input_property.is_some() {
                                "ctc_"
                            } else {
                                "fc_"
                            };
                            if is_different_model
                                || !item_id
                                    .as_deref()
                                    .is_some_and(|id| id.starts_with(item_id_prefix))
                            {
                                item_id = None;
                            }

                            let can_replay_namespace = is_same_model
                                || options.deferred_tools.contains_key(&tool_call.name);
                            let namespace = if can_replay_namespace {
                                tool_call.namespace.clone()
                            } else {
                                None
                            };

                            if let Some(property) = custom_input_property {
                                let input = get_grammar_tool_input(
                                    &tool_call.name,
                                    &tool_call.arguments,
                                    &property,
                                )
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
                                item.insert(
                                    "input".into(),
                                    json!(get_freeform_tool_input(&tool_call.arguments)),
                                );
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
                                    json!(
                                        serde_json::to_string(&tool_call.arguments)
                                            .unwrap_or_else(|_| String::from("{}"))
                                    ),
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
                let custom_input_property = options
                    .grammar_tool_input_properties
                    .get(&result.tool_name)
                    .cloned();
                let is_persisted_freeform =
                    item_id_raw.as_deref() == Some(CUSTOM_TOOL_CALL_ITEM_ID_SENTINEL);

                if custom_input_property.is_some() {
                    messages.push(json!({
                        "type": "custom_tool_call_output",
                        "call_id": call_id,
                        "output": output,
                    }));
                } else if is_freeform_tool_name(&result.tool_name, declared_tools.as_ref())
                    || is_persisted_freeform
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
                    let Some(tool) = options.deferred_tools.get(name) else {
                        continue;
                    };
                    if loaded_tool_names.contains(name) {
                        continue;
                    }
                    loaded_tool_names.insert(name.clone());
                    deferred_tools.push(tool.clone());
                }
                if !deferred_tools.is_empty()
                    && options.deferred_tools_mode
                        == Some(ResponsesDeferredToolsMode::AdditionalTools)
                {
                    let tools = convert_responses_tools(&deferred_tools, &options.tool_options)
                        .unwrap_or_default();
                    messages.push(json!({
                        "type": "additional_tools",
                        "role": "developer",
                        "tools": tools,
                    }));
                } else if !deferred_tools.is_empty()
                    && options.deferred_tools_mode == Some(ResponsesDeferredToolsMode::ToolSearch)
                {
                    let names: Vec<String> = deferred_tools
                        .iter()
                        .map(|tool| tool.name.clone())
                        .collect();
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
                    let tool_options = ConvertResponsesToolsOptions {
                        defer_loading: Some(true),
                        ..options.tool_options.clone()
                    };
                    let tools =
                        convert_responses_tools(&deferred_tools, &tool_options).unwrap_or_default();
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

    // Sorted by name (UTF-16 order, stable) so the wire order is stable for prefix hashing.
    let mut sorted_tools: Vec<&Tool> = tools.iter().collect();
    sorted_tools.sort_by(|a, b| a.name.encode_utf16().cmp(b.name.encode_utf16()));
    let mut converted = Vec::with_capacity(tools.len());
    for tool in sorted_tools {
        if let Some(grammar) =
            resolve_grammar_constrained_sampling(tool, supports_openai_grammar_tools)?
        {
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
            item.insert(
                "format".into(),
                serde_json::to_value(freeform).unwrap_or(Value::Null),
            );
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
        Self {
            kind,
            content_index,
            partial_json: None,
            custom_input: None,
        }
    }
}

struct ResponsesEvent {
    value: Value,
}

impl crate::api::responses_completion_grace::TypedEvent for ResponsesEvent {
    fn event_type(&self) -> &str {
        self.value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }
}

fn field_str<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn field_usize(value: &Value, key: &str) -> Option<usize> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .map(|value| value as usize)
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
fn map_stop_reason(
    status: Option<&str>,
    incomplete_reason: Option<&str>,
) -> Result<MappedStop, ResponsesStreamError> {
    let Some(status) = status else {
        return Ok(MappedStop {
            stop_reason: StopReason::Stop,
            error_message: None,
        });
    };
    match status {
        "completed" => Ok(MappedStop {
            stop_reason: StopReason::Stop,
            error_message: None,
        }),
        "incomplete" => {
            if incomplete_reason == Some("max_output_tokens") {
                return Ok(MappedStop {
                    stop_reason: StopReason::Length,
                    error_message: None,
                });
            }
            let error_message = match incomplete_reason.filter(|reason| !reason.is_empty()) {
                Some(reason) => format!("Response incomplete: {reason}"),
                None => String::from("Response incomplete without a provider reason"),
            };
            Ok(MappedStop {
                stop_reason: StopReason::Error,
                error_message: Some(error_message),
            })
        }
        "failed" | "cancelled" => Ok(MappedStop {
            stop_reason: StopReason::Error,
            error_message: None,
        }),
        "in_progress" | "queued" => Ok(MappedStop {
            stop_reason: StopReason::Stop,
            error_message: None,
        }),
        other => Err(ResponsesStreamError::new(format!(
            "Unhandled stop reason: {other}"
        ))),
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
    let value: Value =
        serde_json::from_str(data).map_err(|error| ResponsesStreamError::new(error.to_string()))?;
    Ok(Some(ResponsesEvent { value }))
}

/// Longest alternative in the SSE boundary pattern (`\r\n\r\n`).
const MAX_SSE_BOUNDARY_LEN: usize = 4;

/// Finds the first SSE boundary at or after `from`; returns the absolute index and length.
/// The pattern has no anchors or lookaround, so a match starting at `from` or later is the
/// same as a full-buffer search when no match starts before `from`.
fn find_sse_boundary(buffer: &str, from: usize) -> Option<(usize, usize)> {
    static BOUNDARY_RE: std::sync::LazyLock<Option<fancy_regex::Regex>> =
        std::sync::LazyLock::new(|| {
            fancy_regex::Regex::new("\r\n\r\n|\r\n\r|\r\n\n|\r\r\n|\n\r\n|\r\r|\n\r|\n\n").ok()
        });
    let found = BOUNDARY_RE.as_ref()?.find(&buffer[from..]).ok()??;
    Some((from + found.start(), found.end() - found.start()))
}

/// Where to resume the boundary search after a miss over the whole of `buffer`: no match can
/// start earlier than `MAX_SSE_BOUNDARY_LEN` bytes before the end, aligned down to a char boundary.
fn sse_boundary_resume_offset(buffer: &str) -> usize {
    let mut offset = buffer.len().saturating_sub(MAX_SSE_BOUNDARY_LEN);
    while !buffer.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
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
        handle_event(
            ResponsesEvent { value },
            output,
            stream,
            model,
            options,
            &mut state,
        )?;
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
    let mut search_from = 0;
    while let Some(chunk) = read_chunk(&mut body, &state).await? {
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        while let Some((index, length)) = find_sse_boundary(&buffer, search_from) {
            let raw: String = buffer[..index].to_owned();
            buffer.drain(..index + length);
            search_from = 0;
            let Some(event) = parse_sse_event(&raw)? else {
                continue;
            };
            if let Some(hook) = options.async_on_provider_stream_event {
                hook(event.value.clone(), model.clone())
                    .await
                    .map_err(ResponsesStreamError::new)?;
            }
            handle_event(event, output, stream, model, options, &mut state)?;
        }
        search_from = sse_boundary_resume_offset(&buffer);
    }
    finish(&state, output)
}

fn finish(state: &StreamState, output: &AssistantMessage) -> Result<(), ResponsesStreamError> {
    let has_finalized_tool_call = output.content.iter().enumerate().any(|(index, block)| {
        matches!(block, ContentBlock::ToolCall(_)) && !state.partial_json_blocks.contains(&index)
    });
    if !state.saw_terminal_response_event && !has_finalized_tool_call {
        return Err(ResponsesStreamError::new(
            "OpenAI Responses stream ended before a terminal response event",
        ));
    }
    if output.stop_reason == StopReason::ToolUse {
        for (index, block) in output.content.iter().enumerate() {
            let ContentBlock::ToolCall(tool_call) = block else {
                continue;
            };
            let unfinished = state.partial_json_blocks.contains(&index)
                || state.output_slots.values().any(|slot| {
                    slot.content_index == index
                        && (slot.partial_json.is_some() || slot.custom_input.is_some())
                });
            if unfinished {
                return Err(ResponsesStreamError::new(format!(
                    "OpenAI Responses stream completed with an unfinished tool call: {} ({})",
                    tool_call.name, tool_call.id
                )));
            }
        }
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
            Some(chunk) => chunk
                .map(Some)
                .map_err(|error| ResponsesStreamError::new(error.to_string())),
        };
    }
    match tokio::time::timeout(
        Duration::from_millis(RESPONSES_COMPLETION_GRACE_MS),
        body.next(),
    )
    .await
    {
        Ok(None) => Ok(None),
        Ok(Some(chunk)) => chunk
            .map(Some)
            .map_err(|error| ResponsesStreamError::new(error.to_string())),
        Err(_) => Err(ResponsesStreamError::new(
            format_responses_completion_stall(RESPONSES_COMPLETION_GRACE_MS),
        )),
    }
}

fn get_slot(state: &StreamState, output_index: usize, kind: SlotKind) -> Option<&Slot> {
    state
        .output_slots
        .get(&output_index)
        .filter(|slot| slot.kind == kind)
}

/// `getCustomToolCallInput`.
fn get_custom_tool_call_input(
    output: &AssistantMessage,
    state: &StreamState,
    output_index: usize,
) -> String {
    let Some(property) = state
        .output_slots
        .get(&output_index)
        .and_then(|slot| slot.custom_input.as_ref())
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
    let content_index = state
        .output_slots
        .get(&output_index)
        .map(|slot| slot.content_index);
    let (property, delta) = {
        let Some(slot) = state.output_slots.get_mut(&output_index) else {
            return Ok(None);
        };
        let Some(custom_input) = slot.custom_input.as_mut() else {
            return Ok(None);
        };
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
            output
                .content
                .push(ContentBlock::Thinking(ThinkingContent::default()));
            let content_index = output.content.len() - 1;
            state
                .output_slots
                .insert(output_index, Slot::new(SlotKind::Thinking, content_index));
            stream.push(AssistantMessageEvent::ThinkingStart {
                content_index,
                partial: output.clone(),
            });
        }
        "message" => {
            if field_str(item, "phase") == Some("final_answer") {
                output.stop_reason = StopReason::Stop;
            }
            output
                .content
                .push(ContentBlock::Text(TextContent::default()));
            let content_index = output.content.len() - 1;
            state
                .output_slots
                .insert(output_index, Slot::new(SlotKind::Text, content_index));
            stream.push(AssistantMessageEvent::TextStart {
                content_index,
                partial: output.clone(),
            });
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
            stream.push(AssistantMessageEvent::ToolcallStart {
                content_index,
                partial: output.clone(),
            });
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
            stream.push(AssistantMessageEvent::ToolcallStart {
                content_index,
                partial: output.clone(),
            });
        }
        _ => {
            let image_item = read_native_image_generation_call(item);
            let raw = match image_item.as_ref() {
                Some(image_item) => reconcile_native_image_generation_call(image_item),
                None => item.clone(),
            };
            let content_index = output.content.len();
            if let Some(image_item) = image_item.as_ref() {
                reconcile_native_image_slot(
                    output_index,
                    content_index,
                    image_item,
                    output,
                    state,
                )?;
            }
            output
                .content
                .push(ContentBlock::ProviderNative(ProviderNativeContent {
                    subtype: item_type,
                    raw,
                }));
            state.output_slots.insert(
                output_index,
                Slot::new(SlotKind::ProviderNative, content_index),
            );
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
    let previous_chars = state
        .native_image_chars_by_output_index
        .get(&output_index)
        .copied()
        .unwrap_or(0);
    let next_chars = field_str(&reconciled, "result").map(str::len).unwrap_or(0);
    let next_total = state
        .native_image_base64_chars
        .saturating_sub(previous_chars)
        + next_chars;
    if next_total > MAX_NATIVE_IMAGE_BASE64_CHARS {
        scrub_native_image_results(output, state);
        return Err(ResponsesStreamError::new(
            "Native image generation results exceed the 24 MiB base64 limit",
        ));
    }
    state.native_image_base64_chars = next_total;
    if next_chars > 0 {
        state
            .native_image_chars_by_output_index
            .insert(output_index, next_chars);
    } else {
        state
            .native_image_chars_by_output_index
            .remove(&output_index);
    }
    if let Some(ContentBlock::ProviderNative(block)) = output.content.get_mut(content_index) {
        block.subtype = String::from("image_generation_call");
        block.raw = reconciled;
    }
    Ok(())
}

fn scrub_native_image_results(output: &mut AssistantMessage, state: &mut StreamState) {
    for block in output.content.iter_mut() {
        let ContentBlock::ProviderNative(native) = block else {
            continue;
        };
        if native.subtype != "image_generation_call" {
            continue;
        }
        let Some(item) = read_native_image_generation_call(&native.raw) else {
            continue;
        };
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
    if let Some(hook) = options.on_provider_stream_event {
        hook(&event, model);
    }
    let event_type = field_str(&event, "type").unwrap_or_default().to_owned();
    let output_index = field_usize(&event, "output_index").unwrap_or(0);

    let apply_message_phase_stop_reason = |item: &Value, output: &mut AssistantMessage| {
        if field_str(item, "type") == Some("message")
            && field_str(item, "phase") == Some("final_answer")
        {
            output.stop_reason = StopReason::Stop;
        }
    };

    match event_type.as_str() {
        "response.created" => {
            if let Some(id) = event
                .get("response")
                .and_then(|response| field_str(response, "id"))
            {
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
            if let Some(content_index) =
                get_slot(state, output_index, SlotKind::Thinking).map(|slot| slot.content_index)
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
            if let Some(content_index) =
                get_slot(state, output_index, SlotKind::Thinking).map(|slot| slot.content_index)
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
            if let Some(content_index) =
                get_slot(state, output_index, SlotKind::Text).map(|slot| slot.content_index)
            {
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
            let Some(slot) = state.output_slots.get(&output_index) else {
                return Ok(());
            };
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
                block.arguments = parse_streaming_json(Some(&partial_json))
                    .as_object()
                    .cloned()
                    .unwrap_or_default();
            }
            let slot = state.output_slots.get(&output_index).expect("checked");
            push_tool_call_delta(slot, Some(delta), output, stream);
        }
        "response.function_call_arguments.done" => {
            let arguments = field_str(&event, "arguments")
                .unwrap_or_default()
                .to_owned();
            let Some(slot) = state.output_slots.get(&output_index) else {
                return Ok(());
            };
            if slot.kind != SlotKind::ToolCall || slot.partial_json.is_none() {
                return Ok(());
            }
            let content_index = slot.content_index;
            let previous_partial_json = slot.partial_json.clone().unwrap_or_default();
            if let Some(slot) = state.output_slots.get_mut(&output_index) {
                slot.partial_json = Some(arguments.clone());
            }
            if let Some(ContentBlock::ToolCall(block)) = output.content.get_mut(content_index) {
                block.arguments = parse_streaming_json(Some(&arguments))
                    .as_object()
                    .cloned()
                    .unwrap_or_default();
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
            let Some(slot) = state.output_slots.get(&output_index) else {
                return Ok(());
            };
            if slot.kind != SlotKind::ToolCall || slot.custom_input.is_none() {
                return Ok(());
            }
            let content_index = slot.content_index;
            let next_input = format!(
                "{}{delta}",
                get_custom_tool_call_input(output, state, output_index)
            );
            let delta =
                append_custom_tool_call_input(output, state, output_index, &next_input, false)?;
            let slot = Slot {
                kind: SlotKind::ToolCall,
                content_index,
                partial_json: None,
                custom_input: None,
            };
            push_tool_call_delta(&slot, delta, output, stream);
        }
        "response.custom_tool_call_input.done" => {
            let input = field_str(&event, "input").unwrap_or_default().to_owned();
            let Some(slot) = state.output_slots.get(&output_index) else {
                return Ok(());
            };
            if slot.kind != SlotKind::ToolCall || slot.custom_input.is_none() {
                return Ok(());
            }
            let content_index = slot.content_index;
            let delta = append_custom_tool_call_input(output, state, output_index, &input, true)?;
            let slot = Slot {
                kind: SlotKind::ToolCall,
                content_index,
                partial_json: None,
                custom_input: None,
            };
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
            let content_index = state
                .output_slots
                .get(&output_index)
                .map(|slot| slot.content_index);

            if let (Some(image_item), Some(SlotKind::ProviderNative), Some(content_index)) =
                (image_item.as_ref(), kind, content_index)
            {
                reconcile_native_image_slot(
                    output_index,
                    content_index,
                    image_item,
                    output,
                    state,
                )?;
                state
                    .finalized_native_image_output_indexes
                    .insert(output_index);
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
                let signature = encode_text_signature_v1(
                    field_str(&item, "id"),
                    text_phase(field_str(&item, "phase")),
                );
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
                && state
                    .output_slots
                    .get(&output_index)
                    .is_some_and(|slot| slot.partial_json.is_some())
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
                        if slot_partial_json.is_empty() {
                            String::from("{}")
                        } else {
                            slot_partial_json
                        }
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
                && state
                    .output_slots
                    .get(&output_index)
                    .is_some_and(|slot| slot.custom_input.is_some())
            {
                let content_index = content_index.expect("kind implies a slot");
                let next_input = field_str(&item, "input")
                    .map(str::to_owned)
                    .unwrap_or_else(|| get_custom_tool_call_input(output, state, output_index));
                let delta =
                    append_custom_tool_call_input(output, state, output_index, &next_input, true)?;
                let namespace = field_str(&item, "namespace").map(str::to_owned);
                if let Some(ContentBlock::ToolCall(block)) = output.content.get_mut(content_index)
                    && let Some(namespace) = namespace
                {
                    block.namespace = Some(namespace);
                }
                if let Some(slot) = state.output_slots.get_mut(&output_index) {
                    slot.custom_input = None;
                }
                let slot = Slot {
                    kind: SlotKind::ToolCall,
                    content_index,
                    partial_json: None,
                    custom_input: None,
                };
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
                if let Some(ContentBlock::ProviderNative(block)) =
                    output.content.get_mut(content_index)
                {
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
            let nested = event.get("error");
            let code = nested
                .and_then(|error| error.get("code"))
                .filter(|code| !code.is_null())
                .or_else(|| event.get("code").filter(|code| !code.is_null()))
                .or_else(|| event.get("status").filter(|code| !code.is_null()));
            let message = nested
                .and_then(|error| js_truthy_str(error.get("message")))
                .or_else(|| js_truthy_str(event.get("message")))
                .unwrap_or_else(|| "Unknown error".to_owned());
            return Err(ResponsesStreamError::new(match code {
                Some(code) => format!("Error Code {}: {message}", js_template_string(Some(code))),
                None => message,
            }));
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
                    js_truthy_str(error.get("message"))
                        .unwrap_or_else(|| String::from("no message")),
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
    let response_output = response
        .get("output")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    backfill_reasoning_signatures(&response_output, output, state);
    backfill_native_image_generation_calls(&response_output, output, state)?;

    if let Some(id) = field_str(response, "id") {
        output.response_id = Some(id.to_owned());
    }
    if let Some(diagnostics) = response
        .get("prompt_cache_diagnostics")
        .and_then(openai_responses_prompt_cache::parse_prompt_cache_diagnostics)
    {
        output.prompt_cache_diagnostics = Some(diagnostics);
    }
    if let Some(usage) = response.get("usage").filter(|usage| usage.is_object()) {
        let input_details = usage.get("input_tokens_details");
        let cached_tokens = input_details
            .map(|details| field_u64(details, "cached_tokens"))
            .unwrap_or(0);
        let cache_write_tokens = input_details
            .map(|details| {
                details
                    .get("cache_write_tokens")
                    .filter(|value| !value.is_null())
                    .or_else(|| details.get("cache_creation_tokens"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
            })
            .unwrap_or(0);
        output.usage = Usage { context_usage: None,
            input: field_u64(usage, "input_tokens")
                .saturating_sub(cached_tokens)
                .saturating_sub(cache_write_tokens),
            output: field_u64(usage, "output_tokens"),
            cache_read: cached_tokens,
            cache_write: cache_write_tokens,
            cache_write_1h: None,
            reasoning: crate::utils::reported_count::reported_count(
                usage
                    .get("output_tokens_details")
                    .and_then(|details| details.get("reasoning_tokens")),
            )
            .map(|count| count.max(0.0) as u64),
            total_tokens: field_u64(usage, "total_tokens"),
            cost: Default::default(),
            ..Default::default()
        };
    }
    calculate_cost(model, &mut output.usage);
    if let Some(apply) = options.apply_service_tier_pricing {
        let resolved = options
            .resolve_service_tier
            .map(|resolve| resolve(field_str(response, "service_tier"), options.service_tier));
        apply(
            &mut output.usage,
            match resolved.as_ref() {
                Some(tier) => tier.as_deref(),
                None => field_str(response, "service_tier").or(options.service_tier),
            },
        );
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
        && output
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::ToolCall(_)))
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
        let Some(encrypted_content) = js_truthy_str(item.get("encrypted_content")) else {
            continue;
        };
        let Some(item_id) = field_str(item, "id") else {
            continue;
        };
        let Some(content_index) = state.reasoning_blocks_by_id.get(item_id) else {
            continue;
        };
        let Some(ContentBlock::Thinking(block)) = output.content.get(*content_index) else {
            continue;
        };
        let Some(signature) = block.thinking_signature.as_deref() else {
            continue;
        };
        let Some(stored_item) = parse_reasoning_signature(Some(signature)) else {
            continue;
        };
        if js_truthy_str(stored_item.get("encrypted_content")).is_some() {
            continue;
        }
        let Some(stored) = stored_item.as_object() else {
            continue;
        };
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
        if state
            .finalized_native_image_output_indexes
            .contains(&output_index)
        {
            continue;
        }
        let Some(image_item) = read_native_image_generation_call(output_item) else {
            continue;
        };
        match state
            .output_slots
            .get(&output_index)
            .filter(|slot| slot.kind == SlotKind::ProviderNative)
        {
            Some(slot) => {
                let content_index = slot.content_index;
                reconcile_native_image_slot(
                    output_index,
                    content_index,
                    &image_item,
                    output,
                    state,
                )?;
            }
            None => {
                let content_index = output.content.len();
                let raw = reconcile_native_image_generation_call(&image_item);
                reconcile_native_image_slot(
                    output_index,
                    content_index,
                    &image_item,
                    output,
                    state,
                )?;
                output
                    .content
                    .push(ContentBlock::ProviderNative(ProviderNativeContent {
                        subtype: String::from("image_generation_call"),
                        raw,
                    }));
                state.output_slots.insert(
                    output_index,
                    Slot::new(SlotKind::ProviderNative, content_index),
                );
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_model(api: &str, id: &str, capability: Option<Value>) -> Model {
        serde_json::from_value(json!({
            "id": id, "name": id, "api": api, "provider": "custom",
            "baseUrl": "", "reasoning": true, "input": ["text"],
            "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
            "contextWindow": 1000, "maxTokens": 100,
            "compat": capability.map(|value| json!({"supportsConfigurationUpdate": value})),
        }))
        .expect("model fixture")
    }

    #[test]
    fn grammar_replay_drops_function_item_ids() {
        let model = test_model("openai-responses", "test", None);
        let message: Message = serde_json::from_value(json!({
            "role":"assistant", "content":[{"type":"toolCall","id":"call_1|fc_1",
                "name":"query","arguments":{"input":"fixture"}}],
            "api":model.api,"provider":model.provider,"model":model.id,
            "usage":Usage::default(),"stopReason":"toolUse","timestamp":0
        }))
        .expect("message");
        let input = convert_responses_messages(
            &model,
            &Context {
                messages: vec![message],
                ..Default::default()
            },
            &BTreeSet::from(["custom".to_owned()]),
            &ConvertResponsesMessagesOptions {
                grammar_tool_input_properties: BTreeMap::from([("query".into(), "input".into())]),
                ..Default::default()
            },
        );
        assert_eq!(input[0]["type"], "custom_tool_call");
        assert!(input[0].get("id").is_none());
    }

    #[test]
    fn finalization_extracts_gateway_cache_writes_and_uses_the_tier_resolver() {
        let model = test_model("openai-responses", "test", None);
        let mut output: AssistantMessage = serde_json::from_value(json!({
            "content":[],"api":model.api,"provider":model.provider,"model":model.id,
            "usage":Usage::default(),"stopReason":"pending","timestamp":0}))
        .expect("output");
        let observed = std::sync::Mutex::new(Vec::new());
        let hook: crate::types::OnProviderStreamEvent = std::sync::Arc::new(|event, _| {
            assert_eq!(event["type"], "response.completed");
        });
        let price = |_: &mut Usage, tier: Option<&str>| {
            observed
                .lock()
                .expect("tiers")
                .push(tier.map(str::to_owned));
        };
        let resolve = |response: Option<&str>, request: Option<&str>| {
            assert_eq!(response, Some("default"));
            request.map(str::to_owned)
        };
        let options = ResponsesStreamOptions {
            on_provider_stream_event: Some(&hook),
            service_tier: Some("priority"),
            resolve_service_tier: Some(&resolve),
            apply_service_tier_pricing: Some(&price),
            ..Default::default()
        };
        process_responses_events([json!({"type":"response.completed","response":{
            "status":"completed","service_tier":"default",
            "prompt_cache_diagnostics":{"type":"miss","cache_missed_tokens":7},
            "usage":{"input_tokens":100,"input_tokens_details":{"cached_tokens":20,"cache_creation_tokens":30}}
        }})], &mut output, &crate::utils::event_stream::create_assistant_message_event_stream(), &model, &options).expect("completion");
        assert_eq!(
            (
                output.usage.input,
                output.usage.cache_read,
                output.usage.cache_write
            ),
            (50, 20, 30)
        );
        assert_eq!(
            observed.into_inner().expect("tiers"),
            vec![Some("priority".to_owned())]
        );
        assert_eq!(
            serde_json::to_value(output).expect("serialized")["promptCacheDiagnostics"],
            json!({"type":"miss","cacheMissedTokens":7.0})
        );
    }

    #[test]
    fn explicit_breakpoint_is_attached_only_to_the_initial_instruction() {
        let model = test_model("openai-responses", "test", None);
        let context = Context {
            system_prompt: Some("fixture".into()),
            ..Default::default()
        };
        let input = convert_responses_messages(
            &model,
            &context,
            &BTreeSet::new(),
            &ConvertResponsesMessagesOptions {
                system_prompt_cache_breakpoint: Some(true),
                ..Default::default()
            },
        );
        assert_eq!(input[0]["content"][0]["type"], "input_text");
        assert_eq!(
            input[0]["content"][0]["prompt_cache_breakpoint"],
            json!({"mode":"explicit"})
        );
    }

    #[test]
    fn web_search_replay_requires_same_model_and_a_terminal_valid_item() {
        let model = test_model("openai-responses", "test", None);
        for (source_model, status, action, expected) in [
            ("test", "completed", true, 1),
            ("test", "failed", true, 1),
            ("other", "completed", true, 0),
            ("test", "in_progress", true, 0),
            ("test", "completed", false, 0),
        ] {
            let mut raw = json!({"type":"web_search_call","id":"ws_1","status":status});
            if action {
                raw["action"] = json!({"type":"search","query":"fixture"});
            }
            let message: Message = serde_json::from_value(json!({"role":"assistant", "content":[{
                "type":"providerNative", "subtype":"web_search_call", "raw":raw}],
                "api":model.api,"provider":model.provider,"model":source_model,
                "usage":Usage::default(),"stopReason":"stop","timestamp":0}))
            .expect("message");
            let context = Context {
                messages: vec![message],
                ..Default::default()
            };
            let input =
                convert_responses_messages(&model, &context, &BTreeSet::new(), &Default::default());
            assert_eq!(input.len(), expected);
            if expected == 1 {
                assert_eq!(input[0], raw);
            }
        }
    }

    #[test]
    fn nested_error_fields_and_status_fallback_are_preserved() {
        let model = test_model("openai-responses", "test", None);
        for (event, expected) in [
            (
                json!({"type":"error","error":{"code":"nested","message":"detail"},"code":"outer"}),
                "Error Code nested: detail",
            ),
            (
                json!({"type":"error","status":429,"message":"limited"}),
                "Error Code 429: limited",
            ),
            (json!({"type":"error"}), "Unknown error"),
        ] {
            let mut output: AssistantMessage = serde_json::from_value(json!({
                "content":[],"api":model.api,"provider":model.provider,"model":model.id,
                "usage":Usage::default(),"stopReason":"pending","timestamp":0}))
            .expect("output");
            let error = process_responses_events(
                [event],
                &mut output,
                &crate::utils::event_stream::create_assistant_message_event_stream(),
                &model,
                &Default::default(),
            )
            .expect_err("error event");
            assert_eq!(error.message, expected);
        }
    }

    #[test]
    fn configuration_updates_require_explicit_capability_on_a_responses_api() {
        for api in [
            "openai-responses",
            "openai-codex-responses",
            "azure-openai-responses",
            "openai-completions",
        ] {
            for capability in [
                None,
                Some(json!(false)),
                Some(json!(true)),
                Some(json!("true")),
            ] {
                let model = test_model(api, "gpt-6-astra", capability.clone());
                let context = Context {
                    system_prompt: None,
                    messages: vec![Message::ConfigurationUpdate(
                        crate::types::ConfigurationUpdateMessage {
                            content: Vec::new(),
                            effort: "high".into(),
                            timestamp: 0,
                        },
                    )],
                    tools: None,
                    ..Context::default()
                };
                let input = convert_responses_messages(
                    &model,
                    &context,
                    &BTreeSet::new(),
                    &Default::default(),
                );
                let expected = if api != "openai-completions" && capability == Some(json!(true)) {
                    vec![json!({"type": "configuration_update", "reasoning": {"effort": "high"}})]
                } else {
                    Vec::new()
                };
                assert_eq!(input, expected, "{api}, {capability:?}");
            }
        }
    }

    #[test]
    fn adjacent_configuration_updates_coalesce_for_an_opted_in_custom_model() {
        let model = test_model(
            "azure-openai-responses",
            "custom-reasoner",
            Some(json!(true)),
        );
        let context = Context {
            system_prompt: None,
            messages: ["low", "high"]
                .into_iter()
                .map(|effort| {
                    Message::ConfigurationUpdate(crate::types::ConfigurationUpdateMessage {
                        content: Vec::new(),
                        effort: effort.into(),
                        timestamp: 0,
                    })
                })
                .collect(),
            tools: None,
            ..Context::default()
        };
        let input =
            convert_responses_messages(&model, &context, &BTreeSet::new(), &Default::default());
        assert_eq!(
            input,
            vec![json!({"type": "configuration_update", "reasoning": {"effort": "high"}})]
        );
    }

    #[test]
    fn tool_use_completion_rejects_unfinished_function_and_custom_calls() {
        for kind in ["function_call", "custom_tool_call"] {
            let model = test_model("openai-responses", "test", None);
            let mut output: AssistantMessage = serde_json::from_value(json!({
                "content": [], "api": model.api, "provider": model.provider, "model": model.id,
                "usage": Usage::default(), "stopReason": "pending", "timestamp": 0,
            }))
            .expect("output fixture");
            let events = [
                json!({"type": "response.output_item.added", "output_index": 0,
                    "item": {"type": kind, "id": "fc_1", "call_id": "call_1", "name": "run"}}),
                json!({"type": "response.completed", "response": {"status": "completed"}}),
            ];
            let result = process_responses_events(
                events,
                &mut output,
                &AssistantMessageEventStream::assistant(),
                &model,
                &Default::default(),
            );
            assert!(result.is_err(), "{kind}");
        }
    }

    #[test]
    fn completed_calls_and_length_limited_partial_calls_are_accepted() {
        for kind in ["function_call", "custom_tool_call"] {
            for finalized in [false, true] {
                let model = test_model("openai-responses", "test", None);
                let mut output: AssistantMessage = serde_json::from_value(json!({
                    "content": [], "api": model.api, "provider": model.provider, "model": model.id,
                    "usage": Usage::default(), "stopReason": "pending", "timestamp": 0,
                }))
                .expect("output fixture");
                let item = json!({"type": kind, "id": "fc_1", "call_id": "call_1", "name": "run", "arguments": "{}", "input": "text"});
                let mut events = vec![
                    json!({"type": "response.output_item.added", "output_index": 0, "item": item}),
                ];
                if finalized {
                    events.push(json!({"type": "response.output_item.done", "output_index": 0, "item": item}));
                }
                events.push(json!({"type": "response.completed", "response": {
                    "status": if finalized { "completed" } else { "incomplete" },
                    "incomplete_details": {"reason": "max_output_tokens"},
                }}));
                let result = process_responses_events(
                    events,
                    &mut output,
                    &AssistantMessageEventStream::assistant(),
                    &model,
                    &Default::default(),
                );
                assert!(result.is_ok(), "{kind}, finalized={finalized}: {result:?}");
            }
        }
    }

    /// The resumed search must split frames exactly like the previous full rescan from
    /// offset 0 after every chunk, for every chunking (boundaries split across chunks,
    /// multibyte text near the resume offset).
    #[test]
    fn incremental_boundary_search_matches_full_rescan() {
        let input = "data: {\"a\":\"\u{e9}\u{1f600}\"}\r\n\r\ndata: 1\n\ndata: 2\r\rdata: \u{1f600}\n\r\ndata: 3\r\n\n";
        for chunk_len in 1..=input.len() {
            let mut chunks = Vec::new();
            let mut start = 0;
            while start < input.len() {
                let mut end = (start + chunk_len).min(input.len());
                while !input.is_char_boundary(end) {
                    end += 1;
                }
                chunks.push(&input[start..end]);
                start = end;
            }

            let mut expected = Vec::new();
            let mut full = String::new();
            for chunk in &chunks {
                full.push_str(chunk);
                while let Some((index, length)) = find_sse_boundary(&full, 0) {
                    expected.push(full[..index].to_owned());
                    full = full[index + length..].to_owned();
                }
            }

            let mut frames = Vec::new();
            let mut buffer = String::new();
            let mut search_from = 0;
            for chunk in &chunks {
                buffer.push_str(chunk);
                while let Some((index, length)) = find_sse_boundary(&buffer, search_from) {
                    frames.push(buffer[..index].to_owned());
                    buffer.drain(..index + length);
                    search_from = 0;
                }
                search_from = sse_boundary_resume_offset(&buffer);
            }
            assert_eq!(frames, expected, "chunk_len {chunk_len}");
            assert_eq!(buffer, full, "chunk_len {chunk_len}");
        }
    }
}
