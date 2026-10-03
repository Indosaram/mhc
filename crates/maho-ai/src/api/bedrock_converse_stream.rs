//! Port of senpi packages/ai/src/api/bedrock-converse-stream.ts.
//!
//! senpi drives Bedrock through `@aws-sdk/client-bedrock-runtime`. This crate has no AWS SDK, so
//! the same wire contract is implemented directly over `reqwest`: SigV4 (or bearer-token) signed
//! `POST {endpoint}/model/{modelId}/converse-stream` plus the AWS event-stream binary framing.

use base64::Engine;
use serde_json::{json, Map, Value};

use crate::api::constrained_sampling::{get_json_schema_tool_parameters, resolve_json_schema_strict_sampling};
use crate::api::simple_options::{
    adjust_max_tokens_for_thinking, apply_extra_body, build_base_options, clamp_max_tokens_to_context, clamp_reasoning,
    BEDROCK_RESERVED_BODY_KEYS,
};
use crate::api::transform_messages::{transform_messages, TransformMessagesOptions};
use crate::models::calculate_cost;
use crate::types::{
    AssistantMessage, AssistantMessageEvent, CacheRetention, ContentBlock, Context, DoneReason, ErrorReason, Model,
    ProviderEnv, SimpleStreamOptions, StopReason, StreamOptions, TextContent, ThinkingBudgets, ThinkingContent,
    ThinkingLevel, Tool, ToolCall,
};
use crate::utils::diagnostics::{append_assistant_message_diagnostic, now_ms, AssistantMessageDiagnostic};
use crate::utils::event_stream::{create_assistant_message_event_stream, AssistantMessageEventStream};
use crate::utils::json_parse::parse_streaming_json;
use crate::utils::prompt_cache_ttl::{
    get_bedrock_model_match_candidates, supports_one_hour_cache_ttl, supports_prompt_caching,
};
use crate::utils::provider_env::get_provider_env_value;
use crate::utils::sanitize_unicode::sanitize_surrogates;
use crate::utils::tool_call_id::normalize_tool_call_id;

pub use crate::utils::prompt_cache_ttl::supports_prompt_caching as bedrock_supports_prompt_caching;

pub const EMPTY_TEXT_PLACEHOLDER: &str = "<empty>";
pub const REDACTED_THINKING_PLACEHOLDER: &str = "[Reasoning redacted]";

const BEDROCK_DATA_RETENTION_DOCS_URL: &str = "https://docs.aws.amazon.com/bedrock/latest/userguide/data-retention.html";
const MAX_BEDROCK_DIAGNOSTIC_VALUE_CHARS: usize = 200;
const DEFAULT_REGION: &str = "us-east-1";

fn bedrock_error_prefix(name: &str) -> Option<&'static str> {
    match name {
        "InternalServerException" => Some("Internal server error"),
        "ModelStreamErrorException" => Some("Model stream error"),
        "ValidationException" => Some("Validation error"),
        "ThrottlingException" => Some("Throttling error"),
        "ServiceUnavailableException" => Some("Service unavailable"),
        _ => None,
    }
}

#[derive(Debug, Clone)]
struct BedrockFailure {
    message: String,
    code: Option<String>,
    status: Option<u16>,
    request_id: Option<String>,
}

fn thinking_level_from_str(level: &str) -> Option<ThinkingLevel> {
    match level {
        "minimal" => Some(ThinkingLevel::Minimal),
        "low" => Some(ThinkingLevel::Low),
        "medium" => Some(ThinkingLevel::Medium),
        "high" => Some(ThinkingLevel::High),
        "xhigh" => Some(ThinkingLevel::Xhigh),
        "max" => Some(ThinkingLevel::Max),
        _ => None,
    }
}

fn thinking_level_as_str(level: ThinkingLevel) -> &'static str {
    match level {
        ThinkingLevel::Minimal => "minimal",
        ThinkingLevel::Low => "low",
        ThinkingLevel::Medium => "medium",
        ThinkingLevel::High => "high",
        ThinkingLevel::Xhigh => "xhigh",
        ThinkingLevel::Max => "max",
    }
}

fn format_bedrock_error(failure: &BedrockFailure) -> String {
    let data_retention_hint = if failure.message.to_lowercase().contains("data retention mode") {
        format!(" See {BEDROCK_DATA_RETENTION_DOCS_URL} for supported data retention modes.")
    } else {
        String::new()
    };
    match failure.code.as_deref().and_then(bedrock_error_prefix) {
        Some(prefix) => format!("{prefix}: {}{data_retention_hint}", failure.message),
        None => match &failure.code {
            Some(code) if code.ends_with("Exception") => {
                format!("{code}: {}{data_retention_hint}", failure.message)
            }
            _ => format!("{}{data_retention_hint}", failure.message),
        },
    }
}

fn normalize_diagnostic_value(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.chars().count() > MAX_BEDROCK_DIAGNOSTIC_VALUE_CHARS {
        return None;
    }
    Some(trimmed.to_owned())
}

fn append_bedrock_failure_diagnostic(output: &mut AssistantMessage, failure: &BedrockFailure) {
    let mut details = Map::new();
    if let Some(status) = failure.status {
        details.insert("status".into(), Value::from(status));
    }
    if let Some(code) = failure.code.as_deref().and_then(normalize_diagnostic_value) {
        details.insert("errorCode".into(), Value::from(code));
    }
    if let Some(request_id) = failure.request_id.as_deref().and_then(normalize_diagnostic_value) {
        details.insert("requestId".into(), Value::from(request_id));
    }
    if details.is_empty() {
        return;
    }
    append_assistant_message_diagnostic(
        &mut output.diagnostics,
        AssistantMessageDiagnostic {
            kind: "bedrock_response_failure".into(),
            timestamp: now_ms(),
            error: None,
            details: Some(details),
        },
    );
}

fn create_output(model: &Model) -> AssistantMessage {
    AssistantMessage {
        content: Vec::new(),
        api: "bedrock-converse-stream".into(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        diagnostics: None,
        usage: crate::types::Usage::default(),
        stop_reason: StopReason::Pending,
        stop_details: None,
        deferred: None,
        error_message: None,
        abort_source: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: now_ms(),
    }
}

#[derive(Debug, Clone, Default)]
struct BlockMeta {
    content_block_index: Option<usize>,
    partial_json: String,
    redacted_chunks: Option<Vec<Vec<u8>>>,
}

fn base64_decode(data: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD.decode(data).ok()
}

fn base64_encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn flush_redacted_content(meta: &mut BlockMeta, block: &mut ContentBlock) {
    let Some(chunks) = meta.redacted_chunks.take() else { return };
    if let ContentBlock::Thinking(thinking) = block {
        let joined: Vec<u8> = chunks.concat();
        thinking.thinking_signature = Some(base64_encode(&joined));
    }
}

fn finalize_streaming_block(meta: &mut BlockMeta, block: &mut ContentBlock) {
    meta.content_block_index = None;
    meta.partial_json.clear();
    flush_redacted_content(meta, block);
}

fn decode_redacted_content(signature: Option<&str>) -> Option<Vec<u8>> {
    base64_decode(signature?)
}

fn map_stop_reason(reason: Option<&str>) -> (StopReason, Option<String>) {
    match reason {
        Some("end_turn") | Some("stop_sequence") => (StopReason::Stop, None),
        Some("max_tokens") | Some("model_context_window_exceeded") => (StopReason::Length, None),
        Some("tool_use") => (StopReason::ToolUse, None),
        Some(other) => (StopReason::Error, Some(format!("Provider stopped with: {other}"))),
        None => (StopReason::Error, None),
    }
}

fn resolve_cache_retention(cache_retention: Option<CacheRetention>, env: Option<&ProviderEnv>) -> CacheRetention {
    if let Some(cache_retention) = cache_retention {
        return cache_retention;
    }
    if get_provider_env_value("PI_CACHE_RETENTION", env).as_deref() == Some("long") {
        return CacheRetention::Long;
    }
    CacheRetention::Short
}

fn is_anthropic_claude_model(model: &Model) -> bool {
    let id = model.id.to_lowercase();
    let name = model.name.to_lowercase();
    id.contains("anthropic.claude")
        || id.contains("anthropic/claude")
        || name.contains("anthropic.claude")
        || name.contains("anthropic/claude")
        || name.contains("claude")
}

fn supports_adaptive_thinking(model: &Model) -> bool {
    get_bedrock_model_match_candidates(&model.id, Some(&model.name)).iter().any(|candidate| {
        candidate.contains("opus-4-6")
            || candidate.contains("opus-4-7")
            || candidate.contains("opus-4-8")
            || candidate.contains("opus-5")
            || candidate.contains("sonnet-4-6")
            || candidate.contains("sonnet-5")
            || candidate.contains("fable-5")
            || candidate.contains("mythos-5")
    })
}

fn supports_native_xhigh_effort(model: &Model) -> bool {
    get_bedrock_model_match_candidates(&model.id, Some(&model.name)).iter().any(|candidate| {
        candidate.contains("opus-4-7")
            || candidate.contains("opus-4-8")
            || candidate.contains("opus-5")
            || candidate.contains("sonnet-5")
            || candidate.contains("fable-5")
            || candidate.contains("mythos-5")
    })
}

fn rejects_disabled_thinking(model: &Model) -> bool {
    if model
        .thinking_level_map
        .as_ref()
        .and_then(|map| map.get(&crate::types::ModelThinkingLevel::Off))
        .is_some_and(Option::is_none)
    {
        return true;
    }
    get_bedrock_model_match_candidates(&model.id, Some(&model.name))
        .iter()
        .any(|candidate| {
            candidate.contains("fable-5")
                || candidate.contains("mythos-5")
                || candidate.contains("opus-5-5")
                || candidate.contains("opus-5.5")
        })
}

fn map_thinking_level_to_effort(model: &Model, level: Option<ThinkingLevel>) -> &'static str {
    if level == Some(ThinkingLevel::Xhigh) && supports_native_xhigh_effort(model) {
        return "xhigh";
    }

    if let Some(level) = level
        && let Some(mapped) = model
            .thinking_level_map
            .as_ref()
            .and_then(|map| map.get(&crate::types::ModelThinkingLevel::from(level)))
            .cloned()
            .flatten()
    {
        return match mapped.as_str() {
            "low" => "low",
            "medium" => "medium",
            "high" => "high",
            "xhigh" => "xhigh",
            "max" => "max",
            _ => "high",
        };
    }

    match level {
        Some(ThinkingLevel::Minimal) | Some(ThinkingLevel::Low) => "low",
        Some(ThinkingLevel::Medium) => "medium",
        Some(ThinkingLevel::High) => "high",
        Some(ThinkingLevel::Xhigh) | Some(ThinkingLevel::Max) => "max",
        None => "high",
    }
}

fn create_non_blank_text_block(text: &str) -> Option<Value> {
    let sanitized = sanitize_surrogates(text);
    if sanitized.trim().is_empty() {
        None
    } else {
        Some(json!({ "text": sanitized }))
    }
}

fn create_required_text_block(text: &str) -> Value {
    create_non_blank_text_block(text).unwrap_or_else(|| json!({ "text": EMPTY_TEXT_PLACEHOLDER }))
}

fn sanitize_bedrock_document(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(sanitize_bedrock_document).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(key, _)| !key.is_empty())
                .map(|(key, nested)| (key.clone(), sanitize_bedrock_document(nested)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn create_image_block(mime_type: &str, data: &str) -> Result<Value, String> {
    let format = match mime_type {
        "image/jpeg" | "image/jpg" => "jpeg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        other => return Err(format!("Unknown image type: {other}")),
    };
    Ok(json!({ "source": { "bytes": data }, "format": format }))
}

fn convert_tool_result_content(content: &[ContentBlock]) -> Result<Vec<Value>, String> {
    let mut result: Vec<Value> = Vec::new();
    for block in content {
        match block {
            ContentBlock::Image(image) => result.push(json!({ "image": create_image_block(&image.mime_type, &image.data)? })),
            ContentBlock::Text(text) => {
                if let Some(text_block) = create_non_blank_text_block(&text.text) {
                    result.push(text_block);
                }
            }
            _ => {}
        }
    }
    if result.is_empty() {
        result.push(json!({ "text": EMPTY_TEXT_PLACEHOLDER }));
    }
    Ok(result)
}

fn cache_point(cache_retention: CacheRetention, model: &Model) -> Value {
    if cache_retention == CacheRetention::Long && supports_one_hour_cache_ttl(model) {
        json!({ "cachePoint": { "type": "default", "ttl": "1h" } })
    } else {
        json!({ "cachePoint": { "type": "default" } })
    }
}

fn build_system_prompt(
    system_prompt: Option<&str>,
    model: &Model,
    cache_retention: CacheRetention,
    env: Option<&ProviderEnv>,
) -> Option<Vec<Value>> {
    let system_prompt = system_prompt?;
    if system_prompt.is_empty() {
        return None;
    }

    let mut blocks = vec![json!({ "text": sanitize_surrogates(system_prompt) })];
    if cache_retention != CacheRetention::None && supports_prompt_caching(model, env) {
        blocks.push(cache_point(cache_retention, model));
    }
    Some(blocks)
}

fn convert_messages(
    context: &Context,
    model: &Model,
    cache_retention: CacheRetention,
    preserve_thinking: bool,
    env: Option<&ProviderEnv>,
) -> Result<Vec<Value>, String> {
    let normalize = |id: &str, _model: &Model, _assistant: &AssistantMessage| normalize_tool_call_id(id);
    let transformed = transform_messages(
        &context.messages,
        model,
        Some(&normalize),
        &TransformMessagesOptions { preserve_thinking: Some(preserve_thinking), ..TransformMessagesOptions::default() },
    );

    let mut result: Vec<Value> = Vec::new();
    let mut index = 0;
    while index < transformed.len() {
        match &transformed[index] {
            crate::types::Message::User(user) => {
                let mut content: Vec<Value> = Vec::new();
                match &user.content {
                    crate::types::UserContent::Text(text) => content.push(create_required_text_block(text)),
                    crate::types::UserContent::Blocks(blocks) => {
                        for block in blocks {
                            match block {
                                ContentBlock::Text(text) => {
                                    if let Some(text_block) = create_non_blank_text_block(&text.text) {
                                        content.push(text_block);
                                    }
                                }
                                ContentBlock::Image(image) => {
                                    content.push(json!({ "image": create_image_block(&image.mime_type, &image.data)? }))
                                }
                                _ => {}
                            }
                        }
                        if content.is_empty() {
                            content.push(json!({ "text": EMPTY_TEXT_PLACEHOLDER }));
                        }
                    }
                }
                result.push(json!({ "role": "user", "content": content }));
            }
            crate::types::Message::Assistant(assistant) => {
                if assistant.content.is_empty() {
                    index += 1;
                    continue;
                }
                let mut content_blocks: Vec<Value> = Vec::new();
                for block in &assistant.content {
                    match block {
                        ContentBlock::Text(text) => {
                            if let Some(text_block) = create_non_blank_text_block(&text.text) {
                                content_blocks.push(text_block);
                            }
                        }
                        ContentBlock::ToolCall(tool_call) => content_blocks.push(json!({
                            "toolUse": {
                                "toolUseId": tool_call.id,
                                "name": tool_call.name,
                                "input": sanitize_bedrock_document(&Value::Object(tool_call.arguments.clone())),
                            }
                        })),
                        ContentBlock::Thinking(thinking) => {
                            if thinking.redacted == Some(true) {
                                if let Some(bytes) = decode_redacted_content(thinking.thinking_signature.as_deref())
                                    && !bytes.is_empty()
                                {
                                    content_blocks.push(json!({ "reasoningContent": { "redactedContent": base64_encode(&bytes) } }));
                                }
                                continue;
                            }
                            let text = sanitize_surrogates(&thinking.thinking);
                            if text.trim().is_empty() {
                                continue;
                            }
                            if is_anthropic_claude_model(model) {
                                let signature = thinking.thinking_signature.as_deref().unwrap_or_default();
                                if signature.trim().is_empty() {
                                    content_blocks.push(json!({ "text": text }));
                                } else {
                                    content_blocks.push(json!({
                                        "reasoningContent": { "reasoningText": { "text": text, "signature": signature } }
                                    }));
                                }
                            } else {
                                content_blocks.push(json!({ "reasoningContent": { "reasoningText": { "text": text } } }));
                            }
                        }
                        _ => {}
                    }
                }
                if content_blocks.is_empty() {
                    index += 1;
                    continue;
                }
                result.push(json!({ "role": "assistant", "content": content_blocks }));
            }
            crate::types::Message::ToolResult(tool_result) => {
                let mut tool_results: Vec<Value> = Vec::new();
                tool_results.push(json!({
                    "toolResult": {
                        "toolUseId": tool_result.tool_call_id,
                        "content": convert_tool_result_content(&tool_result.content)?,
                        "status": if tool_result.is_error { "error" } else { "success" },
                    }
                }));

                let mut lookahead = index + 1;
                while lookahead < transformed.len() {
                    let crate::types::Message::ToolResult(next) = &transformed[lookahead] else { break };
                    tool_results.push(json!({
                        "toolResult": {
                            "toolUseId": next.tool_call_id,
                            "content": convert_tool_result_content(&next.content)?,
                            "status": if next.is_error { "error" } else { "success" },
                        }
                    }));
                    lookahead += 1;
                }
                index = lookahead - 1;

                result.push(json!({ "role": "user", "content": tool_results }));
            }
            crate::types::Message::ConfigurationUpdate(_) => {}
        }
        index += 1;
    }

    if cache_retention != CacheRetention::None && supports_prompt_caching(model, env) && !result.is_empty() {
        let last = result.len() - 1;
        if result[last].get("role").and_then(Value::as_str) == Some("user") {
            let point = cache_point(cache_retention, model);
            if let Some(content) = result[last].get_mut("content").and_then(Value::as_array_mut) {
                content.push(point);
            }
        }
    }

    Ok(result)
}

fn convert_tool_config(
    tools: Option<&[Tool]>,
    tool_choice: Option<&Value>,
    supports_strict_mode: bool,
) -> Result<Option<Value>, String> {
    let Some(tools) = tools.filter(|tools| !tools.is_empty()) else { return Ok(None) };
    if tool_choice.and_then(Value::as_str) == Some("none") {
        return Ok(None);
    }

    let mut bedrock_tools: Vec<Value> = Vec::new();
    for tool in tools {
        let strict = resolve_json_schema_strict_sampling(tool, supports_strict_mode)?;
        let parameters = get_json_schema_tool_parameters(tool, strict).map_err(|error| error.to_string())?;
        let mut spec = Map::new();
        spec.insert("name".into(), Value::from(tool.name.clone()));
        spec.insert("description".into(), Value::from(tool.description.clone()));
        spec.insert("inputSchema".into(), json!({ "json": parameters }));
        if strict == Some(true) {
            spec.insert("strict".into(), Value::Bool(true));
        }
        bedrock_tools.push(json!({ "toolSpec": Value::Object(spec) }));
    }

    let bedrock_tool_choice = match tool_choice {
        Some(Value::String(choice)) if choice == "auto" => Some(json!({ "auto": {} })),
        Some(Value::String(choice)) if choice == "any" => Some(json!({ "any": {} })),
        Some(Value::Object(choice)) if choice.get("type").and_then(Value::as_str) == Some("tool") => {
            Some(json!({ "tool": { "name": choice.get("name").cloned().unwrap_or(Value::Null) } }))
        }
        _ => None,
    };

    let mut config = Map::new();
    config.insert("tools".into(), Value::Array(bedrock_tools));
    if let Some(choice) = bedrock_tool_choice {
        config.insert("toolChoice".into(), choice);
    }
    Ok(Some(Value::Object(config)))
}

fn get_configured_bedrock_region(options: &StreamOptions) -> Option<String> {
    option_str(options, "region")
        .or_else(|| get_provider_env_value("AWS_REGION", options.request.env.as_ref()))
        .or_else(|| get_provider_env_value("AWS_DEFAULT_REGION", options.request.env.as_ref()))
}

fn option_str(options: &StreamOptions, key: &str) -> Option<String> {
    options.extra.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn option_bool(options: &StreamOptions, key: &str) -> Option<bool> {
    options.extra.get(key).and_then(Value::as_bool)
}

fn get_configured_bedrock_credentials(env: Option<&ProviderEnv>) -> Option<(String, String, Option<String>)> {
    let access_key_id = get_provider_env_value("AWS_ACCESS_KEY_ID", env)?;
    let secret_access_key = get_provider_env_value("AWS_SECRET_ACCESS_KEY", env)?;
    Some((access_key_id, secret_access_key, get_provider_env_value("AWS_SESSION_TOKEN", env)))
}

fn get_standard_bedrock_endpoint_region(base_url: &str) -> Option<String> {
    let url = url::Url::parse(base_url).ok()?;
    let hostname = url.host_str()?.to_lowercase();
    let rest = hostname.strip_prefix("bedrock-runtime")?;
    let rest = rest.strip_prefix("-fips").unwrap_or(rest);
    let rest = rest.strip_prefix('.')?;
    let region = rest.strip_suffix(".amazonaws.com").or_else(|| rest.strip_suffix(".amazonaws.com.cn"))?;
    if region.is_empty() {
        return None;
    }
    Some(region.to_owned())
}

fn should_use_explicit_bedrock_endpoint(
    base_url: &str,
    configured_region: Option<&str>,
    has_ambient_configured_profile: bool,
) -> bool {
    if get_standard_bedrock_endpoint_region(base_url).is_none() {
        return true;
    }
    configured_region.is_none() && !has_ambient_configured_profile
}

fn is_gov_cloud_bedrock_target(model: &Model, options: &StreamOptions) -> bool {
    if let Some(region) = get_configured_bedrock_region(options)
        && region.to_lowercase().starts_with("us-gov-")
    {
        return true;
    }
    let model_id = model.id.to_lowercase();
    model_id.starts_with("us-gov.") || model_id.starts_with("arn:aws-us-gov:")
}

fn build_additional_model_request_fields(model: &Model, options: &StreamOptions) -> Option<Value> {
    if !model.reasoning {
        return None;
    }

    let reasoning = option_str(options, "reasoning").and_then(|level| thinking_level_from_str(&level));

    if reasoning.is_none() {
        if !is_anthropic_claude_model(model) || !supports_adaptive_thinking(model) {
            return None;
        }
        return Some(if rejects_disabled_thinking(model) {
            json!({ "output_config": { "effort": "low" } })
        } else {
            json!({ "thinking": { "type": "disabled" } })
        });
    }

    if !is_anthropic_claude_model(model) {
        return None;
    }

    let display = if is_gov_cloud_bedrock_target(model, options) {
        None
    } else {
        Some(option_str(options, "thinkingDisplay").unwrap_or_else(|| "summarized".to_owned()))
    };

    let reasoning = reasoning.expect("reasoning present");
    let mut result = if supports_adaptive_thinking(model) {
        let mut thinking = Map::new();
        thinking.insert("type".into(), Value::from("adaptive"));
        if let Some(display) = &display {
            thinking.insert("display".into(), Value::from(display.clone()));
        }
        json!({
            "thinking": Value::Object(thinking),
            "output_config": { "effort": map_thinking_level_to_effort(model, Some(reasoning)) },
        })
    } else {
        let default_budgets: [(&str, u64); 6] = [
            ("minimal", 1024),
            ("low", 2048),
            ("medium", 8192),
            ("high", 16384),
            ("xhigh", 16384),
            ("max", 16384),
        ];
        let level = match reasoning {
            ThinkingLevel::Xhigh | ThinkingLevel::Max => "high",
            other => thinking_level_as_str(other),
        };
        let budget = options
            .extra
            .get("thinkingBudgets")
            .and_then(|budgets| budgets.get(level))
            .and_then(Value::as_u64)
            .or_else(|| default_budgets.iter().find(|(name, _)| *name == thinking_level_as_str(reasoning)).map(|(_, budget)| *budget))
            .unwrap_or(0);
        let mut thinking = Map::new();
        thinking.insert("type".into(), Value::from("enabled"));
        thinking.insert("budget_tokens".into(), Value::from(budget));
        if let Some(display) = &display {
            thinking.insert("display".into(), Value::from(display.clone()));
        }
        json!({ "thinking": Value::Object(thinking) })
    };

    if !supports_adaptive_thinking(model)
        && option_bool(options, "interleavedThinking").unwrap_or(true)
        && let Some(object) = result.as_object_mut()
    {
        object.insert("anthropic_beta".into(), json!(["interleaved-thinking-2025-05-14"]));
    }

    Some(result)
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};

    let mut key_block = [0u8; 64];
    if key.len() > 64 {
        let mut hasher = Sha256::new();
        hasher.update(key);
        let digest = hasher.finalize();
        key_block[..32].copy_from_slice(&digest);
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }

    let mut inner_pad = [0x36u8; 64];
    let mut outer_pad = [0x5cu8; 64];
    for index in 0..64 {
        inner_pad[index] ^= key_block[index];
        outer_pad[index] ^= key_block[index];
    }

    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(message);
    let inner_digest = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_digest);
    outer.finalize().into()
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

struct SigV4Request<'a> {
    access_key_id: &'a str,
    secret_access_key: &'a str,
    session_token: Option<&'a str>,
    region: &'a str,
    host: &'a str,
    canonical_path: &'a str,
    payload: &'a [u8],
    amz_date: &'a str,
}

fn sigv4_authorization(request: &SigV4Request<'_>) -> String {
    let SigV4Request { access_key_id, secret_access_key, session_token, region, host, canonical_path, payload, amz_date } =
        request;
    let date = &amz_date[..8];
    let payload_hash = sha256_hex(payload);

    let mut signed_headers: Vec<(&str, String)> = vec![
        ("content-type", "application/json".to_owned()),
        ("host", (*host).to_owned()),
        ("x-amz-content-sha256", payload_hash.clone()),
        ("x-amz-date", (*amz_date).to_owned()),
    ];
    if let Some(session_token) = session_token {
        signed_headers.push(("x-amz-security-token", (*session_token).to_owned()));
    }
    signed_headers.sort_by(|a, b| a.0.cmp(b.0));

    let canonical_headers: String = signed_headers
        .iter()
        .map(|(name, value)| format!("{name}:{}\n", value.trim()))
        .collect();
    let signed_header_names: String =
        signed_headers.iter().map(|(name, _)| *name).collect::<Vec<_>>().join(";");

    let canonical_request = format!(
        "POST\n{canonical_path}\n\n{canonical_headers}\n{signed_header_names}\n{payload_hash}"
    );
    let credential_scope = format!("{date}/{region}/bedrock/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{credential_scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );

    let mut signing_key = hmac_sha256(format!("AWS4{secret_access_key}").as_bytes(), date.as_bytes()).to_vec();
    for part in [region, "bedrock", "aws4_request"] {
        signing_key = hmac_sha256(&signing_key, part.as_bytes()).to_vec();
    }
    let signature = hex::encode(hmac_sha256(&signing_key, string_to_sign.as_bytes()));

    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/{credential_scope}, SignedHeaders={signed_header_names}, Signature={signature}"
    )
}

#[derive(Debug, Clone)]
struct EventFrame {
    event_type: String,
    message_type: String,
    exception_type: Option<String>,
    payload: Value,
}

fn parse_event_stream_frame(bytes: &[u8]) -> Result<Option<(EventFrame, usize)>, String> {
    if bytes.len() < 12 {
        return Ok(None);
    }
    let total_length = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    let headers_length = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
    if total_length < 16 || bytes.len() < total_length {
        return Ok(None);
    }

    let headers_start = 12;
    let headers_end = headers_start + headers_length;
    if headers_end > total_length {
        return Err("Invalid Bedrock event stream frame".into());
    }
    let payload_end = total_length - 4;

    let mut event_type = String::new();
    let mut message_type = String::new();
    let mut exception_type = None;
    let mut cursor = headers_start;
    while cursor < headers_end {
        let name_length = bytes[cursor] as usize;
        cursor += 1;
        if cursor + name_length + 1 > headers_end {
            break;
        }
        let name = String::from_utf8_lossy(&bytes[cursor..cursor + name_length]).into_owned();
        cursor += name_length;
        let value_type = bytes[cursor];
        cursor += 1;
        let (value, consumed) = match value_type {
            0 | 1 => (String::new(), 0),
            2 => (String::new(), 1),
            3 => (String::new(), 2),
            4 => (String::new(), 4),
            5 => (String::new(), 8),
            8 => (String::new(), 8),
            9 => (String::new(), 16),
            6 => {
                let length = u16::from_be_bytes([bytes[cursor], bytes[cursor + 1]]) as usize;
                (String::new(), 2 + length)
            }
            7 => {
                let length = u16::from_be_bytes([bytes[cursor], bytes[cursor + 1]]) as usize;
                let value = String::from_utf8_lossy(&bytes[cursor + 2..cursor + 2 + length]).into_owned();
                (value, 2 + length)
            }
            _ => (String::new(), 0),
        };
        cursor += consumed;
        match name.as_str() {
            ":event-type" => event_type = value,
            ":message-type" => message_type = value,
            ":exception-type" => exception_type = Some(value),
            _ => {}
        }
    }

    let payload = if payload_end > headers_end {
        serde_json::from_slice(&bytes[headers_end..payload_end]).unwrap_or(Value::Null)
    } else {
        Value::Null
    };

    Ok(Some((EventFrame { event_type, message_type, exception_type, payload }, total_length)))
}

fn frame_error(frame: &EventFrame) -> BedrockFailure {
    let message = frame
        .payload
        .get("message")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            frame
                .payload
                .get("Message")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| frame.exception_type.clone().unwrap_or_else(|| "UnknownError".into()));
    BedrockFailure { message, code: frame.exception_type.clone(), status: None, request_id: None }
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
    if let Err(failure) = outcome {
        for (index, block) in output.content.iter_mut().enumerate() {
            let mut meta = BlockMeta::default();
            let _ = index;
            finalize_streaming_block(&mut meta, block);
        }
        output.stop_reason = if options.request.signal.as_ref().is_some_and(|signal| signal.aborted()) {
            StopReason::Aborted
        } else {
            StopReason::Error
        };
        output.error_message = Some(format_bedrock_error(&failure));
        if output.stop_reason == StopReason::Error {
            append_bedrock_failure_diagnostic(&mut output, &failure);
        }
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
) -> Result<(), BedrockFailure> {
    let env = options.request.env.as_ref();
    let options_profile = option_str(options, "profile").or_else(|| {
        env.and_then(|env| env.get("AWS_PROFILE").cloned())
    });
    let configured_region = get_configured_bedrock_region(options);
    let has_ambient_configured_profile = get_provider_env_value("AWS_PROFILE", None).is_some();
    let endpoint_region = get_standard_bedrock_endpoint_region(&model.base_url);
    let use_explicit_endpoint =
        should_use_explicit_bedrock_endpoint(&model.base_url, configured_region.as_deref(), has_ambient_configured_profile);

    let region = arn_region(&model.id)
        .or(configured_region.clone())
        .or_else(|| if use_explicit_endpoint { endpoint_region.clone() } else { None })
        .or_else(|| if !has_ambient_configured_profile { Some(DEFAULT_REGION.to_owned()) } else { None })
        .unwrap_or_else(|| DEFAULT_REGION.to_owned());

    let skip_auth = get_provider_env_value("AWS_BEDROCK_SKIP_AUTH", env).as_deref() == Some("1");
    let bearer_token = option_str(options, "bearerToken")
        .or_else(|| options.request.api_key.clone())
        .or_else(|| get_provider_env_value("AWS_BEARER_TOKEN_BEDROCK", env));
    let use_bearer_token = bearer_token.is_some() && !skip_auth;

    let credentials = if skip_auth {
        Some(("dummy-access-key".to_owned(), "dummy-secret-key".to_owned(), None))
    } else if options_profile.is_none() {
        get_configured_bedrock_credentials(env)
    } else {
        None
    };

    let base = if use_explicit_endpoint {
        model.base_url.trim_end_matches('/').to_owned()
    } else {
        format!("https://bedrock-runtime.{region}.amazonaws.com")
    };
    let url = format!("{base}/model/{}/converse-stream", model.id);
    let parsed = url::Url::parse(&url).map_err(|error| BedrockFailure {
        message: error.to_string(),
        code: None,
        status: None,
        request_id: None,
    })?;
    let host = match parsed.port() {
        Some(port) => format!("{}:{port}", parsed.host_str().unwrap_or_default()),
        None => parsed.host_str().unwrap_or_default().to_owned(),
    };
    let canonical_path = if parsed.path().is_empty() { "/" } else { parsed.path() };

    let supports_strict_mode = model
        .compat
        .as_ref()
        .map(|compat| compat.bedrock().supports_strict_mode.unwrap_or(false))
        .unwrap_or(false);
    let cache_retention = resolve_cache_retention(options.cache_retention.or(model.cache_retention), env);
    let inference_max_tokens = options.max_tokens.or_else(|| {
        if is_anthropic_claude_model(model) {
            Some(model.max_tokens)
        } else {
            None
        }
    });

    let messages = convert_messages(
        context,
        model,
        cache_retention,
        option_str(options, "reasoning").is_some(),
        env,
    )
    .map_err(|message| BedrockFailure { message, code: None, status: None, request_id: None })?;
    let tool_config = convert_tool_config(
        context.tools.as_deref(),
        options.extra.get("toolChoice"),
        supports_strict_mode,
    )
    .map_err(|message| BedrockFailure { message, code: None, status: None, request_id: None })?;

    let mut inference_config = Map::new();
    if let Some(max_tokens) = inference_max_tokens {
        inference_config.insert("maxTokens".into(), Value::from(max_tokens));
    }
    if let Some(temperature) = options.temperature {
        inference_config.insert("temperature".into(), json!(temperature));
    }

    let mut command_input = Map::new();
    command_input.insert("modelId".into(), Value::from(model.id.clone()));
    command_input.insert("messages".into(), Value::Array(messages));
    if let Some(system) = build_system_prompt(context.system_prompt.as_deref(), model, cache_retention, env) {
        command_input.insert("system".into(), Value::Array(system));
    }
    command_input.insert("inferenceConfig".into(), Value::Object(inference_config));
    if let Some(tool_config) = tool_config {
        command_input.insert("toolConfig".into(), tool_config);
    }
    if let Some(fields) = build_additional_model_request_fields(model, options) {
        command_input.insert("additionalModelRequestFields".into(), fields);
    }
    if let Some(metadata) = options.extra.get("requestMetadata")
        && !metadata.is_null()
    {
        command_input.insert("requestMetadata".into(), metadata.clone());
    }

    let mut command_input = Value::Object(command_input);
    if let Some(object) = command_input.as_object_mut() {
        apply_extra_body(object, options.extra_body.as_ref(), &BEDROCK_RESERVED_BODY_KEYS);
    }
    if let Some(next) = options.request.apply_payload_hook(&command_input, model, None)
        .await.map_err(failure_from)?
    {
        command_input = next;
    }

    let payload = command_input.to_string().into_bytes();
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(reqwest::header::CONTENT_TYPE, reqwest::header::HeaderValue::from_static("application/json"));
    if let Some(custom) = crate::utils::headers::provider_headers_to_record(options.request.headers.as_ref()) {
        for (name, value) in custom {
            if is_reserved_header(&name) {
                continue;
            }
            if let (Ok(name), Ok(value)) = (
                reqwest::header::HeaderName::from_bytes(name.as_bytes()),
                reqwest::header::HeaderValue::from_str(&value),
            ) {
                headers.insert(name, value);
            }
        }
    }

    let amz_date = amz_datetime();
    if use_bearer_token {
        if let Some(token) = &bearer_token {
            headers.insert(
                reqwest::header::AUTHORIZATION,
                reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
                    .map_err(|error| failure_from(error.to_string()))?,
            );
        }
    } else if let Some((access_key_id, secret_access_key, session_token)) = &credentials {
        let authorization = sigv4_authorization(&SigV4Request {
            access_key_id,
            secret_access_key,
            session_token: session_token.as_deref(),
            region: &region,
            host: &host,
            canonical_path,
            payload: &payload,
            amz_date: &amz_date,
        });
        headers.insert(
            reqwest::header::AUTHORIZATION,
            reqwest::header::HeaderValue::from_str(&authorization).map_err(|error| failure_from(error.to_string()))?,
        );
    }
    headers.insert(
        reqwest::header::HeaderName::from_static("x-amz-date"),
        reqwest::header::HeaderValue::from_str(&amz_date).map_err(|error| failure_from(error.to_string()))?,
    );
    headers.insert(
        reqwest::header::HeaderName::from_static("x-amz-content-sha256"),
        reqwest::header::HeaderValue::from_str(&sha256_hex(&payload))
            .map_err(|error| failure_from(error.to_string()))?,
    );
    if let Some((_, _, Some(session_token))) = &credentials
        && let Ok(value) = reqwest::header::HeaderValue::from_str(session_token)
    {
        headers.insert(reqwest::header::HeaderName::from_static("x-amz-security-token"), value);
    }

    let client = options.request.fetch.clone().unwrap_or_default();
    let response = client
        .post(&url)
        .headers(headers)
        .body(payload)
        .send()
        .await
        .map_err(|error| failure_from(error.to_string()))?;

    let status = response.status().as_u16();
    let request_id = response
        .headers()
        .get("x-amzn-requestid")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    options.request.apply_response_hook(
            &crate::types::ProviderResponse {
                status,
                headers: crate::utils::headers::headers_to_record(response.headers()),
            },
            model,
        ).await.map_err(failure_from)?;

    if !response.status().is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(BedrockFailure {
            message: format_body_failure(status, &body),
            code: None,
            status: Some(status),
            request_id,
        });
    }

    let mut blocks: Vec<BlockMeta> = Vec::new();
    let mut buffer: Vec<u8> = Vec::new();
    let mut stream = response.bytes_stream();
    use futures::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| failure_from(error.to_string()))?;
        buffer.extend_from_slice(&chunk);

        loop {
            let Some((frame, consumed)) = parse_event_stream_frame(&buffer).map_err(failure_from)? else { break };
            buffer.drain(..consumed);
            if frame.message_type == "exception" || frame.event_type.ends_with("Exception") {
                let mut failure = frame_error(&frame);
                failure.status = Some(status);
                failure.request_id = request_id.clone();
                return Err(failure);
            }
            handle_event(&frame, &mut blocks, output, model, sink)?;
        }
    }

    if options.request.signal.as_ref().is_some_and(|signal| signal.aborted()) {
        return Err(failure_from("Request was aborted".to_owned()));
    }
    if output.stop_reason == StopReason::Pending {
        return Err(failure_from("Bedrock stream ended without a stop reason".to_owned()));
    }
    if output.stop_reason == StopReason::Error || output.stop_reason == StopReason::Aborted {
        return Err(failure_from(
            output.error_message.clone().unwrap_or_else(|| "An unknown error occurred".to_owned()),
        ));
    }

    for (index, block) in output.content.iter_mut().enumerate() {
        if let Some(meta) = blocks.get_mut(index) {
            finalize_streaming_block(meta, block);
        }
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

fn failure_from(message: String) -> BedrockFailure {
    BedrockFailure { message, code: None, status: None, request_id: None }
}

fn format_body_failure(status: u16, body: &str) -> String {
    if let Ok(parsed) = serde_json::from_str::<Value>(body) {
        if let Some(message) = parsed.get("message").and_then(Value::as_str) {
            return message.to_owned();
        }
        if let Some(message) = parsed.get("Message").and_then(Value::as_str) {
            return message.to_owned();
        }
    }
    format!("{status}: {body}")
}

fn amz_datetime() -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let total_seconds = now.as_secs() as i64;
    let days = total_seconds.div_euclid(86_400);
    let seconds_of_day = total_seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        seconds_of_day / 3600,
        (seconds_of_day % 3600) / 60,
        seconds_of_day % 60
    )
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn is_reserved_header(key: &str) -> bool {
    let lower = key.to_lowercase();
    lower.starts_with("x-amz-") || lower == "authorization" || lower == "host"
}

fn arn_region(model_id: &str) -> Option<String> {
    let parts: Vec<&str> = model_id.split(':').collect();
    if parts.len() < 4 || parts[0] != "arn" || !parts[1].starts_with("aws") || parts[2] != "bedrock" {
        return None;
    }
    let region = parts[3];
    let valid = !region.is_empty()
        && region
            .chars()
            .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-');
    if valid {
        return Some(region.to_owned());
    }
    None
}

#[allow(dead_code)]
fn arn_region_legacy(model_id: &str) -> Option<String> {
    let rest = model_id.strip_prefix("arn:aws")?;
    let rest = rest.strip_prefix(":").or_else(|| rest.find(':').map(|index| &rest[index..]))?;
    let rest = rest.strip_prefix(":bedrock:")?;
    let end = rest.find(':')?;
    let region = &rest[..end];
    if region.is_empty() {
        None
    } else {
        Some(region.to_owned())
    }
}

fn handle_event(
    frame: &EventFrame,
    blocks: &mut Vec<BlockMeta>,
    output: &mut AssistantMessage,
    model: &Model,
    sink: &AssistantMessageEventStream,
) -> Result<(), BedrockFailure> {
    match frame.event_type.as_str() {
        "messageStart" => {
            let role = frame.payload.get("role").and_then(Value::as_str).unwrap_or("assistant");
            if role != "assistant" {
                return Err(failure_from(
                    "Unexpected assistant message start but got user message start instead".to_owned(),
                ));
            }
            sink.push(AssistantMessageEvent::Start { partial: output.clone() });
        }
        "contentBlockStart" => handle_content_block_start(&frame.payload, blocks, output, sink),
        "contentBlockDelta" => handle_content_block_delta(&frame.payload, blocks, output, sink)?,
        "contentBlockStop" => handle_content_block_stop(&frame.payload, blocks, output, sink)?,
        "messageStop" => {
            let reason = frame.payload.get("stopReason").and_then(Value::as_str);
            output.raw_stop_reason = reason.map(str::to_owned);
            let (stop_reason, error_message) = map_stop_reason(reason);
            output.stop_reason = stop_reason;
            if let Some(error_message) = error_message {
                output.error_message = Some(error_message);
            }
        }
        "metadata" => handle_metadata(&frame.payload, model, output),
        _ => {}
    }
    Ok(())
}

fn handle_metadata(payload: &Value, model: &Model, output: &mut AssistantMessage) {
    let Some(usage) = payload.get("usage") else { return };
    output.usage.input = usage.get("inputTokens").and_then(Value::as_u64).unwrap_or(0);
    output.usage.output = usage.get("outputTokens").and_then(Value::as_u64).unwrap_or(0);
    output.usage.cache_read = usage.get("cacheReadInputTokens").and_then(Value::as_u64).unwrap_or(0);
    output.usage.cache_write = usage.get("cacheWriteInputTokens").and_then(Value::as_u64).unwrap_or(0);
    output.usage.total_tokens = usage
        .get("totalTokens")
        .and_then(Value::as_u64)
        .unwrap_or(output.usage.input + output.usage.output);
    calculate_cost(model, &mut output.usage);
}

fn handle_content_block_start(
    payload: &Value,
    blocks: &mut Vec<BlockMeta>,
    output: &mut AssistantMessage,
    sink: &AssistantMessageEventStream,
) {
    let Some(content_block_index) = payload.get("contentBlockIndex").and_then(Value::as_u64) else { return };
    let Some(tool_use) = payload.get("start").and_then(|start| start.get("toolUse")) else { return };

    output.content.push(ContentBlock::ToolCall(ToolCall {
        id: tool_use.get("toolUseId").and_then(Value::as_str).unwrap_or_default().to_owned(),
        name: tool_use.get("name").and_then(Value::as_str).unwrap_or_default().to_owned(),
        arguments: Map::new(),
        incomplete: None,
        error_message: None,
        thought_signature: None,
        namespace: None,
    }));
    blocks.push(BlockMeta {
        content_block_index: Some(content_block_index as usize),
        partial_json: String::new(),
        redacted_chunks: None,
    });
    sink.push(AssistantMessageEvent::ToolcallStart {
        content_index: output.content.len() - 1,
        partial: output.clone(),
    });
}

fn handle_content_block_delta(
    payload: &Value,
    blocks: &mut Vec<BlockMeta>,
    output: &mut AssistantMessage,
    sink: &AssistantMessageEventStream,
) -> Result<(), BedrockFailure> {
    let Some(content_block_index) = payload.get("contentBlockIndex").and_then(Value::as_u64) else { return Ok(()) };
    let content_block_index = content_block_index as usize;
    let Some(delta) = payload.get("delta") else { return Ok(()) };

    let mut index = blocks.iter().position(|block| block.content_block_index == Some(content_block_index));

    if let Some(text) = delta.get("text").and_then(Value::as_str) {
        if index.is_none() {
            output.content.push(ContentBlock::Text(TextContent::default()));
            blocks.push(BlockMeta {
                content_block_index: Some(content_block_index),
                partial_json: String::new(),
                redacted_chunks: None,
            });
            index = Some(blocks.len() - 1);
            sink.push(AssistantMessageEvent::TextStart {
                content_index: output.content.len() - 1,
                partial: output.clone(),
            });
        }
        let index = index.expect("text block index");
        if let Some(ContentBlock::Text(block)) = output.content.get_mut(index) {
            block.text.push_str(text);
        }
        sink.push(AssistantMessageEvent::TextDelta {
            content_index: index,
            delta: text.to_owned(),
            partial: output.clone(),
        });
        return Ok(());
    }

    if let Some(input) = delta.get("toolUse").and_then(|tool_use| tool_use.get("input")).and_then(Value::as_str) {
        let Some(index) = index else { return Ok(()) };
        if matches!(output.content.get(index), Some(ContentBlock::ToolCall(_))) {
            let meta = &mut blocks[index];
            meta.partial_json.push_str(input);
            let parsed = parse_streaming_json(Some(&meta.partial_json));
            if let Some(ContentBlock::ToolCall(call)) = output.content.get_mut(index) {
                call.arguments = parsed.as_object().cloned().unwrap_or_default();
            }
            sink.push(AssistantMessageEvent::ToolcallDelta {
                content_index: index,
                delta: input.to_owned(),
                partial: output.clone(),
            });
        }
        return Ok(());
    }

    let Some(reasoning) = delta.get("reasoningContent") else { return Ok(()) };
    if index.is_none() {
        output.content.push(ContentBlock::Thinking(ThinkingContent {
            thinking_signature: Some(String::new()),
            ..ThinkingContent::default()
        }));
        blocks.push(BlockMeta {
            content_block_index: Some(content_block_index),
            partial_json: String::new(),
            redacted_chunks: None,
        });
        index = Some(blocks.len() - 1);
        sink.push(AssistantMessageEvent::ThinkingStart {
            content_index: output.content.len() - 1,
            partial: output.clone(),
        });
    }
    let index = index.expect("thinking block index");

    if let Some(text) = reasoning.get("text").and_then(Value::as_str) {
        if let Some(ContentBlock::Thinking(block)) = output.content.get_mut(index) {
            block.thinking.push_str(text);
        }
        sink.push(AssistantMessageEvent::ThinkingDelta {
            content_index: index,
            delta: text.to_owned(),
            partial: output.clone(),
        });
    }

    let is_redacted = matches!(output.content.get(index), Some(ContentBlock::Thinking(block)) if block.redacted == Some(true));
    if let Some(signature) = reasoning.get("signature").and_then(Value::as_str)
        && !is_redacted
        && let Some(ContentBlock::Thinking(block)) = output.content.get_mut(index)
    {
        let existing = block.thinking_signature.clone().unwrap_or_default();
        block.thinking_signature = Some(format!("{existing}{signature}"));
    }

    if let Some(redacted) = reasoning.get("redactedContent").and_then(Value::as_str)
        && !redacted.is_empty()
    {
        if !is_redacted {
            if let Some(ContentBlock::Thinking(block)) = output.content.get_mut(index) {
                block.redacted = Some(true);
                block.thinking_signature = Some(String::new());
                block.thinking.push_str(REDACTED_THINKING_PLACEHOLDER);
            }
            sink.push(AssistantMessageEvent::ThinkingDelta {
                content_index: index,
                delta: REDACTED_THINKING_PLACEHOLDER.to_owned(),
                partial: output.clone(),
            });
        }
        if let Some(bytes) = base64_decode(redacted) {
            blocks[index].redacted_chunks.get_or_insert_with(Vec::new).push(bytes);
        }
    }

    Ok(())
}

fn handle_content_block_stop(
    payload: &Value,
    blocks: &mut [BlockMeta],
    output: &mut AssistantMessage,
    sink: &AssistantMessageEventStream,
) -> Result<(), BedrockFailure> {
    let Some(content_block_index) = payload.get("contentBlockIndex").and_then(Value::as_u64) else { return Ok(()) };
    let Some(index) = blocks.iter().position(|block| block.content_block_index == Some(content_block_index as usize))
    else {
        return Ok(());
    };
    blocks[index].content_block_index = None;

    match output.content.get(index).cloned() {
        Some(ContentBlock::Text(block)) => sink.push(AssistantMessageEvent::TextEnd {
            content_index: index,
            content: block.text,
            partial: output.clone(),
        }),
        Some(ContentBlock::Thinking(_)) => {
            let mut meta = std::mem::take(&mut blocks[index]);
            if let Some(block) = output.content.get_mut(index) {
                flush_redacted_content(&mut meta, block);
                if let ContentBlock::Thinking(thinking) = block {
                    sink.push(AssistantMessageEvent::ThinkingEnd {
                        content_index: index,
                        content: thinking.thinking.clone(),
                        partial: output.clone(),
                    });
                }
            }
            blocks[index] = meta;
        }
        Some(ContentBlock::ToolCall(_)) => {
            let parsed = parse_streaming_json(Some(&blocks[index].partial_json));
            blocks[index].partial_json.clear();
            if let Some(ContentBlock::ToolCall(call)) = output.content.get_mut(index) {
                call.arguments = parsed.as_object().cloned().unwrap_or_default();
                sink.push(AssistantMessageEvent::ToolcallEnd {
                    content_index: index,
                    tool_call: call.clone(),
                    partial: output.clone(),
                });
            }
        }
        _ => {}
    }
    Ok(())
}

pub fn stream_simple(
    model: &Model,
    context: &Context,
    options: Option<SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    let simple = options.unwrap_or_default();
    let mut base = match build_base_options(model, context, Some(&simple), None) {
        Ok(base) => base,
        Err(error) => {
            let stream = create_assistant_message_event_stream();
            let message = crate::utils::lazy::setup_error_message(model, &error.to_string());
            stream.push(AssistantMessageEvent::Error { reason: ErrorReason::Error, error: message.clone() });
            stream.end(Some(message));
            return stream;
        }
    };
    if let Some(tool_choice) = simple.tool_choice {
        base.extra.insert("toolChoice".into(), serde_json::to_value(tool_choice).unwrap_or(Value::Null));
    }

    let Some(reasoning) = simple.reasoning else {
        return stream(model, context, Some(base));
    };

    if is_anthropic_claude_model(model) {
        if supports_adaptive_thinking(model) {
            base.extra.insert("reasoning".into(), serde_json::to_value(reasoning).unwrap_or(Value::Null));
            if let Some(budgets) = simple.thinking_budgets {
                base.extra.insert("thinkingBudgets".into(), serde_json::to_value(budgets).unwrap_or(Value::Null));
            }
            return stream(model, context, Some(base));
        }

        let adjusted = adjust_max_tokens_for_thinking(
            base.max_tokens,
            model.max_tokens,
            reasoning,
            simple.thinking_budgets.as_ref(),
        );
        let max_tokens = match clamp_max_tokens_to_context(model, context, adjusted.max_tokens) {
            Ok(max_tokens) => max_tokens,
            Err(error) => {
                let stream = create_assistant_message_event_stream();
                let message = crate::utils::lazy::setup_error_message(model, &error.to_string());
                stream.push(AssistantMessageEvent::Error { reason: ErrorReason::Error, error: message.clone() });
                stream.end(Some(message));
                return stream;
            }
        };

        let mut budgets: ThinkingBudgets = simple.thinking_budgets.clone().unwrap_or_default();
        let level = clamp_reasoning(reasoning).unwrap_or(reasoning);
        let budget = adjusted.thinking_budget.min(max_tokens.saturating_sub(1024));
        set_budget(&mut budgets, level, budget);

        base.max_tokens = Some(max_tokens);
        base.extra.insert("reasoning".into(), serde_json::to_value(reasoning).unwrap_or(Value::Null));
        base.extra.insert("thinkingBudgets".into(), serde_json::to_value(budgets).unwrap_or(Value::Null));
        return stream(model, context, Some(base));
    }

    base.extra.insert("reasoning".into(), serde_json::to_value(reasoning).unwrap_or(Value::Null));
    if let Some(budgets) = simple.thinking_budgets {
        base.extra.insert("thinkingBudgets".into(), serde_json::to_value(budgets).unwrap_or(Value::Null));
    }
    stream(model, context, Some(base))
}

fn set_budget(budgets: &mut ThinkingBudgets, level: ThinkingLevel, budget: u64) {
    match level {
        ThinkingLevel::Minimal => budgets.minimal = Some(budget),
        ThinkingLevel::Low => budgets.low = Some(budget),
        ThinkingLevel::Medium => budgets.medium = Some(budget),
        ThinkingLevel::High => budgets.high = Some(budget),
        ThinkingLevel::Xhigh | ThinkingLevel::Max => budgets.max = Some(budget),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn any_model() -> Model {
        crate::models_generated::get_builtin_provider_models("anthropic").expect("models")[0].clone()
    }

    fn bedrock_model(id: &str) -> Model {
        let mut model = any_model();
        model.id = id.to_owned();
        model.name = "bedrock-model".into();
        model.api = "bedrock-converse-stream".into();
        model.provider = "amazon-bedrock".into();
        model.base_url = "https://bedrock-runtime.us-east-1.amazonaws.com".into();
        model
    }


    #[test]
    fn arn_regions_are_extracted_from_inference_profiles() {
        assert_eq!(
            arn_region("arn:aws:bedrock:eu-west-1:123:inference-profile/x"),
            Some("eu-west-1".to_owned())
        );
        assert_eq!(arn_region("anthropic.claude-3"), None);
    }

    #[test]
    fn standard_endpoint_regions_parse_and_gate_explicit_endpoints() {
        assert_eq!(
            get_standard_bedrock_endpoint_region("https://bedrock-runtime.us-west-2.amazonaws.com"),
            Some("us-west-2".to_owned())
        );
        assert_eq!(
            get_standard_bedrock_endpoint_region("https://bedrock-runtime-fips.eu-central-1.amazonaws.com"),
            Some("eu-central-1".to_owned())
        );
        assert_eq!(get_standard_bedrock_endpoint_region("https://gateway.example.com"), None);
        assert!(should_use_explicit_bedrock_endpoint("https://gateway.example.com", None, false));
        assert!(should_use_explicit_bedrock_endpoint(
            "https://bedrock-runtime.us-west-2.amazonaws.com",
            None,
            false
        ));
        assert!(!should_use_explicit_bedrock_endpoint(
            "https://bedrock-runtime.us-west-2.amazonaws.com",
            Some("eu-west-1"),
            false
        ));
    }

    #[test]
    fn stop_reasons_map_like_the_sdk_enum_switch() {
        assert_eq!(map_stop_reason(Some("end_turn")), (StopReason::Stop, None));
        assert_eq!(map_stop_reason(Some("stop_sequence")), (StopReason::Stop, None));
        assert_eq!(map_stop_reason(Some("max_tokens")), (StopReason::Length, None));
        assert_eq!(map_stop_reason(Some("model_context_window_exceeded")), (StopReason::Length, None));
        assert_eq!(map_stop_reason(Some("tool_use")), (StopReason::ToolUse, None));
        assert_eq!(
            map_stop_reason(Some("guardrail_intervened")),
            (StopReason::Error, Some("Provider stopped with: guardrail_intervened".into()))
        );
        assert_eq!(map_stop_reason(None), (StopReason::Error, None));
    }

    #[test]
    fn error_prefixes_follow_the_ts_table() {
        let failure = BedrockFailure {
            message: "boom".into(),
            code: Some("ThrottlingException".into()),
            status: Some(429),
            request_id: None,
        };
        assert_eq!(format_bedrock_error(&failure), "Throttling error: boom");

        let retention = BedrockFailure {
            message: "data retention mode 'default' is not available".into(),
            code: None,
            status: None,
            request_id: None,
        };
        assert!(format_bedrock_error(&retention).ends_with("for supported data retention modes."));
    }

    #[test]
    fn sigv4_signature_matches_the_aws_documented_example_shape() {
        let authorization = sigv4_authorization(&SigV4Request {
            access_key_id: "AKIDEXAMPLE",
            secret_access_key: "secret",
            session_token: None,
            region: "us-east-1",
            host: "bedrock-runtime.us-east-1.amazonaws.com",
            canonical_path: "/model/m/converse-stream",
            payload: b"{}",
            amz_date: "20260930T000000Z",
        });
        assert!(authorization.starts_with("AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260930/us-east-1/bedrock/aws4_request, "));
        assert!(authorization.contains("SignedHeaders=content-type;host;x-amz-content-sha256;x-amz-date"));
        assert!(authorization.contains(", Signature="));
        assert_eq!(hmac_sha256(b"key", b"msg").len(), 32);
    }

    #[test]
    fn event_stream_frames_decode_headers_and_payload() {
        let payload = br#"{"stopReason":"end_turn"}"#;
        let mut header = Vec::new();
        for (name, value) in [(":event-type", "messageStop"), (":message-type", "event")] {
            header.push(name.len() as u8);
            header.extend_from_slice(name.as_bytes());
            header.push(7);
            header.extend_from_slice(&(value.len() as u16).to_be_bytes());
            header.extend_from_slice(value.as_bytes());
        }
        let total = 12 + header.len() + payload.len() + 4;
        let mut frame = Vec::new();
        frame.extend_from_slice(&(total as u32).to_be_bytes());
        frame.extend_from_slice(&(header.len() as u32).to_be_bytes());
        frame.extend_from_slice(&0u32.to_be_bytes());
        frame.extend_from_slice(&header);
        frame.extend_from_slice(payload);
        frame.extend_from_slice(&0u32.to_be_bytes());

        let (parsed, consumed) = parse_event_stream_frame(&frame).expect("parse").expect("frame");
        assert_eq!(consumed, total);
        assert_eq!(parsed.event_type, "messageStop");
        assert_eq!(parsed.message_type, "event");
        assert_eq!(parsed.payload.get("stopReason").and_then(Value::as_str), Some("end_turn"));
        assert!(parse_event_stream_frame(&frame[..8]).expect("partial").is_none());
    }

    #[test]
    fn message_conversion_wraps_blank_text_and_batches_tool_results() {
        let model = bedrock_model("anthropic.claude-3-5-sonnet");
        let context = Context {
            messages: vec![
            crate::types::Message::User(crate::types::UserMessage {
                content: crate::types::UserContent::Text("  ".into()),
                timestamp: 0,
            }),
            crate::types::Message::ToolResult(crate::types::ToolResultMessage {
                tool_call_id: "a".into(),
                tool_name: "t".into(),
                content: vec![],
                details: None,
                usage: None,
                added_tool_names: None,
                is_error: true,
                timestamp: 0,
            }),
            crate::types::Message::ToolResult(crate::types::ToolResultMessage {
                tool_call_id: "b".into(),
                tool_name: "t".into(),
                content: vec![ContentBlock::text("ok")],
                details: None,
                usage: None,
                added_tool_names: None,
                is_error: false,
                timestamp: 0,
            }),
            ],
            ..Context::default()
        };
        let converted = convert_messages(&context, &model, CacheRetention::None, false, None).expect("messages");
        assert_eq!(converted[0]["content"][0]["text"], json!(EMPTY_TEXT_PLACEHOLDER));
        assert_eq!(converted[1]["role"], json!("user"));
        assert_eq!(converted[1]["content"].as_array().map(Vec::len), Some(2));
        assert_eq!(converted[1]["content"][0]["toolResult"]["status"], json!("error"));
        assert_eq!(converted[1]["content"][0]["toolResult"]["content"][0]["text"], json!(EMPTY_TEXT_PLACEHOLDER));
    }

    #[test]
    fn cache_points_follow_the_retention_and_ttl_support() {
        let model = bedrock_model("anthropic.claude-3-5-sonnet");
        assert_eq!(cache_point(CacheRetention::Short, &model), json!({ "cachePoint": { "type": "default" } }));
        assert_eq!(resolve_cache_retention(None, None), CacheRetention::Short);
        let env: ProviderEnv = [("PI_CACHE_RETENTION".to_owned(), "long".to_owned())].into_iter().collect();
        assert_eq!(resolve_cache_retention(None, Some(&env)), CacheRetention::Long);
        assert_eq!(resolve_cache_retention(Some(CacheRetention::None), Some(&env)), CacheRetention::None);
    }

    #[test]
    fn tool_config_honors_none_and_strict_sampling() {
        let tool = Tool {
            name: "t".into(),
            description: "d".into(),
            parameters: json!({ "type": "object", "properties": { "a": { "type": "string" } } }),
            freeform: None,
            constrained_sampling: None,
        };
        let config = convert_tool_config(Some(std::slice::from_ref(&tool)), None, false)
            .expect("config")
            .expect("some");
        assert_eq!(config["tools"][0]["toolSpec"]["name"], json!("t"));
        assert!(config.get("toolChoice").is_none());

        let auto = convert_tool_config(Some(std::slice::from_ref(&tool)), Some(&json!("auto")), false)
            .expect("config")
            .expect("some");
        assert_eq!(auto["toolChoice"], json!({ "auto": {} }));

        let forced = convert_tool_config(Some(&[tool]), Some(&json!({ "type": "tool", "name": "t" })), true)
            .expect("config")
            .expect("some");
        assert_eq!(forced["toolChoice"], json!({ "tool": { "name": "t" } }));
        assert_eq!(
            convert_tool_config(None, Some(&json!("none")), false).expect("config"),
            None
        );
    }

    #[test]
    fn image_blocks_reject_unknown_types_with_the_ts_message() {
        assert_eq!(
            create_image_block("image/png", "AA=="),
            Ok(json!({ "source": { "bytes": "AA==" }, "format": "png" }))
        );
        assert_eq!(
            create_image_block("image/tiff", "AA=="),
            Err("Unknown image type: image/tiff".to_owned())
        );
    }

    #[test]
    fn redacted_content_round_trips_through_base64() {
        let encoded = base64_encode(&[1u8, 2, 3]);
        assert_eq!(decode_redacted_content(Some(&encoded)), Some(vec![1u8, 2, 3]));
        assert_eq!(decode_redacted_content(None), None);
        assert_eq!(decode_redacted_content(Some("not base64!")), None);
    }

    #[test]
    fn adaptive_thinking_and_disabled_rejection_follow_the_family_markers() {
        assert!(supports_adaptive_thinking(&bedrock_model("arn:aws:bedrock:us-east-1:1:inference-profile/opus-4-8-x")));
        assert!(!supports_adaptive_thinking(&bedrock_model("anthropic.claude-3-5-sonnet")));
        assert!(rejects_disabled_thinking(&bedrock_model("fable-5-preview")));
        assert!(!rejects_disabled_thinking(&bedrock_model("anthropic.claude-3-5-sonnet")));
    }

    #[test]
    fn thinking_effort_maps_through_the_model_map_and_native_xhigh() {
        let model = bedrock_model("opus-4-8-x");
        assert_eq!(map_thinking_level_to_effort(&model, Some(ThinkingLevel::Xhigh)), "xhigh");
        let mut claude = bedrock_model("anthropic.claude-3");
        assert_eq!(map_thinking_level_to_effort(&claude, Some(ThinkingLevel::Xhigh)), "xhigh");
        claude.thinking_level_map = None;
        assert_eq!(map_thinking_level_to_effort(&claude, Some(ThinkingLevel::Xhigh)), "max");
        assert_eq!(map_thinking_level_to_effort(&claude, Some(ThinkingLevel::Minimal)), "low");
        assert_eq!(map_thinking_level_to_effort(&claude, Some(ThinkingLevel::High)), "high");
        assert_eq!(map_thinking_level_to_effort(&claude, None), "high");
    }

    #[test]
    fn additional_model_request_fields_cover_the_reasoning_branches() {
        let mut model = bedrock_model("anthropic.claude-3-5-sonnet");
        model.reasoning = true;
        let mut options = StreamOptions::default();

        assert_eq!(build_additional_model_request_fields(&model, &options), None);

        let mut adaptive = bedrock_model("anthropic.claude-opus-4-8");
        adaptive.reasoning = true;
        assert_eq!(
            build_additional_model_request_fields(&adaptive, &options),
            Some(json!({ "thinking": { "type": "disabled" } }))
        );

        let mut fable = bedrock_model("anthropic.claude-fable-5");
        fable.reasoning = true;
        assert_eq!(
            build_additional_model_request_fields(&fable, &options),
            Some(json!({ "output_config": { "effort": "low" } }))
        );

        options.extra.insert("reasoning".into(), json!("medium"));
        let fields = build_additional_model_request_fields(&model, &options).expect("fields");
        assert_eq!(fields["thinking"]["type"], json!("enabled"));
        assert_eq!(fields["thinking"]["budget_tokens"], json!(8192));
        assert_eq!(fields["thinking"]["display"], json!("summarized"));
        assert_eq!(fields["anthropic_beta"], json!(["interleaved-thinking-2025-05-14"]));

        let adaptive_fields = build_additional_model_request_fields(&adaptive, &options).expect("fields");
        assert_eq!(adaptive_fields["thinking"]["type"], json!("adaptive"));
        assert_eq!(adaptive_fields["output_config"]["effort"], json!("medium"));
    }

    #[test]
    fn usage_metadata_fills_the_usage_counters() {
        let model = bedrock_model("anthropic.claude-3-5-sonnet");
        let mut output = create_output(&model);
        handle_metadata(
            &json!({ "usage": { "inputTokens": 12, "outputTokens": 5, "cacheReadInputTokens": 1, "cacheWriteInputTokens": 2 } }),
            &model,
            &mut output,
        );
        assert_eq!(output.usage.input, 12);
        assert_eq!(output.usage.output, 5);
        assert_eq!(output.usage.cache_read, 1);
        assert_eq!(output.usage.cache_write, 2);
        assert_eq!(output.usage.total_tokens, 17);
    }

    #[test]
    fn text_deltas_create_blocks_and_content_block_stop_ends_them() {
        let model = bedrock_model("anthropic.claude-3-5-sonnet");
        let sink = create_assistant_message_event_stream();
        let mut output = create_output(&model);
        let mut blocks: Vec<BlockMeta> = Vec::new();

        handle_content_block_delta(
            &json!({ "contentBlockIndex": 0, "delta": { "text": "Hi" } }),
            &mut blocks,
            &mut output,
            &sink,
        )
        .expect("delta");
        handle_content_block_delta(
            &json!({ "contentBlockIndex": 0, "delta": { "text": " there" } }),
            &mut blocks,
            &mut output,
            &sink,
        )
        .expect("delta");
        handle_content_block_stop(&json!({ "contentBlockIndex": 0 }), &mut blocks, &mut output, &sink)
            .expect("stop");

        let events = sink.queue();
        assert!(matches!(events[0], AssistantMessageEvent::TextStart { content_index: 0, .. }));
        assert!(matches!(events[1], AssistantMessageEvent::TextDelta { .. }));
        assert!(matches!(events[3], AssistantMessageEvent::TextEnd { .. }));
        assert_eq!(output.content[0], ContentBlock::text("Hi there"));
    }

    #[test]
    fn redacted_reasoning_is_stored_verbatim_in_the_signature() {
        let model = bedrock_model("openai.gpt-5.6");
        let sink = create_assistant_message_event_stream();
        let mut output = create_output(&model);
        let mut blocks: Vec<BlockMeta> = Vec::new();
        let encoded = base64_encode(&[9u8, 8, 7]);

        handle_content_block_delta(
            &json!({ "contentBlockIndex": 0, "delta": { "reasoningContent": { "redactedContent": encoded } } }),
            &mut blocks,
            &mut output,
            &sink,
        )
        .expect("delta");
        handle_content_block_stop(&json!({ "contentBlockIndex": 0 }), &mut blocks, &mut output, &sink)
            .expect("stop");

        let ContentBlock::Thinking(thinking) = &output.content[0] else { panic!("thinking") };
        assert_eq!(thinking.redacted, Some(true));
        assert!(thinking.thinking.contains(REDACTED_THINKING_PLACEHOLDER));
        assert_eq!(thinking.thinking_signature.as_deref(), Some(encoded.as_str()));
    }
}
