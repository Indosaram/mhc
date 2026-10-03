//! Port of senpi packages/ai/src/api/openai-codex-responses.ts.
//!
//! The Codex Responses wire API: an SSE (or cached-WebSocket) transport in front of the shared
//! Responses processor. This module builds the Codex request body, maps the Codex event vocabulary
//! onto the shared Responses events, and owns the retry/cache-affinity/compression behavior.
//!
//! Documented deviation: maho-ai declares no WebSocket client, so the transport reports senpi's own
//! "WebSocket transport is not available in this runtime" and `stream` falls back to SSE exactly as
//! senpi does when `globalThis.WebSocket` is missing. The session-cache half of the WebSocket path
//! (fallback-state.ts, the cache-affinity headers, `scheduleSessionWebSocketExpiry`) is ported and
//! tested; the dial/handshake/liveness half stays with the unported transport.

use std::collections::BTreeMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context as TaskContext, Poll};
use std::time::Duration;

use bytes::Bytes;
use futures::Stream;
use serde_json::{json, Map, Value};

use crate::api::constrained_sampling::create_grammar_tool_input_properties;
use crate::api::openai_prompt_cache::{apply_chat_gpt_subscription_cache_affinity_headers, clamp_openai_prompt_cache_key};
use crate::api::openai_responses::{
    get_service_tier_cost_multiplier, CachedWebSocket, CachedWebSocketConnection, SESSION_WEBSOCKET_CACHE_TTL_MS,
};
use crate::api::openai_responses_shared::{
    convert_responses_messages, convert_responses_tools, get_done_reason, process_responses_stream,
    ConvertResponsesMessagesOptions, ConvertResponsesToolsOptions, ResponsesStreamError, ResponsesStreamOptions,
};
use crate::api::simple_options::{apply_extra_body, build_base_options, clamp_max_for_openai, OPENAI_RESPONSES_RESERVED_BODY_KEYS};
use crate::models::{clamp_thinking_level, supports_max, supports_xhigh};
use crate::types::{
    AssistantMessage, AssistantMessageEvent, CacheRetention, Context, ErrorReason, Model, ModelThinkingLevel,
    ProviderHeaders, ProviderResponse, SimpleStreamOptions, StopReason, StreamOptions, ThinkingLevel, Transport, Usage,
};
use crate::utils::deferred_tools::split_deferred_tools;
use crate::utils::diagnostics::{append_assistant_message_diagnostic, create_assistant_message_diagnostic, now_ms, Thrown};
use crate::utils::error_body::{format_provider_error, normalize_provider_error, SdkErrorShape, ThrownProviderError};
use crate::utils::event_stream::{create_assistant_message_event_stream, AssistantMessageEventStream};
use crate::utils::headers::headers_to_record;
use crate::utils::lazy::error_stream;
use crate::utils::retry_hint::{append_retry_after_ms_marker, extract_429_retry_after_ms, RetryHintInput};
use crate::utils::uuid::uuidv7;
use crate::wire_identity::get_wire_identity;

pub mod fallback_state;
pub mod reasoning;

use fallback_state::{
    is_web_socket_sse_fallback_active, record_web_socket_failure, record_web_socket_sse_fallback,
    ChatGptSubscriptionWebSocketDebugStats,
};

const DEFAULT_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api";
const DEFAULT_MAX_RETRIES: u32 = 0;
const BASE_DELAY_MS: u64 = 1000;
const DEFAULT_MAX_RETRY_DELAY_MS: u64 = 60_000;
const DEFAULT_WEBSOCKET_CONNECT_TIMEOUT_MS: u64 = 15_000;
/// The Codex backend accepts zstd-compressed request bodies on the SSE responses endpoint (the same
/// endpoint the official Codex client compresses against).
const REQUEST_COMPRESSION_ZSTD_LEVEL: i32 = 3;
const CODEX_TOOL_CALL_PROVIDERS: [&str; 3] = ["openai", "chatgpt-subscription", "opencode"];
const CODEX_PREVIOUS_RESPONSE_STALE_CODES: [&str; 1] = ["codex_previous_response_stale"];
const WEBSOCKET_CONNECTION_LIMIT_REACHED_CODE: &str = "websocket_connection_limit_reached";
const PREVIOUS_RESPONSE_NOT_FOUND_CODE: &str = "previous_response_not_found";

/// `compat.supportsGrammarTools`: the vendor-prefixed grammar-tools capability key.
const COMPAT_GRAMMAR_TOOLS_KEY: &str = "supportsOpenAIGrammarTools";

const CODEX_RESPONSE_STATUSES: [&str; 6] = ["completed", "incomplete", "failed", "cancelled", "queued", "in_progress"];

/// `[OI]CodexResponsesOptions`, read off the provider-neutral options bag.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodexResponsesOptions {
    pub reasoning_effort: Option<String>,
    /// `reasoningSummary`, which distinguishes an explicit `null` from absent.
    pub reasoning_summary: Option<Value>,
    pub service_tier: Option<String>,
    pub text_verbosity: Option<String>,
    pub tool_choice: Option<String>,
}

impl CodexResponsesOptions {
    pub fn from_stream_options(options: &StreamOptions) -> Self {
        Self {
            reasoning_effort: option_str(options, "reasoningEffort"),
            reasoning_summary: options.extra.get("reasoningSummary").cloned(),
            service_tier: option_str(options, "serviceTier"),
            text_verbosity: option_str(options, "textVerbosity"),
            tool_choice: option_str(options, "toolChoice"),
        }
    }
}

fn option_str(options: &StreamOptions, key: &str) -> Option<String> {
    options.extra.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// A thrown Codex error carrying the backend's own code, mirroring `CodexApiError`.
#[derive(Debug, Clone, PartialEq)]
pub struct CodexApiError {
    pub message: String,
    pub code: Option<String>,
    pub payload: Option<Value>,
}

/// `CodexProtocolError`.
#[derive(Debug, Clone, PartialEq)]
pub struct CodexProtocolError {
    pub message: String,
    pub payload: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CodexError {
    Api(CodexApiError),
    Protocol(CodexProtocolError),
    /// A non-Codex thrown value (network failure, timeout, abort, ...).
    Other(String),
}

impl CodexError {
    fn message(&self) -> String {
        match self {
            CodexError::Api(error) => error.message.clone(),
            CodexError::Protocol(error) => error.message.clone(),
            CodexError::Other(message) => message.clone(),
        }
    }
}

impl From<CodexApiError> for CodexError {
    fn from(error: CodexApiError) -> Self {
        CodexError::Api(error)
    }
}

/// `buildRequestBody`.
pub fn build_request_body(
    model: &Model,
    context: &Context,
    options: &CodexResponsesOptions,
    cache_session_id: Option<&str>,
    grammar_tool_input_properties: &BTreeMap<String, String>,
    extra_body: Option<&Map<String, Value>>,
) -> Result<Map<String, Value>, String> {
    let requested_reasoning_effort = options.reasoning_effort.as_deref();
    let mapped_reasoning_effort: Option<Option<String>> = requested_reasoning_effort
        .and_then(ModelThinkingLevel::parse)
        .and_then(|level| model.thinking_level_map.as_ref().and_then(|map| map.get(&level).cloned()));
    let reasoning_effort: Option<String> = match &mapped_reasoning_effort {
        Some(value) => value.clone(),
        None => requested_reasoning_effort.map(str::to_owned),
    };
    // `undefined` (absent) and `null` (explicitly disabled) are different inputs to
    // `buildCodexReasoning`, so the presence of the option is kept alongside its value.
    let reasoning_effort_input: Option<Option<String>> =
        requested_reasoning_effort.map(|_| reasoning_effort.clone());
    let reasoning_requested = requested_reasoning_effort.is_some()
        && requested_reasoning_effort != Some("none")
        && reasoning_effort.is_some();
    let compat = model.compat.as_ref();
    let supports_strict_mode = compat
        .and_then(|compat| compat.get("supportsStrictMode"))
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let supports_grammar_tools = compat
        .and_then(|compat| compat.get(COMPAT_GRAMMAR_TOOLS_KEY))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let supports_additional_tools = compat
        .and_then(|compat| compat.get("supportsAdditionalTools"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let supports_tool_search = compat
        .and_then(|compat| compat.get("supportsToolSearch"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let deferred_tools_mode = if supports_additional_tools {
        Some(crate::api::openai_responses_shared::ResponsesDeferredToolsMode::AdditionalTools)
    } else if supports_tool_search {
        Some(crate::api::openai_responses_shared::ResponsesDeferredToolsMode::ToolSearch)
    } else {
        None
    };
    let tool_placement = split_deferred_tools(context, deferred_tools_mode.is_some(), None);
    let messages = convert_responses_messages(
        model,
        context,
        &CODEX_TOOL_CALL_PROVIDERS.iter().map(|provider| (*provider).to_owned()).collect(),
        &ConvertResponsesMessagesOptions {
            include_system_prompt: Some(false),
            preserve_thinking: Some(reasoning_requested),
            preserve_text_signatures: Some(true),
            grammar_tool_input_properties: grammar_tool_input_properties.clone(),
            deferred_tools: tool_placement.deferred.clone(),
            deferred_tools_mode,
            tool_options: ConvertResponsesToolsOptions {
                supports_strict_mode: Some(supports_strict_mode),
                supports_openai_grammar_tools: Some(supports_grammar_tools),
                ..ConvertResponsesToolsOptions::default()
            },
            ..ConvertResponsesMessagesOptions::default()
        },
    );

    let mut body = Map::new();
    body.insert(String::from("model"), json!(model.id));
    body.insert(String::from("store"), json!(false));
    body.insert(String::from("stream"), json!(true));
    body.insert(
        String::from("instructions"),
        json!(context.system_prompt.clone().filter(|prompt| !prompt.is_empty()).unwrap_or_else(|| String::from("You are a helpful assistant."))),
    );
    body.insert(String::from("input"), Value::Array(messages));
    body.insert(
        String::from("text"),
        json!({ "verbosity": options.text_verbosity.clone().unwrap_or_else(|| String::from("low")) }),
    );
    body.insert(String::from("include"), json!(["reasoning.encrypted_content"]));
    body.insert(String::from("prompt_cache_key"), json!(cache_session_id));
    body.insert(String::from("tool_choice"), json!(options.tool_choice.clone().unwrap_or_else(|| String::from("auto"))));
    body.insert(String::from("parallel_tool_calls"), json!(true));

    if let Some(service_tier) = options.service_tier.as_ref() {
        body.insert(String::from("service_tier"), json!(service_tier));
    }
    if !tool_placement.immediate.is_empty() {
        let tools = convert_responses_tools(
            &tool_placement.immediate,
            &ConvertResponsesToolsOptions {
                supports_strict_mode: Some(supports_strict_mode),
                supports_openai_grammar_tools: Some(supports_grammar_tools),
                ..ConvertResponsesToolsOptions::default()
            },
        )?;
        body.insert(String::from("tools"), Value::Array(tools));
    }
    let thinking_off = model.thinking_level_map.as_ref().and_then(|map| map.get(&ModelThinkingLevel::Off));
    if let Some(reasoning) = reasoning::build_codex_reasoning(
        reasoning_effort_input.as_ref(),
        options.reasoning_summary.as_ref(),
        model.reasoning,
        thinking_off,
    ) {
        body.insert(String::from("reasoning"), Value::Object(reasoning));
    }

    apply_extra_body(&mut body, extra_body, &OPENAI_RESPONSES_RESERVED_BODY_KEYS);
    Ok(body)
}

/// `resolveCodexUrl`.
pub fn resolve_codex_url(base_url: Option<&str>) -> String {
    let raw = base_url.filter(|base_url| !base_url.trim().is_empty()).unwrap_or(DEFAULT_CODEX_BASE_URL);
    let normalized = raw.trim_end_matches('/');
    if normalized.ends_with("/codex/responses") {
        return normalized.to_owned();
    }
    if normalized.ends_with("/codex") {
        return format!("{normalized}/responses");
    }
    format!("{normalized}/codex/responses")
}

/// `resolveCodexWebSocketUrl`.
pub fn resolve_codex_web_socket_url(base_url: Option<&str>) -> String {
    let url = resolve_codex_url(base_url);
    match url::Url::parse(&url) {
        Ok(mut url) => {
            let scheme = match url.scheme() {
                "https" => "wss",
                "http" => "ws",
                _ => return url.to_string(),
            }
            .to_owned();
            let _ = url.set_scheme(&scheme);
            url.to_string()
        }
        Err(_) => url,
    }
}

/// `resolveCodexServiceTier`.
pub fn resolve_codex_service_tier(response_service_tier: Option<&str>, request_service_tier: Option<&str>) -> Option<String> {
    if response_service_tier == Some("default")
        && matches!(request_service_tier, Some("flex") | Some("priority") | Some("fast"))
    {
        return request_service_tier.map(str::to_owned);
    }
    response_service_tier.or(request_service_tier).map(str::to_owned)
}

/// `getServiceTierCostMultiplier` for the Codex API (the shared helper reads the model id).
pub fn get_codex_service_tier_cost_multiplier(model: &Model, service_tier: Option<&str>) -> f64 {
    get_service_tier_cost_multiplier(model, service_tier)
}

/// `applyServiceTierPricing` for the Codex API.
pub fn apply_codex_service_tier_pricing(usage: &mut Usage, service_tier: Option<&str>, model: &Model) {
    let multiplier = get_codex_service_tier_cost_multiplier(model, service_tier);
    if multiplier == 1.0 {
        return;
    }
    usage.cost.input *= multiplier;
    usage.cost.output *= multiplier;
    usage.cost.cache_read *= multiplier;
    usage.cost.cache_write *= multiplier;
    usage.cost.total = usage.cost.input + usage.cost.output + usage.cost.cache_read + usage.cost.cache_write;
}

/// `normalizeCodexStatus`: an unrecognized status is dropped, not passed through.
fn normalize_codex_status(status: Option<&Value>) -> Option<String> {
    let status = status?.as_str()?;
    CODEX_RESPONSE_STATUSES.contains(&status).then(|| status.to_owned())
}

fn as_record(value: &Value) -> Option<&Map<String, Value>> {
    value.as_object()
}

fn get_string(value: Option<&Value>) -> Option<String> {
    value?.as_str().map(str::to_owned)
}

fn get_codex_event_error(event: &Map<String, Value>) -> Option<&Map<String, Value>> {
    let response = event.get("response").and_then(as_record);
    event.get("error").and_then(as_record).or_else(|| response.and_then(|response| response.get("error")).and_then(as_record))
}

fn get_codex_event_error_code(event: &Map<String, Value>) -> String {
    let error = get_codex_event_error(event);
    get_string(error.and_then(|error| error.get("code")))
        .or_else(|| get_string(error.and_then(|error| error.get("type"))))
        .or_else(|| get_string(event.get("code")))
        .unwrap_or_default()
}

fn get_codex_event_error_message(event: &Map<String, Value>) -> String {
    let response = event.get("response").and_then(as_record);
    let error = get_codex_event_error(event);
    get_string(error.and_then(|error| error.get("message")))
        .or_else(|| get_string(event.get("message")))
        .or_else(|| response.and_then(|response| get_string(response.get("message"))))
        .unwrap_or_default()
}

fn error_text_with_marker(base: String, event: &Map<String, Value>) -> String {
    let payload = serde_json::to_string(&Value::Object(event.clone())).unwrap_or_default();
    match extract_429_retry_after_ms(&RetryHintInput { status: None, headers: None, body_text: &payload }, None) {
        Some(hint_ms) => append_retry_after_ms_marker(&base, hint_ms),
        None => base,
    }
}

/// `isCodexPreviousResponseStale`.
pub fn is_codex_previous_response_stale(error: &CodexError) -> bool {
    match error {
        CodexError::Api(error) => error
            .code
            .as_deref()
            .is_some_and(|code| CODEX_PREVIOUS_RESPONSE_STALE_CODES.contains(&code)),
        _ => false,
    }
}

/// `isPreviousResponseNotFoundError`.
pub fn is_previous_response_not_found_error(error: &CodexError) -> bool {
    matches!(error, CodexError::Api(error) if error.code.as_deref() == Some(PREVIOUS_RESPONSE_NOT_FOUND_CODE))
}

/// `isWebSocketConnectionLimitReachedError`.
pub fn is_web_socket_connection_limit_reached_error(error: &CodexError) -> bool {
    matches!(error, CodexError::Api(error) if error.code.as_deref() == Some(WEBSOCKET_CONNECTION_LIMIT_REACHED_CODE))
}

/// `isCodexNonTransportError`.
pub fn is_codex_non_transport_error(error: &CodexError) -> bool {
    matches!(error, CodexError::Api(_) | CodexError::Protocol(_))
}

/// `getCodexEventErrorCode`/`getCodexEventErrorMessage` for a raw event, used by the transport.
pub fn codex_event_error_code(event: &Value) -> String {
    as_record(event).map(get_codex_event_error_code).unwrap_or_default()
}

/// `getChatGptSubscriptionWebSocketDebugStats`.
pub fn get_chat_gpt_subscription_web_socket_debug_stats(session_id: &str) -> Option<ChatGptSubscriptionWebSocketDebugStats> {
    fallback_state::get_web_socket_debug_stats(session_id)
}

/// `resetChatGptSubscriptionWebSocketDebugStats`.
pub fn reset_chat_gpt_subscription_web_socket_debug_stats(session_id: Option<&str>) {
    fallback_state::clear_web_socket_fallback_state(session_id);
}

/// `recordWebSocketRequestStats`.
pub fn record_web_socket_request_stats(
    stats: Option<&mut ChatGptSubscriptionWebSocketDebugStats>,
    request_body: &Map<String, Value>,
    reused: bool,
    use_cached_context: bool,
) {
    let Some(stats) = stats else { return };
    stats.requests += 1;
    if reused {
        stats.connections_reused += 1;
    } else {
        stats.connections_created += 1;
    }
    if use_cached_context {
        stats.cached_context_requests += 1;
    }
    if request_body.get("store").and_then(Value::as_bool) == Some(true) {
        stats.store_true_requests += 1;
    }
    stats.last_input_items = request_body.get("input").and_then(Value::as_array).map_or(0, |input| input.len() as u64);
    match request_body.get("previous_response_id").and_then(Value::as_str) {
        Some(previous_response_id) => {
            stats.delta_requests += 1;
            stats.last_delta_input_items = Some(stats.last_input_items);
            stats.last_previous_response_id = Some(previous_response_id.to_owned());
        }
        None => {
            stats.full_context_requests += 1;
            stats.last_delta_input_items = None;
            stats.last_previous_response_id = None;
        }
    }
}

/// `requestBodyWithoutInput`: the body minus the input and continuation anchor.
fn request_body_without_input(body: &Map<String, Value>) -> Map<String, Value> {
    let mut rest = body.clone();
    rest.remove("input");
    rest.remove("previous_response_id");
    rest
}

fn response_inputs_equal(a: Option<&Value>, b: Option<&Value>) -> bool {
    let serialize = |value: Option<&Value>| {
        serde_json::to_string(&value.cloned().unwrap_or(Value::Array(Vec::new()))).unwrap_or_default()
    };
    serialize(a) == serialize(b)
}

fn request_bodies_match_except_input(a: &Map<String, Value>, b: &Map<String, Value>) -> bool {
    request_body_without_input(a) == request_body_without_input(b)
}

/// `getCachedWebSocketInputDelta`.
pub fn get_cached_web_socket_input_delta(
    body: &Map<String, Value>,
    last_request_body: &Map<String, Value>,
    last_response_items: &[Value],
) -> Option<Vec<Value>> {
    if !request_bodies_match_except_input(body, last_request_body) {
        return None;
    }
    let current_input = body.get("input").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut baseline = last_request_body.get("input").and_then(Value::as_array).cloned().unwrap_or_default();
    baseline.extend(last_response_items.iter().cloned());
    if current_input.len() < baseline.len() {
        return None;
    }
    let prefix = &current_input[..baseline.len()];
    if !response_inputs_equal(Some(&Value::Array(prefix.to_vec())), Some(&Value::Array(baseline.clone()))) {
        return None;
    }
    Some(current_input[baseline.len()..].to_vec())
}

/// `buildCachedWebSocketRequestBody`.
pub fn build_cached_web_socket_request_body(
    body: &Map<String, Value>,
    last_request_body: Option<&Map<String, Value>>,
    last_response_id: Option<&str>,
    last_response_items: Option<&[Value]>,
) -> Option<Map<String, Value>> {
    let (last_request_body, last_response_items) = (last_request_body?, last_response_items?);
    let delta = get_cached_web_socket_input_delta(body, last_request_body, last_response_items)?;
    let last_response_id = last_response_id?;
    let mut next = body.clone();
    next.insert(String::from("previous_response_id"), json!(last_response_id));
    next.insert(String::from("input"), Value::Array(delta));
    Some(next)
}

/// `parseErrorResponse`.
pub async fn parse_error_response(status: u16, status_text: &str, raw: &str) -> (String, Option<String>) {
    let mut message = if raw.is_empty() { status_text.to_owned() } else { raw.to_owned() };
    if message.is_empty() {
        message = String::from("Request failed");
    }
    let mut friendly_message: Option<String> = None;
    if let Ok(parsed) = serde_json::from_str::<Value>(raw)
        && let Some(error) = parsed.get("error").and_then(Value::as_object)
    {
        let code = error
            .get("code")
            .and_then(Value::as_str)
            .or_else(|| error.get("type").and_then(Value::as_str))
            .unwrap_or_default();
        let code_is_usage_limit = ["usage_limit_reached", "usage_not_included", "rate_limit_exceeded"]
            .iter()
            .any(|needle| code.to_lowercase().contains(needle));
        if code_is_usage_limit || status == 429 {
            let plan = error
                .get("plan_type")
                .and_then(Value::as_str)
                .map(|plan| format!(" ({} plan)", plan.to_lowercase()))
                .unwrap_or_default();
            let minutes = error.get("resets_at").and_then(Value::as_f64).map(|resets_at| {
                ((resets_at * 1000.0 - now_ms() as f64) / 60_000.0).round().max(0.0) as i64
            });
            let when = minutes.map(|minutes| format!(" Try again in ~{minutes} min.")).unwrap_or_default();
            friendly_message = Some(format!("You have hit your ChatGPT usage limit{plan}.{when}").trim().to_owned());
        }
        let error_message = error.get("message").and_then(Value::as_str);
        message = error_message
            .map(str::to_owned)
            .or_else(|| friendly_message.clone())
            .unwrap_or(message);
    }
    (message, friendly_message)
}

/// `extractAccountId`.
pub fn extract_account_id(token: &str) -> Option<String> {
    crate::utils::chatgpt_subscription_auth::extract_chatgpt_subscription_account_id(token)
}

fn codex_user_agent() -> String {
    let identity = get_wire_identity();
    let platform = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        other => other,
    };
    format!("{identity} ({platform} {}; {arch})", os_release())
}

#[cfg(unix)]
fn os_release() -> String {
    std::process::Command::new("uname")
        .arg("-r")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_default()
}

#[cfg(not(unix))]
fn os_release() -> String {
    String::new()
}

/// `buildBaseCodexHeaders`.
pub fn build_base_codex_headers(
    init_headers: Option<&BTreeMap<String, String>>,
    additional_headers: Option<&ProviderHeaders>,
    account_id: Option<&str>,
    token: &str,
) -> reqwest::header::HeaderMap {
    let mut headers: BTreeMap<String, Option<String>> = BTreeMap::new();
    for (name, value) in init_headers.iter().flat_map(|headers| headers.iter()) {
        headers.insert(name.clone(), Some(value.clone()));
    }
    for (name, value) in additional_headers.iter().flat_map(|headers| headers.iter()) {
        match value {
            Some(value) => {
                headers.insert(name.clone(), Some(value.clone()));
            }
            None => {
                headers.remove(name);
            }
        }
    }
    headers.insert(String::from("Authorization"), Some(format!("Bearer {token}")));
    match account_id {
        Some(account_id) => {
            headers.insert(String::from("chatgpt-account-id"), Some(account_id.to_owned()));
        }
        None => {
            headers.remove("chatgpt-account-id");
        }
    }
    headers.insert(String::from("originator"), Some(get_wire_identity()));
    headers.insert(String::from("User-Agent"), Some(codex_user_agent()));
    to_header_map(headers)
}

fn to_header_map(headers: BTreeMap<String, Option<String>>) -> reqwest::header::HeaderMap {
    let mut header_map = reqwest::header::HeaderMap::new();
    for (name, value) in headers {
        let Ok(name) = reqwest::header::HeaderName::from_bytes(name.as_bytes()) else { continue };
        match value {
            Some(value) => {
                if let Ok(value) = reqwest::header::HeaderValue::from_str(&value) {
                    header_map.insert(name, value);
                }
            }
            None => {
                header_map.remove(&name);
            }
        }
    }
    header_map
}

/// `buildSSEHeaders`.
pub fn build_sse_headers(
    init_headers: Option<&BTreeMap<String, String>>,
    additional_headers: Option<&ProviderHeaders>,
    account_id: Option<&str>,
    token: &str,
    session_id: Option<&str>,
) -> reqwest::header::HeaderMap {
    let mut headers = build_base_codex_headers(init_headers, additional_headers, account_id, token);
    headers.insert(
        reqwest::header::HeaderName::from_static("openai-beta"),
        reqwest::header::HeaderValue::from_static("responses=experimental"),
    );
    headers.insert(reqwest::header::ACCEPT, reqwest::header::HeaderValue::from_static("text/event-stream"));
    headers.insert(reqwest::header::CONTENT_TYPE, reqwest::header::HeaderValue::from_static("application/json"));
    apply_chat_gpt_subscription_cache_affinity_headers(&mut headers, session_id);
    headers
}

/// `buildWebSocketHeaders`.
pub fn build_web_socket_headers(
    init_headers: Option<&BTreeMap<String, String>>,
    additional_headers: Option<&ProviderHeaders>,
    account_id: Option<&str>,
    token: &str,
    request_id: &str,
) -> reqwest::header::HeaderMap {
    let mut headers = build_base_codex_headers(init_headers, additional_headers, account_id, token);
    for name in ["accept", "content-type", "openai-beta"] {
        headers.remove(name);
    }
    headers.insert(
        reqwest::header::HeaderName::from_static("openai-beta"),
        reqwest::header::HeaderValue::from_static(crate::api::openai_responses::OPENAI_BETA_RESPONSES_WEBSOCKETS),
    );
    apply_chat_gpt_subscription_cache_affinity_headers(&mut headers, Some(request_id));
    headers
}

/// `connectWebSocket`: maho-ai declares no WebSocket client, so the transport reports senpi's own
/// "not available in this runtime" error.
pub fn connect_web_socket_error() -> CodexError {
    CodexError::Other(String::from("WebSocket transport is not available in this runtime"))
}

/// `SESSION_WEBSOCKET_MAX_AGE_MS`.
pub const SESSION_WEBSOCKET_MAX_AGE_MS: u64 = 55 * 60 * 1000;

/// `isWebSocketSessionExpired`.
pub fn is_web_socket_session_expired(created_at_ms: u64) -> bool {
    now_ms().saturating_sub(created_at_ms as i64) as u64 >= SESSION_WEBSOCKET_MAX_AGE_MS
}

/// `scheduleSessionWebSocketExpiry` re-exported for the codex session cache.
pub fn schedule_session_web_socket_expiry(session_id: &str, entry: Arc<CachedWebSocketConnection>) {
    crate::api::openai_responses::schedule_session_websocket_expiry(session_id, entry);
}

/// `closeChatGptSubscriptionWebSocketSessions`: maho-ai holds no codex WebSocket client, so this
/// drops the fallback bookkeeping the transport would have left behind.
pub fn close_chat_gpt_subscription_web_socket_sessions(session_id: Option<&str>) {
    fallback_state::clear_web_socket_fallback_state(session_id);
}

/// The codex session-resource cleanup senpi registers at module load
/// (`registerSessionResourceCleanup(closeChatGptSubscriptionWebSocketSessions)`). A Rust module has
/// no load hook, so the host registers this through [`register_session_resource_cleanup`].
pub fn codex_session_resource_cleanup(session_id: Option<&str>) -> Result<(), String> {
    close_chat_gpt_subscription_web_socket_sessions(session_id);
    Ok(())
}

/// `registerSessionResourceCleanup` for the codex WebSocket sessions.
pub fn register_session_resource_cleanup(
) -> impl FnOnce() {
    crate::session_resources::register_session_resource_cleanup(Arc::new(codex_session_resource_cleanup))
}

/// The Codex SSE adapter state shared with the driver: a Codex-level failure raised while mapping
/// the event stream (senpi throws these out of `mapCodexEvents`).
#[derive(Default)]
struct CodexStreamAdapterState {
    error: Option<String>,
}

/// `mapCodexEvents` + `parseSSE` as a byte stream: each Codex event is normalized onto the shared
/// Responses vocabulary and re-encoded as an SSE frame. A terminal event ends the stream exactly as
/// senpi's generator `return`s, so a body that stays open after completion is not read further.
struct CodexEventStream {
    body: Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    buffer: String,
    done: bool,
    state: Arc<Mutex<CodexStreamAdapterState>>,
}

impl Stream for CodexEventStream {
    type Item = Result<Bytes, reqwest::Error>;

    fn poll_next(self: Pin<&mut Self>, context: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            if this.done {
                return Poll::Ready(None);
            }
            if let Some(index) = this.buffer.find("\n\n") {
                let chunk: String = this.buffer[..index].to_owned();
                this.buffer = this.buffer[index + 2..].to_owned();
                match map_codex_frame(&chunk, &this.state) {
                    Ok(Some(bytes)) => return Poll::Ready(Some(Ok(bytes))),
                    Ok(None) => continue,
                    Err(()) => {
                        this.done = true;
                        return Poll::Ready(Some(Ok(Bytes::from(synthetic_error_frame(&this.state)))));
                    }
                }
            }
            match this.body.as_mut().poll_next(context) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => {
                    this.done = true;
                    let residual = std::mem::take(&mut this.buffer);
                    if residual.trim().is_empty() {
                        return Poll::Ready(None);
                    }
                    match map_codex_frame(&residual, &this.state) {
                        Ok(Some(bytes)) => return Poll::Ready(Some(Ok(bytes))),
                        _ => return Poll::Ready(None),
                    }
                }
                Poll::Ready(Some(Err(error))) => {
                    this.done = true;
                    let mut state = this.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                    if state.error.is_none() {
                        state.error = Some(error.to_string());
                    }
                    return Poll::Ready(None);
                }
                Poll::Ready(Some(Ok(chunk))) => {
                    this.buffer.push_str(&String::from_utf8_lossy(&chunk));
                }
            }
        }
    }
}

fn synthetic_error_frame(state: &Arc<Mutex<CodexStreamAdapterState>>) -> Vec<u8> {
    let message = state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .error
        .clone()
        .unwrap_or_default();
    let frame = json!({ "type": "error", "code": "", "message": message }).to_string();
    format!("data: {frame}\n\n").into_bytes()
}

/// `parseSSE` + `mapCodexEvents` for one frame: `Ok(None)` when the frame carries no event,
/// `Err(())` when a Codex-level failure was recorded (the caller emits the terminal frame).
fn map_codex_frame(chunk: &str, state: &Arc<Mutex<CodexStreamAdapterState>>) -> Result<Option<Bytes>, ()> {
    let data_lines: Vec<String> = chunk
        .split('\n')
        .filter(|line| line.starts_with("data:"))
        .map(|line| line[5..].trim().to_owned())
        .collect();
    if data_lines.is_empty() {
        return Ok(None);
    }
    let data = data_lines.join("\n");
    let data = data.trim();
    if data.is_empty() || data == "[DONE]" {
        return Ok(None);
    }
    let Ok(event) = serde_json::from_str::<Value>(data) else {
        return fail(state, format!("Invalid Codex SSE JSON: invalid JSON in {data:?}"));
    };
    let Some(event_type) = event.get("type").and_then(Value::as_str).map(str::to_owned) else {
        return Ok(None);
    };
    let Some(event) = event.as_object().cloned() else { return Ok(None) };

    match event_type.as_str() {
        "error" => {
            let code = get_codex_event_error_code(&event);
            let message = get_codex_event_error_message(&event);
            let fallback = serde_json::to_string(&Value::Object(event.clone())).unwrap_or_default();
            let base = format!("Codex error: {}", if !message.is_empty() { message } else if !code.is_empty() { code } else { fallback });
            fail(state, error_text_with_marker(base, &event))
        }
        "response.failed" => {
            let message = get_codex_event_error_message(&event);
            let base = if message.is_empty() { String::from("Codex response failed") } else { message };
            fail(state, error_text_with_marker(base, &event))
        }
        "response.done" | "response.completed" | "response.incomplete" => {
            let response = event.get("response").cloned().unwrap_or(Value::Null);
            let mut normalized = response.as_object().cloned().unwrap_or_default();
            if let Some(status) = normalize_codex_status(normalized.get("status")) {
                normalized.insert(String::from("status"), json!(status));
            } else {
                normalized.remove("status");
            }
            let frame = json!({ "type": "response.completed", "response": Value::Object(normalized) }).to_string();
            let mut state_guard = state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            state_guard.error = None;
            Ok(Some(Bytes::from(format!("data: {frame}\n\n"))))
        }
        _ => Ok(Some(Bytes::from(format!("data: {}\n\n", Value::Object(event))))),
    }
}

fn fail(state: &Arc<Mutex<CodexStreamAdapterState>>, message: String) -> Result<Option<Bytes>, ()> {
    state.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).error = Some(message);
    Err(())
}

/// `stream`.
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
        timestamp: now_ms(),
    }
}

async fn run_stream(model: Model, context: Context, options: StreamOptions, sink: AssistantMessageEventStream) {
    let mut output = create_output(&model);
    let codex_options = CodexResponsesOptions::from_stream_options(&options);
    if let Err(error) = drive(&model, &context, &options, &codex_options, &mut output, &sink).await {
        output.stop_reason = if options.request.signal.as_ref().is_some_and(|signal| signal.aborted()) {
            StopReason::Aborted
        } else {
            StopReason::Error
        };
        output.error_message = Some(format_codex_error(&error));
        sink.push(AssistantMessageEvent::Error {
            reason: if output.stop_reason == StopReason::Aborted { ErrorReason::Aborted } else { ErrorReason::Error },
            error: output.clone(),
        });
        sink.end(None);
    }
}

/// `formatProviderError(normalizeProviderError(error))` with no api prefix, as the Codex adapter
/// reports it.
fn format_codex_error(error: &CodexError) -> String {
    let shape = SdkErrorShape {
        message: error.message(),
        status_code: match error {
            CodexError::Api(api) => api.payload.as_ref().and_then(|payload| payload.get("status")).cloned(),
            _ => None,
        },
        ..SdkErrorShape::default()
    };
    format_provider_error(&normalize_provider_error(&ThrownProviderError::Error(Box::new(shape))), None)
}

async fn drive(
    model: &Model,
    context: &Context,
    options: &StreamOptions,
    codex_options: &CodexResponsesOptions,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
) -> Result<(), CodexError> {
    let api_key = options
        .request
        .api_key
        .clone()
        .filter(|key| !key.is_empty())
        .ok_or_else(|| CodexError::Other(format!("No API key for provider: {}", model.provider)))?;

    let account_id = extract_account_id(&api_key);
    let grammar_tool_input_properties = create_grammar_tool_input_properties(
        context.tools.as_deref(),
        model
            .compat
            .as_ref()
            .and_then(|compat| compat.get(COMPAT_GRAMMAR_TOOLS_KEY))
            .and_then(Value::as_bool)
            .unwrap_or(false),
    )
    .map_err(CodexError::Other)?;
    let cache_session_id = (options.cache_retention != Some(CacheRetention::None))
        .then(|| clamp_openai_prompt_cache_key(options.session_id.as_deref()))
        .flatten();
    let mut body = build_request_body(model, context, codex_options, cache_session_id.as_deref(), &grammar_tool_input_properties, options.extra_body.as_ref())
        .map_err(CodexError::Other)?;
    if let Some(next) = options.request.apply_payload_hook(&Value::Object(body.clone()), model, None)
        .await.map_err(CodexError::Other)?
    {
        body = next.as_object().cloned().unwrap_or_default();
    }

    let websocket_request_id = cache_session_id.clone().unwrap_or_else(|| uuidv7(None).unwrap_or_default());
    let sse_headers = build_sse_headers(
        model.headers.as_ref(),
        options.request.headers.as_ref(),
        account_id.as_deref(),
        &api_key,
        cache_session_id.as_deref(),
    );
    let _websocket_headers = build_web_socket_headers(
        model.headers.as_ref(),
        options.request.headers.as_ref(),
        account_id.as_deref(),
        &api_key,
        &websocket_request_id,
    );
    let body_json = Value::Object(body.clone()).to_string();
    let http_timeout_ms = normalize_timeout_ms(options.request.timeout_ms)?;
    let _websocket_connect_timeout_ms =
        normalize_timeout_ms(options.websocket_connect_timeout_ms.or(Some(DEFAULT_WEBSOCKET_CONNECT_TIMEOUT_MS)))?;
    let transport = options.transport.unwrap_or(Transport::Sse);
    let websocket_disabled_for_session = transport != Transport::Sse && is_web_socket_sse_fallback_active(cache_session_id.as_deref());
    if websocket_disabled_for_session {
        record_web_socket_sse_fallback(cache_session_id.as_deref());
    }

    // maho-ai has no WebSocket client: senpi's own runtime error is raised and, exactly as senpi
    // does when the transport never opened, the turn falls back to SSE.
    if transport != Transport::Sse && !websocket_disabled_for_session {
        let error = connect_web_socket_error();
        if transport == Transport::Websocket {
            return Err(error);
        }
        append_assistant_message_diagnostic(
            &mut output.diagnostics,
            create_assistant_message_diagnostic(
                "provider_transport_failure",
                &Thrown::error("Error", error.message()),
                None,
            ),
        );
        record_web_socket_failure(cache_session_id.as_deref(), &Thrown::error("Error", error.message()));
        record_web_socket_sse_fallback(cache_session_id.as_deref());
    }

    let mut sse_headers = sse_headers;
    let compressed_body = zstd::bulk::compress(body_json.as_bytes(), REQUEST_COMPRESSION_ZSTD_LEVEL).ok();
    if compressed_body.is_some() {
        sse_headers.insert(
            reqwest::header::HeaderName::from_static("content-encoding"),
            reqwest::header::HeaderValue::from_static("zstd"),
        );
    }
    let sse_body: Vec<u8> = compressed_body.unwrap_or_else(|| body_json.clone().into_bytes());

    let client = options.request.fetch.clone().unwrap_or_default();
    let url = resolve_codex_url(Some(model.base_url.as_str()));
    let max_retries = options.request.max_retries.unwrap_or(DEFAULT_MAX_RETRIES);
    let signal = options.request.signal.clone();
    let mut last_error: Option<CodexError> = None;
    let mut response: Option<reqwest::Response> = None;

    for attempt in 0..=max_retries {
        if signal.as_ref().is_some_and(|signal| signal.aborted()) {
            return Err(CodexError::Other(String::from("Request was aborted")));
        }
        let headers = sse_headers.clone();
        let body = sse_body.clone();
        let request = client.post(&url).headers(headers).body(body);
        let send = match http_timeout_ms {
            Some(timeout_ms) => match tokio::time::timeout(Duration::from_millis(timeout_ms), request.send()).await {
                Ok(result) => result,
                Err(_) => {
                    return Err(CodexError::Other(format!("Codex SSE response headers timed out after {timeout_ms}ms")));
                }
            },
            None => request.send().await,
        };
        let response_value = match send {
            Ok(response) => response,
            Err(error) => {
                let message = error.to_string();
                if signal.as_ref().is_some_and(|signal| signal.aborted()) {
                    return Err(CodexError::Other(String::from("Request was aborted")));
                }
                last_error = Some(CodexError::Other(message.clone()));
                if attempt < max_retries && !message.contains("usage limit") {
                    let delay = BASE_DELAY_MS * 2u64.pow(attempt);
                    if sleep(delay, signal.as_ref()).await.is_err() {
                        return Err(CodexError::Other(String::from("Request was aborted")));
                    }
                    continue;
                }
                return Err(CodexError::Other(message));
            }
        };

        options.request.apply_response_hook(
                &ProviderResponse { status: response_value.status().as_u16(), headers: headers_to_record(response_value.headers()) },
                model,
            ).await.map_err(CodexError::Other)?;

        if response_value.status().is_success() {
            response = Some(response_value);
            break;
        }

        let status = response_value.status().as_u16();
        let headers = response_value.headers().clone();
        let error_text = response_value.text().await.unwrap_or_default();
        if attempt < max_retries && is_retryable_error(status, &error_text) {
            let retry_after_delay_ms = extract_429_retry_after_ms(
                &RetryHintInput { status: Some(429), headers: Some(&headers), body_text: "" },
                None,
            );
            let delay_ms = match retry_after_delay_ms {
                Some(delay_ms) => validate_retry_delay_ms(delay_ms, options.request.max_retry_delay_ms)?,
                None => BASE_DELAY_MS * 2u64.pow(attempt),
            };
            if sleep(delay_ms, signal.as_ref()).await.is_err() {
                return Err(CodexError::Other(String::from("Request was aborted")));
            }
            continue;
        }

        let (message, friendly_message) = parse_error_response(status, "", &error_text).await;
        let mut error_message = friendly_message.unwrap_or(message);
        if status == 429 {
            let hint_ms = extract_429_retry_after_ms(
                &RetryHintInput { status: Some(429), headers: Some(&headers), body_text: &error_text },
                None,
            );
            if let Some(hint_ms) = hint_ms {
                error_message = append_retry_after_ms_marker(&error_message, hint_ms);
            }
        }
        return Err(CodexError::Other(error_message));
    }

    let Some(response) = response else {
        return Err(last_error.unwrap_or_else(|| CodexError::Other(String::from("Failed after retries"))));
    };

    stream.push(AssistantMessageEvent::Start { partial: output.clone() });
    let adapter_state = Arc::new(Mutex::new(CodexStreamAdapterState::default()));
    let adapter = CodexEventStream {
        body: Box::pin(response.bytes_stream()),
        buffer: String::new(),
        done: false,
        state: adapter_state.clone(),
    };
    let service_tier = codex_options.service_tier.as_deref();
    let stream_options = ResponsesStreamOptions {
        service_tier,
        grammar_tool_input_properties: &grammar_tool_input_properties,
        apply_service_tier_pricing: Some(&|usage: &mut Usage, service_tier: Option<&str>| {
            apply_codex_service_tier_pricing(usage, service_tier, model)
        }),
    };
    let processed = process_responses_stream(Box::pin(adapter), output, stream, model, &stream_options).await;
    let adapter_error = adapter_state.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).error.clone();
    if let Some(error) = adapter_error {
        return Err(CodexError::Other(error));
    }
    processed.map_err(|error: ResponsesStreamError| CodexError::Other(error.message))?;

    if signal.as_ref().is_some_and(|signal| signal.aborted()) {
        return Err(CodexError::Other(String::from("Request was aborted")));
    }
    assert_successful_output(output)?;
    stream.push(AssistantMessageEvent::Done { reason: get_done_reason(output.stop_reason), message: output.clone() });
    stream.end(None);
    Ok(())
}

fn assert_successful_output(output: &AssistantMessage) -> Result<(), CodexError> {
    if output.stop_reason == StopReason::Pending {
        return Err(CodexError::Other(String::from("Codex stream ended without a stop reason")));
    }
    if output.stop_reason == StopReason::Error || output.stop_reason == StopReason::Aborted {
        return Err(CodexError::Other(
            output.error_message.clone().unwrap_or_else(|| String::from("An unknown error occurred")),
        ));
    }
    Ok(())
}

/// `normalizeTimeoutMs`.
fn normalize_timeout_ms(value: Option<u64>) -> Result<Option<u64>, CodexError> {
    Ok(value)
}

/// `isTerminalRateLimitError`.
fn is_terminal_rate_limit_error(error_text: &str) -> bool {
    let lowered = error_text.to_lowercase();
    [
        "gousagelimiterror",
        "freeusagelimiterror",
        "monthly usage limit reached",
        "available balance",
        "insufficient_quota",
        "out of budget",
        "quota exceeded",
        "billing",
    ]
    .iter()
    .any(|needle| lowered.contains(needle))
}

/// `isRetryableError`.
fn is_retryable_error(status: u16, error_text: &str) -> bool {
    if status == 429 && is_terminal_rate_limit_error(error_text) {
        return false;
    }
    if matches!(status, 429 | 500 | 502 | 503 | 504) {
        return true;
    }
    let lowered = error_text.to_lowercase();
    ["ratelimit", "rate limit", "rate_limit", "overloaded", "service unavailable", "serviceunavailable", "upstream connect", "connection refused"]
        .iter()
        .any(|needle| lowered.replace([' ', '.', '-', '_'], "").contains(&needle.replace([' ', '.', '-', '_'], "")))
}

/// `validateRetryDelayMs`.
fn validate_retry_delay_ms(delay_ms: u64, max_retry_delay_ms: Option<u64>) -> Result<u64, CodexError> {
    let max_retry_delay_ms = max_retry_delay_ms.unwrap_or(DEFAULT_MAX_RETRY_DELAY_MS);
    if max_retry_delay_ms > 0 && delay_ms > max_retry_delay_ms {
        return Err(CodexError::Other(format!(
            "Server requested {}s retry delay (max: {}s)",
            delay_ms.div_ceil(1000),
            max_retry_delay_ms.div_ceil(1000)
        )));
    }
    Ok(delay_ms)
}

/// `sleep`: the abortable delay between retries.
async fn sleep(ms: u64, signal: Option<&crate::utils::abort::AbortSignal>) -> Result<(), ()> {
    let Some(signal) = signal else {
        tokio::time::sleep(Duration::from_millis(ms)).await;
        return Ok(());
    };
    if signal.aborted() {
        return Err(());
    }
    tokio::select! {
        biased;
        () = signal.cancelled() => Err(()),
        () = tokio::time::sleep(Duration::from_millis(ms)) => Ok(()),
    }
}

/// `streamSimple`.
pub fn stream_simple(model: &Model, context: &Context, options: Option<SimpleStreamOptions>) -> AssistantMessageEventStream {
    let simple = options.unwrap_or_default();
    let api_key = simple.stream.request.api_key.clone().filter(|key| !key.is_empty());
    let Some(api_key) = api_key else {
        return error_stream(model, &format!("No API key for provider: {}", model.provider));
    };
    let mut stream_options = match build_base_options(model, context, Some(&simple), Some(&api_key)) {
        Ok(stream_options) => stream_options,
        Err(error) => return error_stream(model, &error.to_string()),
    };
    if let Some(tool_choice) = simple.tool_choice {
        stream_options.extra.insert(String::from("toolChoice"), serde_json::to_value(tool_choice).unwrap_or(Value::Null));
    }
    if let Some(service_tier) = simple.service_tier {
        let tier = match service_tier {
            crate::types::ServiceTierPreference::Auto => "auto",
            crate::types::ServiceTierPreference::Flex => "flex",
            crate::types::ServiceTierPreference::Priority => "priority",
        };
        stream_options.extra.insert(String::from("serviceTier"), json!(tier));
    }
    let clamped_reasoning = simple.reasoning.map(|reasoning| clamp_thinking_level(model, reasoning.into()));
    let reasoning_effort = match clamped_reasoning {
        None | Some(ModelThinkingLevel::Off) => None,
        Some(ModelThinkingLevel::Max) if supports_max(model) => Some(String::from("max")),
        Some(level) => {
            let thinking = match level {
                ModelThinkingLevel::Off => ThinkingLevel::Medium,
                ModelThinkingLevel::Minimal => ThinkingLevel::Minimal,
                ModelThinkingLevel::Low => ThinkingLevel::Low,
                ModelThinkingLevel::Medium => ThinkingLevel::Medium,
                ModelThinkingLevel::High => ThinkingLevel::High,
                ModelThinkingLevel::Xhigh => ThinkingLevel::Xhigh,
                ModelThinkingLevel::Max => ThinkingLevel::Max,
            };
            clamp_max_for_openai(thinking, supports_xhigh(model))
                .map(|level| ModelThinkingLevel::from(level).as_str().to_owned())
        }
    };
    if let Some(reasoning_effort) = reasoning_effort {
        stream_options.extra.insert(String::from("reasoningEffort"), json!(reasoning_effort));
    }
    stream(model, context, Some(stream_options))
}

/// The codex session-cache entry point senpi registers as a session resource cleanup.
pub fn close_openai_codex_web_socket_sessions(session_id: Option<&str>) {
    close_chat_gpt_subscription_web_socket_sessions(session_id);
}

/// Kept for the cached-transport contract: the idle TTL the session cache arms.
pub const SESSION_WEBSOCKET_CACHE_TTL_MS_REEXPORT: u64 = SESSION_WEBSOCKET_CACHE_TTL_MS;

/// The transport-level socket the cached-session bookkeeping touches.
pub use crate::api::openai_responses::CachedWebSocket as CodexCachedWebSocket;

/// Unused import guard: the cached-session types are part of the ported surface.
#[allow(dead_code)]
fn _cached_socket_marker(socket: &dyn CachedWebSocket) -> Option<u8> {
    socket.ready_state()
}
