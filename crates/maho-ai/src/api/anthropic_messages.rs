//! Port of senpi packages/ai/src/api/anthropic-messages.ts.
//!
//! The TS module drives the `@anthropic-ai/sdk` client; this port issues the same wire protocol
//! itself over `reqwest` (the SDK is a JavaScript package with no Rust analogue, the same trade the
//! other wire-API ports in this crate make). Everything the TS module decides *around* the SDK is
//! ported verbatim: the payload it builds, the request headers, the retry loop, the SSE decoder,
//! the event order and the error texts.

use std::collections::{BTreeMap, HashSet};
use std::sync::{LazyLock, Mutex};

use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::{json, Map, Value};

use crate::api::anthropic_tool_pairs::sanitize_anthropic_tool_pairs;
use crate::api::anthropic_tool_references::demote_unavailable_tool_references;
use crate::api::cloudflare::resolve_cloudflare_base_url;
use crate::api::constrained_sampling::{get_json_schema_tool_parameters, resolve_json_schema_strict_sampling};
use crate::api::github_copilot_headers::{build_copilot_dynamic_headers, has_copilot_vision_input};
use crate::api::simple_options::{
    ANTHROPIC_RESERVED_BODY_KEYS, adjust_max_tokens_for_thinking, build_base_options, clamp_max_tokens_to_context,
};
use crate::api::transform_messages::{transform_messages, TransformMessagesOptions};
use crate::models::calculate_cost;
use crate::session_resources::register_session_resource_cleanup;
use crate::types::{
    AllowedFallbackModel, AssistantMessage, AssistantMessageEvent, AssistantStopDetails, CacheRetention, ContentBlock,
    Context, DoneReason, ErrorReason, Message, Model, ModelThinkingLevel, ProviderEnv, SimpleStreamOptions, StopReason,
    StreamOptions, TextContent, ThinkingLevel, Tool, ToolCall, ToolResultMessage, UserContent, UserMessage,
};
use crate::utils::abort::{AbortController, AbortSignal};
use crate::utils::abort_signals::combine_abort_signals;
use crate::utils::deferred_tools::split_deferred_tools;
use crate::utils::diagnostics::{
    AssistantMessageDiagnostic, append_assistant_message_diagnostic, now_ms,
};
use crate::utils::event_stream::{create_assistant_message_event_stream, AssistantMessageEventStream};
use crate::utils::headers::{headers_to_record, provider_headers_to_record};
use crate::utils::json_parse::{parse_json_with_repair, parse_streaming_json};
use crate::utils::pi_user_agent::get_pi_user_agent;
use crate::utils::prompt_cache_ttl::{get_anthropic_compat, is_anthropic_api_base_url};
use crate::utils::provider_env::get_provider_env_value;
use crate::utils::provider_retry::{retry_provider_request, ProviderErrorStatus, ProviderRequestError, ProviderRetryError, ProviderRetryOptions};
use crate::utils::retry_hint::{append_retry_after_ms_marker, extract_429_retry_after_ms, RetryHintInput};
use crate::utils::retry_profile::failure::{normalize_anthropic_retry_failure, RetryErrorShape};
use crate::utils::retry_profile::types::{RetryFailure, RetryFailureKind};
use crate::utils::sanitize_unicode::sanitize_surrogates;
use crate::utils::server_fallback_receipt::{
    apply_server_fallback_abort, apply_server_fallback_continuation, parse_server_fallback_receipt,
    parse_sticky_fallback_receipt, ServerFallbackReceipt,
};
use crate::utils::tool_call_id::normalize_tool_call_id;
use crate::utils::tool_choice_fallback::{is_forced_tool_choice_unsupported_error, omit_tool_choice_param, HttpFailure};
use crate::utils::tool_schema_compat::resolve_root_object_schema;

pub use crate::utils::prompt_cache_ttl::get_anthropic_compat as anthropic_compat;

/// Stealth mode: Mimic Claude Code's tool naming exactly.
const CLAUDE_CODE_VERSION: &str = "2.1.280";

/// Claude Code 2.x tool names (canonical casing).
const CLAUDE_CODE_TOOLS: [&str; 17] = [
    "Read",
    "Write",
    "Edit",
    "Bash",
    "Grep",
    "Glob",
    "AskUserQuestion",
    "EnterPlanMode",
    "ExitPlanMode",
    "KillShell",
    "NotebookEdit",
    "Skill",
    "Task",
    "TaskOutput",
    "TodoWrite",
    "WebFetch",
    "WebSearch",
];

const CC_TOOL_ALIASES: [(&str, &str); 1] = [("ask_user_question", "AskUserQuestion")];

/// Convert a tool name to Claude Code canonical casing when it matches (case-insensitive).
pub fn to_claude_code_name(name: &str) -> String {
    if let Some((_, wire)) = CC_TOOL_ALIASES.iter().find(|(alias, _)| *alias == name) {
        return (*wire).to_owned();
    }
    let lower = name.to_lowercase();
    CLAUDE_CODE_TOOLS
        .iter()
        .find(|tool| tool.to_lowercase() == lower)
        .map_or_else(|| name.to_owned(), |tool| (*tool).to_owned())
}

pub fn from_claude_code_name(name: &str, tools: Option<&[Tool]>) -> String {
    if let Some(tools) = tools.filter(|tools| !tools.is_empty()) {
        if let Some((alias, _)) = CC_TOOL_ALIASES.iter().find(|(_, wire)| *wire == name)
            && tools.iter().any(|tool| tool.name == *alias)
        {
            return (*alias).to_owned();
        }
        let lower = name.to_lowercase();
        if let Some(tool) = tools.iter().find(|tool| tool.name.to_lowercase() == lower) {
            return tool.name.clone();
        }
    }
    name.to_owned()
}

const FINE_GRAINED_TOOL_STREAMING_BETA: &str = "fine-grained-tool-streaming-2025-05-14";
const INTERLEAVED_THINKING_BETA: &str = "interleaved-thinking-2025-05-14";
const COMPUTER_USE_BETA_PREFIX: &str = "computer-use-";
const NATIVE_COMPUTER_TOOL_TYPE: &str = "computer_20250124";
const SERVER_SIDE_FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
const MID_CONVERSATION_OUTPUT_CONFIG_BETA: &str = "mid-conversation-output-config-2026-07-01";
const THINKING_BINDING_CONTROLS_BETA: &str = "thinking-binding-controls-2026-08-01";

const ADAPTIVE_THINKING_MODEL_MARKERS: [&str; 8] = [
    "opus-4-6",
    "opus-4-7",
    "opus-4-8",
    "opus-5",
    "sonnet-4-6",
    "sonnet-5",
    "fable-5",
    "mythos-5",
];

const NATIVE_XHIGH_EFFORT_MODEL_MARKERS: [&str; 6] = ["opus-4-7", "opus-4-8", "opus-5", "sonnet-5", "fable-5", "mythos-5"];

const DISABLED_THINKING_REJECTING_MODEL_MARKERS: [&str; 4] = ["fable-5", "mythos-5", "opus-5-5", "opus-5.5"];

const UNSUPPORTED_NATIVE_COMPUTER_TOOL_MODEL_MARKERS: [&str; 6] =
    ["opus-4-6", "opus-4.6", "opus-4-7", "opus-4.7", "opus-4-8", "opus-4.8"];

/// Provider-native block types that replay verbatim on the same model. The server-side fallback
/// marker (`fallback`) is deliberately absent: the Messages API rejects it as an input tag.
const REPLAYABLE_ANTHROPIC_PROVIDER_NATIVE_TYPES: [&str; 8] = [
    "server_tool_use",
    "web_search_tool_result",
    "web_fetch_tool_result",
    "code_execution_tool_result",
    "bash_code_execution_tool_result",
    "text_editor_code_execution_tool_result",
    "tool_search_tool_result",
    "container_upload",
];

/// Anthropic SDK default request timeout (10 minutes).
const DEFAULT_TIMEOUT_MS: u64 = 600_000;

/// TS `AnthropicOptions extends StreamOptions`: the API-specific fields ride on `StreamOptions.extra`.
fn option_value(options: &StreamOptions, key: &str) -> Option<Value> {
    options.extra.get(key).cloned().filter(|value| !value.is_null())
}

fn option_bool(options: &StreamOptions, key: &str) -> Option<bool> {
    option_value(options, key).and_then(|value| value.as_bool())
}

fn option_string(options: &StreamOptions, key: &str) -> Option<String> {
    option_value(options, key).and_then(|value| value.as_str().map(str::to_owned))
}

fn option_u64(options: &StreamOptions, key: &str) -> Option<u64> {
    option_value(options, key).and_then(|value| value.as_u64())
}

fn thinking_enabled(options: &StreamOptions) -> Option<bool> {
    option_bool(options, "thinkingEnabled")
}

fn thinking_display(options: &StreamOptions) -> Option<String> {
    option_string(options, "thinkingDisplay")
}

fn interleaved_thinking(options: &StreamOptions) -> bool {
    option_bool(options, "interleavedThinking").unwrap_or(true)
}

fn effort_option(options: &StreamOptions) -> Option<String> {
    option_string(options, "effort")
}


/// Resolve cache retention preference; `PI_CACHE_RETENTION` is the backward-compatible override.
fn resolve_cache_retention(cache_retention: Option<CacheRetention>, env: Option<&ProviderEnv>) -> CacheRetention {
    if let Some(cache_retention) = cache_retention {
        return cache_retention;
    }
    if get_provider_env_value("PI_CACHE_RETENTION", env).as_deref() == Some("long") {
        return CacheRetention::Long;
    }
    if std::env::var_os("PI_CACHE_RETENTION").is_some() {
        return CacheRetention::Short;
    }
    CacheRetention::Short
}

fn get_cache_control(
    model: &Model,
    cache_retention: Option<CacheRetention>,
    env: Option<&ProviderEnv>,
) -> (CacheRetention, Option<Value>) {
    let retention = resolve_cache_retention(cache_retention, env);
    if retention == CacheRetention::None {
        return (retention, None);
    }
    let ttl = if retention == CacheRetention::Long
        && is_anthropic_api_base_url(&model.base_url)
        && get_anthropic_compat(model).supports_long_cache_retention
    {
        Some("1h")
    } else {
        None
    };
    let mut cache_control = Map::new();
    cache_control.insert("type".into(), Value::from("ephemeral"));
    if let Some(ttl) = ttl {
        cache_control.insert("ttl".into(), Value::from(ttl));
    }
    (retention, Some(Value::Object(cache_control)))
}


fn get_model_match_candidates(model: &Model) -> Vec<String> {
    let mut candidates = Vec::new();
    for value in [&model.id, &model.name] {
        let lower = value.to_lowercase();
        candidates.push(lower.clone());
        candidates.push(
            lower
                .chars()
                .map(|character| if matches!(character, ' ' | '_' | '.' | ':') { '-' } else { character })
                .collect(),
        );
    }
    candidates
}

fn matches_model_marker(model: &Model, markers: &[&str]) -> bool {
    get_model_match_candidates(model).iter().any(|candidate| markers.iter().any(|marker| candidate.contains(marker)))
}

fn supports_native_xhigh_effort(model: &Model) -> bool {
    matches_model_marker(model, &NATIVE_XHIGH_EFFORT_MODEL_MARKERS)
}

/// True when the model cannot accept `thinking: {type: "disabled"}` on the wire.
fn cannot_disable_thinking(model: &Model, supports_disabled_thinking: bool) -> bool {
    if !supports_disabled_thinking {
        return true;
    }
    matches_model_marker(model, &DISABLED_THINKING_REJECTING_MODEL_MARKERS)
}

fn disable_thinking_for_request(params: &mut Map<String, Value>, model: &Model, supports_disabled_thinking: bool) {
    // A degraded/disabled turn must not retain the caller's higher effort.
    params.remove("output_config");
    if cannot_disable_thinking(model, supports_disabled_thinking) {
        params.remove("thinking");
        if supports_adaptive_thinking(model) {
            params.insert("output_config".into(), json!({ "effort": "low" }));
        }
        return;
    }
    params.insert("thinking".into(), json!({ "type": "disabled" }));
}

pub(crate) fn supports_adaptive_thinking(model: &Model) -> bool {
    if let Some(forced) = model.compat.as_ref().and_then(|compat| compat.anthropic_messages().force_adaptive_thinking) {
        return forced;
    }
    matches_model_marker(model, &ADAPTIVE_THINKING_MODEL_MARKERS)
}

fn map_thinking_level_to_effort(model: &Model, level: Option<ThinkingLevel>) -> String {
    if let Some(mapped) = level
        .and_then(|level| model.thinking_level_map.as_ref().and_then(|map| map.get(&ModelThinkingLevel::from(level))))
        .and_then(|mapped| mapped.clone())
    {
        return mapped;
    }
    match level {
        Some(ThinkingLevel::Minimal) | Some(ThinkingLevel::Low) => "low",
        Some(ThinkingLevel::Medium) => "medium",
        Some(ThinkingLevel::High) => "high",
        Some(ThinkingLevel::Xhigh) => {
            // Only called for adaptive models, so the floor is the adaptive ladder's top tier.
            if supports_native_xhigh_effort(model) { "xhigh" } else { "max" }
        }
        Some(ThinkingLevel::Max) => "max",
        None => "high",
    }
    .to_owned()
}

fn is_anthropic_effort(value: Option<&str>) -> bool {
    matches!(value, Some("low" | "medium" | "high" | "xhigh" | "max"))
}


fn merge_headers(sources: &[Option<&BTreeMap<String, Option<String>>>]) -> BTreeMap<String, Option<String>> {
    let mut merged: BTreeMap<String, Option<String>> = BTreeMap::new();
    for source in sources.iter().flatten() {
        for (key, value) in source.iter() {
            merged.insert(key.clone(), value.clone());
        }
    }
    merged
}

fn nullable_headers(headers: &BTreeMap<String, String>) -> BTreeMap<String, Option<String>> {
    headers.iter().map(|(key, value)| (key.clone(), Some(value.clone()))).collect()
}

fn has_header(headers: Option<&BTreeMap<String, String>>, name: &str) -> bool {
    let Some(headers) = headers else {
        return false;
    };
    let expected = name.to_lowercase();
    headers.iter().any(|(key, value)| key.to_lowercase() == expected && !value.trim().is_empty())
}

fn assert_request_auth(
    provider: &str,
    api_key: Option<&str>,
    headers: Option<&BTreeMap<String, String>>,
) -> Result<(), String> {
    if api_key.is_some_and(|key| !key.is_empty()) {
        return Ok(());
    }
    if has_header(headers, "authorization")
        || has_header(headers, "x-api-key")
        || has_header(headers, "cf-aig-authorization")
    {
        return Ok(());
    }
    Err(format!("No API key for provider: {provider}"))
}

/// `removeAnthropicBetaHeaders`: drop the beta features `should_remove_beta` rejects.
fn remove_anthropic_beta_headers(
    headers: Option<&BTreeMap<String, Option<String>>>,
    should_remove_beta: impl Fn(&str) -> bool,
) -> (bool, Option<BTreeMap<String, Option<String>>>) {
    let Some(headers) = headers else {
        return (false, None);
    };
    let mut next_headers: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut changed = false;

    for (key, value) in headers.iter() {
        let Some(value) = value else {
            next_headers.insert(key.clone(), None);
            continue;
        };
        if key.to_lowercase() != "anthropic-beta" {
            next_headers.insert(key.clone(), Some(value.clone()));
            continue;
        }
        let betas: Vec<&str> = value.split(',').map(str::trim).filter(|beta| !beta.is_empty()).collect();
        let supported: Vec<&str> = betas.iter().copied().filter(|beta| !should_remove_beta(beta)).collect();
        changed = changed || supported.len() != betas.len();
        if !supported.is_empty() {
            next_headers.insert(key.clone(), Some(supported.join(", ")));
        }
    }

    if !changed {
        return (false, Some(headers.clone()));
    }
    (true, if next_headers.is_empty() { None } else { Some(next_headers) })
}

fn remove_computer_use_beta_header(
    headers: Option<&BTreeMap<String, Option<String>>>,
) -> (bool, Option<BTreeMap<String, Option<String>>>) {
    remove_anthropic_beta_headers(headers, |beta| beta.starts_with(COMPUTER_USE_BETA_PREFIX))
}

fn sanitize_adaptive_thinking_headers(
    model: &Model,
    headers: BTreeMap<String, Option<String>>,
) -> BTreeMap<String, Option<String>> {
    if !supports_adaptive_thinking(model) {
        return headers;
    }
    let (changed, sanitized) = remove_anthropic_beta_headers(Some(&headers), |beta| beta == INTERLEAVED_THINKING_BETA);
    if changed { sanitized.unwrap_or_default() } else { headers }
}

fn rejects_native_computer_tool(model: &Model, tool_type: &str) -> bool {
    if model.provider == "cloudflare-ai-gateway" && model.base_url.contains("anthropic") {
        return tool_type.starts_with("computer_");
    }
    matches_model_marker(model, &UNSUPPORTED_NATIVE_COMPUTER_TOOL_MODEL_MARKERS)
        && tool_type == NATIVE_COMPUTER_TOOL_TYPE
}

fn rejects_computer_use_beta(model: &Model) -> bool {
    (model.provider == "cloudflare-ai-gateway" && model.base_url.contains("anthropic"))
        || matches_model_marker(model, &UNSUPPORTED_NATIVE_COMPUTER_TOOL_MODEL_MARKERS)
}

fn is_anthropic_web_search_tool_type(tool_type: &str) -> bool {
    tool_type.starts_with("web_search_")
}

fn string_record(value: Option<&Value>) -> Option<BTreeMap<String, String>> {
    let object = value?.as_object()?;
    let mut record = BTreeMap::new();
    for (key, item) in object {
        record.insert(key.clone(), item.as_str()?.to_owned());
    }
    Some(record)
}


#[derive(Debug, Clone, PartialEq)]
pub struct ServerSentEvent {
    pub event: Option<String>,
    pub data: String,
    pub raw: Vec<String>,
}

#[derive(Debug, Default)]
struct SseDecoderState {
    event: Option<String>,
    data: Vec<String>,
    raw: Vec<String>,
}

fn flush_sse_event(state: &mut SseDecoderState) -> Option<ServerSentEvent> {
    if state.event.is_none() && state.data.is_empty() {
        return None;
    }
    let event = ServerSentEvent {
        event: state.event.take(),
        data: state.data.join("\n"),
        raw: std::mem::take(&mut state.raw),
    };
    state.event = None;
    state.data = Vec::new();
    Some(event)
}

fn decode_sse_line(line: &str, state: &mut SseDecoderState) -> Option<ServerSentEvent> {
    if line.is_empty() {
        return flush_sse_event(state);
    }

    state.raw.push(line.to_owned());
    if line.starts_with(':') {
        return None;
    }

    let delimiter_index = line.find(':');
    let field_name = delimiter_index.map_or(line, |index| &line[..index]);
    let value = delimiter_index.map_or("", |index| &line[index + 1..]);
    let value = value.strip_prefix(' ').unwrap_or(value);

    if field_name == "event" {
        state.event = Some(value.to_owned());
    } else if field_name == "data" {
        state.data.push(value.to_owned());
    }

    None
}

fn next_line_break_index(text: &str) -> Option<usize> {
    match (text.find('\r'), text.find('\n')) {
        (None, None) => None,
        (Some(index), None) | (None, Some(index)) => Some(index),
        (Some(carriage_return), Some(newline)) => Some(carriage_return.min(newline)),
    }
}

fn consume_line(text: &str) -> Option<(String, String)> {
    let line_break_index = next_line_break_index(text)?;
    let mut next_index = line_break_index + 1;
    if text.as_bytes()[line_break_index] == b'\r' && text.as_bytes().get(next_index) == Some(&b'\n') {
        next_index += 1;
    }
    Some((text[..line_break_index].to_owned(), text[next_index..].to_owned()))
}

/// The incremental SSE decoder `iterateSseMessages` drives: feed bytes, take whole events.
#[derive(Default)]
struct SseDecoder {
    state: SseDecoderState,
    buffer: String,
}

impl SseDecoder {
    fn push_bytes(&mut self, chunk: &[u8]) -> Vec<ServerSentEvent> {
        self.buffer.push_str(&String::from_utf8_lossy(chunk));
        let mut events = Vec::new();
        while let Some((line, rest)) = consume_line(&self.buffer) {
            self.buffer = rest;
            if let Some(event) = decode_sse_line(&line, &mut self.state) {
                events.push(event);
            }
        }
        events
    }

    /// The stream's tail: a final unterminated line, then the trailing event.
    fn finish(&mut self) -> Vec<ServerSentEvent> {
        let mut events = Vec::new();
        while let Some((line, rest)) = consume_line(&self.buffer) {
            self.buffer = rest;
            if let Some(event) = decode_sse_line(&line, &mut self.state) {
                events.push(event);
            }
        }
        if !self.buffer.is_empty() {
            let line = std::mem::take(&mut self.buffer);
            if let Some(event) = decode_sse_line(&line, &mut self.state) {
                events.push(event);
            }
        }
        if let Some(event) = flush_sse_event(&mut self.state) {
            events.push(event);
        }
        events
    }
}

const ANTHROPIC_MESSAGE_EVENTS: [&str; 6] = [
    "message_start",
    "message_delta",
    "message_stop",
    "content_block_start",
    "content_block_delta",
    "content_block_stop",
];

fn is_anthropic_message_event(event: Option<&str>) -> bool {
    event.is_some_and(|event| ANTHROPIC_MESSAGE_EVENTS.contains(&event))
}


fn is_replayable_anthropic_provider_native_block(raw: &Value) -> bool {
    raw.get("type")
        .and_then(Value::as_str)
        .is_some_and(|kind| REPLAYABLE_ANTHROPIC_PROVIDER_NATIVE_TYPES.contains(&kind))
}

fn is_same_anthropic_model(message: &AssistantMessage, model: &Model) -> bool {
    message.provider == model.provider && message.api == model.api && message.model == model.id
}

fn is_anthropic_fallback_marker_block(block: &ContentBlock) -> bool {
    match block {
        ContentBlock::ProviderNative(native) => {
            native.subtype == "fallback" && native.raw.get("type").and_then(Value::as_str) == Some("fallback")
        }
        _ => false,
    }
}

/// Index of the last server-side fallback marker, or `None` when the message has none.
fn last_anthropic_fallback_boundary(content: &[ContentBlock]) -> Option<usize> {
    let mut boundary = None;
    for (index, block) in content.iter().enumerate() {
        if is_anthropic_fallback_marker_block(block) {
            boundary = Some(index);
        }
    }
    boundary
}

fn is_anthropic_server_tool_use_block(raw: &Value) -> bool {
    raw.get("type").and_then(Value::as_str) == Some("server_tool_use")
        && raw.get("id").and_then(Value::as_str).is_some()
}

/// Only tool_use-shaped provider-native blocks stream their input via `input_json_delta`.
fn is_provider_native_tool_use_block(raw: &Value) -> bool {
    matches!(raw.get("type").and_then(Value::as_str), Some("server_tool_use" | "mcp_tool_use"))
}

fn is_anthropic_web_search_replay_block(raw: &Value) -> bool {
    match raw.get("type").and_then(Value::as_str) {
        Some("web_search_tool_result") => true,
        Some("server_tool_use") => raw.get("name").and_then(Value::as_str) == Some("web_search"),
        _ => false,
    }
}

#[derive(Debug, Default)]
struct ProviderNativeToolPairing {
    resolved_use_ids: HashSet<String>,
    live_use_ids: HashSet<String>,
    valid_result_ids: HashSet<String>,
}

/// The user turn a pending server tool can survive is one that carries only tool results.
fn user_message_closes_server_turn(message: &UserMessage) -> bool {
    match &message.content {
        UserContent::Text(text) => !crate::utils::js::trim(text).is_empty(),
        UserContent::Blocks(blocks) => blocks.iter().any(|block| match block {
            ContentBlock::Text(text) => !crate::utils::js::trim(&text.text).is_empty(),
            ContentBlock::Image(_) => true,
            _ => false,
        }),
    }
}

fn collect_provider_native_tool_pairing(
    messages: &[Message],
    model: &Model,
    deferred_tool_names: &HashSet<String>,
    normalize_tool_name: &dyn Fn(&str) -> String,
    discarded_fallback_tool_call_ids: &HashSet<String>,
) -> ProviderNativeToolPairing {
    let mut resolved_use_ids = HashSet::new();
    let mut valid_result_ids = HashSet::new();
    let mut pending_use_ids: HashSet<String> = HashSet::new();
    let mut loaded_reference_names = HashSet::new();

    for message in messages {
        match message {
            Message::Assistant(assistant) => {
                let prior_use_ids = std::mem::take(&mut pending_use_ids);
                if !is_same_anthropic_model(assistant, model) {
                    continue;
                }
                for block in &assistant.content {
                    let ContentBlock::ProviderNative(native) = block else { continue };
                    let raw = &native.raw;
                    if !is_replayable_anthropic_provider_native_block(raw) {
                        continue;
                    }
                    if is_provider_native_tool_use_block(raw)
                        && let Some(id) = raw.get("id").and_then(Value::as_str)
                    {
                        pending_use_ids.insert(id.to_owned());
                    }
                }
                for block in &assistant.content {
                    let ContentBlock::ProviderNative(native) = block else { continue };
                    let raw = &native.raw;
                    if !is_replayable_anthropic_provider_native_block(raw) {
                        continue;
                    }
                    let Some(tool_use_id) = raw.get("tool_use_id").and_then(Value::as_str) else { continue };
                    if prior_use_ids.contains(tool_use_id) || pending_use_ids.contains(tool_use_id) {
                        resolved_use_ids.insert(tool_use_id.to_owned());
                        valid_result_ids.insert(tool_use_id.to_owned());
                    }
                }
            }
            Message::ToolResult(result) => {
                if discarded_fallback_tool_call_ids.contains(&result.tool_call_id) {
                    continue;
                }
                let mut emits_references = false;
                for name in result.added_tool_names.iter().flatten() {
                    let normalized_name = normalize_tool_name(name);
                    if !deferred_tool_names.contains(&normalized_name) || loaded_reference_names.contains(&normalized_name)
                    {
                        continue;
                    }
                    loaded_reference_names.insert(normalized_name);
                    emits_references = true;
                }
                if emits_references {
                    pending_use_ids = HashSet::new();
                }
            }
            Message::User(user) => {
                if user_message_closes_server_turn(user) {
                    pending_use_ids = HashSet::new();
                }
            }
            Message::ConfigurationUpdate(_) => {}
        }
    }

    ProviderNativeToolPairing { resolved_use_ids, live_use_ids: pending_use_ids, valid_result_ids }
}

/// True for a server-tool block whose counterpart can never arrive.
fn is_unpaired_provider_native_tool_block(raw: &Value, pairing: &ProviderNativeToolPairing) -> bool {
    if is_provider_native_tool_use_block(raw) {
        return match raw.get("id").and_then(Value::as_str) {
            Some(id) => !(pairing.resolved_use_ids.contains(id) || pairing.live_use_ids.contains(id)),
            None => true,
        };
    }
    raw.get("tool_use_id")
        .and_then(Value::as_str)
        .is_some_and(|tool_use_id| !pairing.valid_result_ids.contains(tool_use_id))
}

/// `tool_use` ids referenced by server-tool result blocks in `content[0, boundary)`.
fn paired_server_tool_use_ids_before_boundary(content: &[ContentBlock], boundary: usize) -> HashSet<String> {
    let mut paired = HashSet::new();
    for block in content.iter().take(boundary) {
        let ContentBlock::ProviderNative(native) = block else { continue };
        if let Some(tool_use_id) = native.raw.get("tool_use_id").and_then(Value::as_str) {
            paired.insert(tool_use_id.to_owned());
        }
    }
    paired
}

/// Tool-call ids emitted by a discarded pre-fallback attempt on a same-model assistant turn.
fn collect_discarded_fallback_tool_call_ids(messages: &[Message], model: &Model) -> HashSet<String> {
    let mut discarded = HashSet::new();
    for message in messages {
        let Message::Assistant(assistant) = message else { continue };
        if !is_same_anthropic_model(assistant, model) {
            continue;
        }
        let Some(boundary) = last_anthropic_fallback_boundary(&assistant.content) else { continue };
        for block in assistant.content.iter().take(boundary) {
            if let ContentBlock::ToolCall(call) = block {
                discarded.insert(call.id.clone());
            }
        }
    }
    discarded
}


/// Convert text/image content to Anthropic API format (a joined string, or content blocks).
fn convert_content_blocks(content: &[ContentBlock]) -> Value {
    let has_images = content.iter().any(|block| matches!(block, ContentBlock::Image(_)));
    if !has_images {
        let text = content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text(text) => Some(text.text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        return Value::String(sanitize_surrogates(&text));
    }

    let mut blocks: Vec<Value> = content
        .iter()
        .map(|block| match block {
            ContentBlock::Text(text) => json!({ "type": "text", "text": sanitize_surrogates(&text.text) }),
            ContentBlock::Image(image) if crate::types::is_video_mime_type(&image.mime_type) => json!({
                "type": "video",
                "source": { "type": "base64", "media_type": image.mime_type, "data": image.data },
            }),
            ContentBlock::Image(image) => json!({
                "type": "image",
                "source": { "type": "base64", "media_type": image.mime_type, "data": image.data },
            }),
            other => serde_json::to_value(other).unwrap_or(Value::Null),
        })
        .collect();

    let has_text = blocks.iter().any(|block| block.get("type").and_then(Value::as_str) == Some("text"));
    if !has_text {
        blocks.insert(0, json!({ "type": "text", "text": "(see attached image)" }));
    }
    Value::Array(blocks)
}

fn apply_extra_body_to_anthropic_params(params: &mut Map<String, Value>, extra_body: Option<&Map<String, Value>>) {
    let Some(extra_body) = extra_body else { return };
    for (key, value) in extra_body {
        if ANTHROPIC_RESERVED_BODY_KEYS.contains(key.as_str()) {
            continue;
        }
        params.insert(key.clone(), value.clone());
    }
}

fn extract_payload_request_metadata(params: &mut Map<String, Value>) -> Option<BTreeMap<String, String>> {
    let headers = string_record(params.get("headers"));
    if !params.contains_key("headers") && !params.contains_key("extra_body") {
        return headers;
    }
    params.remove("headers");
    params.remove("extra_body");
    headers
}

fn should_use_server_side_fallback_beta(model: &Model) -> bool {
    model
        .compat
        .as_ref()
        .and_then(|compat| compat.anthropic_messages().allowed_fallback_models)
        .is_some_and(|models| !models.is_empty())
}

fn should_use_fine_grained_tool_streaming_beta(model: &Model, context: &Context) -> bool {
    context.tools.as_ref().is_some_and(|tools| !tools.is_empty())
        && !get_anthropic_compat(model).supports_eager_tool_input_streaming
}

fn get_beta_features(model: &Model, context: &Context, is_oauth_token: bool, options: &StreamOptions) -> Vec<String> {
    let mut configured: Option<Option<String>> = None;
    if let Some(headers) = &model.headers {
        for (name, value) in headers {
            if name.to_lowercase() == "anthropic-beta" {
                configured = Some(Some(value.clone()));
            }
        }
    }
    if let Some(headers) = &options.request.headers {
        for (name, value) in headers {
            if name.to_lowercase() == "anthropic-beta" {
                configured = Some(value.clone());
            }
        }
    }
    match configured {
        Some(None) => return Vec::new(),
        Some(Some(text)) => {
            let mut features: Vec<String> = Vec::new();
            for feature in text.split(',').map(str::trim).filter(|feature| !feature.is_empty()) {
                if !features.iter().any(|existing| existing == feature) {
                    features.push(feature.to_owned());
                }
            }
            return features;
        }
        None => {}
    }

    let mut features: Vec<String> = Vec::new();
    let mut push = |feature: &str| {
        if !features.iter().any(|existing| existing == feature) {
            features.push(feature.to_owned());
        }
    };
    if is_oauth_token {
        push("claude-code-20250219");
        push("oauth-2025-04-20");
    }
    if should_use_fine_grained_tool_streaming_beta(model, context) {
        push(FINE_GRAINED_TOOL_STREAMING_BETA);
    }
    if model.reasoning
        && thinking_enabled(options) == Some(true)
        && interleaved_thinking(options)
        && !supports_adaptive_thinking(model)
    {
        push(INTERLEAVED_THINKING_BETA);
    }
    if should_use_server_side_fallback_beta(model) {
        push(SERVER_SIDE_FALLBACK_BETA);
    }
    if model.compat.as_ref().and_then(|compat| compat.anthropic_messages().supports_mid_convo_effort) == Some(true) {
        push(MID_CONVERSATION_OUTPUT_CONFIG_BETA);
        push(THINKING_BINDING_CONTROLS_BETA);
    }
    features
}

fn is_cacheable_user_content_block(block: Option<&Value>) -> bool {
    matches!(block.and_then(|block| block.get("type")).and_then(Value::as_str), Some("text" | "image" | "tool_result"))
}

fn append_user_blocks(params: &mut Vec<Value>, new_blocks: Vec<Value>) {
    if new_blocks.is_empty() {
        return;
    }
    let last_index = params.len().checked_sub(1);
    let last_role = last_index
        .and_then(|index| params.get(index))
        .and_then(|param| param.get("role"))
        .and_then(Value::as_str);
    if last_role == Some("user")
        && let Some(last_param) = last_index.and_then(|index| params.get_mut(index))
    {
        let Some(object) = last_param.as_object_mut() else { return };
        match object.get_mut("content") {
            Some(Value::String(text)) => {
                let mut blocks = vec![json!({ "type": "text", "text": text.clone() })];
                blocks.extend(new_blocks);
                object.insert("content".into(), Value::Array(blocks));
            }
            Some(Value::Array(blocks)) => blocks.extend(new_blocks),
            _ => {}
        }
        return;
    }
    params.push(json!({ "role": "user", "content": new_blocks }));
}

fn is_tool_loop_continuation(messages: &[Value]) -> bool {
    let tail = messages.last();
    let preceding = messages.len().checked_sub(2).and_then(|index| messages.get(index));
    let tail_is_user_with_tool_result = tail.is_some_and(|tail| {
        tail.get("role").and_then(Value::as_str) == Some("user")
            && tail
                .get("content")
                .and_then(Value::as_array)
                .is_some_and(|content| content.iter().any(|block| block.get("type").and_then(Value::as_str) == Some("tool_result")))
    });
    let preceding_is_assistant_with_tool_use = preceding.is_some_and(|preceding| {
        preceding.get("role").and_then(Value::as_str) == Some("assistant")
            && preceding
                .get("content")
                .and_then(Value::as_array)
                .is_some_and(|content| content.iter().any(|block| block.get("type").and_then(Value::as_str) == Some("tool_use")))
    });
    tail_is_user_with_tool_result && preceding_is_assistant_with_tool_use
}

fn mark_user_message_cache_checkpoint(message: &mut Value, cache_control: &Value) -> bool {
    let Some(object) = message.as_object_mut() else { return false };
    if object.get("role").and_then(Value::as_str) != Some("user") {
        return false;
    }
    match object.get_mut("content") {
        Some(Value::Array(content)) => {
            let last = content.last_mut();
            if !is_cacheable_user_content_block(last.as_deref()) {
                return false;
            }
            if let Some(Value::Object(block)) = content.last_mut() {
                block.insert("cache_control".into(), cache_control.clone());
            }
            true
        }
        Some(Value::String(text)) => {
            let text = text.clone();
            object.insert(
                "content".into(),
                json!([{ "type": "text", "text": text, "cache_control": cache_control.clone() }]),
            );
            true
        }
        _ => false,
    }
}

fn final_assistant_turn_is_foreign(messages: &[Message]) -> bool {
    for message in messages.iter().rev() {
        let Message::Assistant(assistant) = message else { continue };
        return assistant.api != "anthropic-messages";
    }
    false
}

fn final_assistant_turn_starts_with_tool_use(messages: &[Value]) -> bool {
    for message in messages.iter().rev() {
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(content) = message.get("content").and_then(Value::as_array) else { return false };
        let Some(first) = content.first() else { return false };
        if matches!(first.get("type").and_then(Value::as_str), Some("thinking" | "redacted_thinking")) {
            return false;
        }
        return content.iter().any(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"));
    }
    false
}

fn convert_tool_result(
    message: &ToolResultMessage,
    is_oauth_token: bool,
    deferred_tool_names: &HashSet<String>,
    loaded_tool_names: &mut HashSet<String>,
    normalize_tool_name: &dyn Fn(&str) -> String,
) -> (Value, Vec<Value>) {
    let mut references: Vec<Value> = Vec::new();
    for name in message.added_tool_names.iter().flatten() {
        let normalized_name = normalize_tool_name(name);
        if !deferred_tool_names.contains(&normalized_name) || loaded_tool_names.contains(&normalized_name) {
            continue;
        }
        loaded_tool_names.insert(normalized_name);
        references.push(json!({ "type": "tool_reference", "tool_name": if is_oauth_token { to_claude_code_name(name) } else { name.clone() } }));
    }

    let converted_content = convert_content_blocks(&message.content);
    let content = if references.is_empty() { converted_content.clone() } else { Value::Array(references.clone()) };
    let sibling_content = if references.is_empty() {
        Vec::new()
    } else {
        match converted_content {
            Value::String(text) => vec![json!({ "type": "text", "text": text })],
            Value::Array(blocks) => blocks,
            _ => Vec::new(),
        }
    };
    (
        json!({
            "type": "tool_result",
            "tool_use_id": message.tool_call_id,
            "content": content,
            "is_error": message.is_error,
        }),
        sibling_content,
    )
}

// The TS signature takes the same eight parameters positionally; boxing them into a struct would
// diverge from the source for no behavioral gain (same annotation as api/openai-images.ts's port).
#[allow(clippy::too_many_arguments)]
fn convert_messages(
    transformed_messages: &[Message],
    model: &Model,
    is_oauth_token: bool,
    cache_control: Option<&Value>,
    unsigned_thinking_replay: &str,
    deferred_tool_names: &HashSet<String>,
    normalize_tool_name: &dyn Fn(&str) -> String,
    managed_provider: Option<&str>,
) -> (Vec<Value>, BTreeMap<usize, String>) {
    let mut params: Vec<Value> = Vec::new();
    let mut assistant_levels: BTreeMap<usize, String> = BTreeMap::new();
    let mut loaded_tool_names: HashSet<String> = HashSet::new();
    let discarded_fallback_tool_call_ids = collect_discarded_fallback_tool_call_ids(transformed_messages, model);
    let rejects_native_web_search_replay = !get_anthropic_compat(model).supports_web_search;
    let provider_native_tool_pairing = collect_provider_native_tool_pairing(
        transformed_messages,
        model,
        deferred_tool_names,
        normalize_tool_name,
        &discarded_fallback_tool_call_ids,
    );

    let mut index = 0;
    while index < transformed_messages.len() {
        let message = &transformed_messages[index];
        match message {
            Message::User(user) => match &user.content {
                UserContent::Text(text) => {
                    if !crate::utils::js::trim(text).is_empty() {
                        let content = sanitize_surrogates(text);
                        let last_is_user =
                            params.last().and_then(|param| param.get("role")).and_then(Value::as_str) == Some("user");
                        if last_is_user {
                            append_user_blocks(&mut params, vec![json!({ "type": "text", "text": content })]);
                        } else {
                            params.push(json!({ "role": "user", "content": content }));
                        }
                    }
                }
                UserContent::Blocks(blocks) => {
                    let blocks: Vec<Value> = blocks
                        .iter()
                        .map(|block| match block {
                            ContentBlock::Text(text) => json!({ "type": "text", "text": sanitize_surrogates(&text.text) }),
                            ContentBlock::Image(image) if crate::types::is_video_mime_type(&image.mime_type) => json!({
                                "type": "video",
                                "source": { "type": "base64", "media_type": image.mime_type, "data": image.data },
                            }),
                            ContentBlock::Image(image) => json!({
                                "type": "image",
                                "source": { "type": "base64", "media_type": image.mime_type, "data": image.data },
                            }),
                            other => serde_json::to_value(other).unwrap_or(Value::Null),
                        })
                        .collect();
                    let filtered: Vec<Value> = blocks
                        .into_iter()
                        .filter(|block| {
                            block.get("type").and_then(Value::as_str) != Some("text")
                                || block
                                    .get("text")
                                    .and_then(Value::as_str)
                                    .is_some_and(|text| !crate::utils::js::trim(text).is_empty())
                        })
                        .collect();
                    if !filtered.is_empty() {
                        append_user_blocks(&mut params, filtered);
                    }
                }
            },
            Message::Assistant(assistant) => {
                let mut blocks: Vec<Value> = Vec::new();
                let is_same_model = is_same_anthropic_model(assistant, model);
                let fallback_boundary = if is_same_model {
                    last_anthropic_fallback_boundary(&assistant.content)
                } else {
                    None
                };
                let pre_boundary_paired_server_tool_use_ids = fallback_boundary
                    .map(|boundary| paired_server_tool_use_ids_before_boundary(&assistant.content, boundary))
                    .unwrap_or_default();

                for (block_index, block) in assistant.content.iter().enumerate() {
                    if let Some(boundary) = fallback_boundary
                        && block_index < boundary
                    {
                        if matches!(block, ContentBlock::Thinking(_) | ContentBlock::ToolCall(_)) {
                            continue;
                        }
                        if let ContentBlock::ProviderNative(native) = block
                            && is_anthropic_server_tool_use_block(&native.raw)
                            && !native
                                .raw
                                .get("id")
                                .and_then(Value::as_str)
                                .is_some_and(|id| pre_boundary_paired_server_tool_use_ids.contains(id))
                        {
                            continue;
                        }
                    }
                    match block {
                        ContentBlock::Text(text) => {
                            if crate::utils::js::trim(&text.text).is_empty() {
                                continue;
                            }
                            blocks.push(json!({ "type": "text", "text": sanitize_surrogates(&text.text) }));
                        }
                        ContentBlock::Thinking(thinking) => {
                            if thinking.redacted == Some(true) {
                                blocks.push(json!({
                                    "type": "redacted_thinking",
                                    "data": thinking.thinking_signature.clone().unwrap_or_default(),
                                }));
                                continue;
                            }
                            let thinking_signature = thinking.thinking_signature.clone().unwrap_or_default();
                            let has_thinking_signature = !crate::utils::js::trim(&thinking_signature).is_empty();
                            if crate::utils::js::trim(&thinking.thinking).is_empty() && !has_thinking_signature {
                                continue;
                            }
                            if !has_thinking_signature {
                                if unsigned_thinking_replay == "empty-signature" {
                                    blocks.push(json!({
                                        "type": "thinking",
                                        "thinking": sanitize_surrogates(&thinking.thinking),
                                        "signature": "",
                                    }));
                                } else {
                                    blocks.push(json!({
                                        "type": "text",
                                        "text": sanitize_surrogates(&thinking.thinking),
                                    }));
                                }
                            } else {
                                blocks.push(json!({
                                    "type": "thinking",
                                    "thinking": thinking.thinking,
                                    "signature": thinking_signature,
                                }));
                            }
                        }
                        ContentBlock::ToolCall(call) => {
                            blocks.push(json!({
                                "type": "tool_use",
                                "id": call.id,
                                "name": if is_oauth_token { to_claude_code_name(&call.name) } else { call.name.clone() },
                                "input": Value::Object(call.arguments.clone()),
                            }));
                        }
                        ContentBlock::ProviderNative(native) => {
                            if is_same_model
                                && is_replayable_anthropic_provider_native_block(&native.raw)
                                && !(rejects_native_web_search_replay && is_anthropic_web_search_replay_block(&native.raw))
                                && !is_unpaired_provider_native_tool_block(&native.raw, &provider_native_tool_pairing)
                            {
                                blocks.push(native.raw.clone());
                            }
                        }
                        ContentBlock::Image(_) => {}
                    }
                }

                if blocks.is_empty() {
                    index += 1;
                    continue;
                }
                let message_index = params.len();
                params.push(json!({ "role": "assistant", "content": blocks }));
                if let Some(managed_provider) = managed_provider
                    && assistant.api == "anthropic-messages"
                    && assistant.provider == managed_provider
                    && is_anthropic_effort(assistant.provider_thinking_level.as_deref())
                {
                    assistant_levels.insert(message_index, assistant.provider_thinking_level.clone().unwrap_or_default());
                }
            }
            Message::ToolResult(_) => {
                let mut tool_results: Vec<Value> = Vec::new();
                let mut sibling_content: Vec<Value> = Vec::new();
                let mut next = index;
                while next < transformed_messages.len() {
                    let Message::ToolResult(result) = &transformed_messages[next] else { break };
                    if !discarded_fallback_tool_call_ids.contains(&result.tool_call_id) {
                        let (tool_result, siblings) = convert_tool_result(
                            result,
                            is_oauth_token,
                            deferred_tool_names,
                            &mut loaded_tool_names,
                            normalize_tool_name,
                        );
                        tool_results.push(tool_result);
                        sibling_content.extend(siblings);
                    }
                    next += 1;
                }
                // The TS loop assigns `i = j - 1` so the loop's own `i++` lands on the message after
                // the run; assigning `j` here would skip it.
                index = next - 1;
                if tool_results.is_empty() {
                    continue;
                }
                let mut blocks = tool_results;
                blocks.extend(sibling_content);
                append_user_blocks(&mut params, blocks);
            }
            Message::ConfigurationUpdate(_) => {}
        }
        index += 1;
    }

    if let Some(cache_control) = cache_control
        && !params.is_empty()
    {
        let retain_preceding_checkpoint = is_tool_loop_continuation(&params);
        let last_index = params.len() - 1;
        if mark_user_message_cache_checkpoint(&mut params[last_index], cache_control)
            && retain_preceding_checkpoint
        {
            let mut index = params.len() as i64 - 2;
            while index >= 0 {
                if mark_user_message_cache_checkpoint(&mut params[index as usize], cache_control) {
                    break;
                }
                index -= 1;
            }
        }
    }

    (params, assistant_levels)
}

fn insert_thinking_level_messages(messages: Vec<Value>, assistant_levels: &BTreeMap<usize, String>, active_effort: &str) -> Vec<Value> {
    let mut result: Vec<Value> = Vec::new();
    for (index, message) in messages.into_iter().enumerate() {
        if let Some(historical_effort) = assistant_levels.get(&index) {
            result.push(json!({ "role": "system", "content": [], "output_config": { "effort": historical_effort } }));
        }
        result.push(message);
    }
    result.push(json!({ "role": "system", "content": [], "output_config": { "effort": active_effort } }));
    result
}

fn convert_tools(
    tools: &[Tool],
    is_oauth_token: bool,
    supports_eager_tool_input_streaming: bool,
    supports_strict_tools: bool,
    cache_control: Option<&Value>,
    defer_loading: bool,
) -> Result<Vec<Value>, String> {
    let mut result = Vec::with_capacity(tools.len());
    for (index, tool) in tools.iter().enumerate() {
        let strict = resolve_json_schema_strict_sampling(tool, supports_strict_tools)?;
        let parameters = get_json_schema_tool_parameters(tool, strict).map_err(|error| error.to_string())?;
        let schema = resolve_root_object_schema(parameters.as_object().unwrap_or(&Map::new()));
        let legacy_input_schema = json!({
            "type": "object",
            "properties": schema.get("properties").cloned().unwrap_or_else(|| json!({})),
            "required": schema.get("required").cloned().unwrap_or_else(|| json!([])),
        });
        let input_schema = if strict == Some(true) {
            let mut merged = parameters.as_object().cloned().unwrap_or_default();
            if let Some(object) = legacy_input_schema.as_object() {
                for (key, value) in object {
                    merged.insert(key.clone(), value.clone());
                }
            }
            Value::Object(merged)
        } else {
            legacy_input_schema
        };

        let mut converted = Map::new();
        converted.insert("name".into(), Value::from(if is_oauth_token { to_claude_code_name(&tool.name) } else { tool.name.clone() }));
        converted.insert("description".into(), Value::from(tool.description.clone()));
        if supports_eager_tool_input_streaming {
            converted.insert("eager_input_streaming".into(), Value::from(true));
        }
        if strict == Some(true) {
            converted.insert("strict".into(), Value::from(true));
        }
        converted.insert("input_schema".into(), input_schema);
        if defer_loading {
            converted.insert("defer_loading".into(), Value::from(true));
        }
        if let Some(cache_control) = cache_control
            && index == tools.len() - 1
        {
            converted.insert("cache_control".into(), cache_control.clone());
        }
        result.push(Value::Object(converted));
    }
    Ok(result)
}

fn map_stop_reason(
    reason: &str,
    stop_details: Option<&Value>,
) -> Result<(StopReason, Option<String>, Option<AssistantStopDetails>), String> {
    let explanation = stop_details
        .and_then(|details| details.get("explanation"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    match reason {
        "end_turn" => Ok((StopReason::Stop, None, None)),
        "max_tokens" => Ok((StopReason::Length, None, None)),
        "tool_use" => Ok((StopReason::ToolUse, None, None)),
        "refusal" => Ok((
            StopReason::Error,
            Some(explanation.clone().unwrap_or_else(|| "The model refused to complete the request".to_owned())),
            Some(AssistantStopDetails::Refusal { explanation }),
        )),
        "pause_turn" => Ok((StopReason::Stop, None, None)),
        "stop_sequence" => Ok((StopReason::Stop, None, None)),
        "sensitive" => Ok((
            StopReason::Error,
            Some("Provider stopped with: sensitive".to_owned()),
            Some(AssistantStopDetails::Sensitive),
        )),
        other => Err(format!("Unhandled stop reason: {other}")),
    }
}

fn sanitize_unsupported_native_tools(model: &Model, params: &mut Map<String, Value>) {
    let headers = string_record(params.get("headers")).map(|record| {
        record.into_iter().map(|(key, value)| (key, Some(value))).collect::<BTreeMap<_, _>>()
    });
    let header_sanitization = if rejects_computer_use_beta(model) {
        remove_computer_use_beta_header(headers.as_ref())
    } else {
        (false, None)
    };
    let rejects_native_web_search = !get_anthropic_compat(model).supports_web_search;
    let mut changed = false;

    if let Some(Value::Array(tools)) = params.get("tools").cloned().as_ref() {
        let mut supported_tools: Vec<Value> = Vec::new();
        for tool in tools {
            let rejected = tool.get("type").and_then(Value::as_str).is_some_and(|tool_type| {
                rejects_native_computer_tool(model, tool_type)
                    || (rejects_native_web_search && is_anthropic_web_search_tool_type(tool_type))
            });
            if rejected {
                changed = true;
                continue;
            }
            supported_tools.push(tool.clone());
        }
        if changed {
            if supported_tools.is_empty() {
                params.remove("tools");
            } else {
                params.insert("tools".into(), Value::Array(supported_tools));
            }
        }
    }

    if header_sanitization.0 {
        changed = true;
        match header_sanitization.1 {
            Some(headers) => {
                let record: Map<String, Value> = headers
                    .into_iter()
                    .map(|(key, value)| (key, value.map_or(Value::Null, Value::from)))
                    .collect();
                params.insert("headers".into(), Value::Object(record));
            }
            None => {
                params.remove("headers");
            }
        }
    }

    if changed
        && let Some(tool_choice) = params.get("tool_choice").and_then(Value::as_object)
    {
        let tool_choice_name = tool_choice.get("name").and_then(Value::as_str);
        let has_selected_tool = tool_choice_name.is_some_and(|name| {
            params
                .get("tools")
                .and_then(Value::as_array)
                .is_some_and(|tools| tools.iter().any(|tool| tool.get("name").and_then(Value::as_str) == Some(name)))
        });
        let should_remove_tool_choice = !params.contains_key("tools")
            || (tool_choice_name.is_some() && !has_selected_tool);
        if should_remove_tool_choice {
            params.remove("tool_choice");
        }
    }
}

fn sanitize_adaptive_thinking_payload(model: &Model, params: &mut Map<String, Value>, options: &StreamOptions) {
    if !supports_adaptive_thinking(model) {
        return;
    }
    let headers = string_record(params.get("headers")).map(|record| {
        record.into_iter().map(|(key, value)| (key, Some(value))).collect::<BTreeMap<_, _>>()
    });
    let header_sanitization = remove_anthropic_beta_headers(headers.as_ref(), |beta| beta == INTERLEAVED_THINKING_BETA);
    let mut changed = false;

    let thinking = params.get("thinking").and_then(Value::as_object).cloned();
    if thinking.as_ref().and_then(|thinking| thinking.get("type")).and_then(Value::as_str) == Some("enabled") {
        let display = match thinking.as_ref().and_then(|thinking| thinking.get("display")).and_then(Value::as_str) {
            Some(display @ ("omitted" | "summarized")) => display.to_owned(),
            _ => thinking_display(options).unwrap_or_else(|| "summarized".to_owned()),
        };
        params.insert("thinking".into(), json!({ "type": "adaptive", "display": display }));
        if let Some(effort) = effort_option(options)
            && params.get("output_config").and_then(Value::as_object).is_none()
        {
            params.insert("output_config".into(), json!({ "effort": effort }));
        }
        changed = true;
    }

    if header_sanitization.0 {
        changed = true;
        match header_sanitization.1 {
            Some(headers) => {
                let record: Map<String, Value> = headers
                    .into_iter()
                    .map(|(key, value)| (key, value.map_or(Value::Null, Value::from)))
                    .collect();
                params.insert("headers".into(), Value::Object(record));
            }
            None => {
                params.remove("headers");
            }
        }
    }

    let _ = changed;
}

fn build_params(
    model: &Model,
    context: &Context,
    is_oauth_token: bool,
    options: &StreamOptions,
    unsigned_thinking_replay: &str,
) -> Result<Map<String, Value>, String> {
    let compat = get_anthropic_compat(model);
    let (_, cache_control) = get_cache_control(
        model,
        options.cache_retention.or(model.cache_retention),
        options.request.env.as_ref(),
    );
    let transformed_messages = transform_messages(
        &context.messages,
        model,
        Some(&|id: &str, _model: &Model, _assistant: &AssistantMessage| normalize_tool_call_id(id)),
        &TransformMessagesOptions {
            preserve_thinking: Some(thinking_enabled(options) == Some(true)),
            preserve_unsigned_thinking: Some(true),
            ..TransformMessagesOptions::default()
        },
    );
    let normalize_tool_name = |name: &str| {
        if is_oauth_token { to_claude_code_name(name) } else { name.to_owned() }
    };
    let discarded_fallback_tool_call_ids = collect_discarded_fallback_tool_call_ids(&transformed_messages, model);
    let partition_messages: Vec<Message> = transformed_messages
        .iter()
        .filter(|message| {
            !matches!(message, Message::ToolResult(result) if discarded_fallback_tool_call_ids.contains(&result.tool_call_id))
        })
        .cloned()
        .collect();
    let placement = split_deferred_tools(
        &Context { system_prompt: context.system_prompt.clone(), messages: partition_messages, tools: context.tools.clone() },
        compat.supports_tool_references,
        Some(&normalize_tool_name),
    );
    let mut immediate_tools = placement.immediate;
    let mut deferred_tools: Vec<Tool> = placement.deferred.into_values().collect();
    if immediate_tools.is_empty() && !deferred_tools.is_empty() {
        immediate_tools = std::mem::take(&mut deferred_tools);
    }
    let deferred_tool_names: HashSet<String> = deferred_tools.iter().map(|tool| normalize_tool_name(&tool.name)).collect();
    let active_effort = effort_option(options).unwrap_or_else(|| "high".to_owned());
    let beta_features = get_beta_features(model, context, is_oauth_token, options);
    let (converted_messages, assistant_levels) = convert_messages(
        &transformed_messages,
        model,
        is_oauth_token,
        cache_control.as_ref(),
        unsigned_thinking_replay,
        &deferred_tool_names,
        &normalize_tool_name,
        model.compat.as_ref().and_then(|compat| compat.anthropic_messages().supports_mid_convo_effort).filter(|supported| *supported).map(|_| model.provider.as_str()),
    );
    let mut messages = if model.compat.as_ref().and_then(|compat| compat.anthropic_messages().supports_mid_convo_effort) == Some(true) {
        insert_thinking_level_messages(converted_messages, &assistant_levels, &active_effort)
    } else {
        converted_messages
    };

    if model.compat.as_ref().and_then(|compat| compat.anthropic_messages().supports_mid_convo_effort) == Some(true)
        && let Some(cache_control) = cache_control.as_ref()
    {
        let mut index = messages.len() as i64 - 1;
        while index >= 0 {
            if messages[index as usize].get("role").and_then(Value::as_str) == Some("user") {
                mark_user_message_cache_checkpoint(&mut messages[index as usize], cache_control);
                break;
            }
            index -= 1;
        }
    }

    let mut params = Map::new();
    params.insert("model".into(), Value::from(model.id.clone()));
    params.insert("messages".into(), Value::Array(messages));
    params.insert("max_tokens".into(), Value::from(options.max_tokens.unwrap_or(model.max_tokens)));
    params.insert("stream".into(), Value::from(true));
    if !beta_features.is_empty() {
        params.insert("betas".into(), json!(beta_features));
    }

    if is_oauth_token {
        let mut system: Vec<Value> = Vec::new();
        let mut identity = Map::new();
        identity.insert("type".into(), Value::from("text"));
        identity.insert("text".into(), Value::from(""));
        if context.system_prompt.is_none()
            && let Some(cache_control) = cache_control.as_ref()
        {
            identity.insert("cache_control".into(), cache_control.clone());
        }
        system.push(Value::Object(identity));
        if let Some(system_prompt) = &context.system_prompt {
            let mut block = Map::new();
            block.insert("type".into(), Value::from("text"));
            block.insert("text".into(), Value::from(sanitize_surrogates(system_prompt)));
            if let Some(cache_control) = cache_control.as_ref() {
                block.insert("cache_control".into(), cache_control.clone());
            }
            system.push(Value::Object(block));
        }
        params.insert("system".into(), Value::Array(system));
    } else if let Some(system_prompt) = &context.system_prompt {
        let mut block = Map::new();
        block.insert("type".into(), Value::from("text"));
        block.insert("text".into(), Value::from(sanitize_surrogates(system_prompt)));
        if let Some(cache_control) = cache_control.as_ref() {
            block.insert("cache_control".into(), cache_control.clone());
        }
        params.insert("system".into(), Value::Array(vec![Value::Object(block)]));
    }

    if let Some(temperature) = options.temperature
        && thinking_enabled(options) != Some(true)
        && compat.supports_temperature
    {
        params.insert("temperature".into(), json!(temperature));
    }

    if !immediate_tools.is_empty() || !deferred_tools.is_empty() {
        let mut tools = convert_tools(
            &immediate_tools,
            is_oauth_token,
            compat.supports_eager_tool_input_streaming,
            compat.supports_strict_tools,
            if compat.supports_cache_control_on_tools { cache_control.as_ref() } else { None },
            false,
        )?;
        tools.extend(convert_tools(
            &deferred_tools,
            is_oauth_token,
            compat.supports_eager_tool_input_streaming,
            compat.supports_strict_tools,
            None,
            true,
        )?);
        params.insert("tools".into(), Value::Array(tools));
    }

    let managed_effort = model.compat.as_ref().and_then(|compat| compat.anthropic_messages().supports_mid_convo_effort) == Some(true);
    if managed_effort && thinking_enabled(options) != Some(false) {
        params.insert(
            "thinking".into(),
            json!({
                "type": "adaptive",
                "display": thinking_display(options).unwrap_or_else(|| "summarized".to_owned()),
                "block_binding": { "prefix_mismatch_behavior": "drop_block" },
            }),
        );
        params.insert("output_config".into(), json!({ "effort": "high" }));
    } else if model.reasoning {
        if thinking_enabled(options) == Some(true) {
            let display = thinking_display(options).unwrap_or_else(|| "summarized".to_owned());
            if supports_adaptive_thinking(model) {
                params.insert("thinking".into(), json!({ "type": "adaptive", "display": display }));
                if let Some(effort) = effort_option(options) {
                    params.insert("output_config".into(), json!({ "effort": effort }));
                }
            } else {
                let budget_tokens = option_u64(options, "thinkingBudgetTokens").filter(|budget| *budget != 0).unwrap_or(1024);
                params.insert(
                    "thinking".into(),
                    json!({ "type": "enabled", "budget_tokens": budget_tokens, "display": display }),
                );
            }
        } else if thinking_enabled(options) == Some(false) {
            disable_thinking_for_request(&mut params, model, compat.supports_disabled_thinking);
        }
    }

    if params.get("thinking").and_then(|thinking| thinking.get("type")).and_then(Value::as_str) == Some("enabled")
        && final_assistant_turn_is_foreign(&context.messages)
        && final_assistant_turn_starts_with_tool_use(&params.get("messages").and_then(Value::as_array).cloned().unwrap_or_default())
    {
        disable_thinking_for_request(&mut params, model, compat.supports_disabled_thinking);
    }

    if let Some(user_id) = options
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("user_id"))
        .and_then(Value::as_str)
    {
        params.insert("metadata".into(), json!({ "user_id": user_id }));
    }

    if let Some(tool_choice) = option_value(options, "toolChoice")
        && compat.supports_tool_choice
    {
        let is_forced_tool_choice = tool_choice.as_str() == Some("any") || tool_choice.is_object();
        if !is_forced_tool_choice || compat.supports_forced_tool_choice {
            let mapped = match &tool_choice {
                Value::String(choice) => json!({ "type": choice }),
                other => other.clone(),
            };
            params.insert("tool_choice".into(), mapped);
        }
    }

    apply_extra_body_to_anthropic_params(&mut params, options.extra_body.as_ref());

    if let Some(fallbacks) = option_value(options, "refusalFallbacks") {
        let mapped = match fallbacks {
            Value::String(tag) if tag == "default" => Value::from("default"),
            Value::Array(models) => Value::Array(models),
            other => other,
        };
        params.insert("fallbacks".into(), mapped);
    }

    Ok(params)
}

#[derive(Debug, Clone)]
pub(crate) enum AnthropicStreamError {
    Http { status: u16, message: String, headers: Box<HeaderMap>, parsed_body: Option<Box<Value>> },
    Network { message: String, timeout: bool },
    Message(String),
    Aborted,
}

impl std::fmt::Display for AnthropicStreamError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http { message, .. } | Self::Network { message, .. } | Self::Message(message) => {
                formatter.write_str(message)
            }
            Self::Aborted => formatter.write_str("Request aborted"),
        }
    }
}

impl AnthropicStreamError {
    fn status(&self) -> Option<u16> {
        match self {
            Self::Http { status, .. } => Some(*status),
            _ => None,
        }
    }

    fn headers(&self) -> Option<&HeaderMap> {
        match self {
            Self::Http { headers, .. } => Some(headers.as_ref()),
            _ => None,
        }
    }

    /// The error shape the TS `normalizeAnthropicRetryFailure` duck-types on a thrown error.
    fn retry_error_shape(&self) -> RetryErrorShape {
        match self {
            Self::Http { status, message, parsed_body, headers, .. } => RetryErrorShape {
                is_error: true,
                name: "Error".into(),
                class_name: Some(sdk_error_class_name(*status).to_owned()),
                message: message.clone(),
                status: Some(*status),
                headers: Some(headers.as_ref().clone()),
                error: parsed_body.as_deref().cloned(),
            },
            Self::Network { message, timeout } => RetryErrorShape {
                is_error: true,
                name: "Error".into(),
                class_name: Some(
                    if *timeout { "APIConnectionTimeoutError" } else { "APIConnectionError" }.to_owned(),
                ),
                message: message.clone(),
                status: None,
                headers: None,
                error: None,
            },
            Self::Message(message) => RetryErrorShape {
                is_error: true,
                name: "Error".into(),
                class_name: None,
                message: message.clone(),
                status: None,
                headers: None,
                error: None,
            },
            Self::Aborted => RetryErrorShape {
                is_error: true,
                name: "AbortError".into(),
                class_name: Some("Error".to_owned()),
                message: "Request aborted".into(),
                status: None,
                headers: None,
                error: None,
            },
        }
    }
}

impl ProviderRequestError for AnthropicStreamError {
    fn provider_status(&self) -> Option<ProviderErrorStatus<'_>> {
        match self {
            Self::Http { status, headers, .. } => {
                Some(ProviderErrorStatus { status: Some(*status), headers: Some(headers.as_ref()) })
            }
            Self::Network { .. } => Some(ProviderErrorStatus { status: None, headers: None }),
            _ => None,
        }
    }
}

fn sdk_error_class_name(status: u16) -> &'static str {
    match status {
        400 => "BadRequestError",
        401 => "AuthenticationError",
        403 => "PermissionDeniedError",
        404 => "NotFoundError",
        422 => "UnprocessableEntityError",
        429 => "RateLimitError",
        500..=599 => "InternalServerError",
        _ => "APIError",
    }
}

/// The `@anthropic-ai/sdk` `APIError.makeMessage` text for a failed response.
fn sdk_api_error_message(status: u16, parsed_body: Option<&Value>, text: &str) -> String {
    let body_message = parsed_body.and_then(|body| body.get("message")).map(|message| match message {
        Value::String(message) => message.clone(),
        other => other.to_string(),
    });
    let message = match body_message {
        Some(message) => Some(message),
        None => match parsed_body {
            Some(body) => Some(body.to_string()),
            None => Some(text.to_owned()),
        },
    };
    match message {
        Some(message) if !message.is_empty() => format!("{status} {message}"),
        _ => format!("{status} status code (no body)"),
    }
}

fn retry_failure_kind_str(kind: RetryFailureKind) -> &'static str {
    match kind {
        RetryFailureKind::Abort => "abort",
        RetryFailureKind::Connection => "connection",
        RetryFailureKind::Timeout => "timeout",
        RetryFailureKind::EmptyResponse => "empty-response",
        RetryFailureKind::QuotaExhausted => "quota-exhausted",
        RetryFailureKind::HttpStatus => "http-status",
        RetryFailureKind::ImageFormat => "image-format",
        RetryFailureKind::Provider => "provider",
        RetryFailureKind::Refusal => "refusal",
        RetryFailureKind::Sensitive => "sensitive",
        RetryFailureKind::Unknown => "unknown",
    }
}

fn retry_failure_details(failure: &RetryFailure) -> Option<Map<String, Value>> {
    let mut details = Map::new();
    details.insert("kind".into(), Value::from(retry_failure_kind_str(failure.kind)));
    if let Some(status_code) = failure.status_code {
        details.insert("statusCode".into(), Value::from(status_code));
    }
    if let Some(provider_codes) = &failure.provider_codes {
        details.insert("providerCodes".into(), json!(provider_codes));
    }
    if let Some(retry_after_ms) = failure.retry_after_ms {
        details.insert("retryAfterMs".into(), Value::from(retry_after_ms));
    }
    if let Some(should_retry) = failure.should_retry {
        details.insert("shouldRetry".into(), Value::from(should_retry));
    }
    Some(details)
}

// --- The unsigned-thinking text-replay fallback (one conversation, learned from a 400).

static UNSIGNED_THINKING_TEXT_REPLAY_FALLBACKS: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

static UNSIGNED_THINKING_CLEANUP: LazyLock<()> = LazyLock::new(|| {
    let _ = register_session_resource_cleanup(std::sync::Arc::new(|session_id: Option<&str>| {        let mut fallbacks = UNSIGNED_THINKING_TEXT_REPLAY_FALLBACKS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        match session_id {
            None => fallbacks.clear(),
            Some(session_id) => {
                let prefix = format!("{session_id}\u{0}");
                fallbacks.retain(|key| !key.starts_with(&prefix));
            }
        }
        Ok(())
    }));
});

fn unsigned_thinking_fallback_key(model: &Model, session_id: Option<&str>) -> Option<String> {
    session_id.map(|session_id| format!("{session_id}\u{0}{}\u{0}{}", model.base_url, model.id))
}

fn is_invalid_unsigned_thinking_signature_error(error: &AnthropicStreamError) -> bool {
    match error {
        AnthropicStreamError::Http { status, message, .. } => *status == 400 && message.contains("Invalid signature in thinking block"),
        _ => false,
    }
}

fn unsigned_thinking_replay_value(value: Option<&crate::types::UnsignedThinkingReplay>) -> &'static str {
    match value {
        Some(crate::types::UnsignedThinkingReplay::EmptySignature) => "empty-signature",
        _ => "text",
    }
}

/// The TS fallback flips a shared mutable variable that the retry closure reads on its next call;
/// a `Mutex` gives the same cross-call mutation while staying `Sync` for the spawned stream task.
fn replay_mode(replay: &Mutex<&'static str>) -> &'static str {
    *replay.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

// --- Request creation.

struct AnthropicClient {
    base_url: String,
    headers: BTreeMap<String, Option<String>>,
    is_oauth_token: bool,
}

fn create_client(
    model: &Model,
    context: &Context,
    options: &StreamOptions,
    api_key: Option<&str>,
    dynamic_headers: Option<&BTreeMap<String, String>>,
    session_id: Option<&str>,
) -> AnthropicClient {
    let compat = get_anthropic_compat(model);
    let is_oauth_token = api_key.is_some_and(|key| key.contains("sk-ant-oat"));
    let needs_interleaved_beta = interleaved_thinking(options) && !supports_adaptive_thinking(model);
    let mut beta_features: Vec<String> = Vec::new();
    if should_use_fine_grained_tool_streaming_beta(model, context) {
        beta_features.push(FINE_GRAINED_TOOL_STREAMING_BETA.to_owned());
    }
    if needs_interleaved_beta {
        beta_features.push(INTERLEAVED_THINKING_BETA.to_owned());
    }
    if options.extra.contains_key("refusalFallbacks") {
        beta_features.push(SERVER_SIDE_FALLBACK_BETA.to_owned());
    }
    let beta_header = (!beta_features.is_empty()).then(|| beta_features.join(","));

    let mut base: BTreeMap<String, Option<String>> = BTreeMap::new();
    let base_url = if model.provider == "cloudflare-ai-gateway" {
        base.insert("accept".into(), Some("application/json".into()));
        base.insert("anthropic-dangerous-direct-browser-access".into(), Some("true".into()));
        base.insert("cf-aig-authorization".into(), Some(format!("Bearer {}", api_key.unwrap_or_default())));
        base.insert("x-api-key".into(), None);
        base.insert("Authorization".into(), None);
        if let Some(beta_header) = &beta_header {
            base.insert("anthropic-beta".into(), Some(beta_header.clone()));
        }
        resolve_cloudflare_base_url(model, options.request.env.as_ref())
    } else if model.provider == "github-copilot" {
        base.insert("User-Agent".into(), Some(get_pi_user_agent()));
        base.insert("accept".into(), Some("application/json".into()));
        base.insert("anthropic-dangerous-direct-browser-access".into(), Some("true".into()));
        if let Some(beta_header) = &beta_header {
            base.insert("anthropic-beta".into(), Some(beta_header.clone()));
        }
        model.base_url.clone()
    } else if is_oauth_token {
        base.insert("User-Agent".into(), Some(get_pi_user_agent()));
        base.insert("accept".into(), Some("application/json".into()));
        base.insert("anthropic-dangerous-direct-browser-access".into(), Some("true".into()));
        let mut oauth_betas = vec!["claude-code-20250219".to_owned(), "oauth-2025-04-20".to_owned()];
        oauth_betas.extend(beta_features.clone());
        base.insert("anthropic-beta".into(), Some(oauth_betas.join(",")));
        base.insert("user-agent".into(), Some(format!("claude-cli/{CLAUDE_CODE_VERSION}")));
        base.insert("x-app".into(), Some("cli".into()));
        model.base_url.clone()
    } else {
        base.insert("User-Agent".into(), Some(get_pi_user_agent()));
        base.insert("accept".into(), Some("application/json".into()));
        base.insert("anthropic-dangerous-direct-browser-access".into(), Some("true".into()));
        if let Some(beta_header) = &beta_header {
            base.insert("anthropic-beta".into(), Some(beta_header.clone()));
        }
        if let Some(session_id) = session_id.filter(|_| compat.send_session_affinity_headers) {
            let header = if compat.session_affinity_format == Some(crate::types::AnthropicSessionAffinityFormat::Openrouter) {
                "x-session-id"
            } else {
                "x-session-affinity"
            };
            base.insert(header.to_owned(), Some(session_id.to_owned()));
        }
        model.base_url.clone()
    };

    let model_headers = model.headers.as_ref().map(nullable_headers);
    let dynamic_nullable = dynamic_headers.map(nullable_headers);
    let sources = if model.provider == "github-copilot" {
        vec![Some(&base), model_headers.as_ref(), dynamic_nullable.as_ref(), options.request.headers.as_ref()]
    } else {
        vec![Some(&base), model_headers.as_ref(), options.request.headers.as_ref()]
    };
    let headers = sanitize_adaptive_thinking_headers(model, merge_headers(&sources));
    AnthropicClient { base_url, headers, is_oauth_token }
}

/// The SDK's per-request headers: default headers, the `anthropic-version` constant and the auth
/// header its `apiKey`/`authToken` option would add.
fn request_headers(client: &AnthropicClient, model: &Model, api_key: Option<&str>) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (name, value) in &client.headers {
        let Ok(name) = HeaderName::from_bytes(name.as_bytes()) else { continue };
        match value {
            None => {
                map.remove(name);
            }
            Some(value) => {
                if let Ok(value) = HeaderValue::from_str(value) {
                    map.insert(name, value);
                }
            }
        }
    }
    map.insert(HeaderName::from_static("anthropic-version"), HeaderValue::from_static("2023-06-01"));
    map.insert(reqwest::header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    if model.provider == "cloudflare-ai-gateway" {
        return map;
    }
    let Some(api_key) = api_key else { return map };
    let (name, value) = if model.provider == "github-copilot" || client.is_oauth_token {
        (HeaderName::from_static("authorization"), format!("Bearer {api_key}"))
    } else {
        (HeaderName::from_static("x-api-key"), api_key.to_owned())
    };
    if let Ok(value) = HeaderValue::from_str(&value) {
        map.insert(name, value);
    }
    map
}

struct PreparedRequest {
    params: Map<String, Value>,
    headers: Option<BTreeMap<String, String>>,
    sent_model: String,
}

async fn build_prepared_request(
    model: &Model,
    context: &Context,
    is_oauth_token: bool,
    options: &StreamOptions,
    unsigned_thinking_replay: &str,
) -> Result<PreparedRequest, AnthropicStreamError> {
    let mut params = build_params(model, context, is_oauth_token, options, unsigned_thinking_replay)
        .map_err(AnthropicStreamError::Message)?;
    if let Some(next) = options.request.apply_payload_hook(&Value::Object(params.clone()), model, None)
        .await.map_err(AnthropicStreamError::Message)?
        && let Some(next) = next.as_object()
    {
        params = next.clone();
    }
    sanitize_adaptive_thinking_payload(model, &mut params, options);
    sanitize_unsupported_native_tools(model, &mut params);
    params = demote_unavailable_tool_references(&params);
    params = sanitize_anthropic_tool_pairs(&params);
    let headers = extract_payload_request_metadata(&mut params);
    Ok(PreparedRequest {
        sent_model: params.get("model").and_then(Value::as_str).unwrap_or_default().to_owned(),
        params,
        headers,
    })
}

fn is_forced_anthropic_tool_choice(tool_choice: Option<&Value>) -> bool {
    match tool_choice {
        Some(Value::Object(choice)) => matches!(choice.get("type").and_then(Value::as_str), Some("any" | "tool")),
        _ => false,
    }
}

async fn send_request(
    model: &Model,
    client: &AnthropicClient,
    options: &StreamOptions,
    api_key: Option<&str>,
    prepared: &PreparedRequest,
) -> Result<reqwest::Response, AnthropicStreamError> {
    let base_url = client.base_url.trim_end_matches('/');
    let url = format!("{base_url}/v1/messages");
    let http_client = options.request.fetch.clone().unwrap_or_default();
    let mut body = prepared.params.clone();
    let mut headers = request_headers(client, model, api_key);
    if let Some(betas) = body.remove("betas") {
        let joined = match betas {
            Value::Array(items) => items.iter().map(js_to_string).collect::<Vec<_>>().join(","),
            other => js_to_string(&other),
        };
        if let Ok(value) = HeaderValue::from_str(&joined) {
            headers.insert(HeaderName::from_static("anthropic-beta"), value);
        }
    }
    if let Some(overrides) = &prepared.headers {
        for (name, value) in overrides {
            let Ok(name) = HeaderName::from_bytes(name.as_bytes()) else { continue };
            if let Ok(value) = HeaderValue::from_str(value) {
                headers.insert(name, value);
            }
        }
    }
    let body = Value::Object(body).to_string();
    let request = http_client.post(&url).headers(headers).body(body);
    let timeout_ms = options.request.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
    match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), request.send()).await {
        Err(_) => Err(AnthropicStreamError::Network {
            message: format!("Request timed out after {timeout_ms}ms"),
            timeout: true,
        }),
        Ok(Err(error)) => {
            let timeout = error.is_timeout();
            Err(AnthropicStreamError::Network { message: error.to_string(), timeout })
        }
        Ok(Ok(response)) => Ok(response),
    }
}

fn js_to_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

async fn classify_response_failure(response: reqwest::Response) -> AnthropicStreamError {
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let text = response.text().await.unwrap_or_default();
    let parsed_body: Option<Value> = serde_json::from_str(&text).ok();
    let message = sdk_api_error_message(status, parsed_body.as_ref(), &text);
    AnthropicStreamError::Http {
        status,
        message,
        headers: Box::new(headers),
        parsed_body: parsed_body.map(Box::new),
    }
}

fn is_anthropic_message_start(event: &Value) -> bool {
    event.get("type").and_then(Value::as_str) == Some("message_start")
}

fn is_anthropic_message_stop(event: &Value) -> bool {
    event.get("type").and_then(Value::as_str) == Some("message_stop")
}

fn parse_anthropic_event(sse: &ServerSentEvent) -> Result<Value, AnthropicStreamError> {
    parse_json_with_repair::<Value>(&sse.data).map_err(|error| {
        AnthropicStreamError::Message(format!(
            "Could not parse Anthropic SSE event {}: {}; data={}; raw={}",
            sse.event.clone().unwrap_or_default(),
            error,
            sse.data,
            sse.raw.join("\\n")
        ))
    })
}

fn decode_anthropic_event(sse: &ServerSentEvent) -> Result<Option<Value>, AnthropicStreamError> {
    if sse.event.as_deref() == Some("error") {
        let mut error_text = sse.data.clone();
        if let Some(hint_ms) = extract_429_retry_after_ms(&RetryHintInput { status: None, headers: None, body_text: &sse.data }, None)
        {
            error_text = append_retry_after_ms_marker(&error_text, hint_ms);
        }
        return Err(AnthropicStreamError::Message(error_text));
    }
    if !is_anthropic_message_event(sse.event.as_deref()) {
        return Ok(None);
    }
    Ok(Some(parse_anthropic_event(sse)?))
}

struct StreamState {
    output: AssistantMessage,
    indices: Vec<Option<usize>>,
    partial_json: Vec<Option<String>>,
    input_transformations: Option<Vec<Value>>,
    usage_model: Model,
    is_oauth_token: bool,
    server_fallback_receipt: Option<ServerFallbackReceipt>,
    abort_controller: AbortController,
    abort_server_side_fallback: bool,
}

impl StreamState {
    fn find_block(&self, index: usize) -> Option<usize> {
        self.indices.iter().position(|block_index| *block_index == Some(index))
    }

    fn push_block(&mut self, block: ContentBlock, index: usize) -> usize {
        self.output.content.push(block);
        self.indices.push(Some(index));
        self.partial_json.push(None);
        self.output.content.len() - 1
    }
}

fn create_output(model: &Model, options: &StreamOptions) -> AssistantMessage {
    let provider_thinking_level = model
        .compat
        .as_ref()
        .and_then(|compat| compat.anthropic_messages().supports_mid_convo_effort)
        .filter(|supported| *supported)
        .map(|_| effort_option(options).unwrap_or_else(|| "high".to_owned()));
    AssistantMessage {
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        provider_thinking_level,
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

fn handle_message_start(state: &mut StreamState, model: &Model, event: &Value, sent_model: &str) {
    let message = event.get("message").cloned().unwrap_or(Value::Null);
    state.output.response_id = message.get("id").and_then(Value::as_str).map(str::to_owned);
    if let Some(transformations) = message.get("input_transformations").and_then(Value::as_array) {
        state.input_transformations = Some(transformations.clone());
    }
    if let Some(response_model) = message.get("model").and_then(Value::as_str) {
        state.output.model = response_model.to_owned();
        let fallback_cost = if state.output.model == model.id {
            None
        } else {
            model.compat.as_ref().and_then(|compat| {
                compat.anthropic_messages().allowed_fallback_models.and_then(|models| {
                    models.into_iter().find_map(|candidate| match candidate {
                        AllowedFallbackModel::Model(fallback)
                            if fallback.provider == model.provider && fallback.model == state.output.model =>
                        {
                            Some(fallback.cost)
                        }
                        _ => None,
                    })
                })
            })
        };
        state.usage_model = match fallback_cost {
            Some(cost) => Model { id: state.output.model.clone(), cost, ..model.clone() },
            None => model.clone(),
        };
    }

    let usage = message.get("usage").cloned().unwrap_or(Value::Null);
    state.output.usage.input = usage.get("input_tokens").and_then(Value::as_u64).unwrap_or(0);
    state.output.usage.output = usage.get("output_tokens").and_then(Value::as_u64).unwrap_or(0);
    state.output.usage.cache_read = usage.get("cache_read_input_tokens").and_then(Value::as_u64).unwrap_or(0);
    state.output.usage.cache_write = usage.get("cache_creation_input_tokens").and_then(Value::as_u64).unwrap_or(0);
    state.output.usage.cache_write_1h = Some(
        usage
            .get("cache_creation")
            .and_then(|creation| creation.get("ephemeral_1h_input_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
    );
    state.output.usage.total_tokens = state.output.usage.input
        + state.output.usage.output
        + state.output.usage.cache_read
        + state.output.usage.cache_write;
    calculate_cost(model, &mut state.output.usage);

    if state.abort_server_side_fallback
        && let Some(receipt) =
            parse_sticky_fallback_receipt(&usage, sent_model, message.get("model").and_then(Value::as_str))
    {
        state.server_fallback_receipt = Some(receipt);
        state.abort_controller.abort(None);
    }
}

fn handle_content_block_start(
    state: &mut StreamState,
    model: &Model,
    context: &Context,
    event: &Value,
    sink: &AssistantMessageEventStream,
) {
    let block = event.get("content_block").cloned().unwrap_or(Value::Null);
    let index = event.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
    if let Some(receipt) = parse_server_fallback_receipt(&block) {
        if state.abort_server_side_fallback {
            state.server_fallback_receipt = Some(receipt);
            state.abort_controller.abort(None);
            return;
        }
        state.usage_model = apply_server_fallback_continuation(&mut state.output, model, &receipt);
    }
    match block.get("type").and_then(Value::as_str) {
        Some("text") => {
            let content_index = state.push_block(
                ContentBlock::Text(TextContent {
                    text: block.get("text").and_then(Value::as_str).unwrap_or_default().to_owned(),
                    ..TextContent::default()
                }),
                index,
            );
            sink.push(AssistantMessageEvent::TextStart { content_index, partial: state.output.clone() });
        }
        Some("thinking") => {
            let content_index = state.push_block(
                ContentBlock::Thinking(crate::types::ThinkingContent {
                    thinking: block.get("thinking").and_then(Value::as_str).unwrap_or_default().to_owned(),
                    thinking_signature: Some(
                        block.get("signature").and_then(Value::as_str).unwrap_or_default().to_owned(),
                    ),
                    ..crate::types::ThinkingContent::default()
                }),
                index,
            );
            sink.push(AssistantMessageEvent::ThinkingStart { content_index, partial: state.output.clone() });
        }
        Some("redacted_thinking") => {
            let content_index = state.push_block(
                ContentBlock::Thinking(crate::types::ThinkingContent {
                    thinking: "[Reasoning redacted]".into(),
                    thinking_signature: block.get("data").and_then(Value::as_str).map(str::to_owned),
                    redacted: Some(true),
                    ..crate::types::ThinkingContent::default()
                }),
                index,
            );
            sink.push(AssistantMessageEvent::ThinkingStart { content_index, partial: state.output.clone() });
        }
        Some("tool_use") => {
            let raw_name = block.get("name").and_then(Value::as_str).unwrap_or_default();
            let name = if state.is_oauth_token {
                from_claude_code_name(raw_name, context.tools.as_deref())
            } else {
                raw_name.to_owned()
            };
            let arguments = block.get("input").and_then(Value::as_object).cloned().unwrap_or_default();
            let content_index = state.push_block(
                ContentBlock::ToolCall(ToolCall {
                    id: block.get("id").and_then(Value::as_str).unwrap_or_default().to_owned(),
                    name,
                    arguments,
                    incomplete: None,
                    error_message: None,
                    thought_signature: None,
                    namespace: None,
                }),
                index,
            );
            sink.push(AssistantMessageEvent::ToolcallStart { content_index, partial: state.output.clone() });
        }
        Some(other) => {
            state.push_block(
                ContentBlock::ProviderNative(crate::types::ProviderNativeContent { subtype: other.to_owned(), raw: block }),
                index,
            );
        }
        None => {}
    }
}

fn handle_content_block_delta(state: &mut StreamState, event: &Value, sink: &AssistantMessageEventStream) {
    let delta = event.get("delta").cloned().unwrap_or(Value::Null);
    let index = event.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
    match delta.get("type").and_then(Value::as_str) {
        Some("text_delta") => {
            let text = delta.get("text").and_then(Value::as_str).unwrap_or_default();
            if let Some(content_index) = state.find_block(index)
                && let Some(ContentBlock::Text(block)) = state.output.content.get_mut(content_index)
            {
                block.text.push_str(text);
                sink.push(AssistantMessageEvent::TextDelta {
                    content_index,
                    delta: text.to_owned(),
                    partial: state.output.clone(),
                });
            }
        }
        Some("thinking_delta") => {
            let thinking = delta.get("thinking").and_then(Value::as_str).unwrap_or_default();
            if let Some(content_index) = state.find_block(index)
                && let Some(ContentBlock::Thinking(block)) = state.output.content.get_mut(content_index)
            {
                block.thinking.push_str(thinking);
                sink.push(AssistantMessageEvent::ThinkingDelta {
                    content_index,
                    delta: thinking.to_owned(),
                    partial: state.output.clone(),
                });
            }
        }
        Some("input_json_delta") => {
            let partial = delta.get("partial_json").and_then(Value::as_str).unwrap_or_default().to_owned();
            let Some(content_index) = state.find_block(index) else { return };
            if matches!(state.output.content.get(content_index), Some(ContentBlock::ToolCall(_))) {
                let scratch = state.partial_json[content_index].get_or_insert_with(String::new);
                scratch.push_str(&partial);
                let parsed = parse_streaming_json(Some(scratch));
                if let Some(ContentBlock::ToolCall(call)) = state.output.content.get_mut(content_index) {
                    call.arguments = parsed.as_object().cloned().unwrap_or_default();
                }
                sink.push(AssistantMessageEvent::ToolcallDelta {
                    content_index,
                    delta: partial,
                    partial: state.output.clone(),
                });
            } else if let Some(ContentBlock::ProviderNative(native)) = state.output.content.get(content_index)
                && is_provider_native_tool_use_block(&native.raw)
            {
                let scratch = state.partial_json[content_index].get_or_insert_with(String::new);
                scratch.push_str(&partial);
            }
        }
        Some("signature_delta") => {
            let signature = delta.get("signature").and_then(Value::as_str).unwrap_or_default();
            if let Some(content_index) = state.find_block(index)
                && let Some(ContentBlock::Thinking(block)) = state.output.content.get_mut(content_index)
            {
                block.thinking_signature.get_or_insert_with(String::new).push_str(signature);
            }
        }
        _ => {}
    }
}

fn handle_content_block_stop(state: &mut StreamState, event: &Value, sink: &AssistantMessageEventStream) {
    let index = event.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
    let Some(content_index) = state.find_block(index) else { return };
    state.indices[content_index] = None;
    match state.output.content.get(content_index) {
        Some(ContentBlock::Text(block)) => {
            let content = block.text.clone();
            sink.push(AssistantMessageEvent::TextEnd { content_index, content, partial: state.output.clone() });
        }
        Some(ContentBlock::Thinking(block)) => {
            let content = block.thinking.clone();
            sink.push(AssistantMessageEvent::ThinkingEnd { content_index, content, partial: state.output.clone() });
        }
        Some(ContentBlock::ToolCall(_)) => {
            let partial_json = state.partial_json[content_index].clone().unwrap_or_default();
            state.partial_json[content_index] = None;
            let parsed = parse_streaming_json(Some(&partial_json));
            if let Some(ContentBlock::ToolCall(call)) = state.output.content.get_mut(content_index) {
                call.arguments = parsed.as_object().cloned().unwrap_or_default();
            }
            let Some(ContentBlock::ToolCall(call)) = state.output.content.get(content_index) else { return };
            sink.push(AssistantMessageEvent::ToolcallEnd {
                content_index,
                tool_call: call.clone(),
                partial: state.output.clone(),
            });
        }
        Some(ContentBlock::ProviderNative(_)) => {
            let partial_json = state.partial_json[content_index].take();
            if let Some(partial_json) = partial_json
                && let Some(ContentBlock::ProviderNative(native)) = state.output.content.get_mut(content_index)
            {
                native.raw = merge_provider_native_input(&native.raw, &partial_json);
            }
        }
        _ => {}
    }
}

fn handle_message_delta(state: &mut StreamState, event: &Value) -> Result<(), AnthropicStreamError> {
    if let Some(transformations) = event.get("input_transformations").and_then(Value::as_array) {
        state.input_transformations = Some(transformations.clone());
    }
    let delta = event.get("delta").cloned().unwrap_or(Value::Null);
    if let Some(stop_reason) = delta.get("stop_reason").and_then(Value::as_str) {
        state.output.raw_stop_reason = Some(stop_reason.to_owned());
        let (mapped, error_message, stop_details) =
            map_stop_reason(stop_reason, delta.get("stop_details")).map_err(AnthropicStreamError::Message)?;
        state.output.stop_reason = mapped;
        if let Some(error_message) = error_message {
            state.output.error_message = Some(error_message);
        }
        if let Some(stop_details) = stop_details {
            state.output.stop_details = Some(stop_details);
        }
    }
    if let Some(usage) = event.get("usage").filter(|usage| !usage.is_null()) {
        if let Some(input) = usage.get("input_tokens").and_then(Value::as_u64) {
            state.output.usage.input = input;
        }
        if let Some(output) = usage.get("output_tokens").and_then(Value::as_u64) {
            state.output.usage.output = output;
        }
        if let Some(cache_read) = usage.get("cache_read_input_tokens").and_then(Value::as_u64) {
            state.output.usage.cache_read = cache_read;
        }
        if let Some(cache_write) = usage.get("cache_creation_input_tokens").and_then(Value::as_u64) {
            state.output.usage.cache_write = cache_write;
        }
        if let Some(thinking_tokens) = usage
            .get("output_tokens_details")
            .and_then(|details| details.get("thinking_tokens"))
            .and_then(Value::as_u64)
        {
            state.output.usage.reasoning = Some(thinking_tokens);
        }
    }
    state.output.usage.total_tokens = state.output.usage.input
        + state.output.usage.output
        + state.output.usage.cache_read
        + state.output.usage.cache_write;
    calculate_cost(&state.usage_model, &mut state.output.usage);
    Ok(())
}

fn handle_event(
    state: &mut StreamState,
    model: &Model,
    context: &Context,
    event: &Value,
    sent_model: &str,
    sink: &AssistantMessageEventStream,
) -> Result<(), AnthropicStreamError> {
    match event.get("type").and_then(Value::as_str) {
        Some("message_start") => handle_message_start(state, model, event, sent_model),
        Some("content_block_start") => handle_content_block_start(state, model, context, event, sink),
        Some("content_block_delta") => handle_content_block_delta(state, event, sink),
        Some("content_block_stop") => handle_content_block_stop(state, event, sink),
        Some("message_delta") => handle_message_delta(state, event)?,
        _ => {}
    }
    Ok(())
}

fn merge_provider_native_input(raw: &Value, partial_json: &str) -> Value {
    let mut raw = raw.clone();
    if let Some(object) = raw.as_object_mut() {
        object.insert("input".into(), parse_streaming_json(Some(partial_json)));
    }
    raw
}

fn strip_streaming_scratch(state: &mut StreamState) {
    state.indices.iter_mut().for_each(|index| *index = None);
    for content_index in 0..state.output.content.len() {
        let Some(partial_json) = state.partial_json[content_index].take() else { continue };
        if let Some(ContentBlock::ProviderNative(native)) = state.output.content.get_mut(content_index) {
            native.raw = merge_provider_native_input(&native.raw, &partial_json);
        }
    }
}

async fn attempt_request(
    model: &Model,
    context: &Context,
    client: &AnthropicClient,
    options: &StreamOptions,
    api_key: Option<&str>,
    unsigned_thinking_replay: &Mutex<&'static str>,
    fallback_key: Option<&str>,
) -> Result<(PreparedRequest, reqwest::Response), AnthropicStreamError> {
    let prepared = build_prepared_request(model, context, client.is_oauth_token, options, replay_mode(unsigned_thinking_replay)).await?;
    let forced_tool_choice = is_forced_anthropic_tool_choice(prepared.params.get("tool_choice"));
    let response = match send_request(model, client, options, api_key, &prepared).await {
        Ok(response) if response.status().is_success() => return Ok((prepared, response)),
        Ok(response) => classify_response_failure(response).await,
        Err(error) => error,
    };

    if is_forced_tool_choice_unsupported_error(
        &HttpFailure { status: response.status(), message: response.to_string() },
        forced_tool_choice,
    ) {
        let retried = PreparedRequest {
            params: omit_tool_choice_param(&prepared.params),
            headers: prepared.headers.clone(),
            sent_model: prepared.sent_model.clone(),
        };
        return match send_request(model, client, options, api_key, &retried).await {
            Ok(response) if response.status().is_success() => Ok((retried, response)),
            Ok(response) => Err(classify_response_failure(response).await),
            Err(error) => Err(error),
        };
    }

    if replay_mode(unsigned_thinking_replay) != "text" && is_invalid_unsigned_thinking_signature_error(&response) {
        *unsigned_thinking_replay.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = "text";
        if let Some(key) = fallback_key {
            UNSIGNED_THINKING_TEXT_REPLAY_FALLBACKS
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(key.to_owned());
        }
        let prepared = build_prepared_request(model, context, client.is_oauth_token, options, replay_mode(unsigned_thinking_replay)).await?;
        return match send_request(model, client, options, api_key, &prepared).await {
            Ok(response) if response.status().is_success() => Ok((prepared, response)),
            Ok(response) => Err(classify_response_failure(response).await),
            Err(error) => Err(error),
        };
    }

    Err(response)
}

async fn drive(
    model: &Model,
    context: &Context,
    options: &StreamOptions,
    state: &mut StreamState,
    sink: &AssistantMessageEventStream,
) -> Result<(), AnthropicStreamError> {
    let api_key = options.request.api_key.clone().filter(|key| !key.is_empty());
    let options_headers = provider_headers_to_record(options.request.headers.as_ref());
    assert_request_auth(&model.provider, api_key.as_deref(), options_headers.as_ref())
        .map_err(AnthropicStreamError::Message)?;

    let dynamic_headers = (model.provider == "github-copilot").then(|| {
        let has_images = has_copilot_vision_input(&context.messages);
        build_copilot_dynamic_headers(&context.messages, has_images)
    });

    let cache_retention = resolve_cache_retention(
        options.cache_retention.or(model.cache_retention),
        options.request.env.as_ref(),
    );
    let cache_session_id = if cache_retention == CacheRetention::None { None } else { options.session_id.clone() };

    let client = create_client(
        model,
        context,
        options,
        api_key.as_deref(),
        dynamic_headers.as_ref(),
        cache_session_id.as_deref(),
    );
    state.is_oauth_token = client.is_oauth_token;

    let fallback_key = unsigned_thinking_fallback_key(model, options.session_id.as_deref());
    let unsigned_thinking_replay: Mutex<&'static str> = Mutex::new(if fallback_key.as_ref().is_some_and(|key| {
        UNSIGNED_THINKING_TEXT_REPLAY_FALLBACKS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(key)
    }) {
        "text"
    } else {
        unsigned_thinking_replay_value(Some(&get_anthropic_compat(model).unsigned_thinking_replay))
    });

    let _ = LazyLock::force(&UNSIGNED_THINKING_CLEANUP);

    let mut sent_model = String::new();
    let client_ref = &client;
    let api_key_ref = api_key.as_deref();
    let fallback_key_ref = fallback_key.as_deref();
    let replay_cell = &unsigned_thinking_replay;
    let request_outcome = retry_provider_request(
        move || async move {
            attempt_request(model, context, client_ref, options, api_key_ref, replay_cell, fallback_key_ref).await
        },
        &ProviderRetryOptions {
            max_retries: options.request.max_retries,
            max_retry_delay_ms: options.request.max_retry_delay_ms,
            signal: options.request.signal.clone(),
        },
    )
    .await;

    let (prepared, response) = match request_outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            let error = map_retry_error(error);
            if let Some(status) = error.status() {
                options.request.apply_response_hook(
                    &crate::types::ProviderResponse { status, headers: Default::default() }, model,
                ).await.map_err(AnthropicStreamError::Message)?;
            }
            return Err(error);
        }
    };
    sent_model.clone_from(&prepared.sent_model);

    options.request.apply_response_hook(
            &crate::types::ProviderResponse { status: response.status().as_u16(), headers: headers_to_record(response.headers()) },
            model,
        ).await.map_err(AnthropicStreamError::Message)?;
    sink.push(AssistantMessageEvent::Start { partial: state.output.clone() });

    let mut decoder = SseDecoder::default();
    let mut saw_message_start = false;
    let mut saw_message_end = false;
    let mut body = response.bytes_stream();
    while let Some(chunk) = body.next().await {
        if options.request.signal.as_ref().is_some_and(AbortSignal::aborted) {
            return Err(AnthropicStreamError::Message("Request was aborted".into()));
        }
        let chunk = chunk.map_err(|error| AnthropicStreamError::Network { message: error.to_string(), timeout: false })?;
        for sse in decoder.push_bytes(&chunk) {
            if let Some(event) = decode_anthropic_event(&sse)? {
                if is_anthropic_message_start(&event) {
                    saw_message_start = true;
                } else if is_anthropic_message_stop(&event) {
                    saw_message_end = true;
                }
                handle_event(state, model, context, &event, &sent_model, sink)?;
                if state.server_fallback_receipt.is_some() && state.abort_controller.signal().aborted() {
                    return finish_stream(state, options, sink, None);
                }
            }
        }
    }
    for sse in decoder.finish() {
        if let Some(event) = decode_anthropic_event(&sse)? {
            if is_anthropic_message_start(&event) {
                saw_message_start = true;
            } else if is_anthropic_message_stop(&event) {
                saw_message_end = true;
            }
            handle_event(state, model, context, &event, &sent_model, sink)?;
        }
    }

    if options.request.signal.as_ref().is_some_and(AbortSignal::aborted) {
        return Err(AnthropicStreamError::Message("Request was aborted".into()));
    }
    if saw_message_start && !saw_message_end {
        return Err(AnthropicStreamError::Message("Anthropic stream ended before message_stop".into()));
    }
    finish_stream(state, options, sink, None)
}

fn map_retry_error(error: ProviderRetryError<AnthropicStreamError>) -> AnthropicStreamError {
    match error {
        ProviderRetryError::Request(error) => error,
        ProviderRetryError::RetryDelay { message, .. } => AnthropicStreamError::Message(message),
        ProviderRetryError::Aborted => AnthropicStreamError::Aborted,
    }
}

fn finish_stream(
    state: &mut StreamState,
    options: &StreamOptions,
    sink: &AssistantMessageEventStream,
    _unused: Option<()>,
) -> Result<(), AnthropicStreamError> {
    if let Some(receipt) = state.server_fallback_receipt.clone() {
        apply_server_fallback_abort(&mut state.output, &receipt);
    }
    if state.output.stop_reason == StopReason::Pending {
        return Err(AnthropicStreamError::Message("Anthropic stream ended without a stop reason".into()));
    }
    if matches!(state.output.stop_reason, StopReason::Aborted | StopReason::Error) {
        return Err(AnthropicStreamError::Message(
            state.output.error_message.clone().unwrap_or_else(|| "An unknown error occurred".to_owned()),
        ));
    }
    if let Some(transformations) = state.input_transformations.clone()
        && !transformations.is_empty()
    {
        append_assistant_message_diagnostic(
            &mut state.output.diagnostics,
            AssistantMessageDiagnostic {
                kind: "anthropic_input_transformations".into(),
                timestamp: now_ms(),
                error: None,
                details: Some(
                    json!({
                        "transformations": transformations
                            .iter()
                            .map(|transformation| json!({
                                "type": transformation.get("type").and_then(Value::as_str),
                                "path": transformation.get("path").and_then(Value::as_str),
                                "reason": transformation.get("reason").and_then(Value::as_str),
                            }))
                            .collect::<Vec<_>>()
                    })
                    .as_object()
                    .cloned()
                    .unwrap_or_default(),
                ),
            },
        );
    }
    let _ = options;
    sink.push(AssistantMessageEvent::Done { reason: done_reason(state.output.stop_reason), message: state.output.clone() });
    sink.end(Some(state.output.clone()));
    Ok(())
}

fn done_reason(stop_reason: StopReason) -> DoneReason {
    match stop_reason {
        StopReason::Length => DoneReason::Length,
        StopReason::ToolUse => DoneReason::ToolUse,
        StopReason::Deferred => DoneReason::Deferred,
        _ => DoneReason::Stop,
    }
}

async fn run_stream(model: Model, context: Context, options: StreamOptions, sink: AssistantMessageEventStream) {
    let output = create_output(&model, &options);
    let abort_controller = AbortController::new();
    let combined = combine_abort_signals(&[options.request.signal.clone(), Some(abort_controller.signal())]);
    let mut state = StreamState {
        output,
        indices: Vec::new(),
        partial_json: Vec::new(),
        input_transformations: None,
        usage_model: model.clone(),
        is_oauth_token: false,
        server_fallback_receipt: None,
        abort_controller,
        abort_server_side_fallback: options.request.abort_server_side_fallback == Some(true),
    };

    let outcome = drive(&model, &context, &options, &mut state, &sink).await;
    combined.cleanup();
    if let Err(error) = outcome {
        strip_streaming_scratch(&mut state);
        let aborted = options.request.signal.as_ref().is_some_and(AbortSignal::aborted);
        state.output.stop_reason = if aborted { StopReason::Aborted } else { StopReason::Error };
        let mut error_message = error.to_string();
        if error.status() == Some(429)
            && let Some(headers) = error.headers()
            && let Some(hint_ms) = extract_429_retry_after_ms(
                &RetryHintInput { status: Some(429), headers: Some(headers), body_text: &error_message },
                None,
            )
        {
            error_message = append_retry_after_ms_marker(&error_message, hint_ms);
        }
        let failure = normalize_anthropic_retry_failure(&error.retry_error_shape(), None);
        append_assistant_message_diagnostic(
            &mut state.output.diagnostics,
            AssistantMessageDiagnostic {
                kind: "provider_retry_failure".into(),
                timestamp: now_ms(),
                error: None,
                details: retry_failure_details(&failure),
            },
        );
        state.output.error_message = Some(error_message);
        sink.push(AssistantMessageEvent::Error {
            reason: if aborted { ErrorReason::Aborted } else { ErrorReason::Error },
            error: state.output.clone(),
        });
        sink.end(Some(state.output.clone()));
    }
}

pub fn stream(model: &Model, context: &Context, options: Option<StreamOptions>) -> AssistantMessageEventStream {
    let stream = create_assistant_message_event_stream();
    let sink = stream.clone();
    let model = model.clone();
    let context = context.clone();
    let options = options.unwrap_or_default();
    tokio::spawn(async move { run_stream(model, context, options, sink).await });
    stream
}

pub fn stream_simple(
    model: &Model,
    context: &Context,
    options: Option<SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    let simple = options.unwrap_or_default();
    let api_key = simple.stream.request.api_key.clone();
    let options_headers = provider_headers_to_record(simple.stream.request.headers.as_ref());
    if let Err(message) = assert_request_auth(&model.provider, api_key.as_deref(), options_headers.as_ref()) {
        return crate::utils::lazy::error_stream(model, &message);
    }

    let mut stream_options = match build_base_options(model, context, Some(&simple), api_key.as_deref()) {
        Ok(stream_options) => stream_options,
        Err(error) => return crate::utils::lazy::error_stream(model, &error.to_string()),
    };
    if let Some(tool_choice) = &simple.tool_choice {
        stream_options.extra.insert("toolChoice".into(), serde_json::to_value(tool_choice).unwrap_or(Value::Null));
    }

    let Some(reasoning) = simple.reasoning else {
        stream_options.extra.insert("thinkingEnabled".into(), Value::from(false));
        return stream(model, context, Some(stream_options));
    };

    if supports_adaptive_thinking(model) {
        let effort = map_thinking_level_to_effort(model, Some(reasoning));
        stream_options.extra.insert("thinkingEnabled".into(), Value::from(true));
        stream_options.extra.insert("effort".into(), Value::from(effort));
        return stream(model, context, Some(stream_options));
    }

    let adjusted = adjust_max_tokens_for_thinking(
        stream_options.max_tokens,
        model.max_tokens,
        reasoning,
        simple.thinking_budgets.as_ref(),
    );
    let max_tokens = clamp_max_tokens_to_context(model, context, adjusted.max_tokens).unwrap_or(adjusted.max_tokens);
    stream_options.extra.insert("thinkingEnabled".into(), Value::from(true));
    stream_options
        .extra
        .insert("thinkingBudgetTokens".into(), Value::from(adjusted.thinking_budget.min(max_tokens.saturating_sub(1024))));
    stream_options.max_tokens = Some(max_tokens);
    stream(model, context, Some(stream_options))
}

pub fn build_anthropic_warm_prompt_cache_params(
    model: &Model,
    context: &Context,
    options: Option<&StreamOptions>,
) -> Map<String, Value> {
    let mut overridden = options.cloned().unwrap_or_default();
    overridden.max_tokens = Some(0);
    overridden.extra.remove("thinkingEnabled");
    overridden.extra.remove("toolChoice");
    let params = build_params(model, context, false, &overridden, unsigned_thinking_replay_value(Some(&get_anthropic_compat(model).unsigned_thinking_replay)))
        .unwrap_or_default();
    let mut non_streaming = params;
    non_streaming.remove("stream");
    non_streaming.remove("thinking");
    non_streaming.remove("output_config");
    non_streaming.remove("tool_choice");
    non_streaming.insert("max_tokens".into(), Value::from(0));
    sanitize_anthropic_tool_pairs(&non_streaming)
}

// __PART5__
