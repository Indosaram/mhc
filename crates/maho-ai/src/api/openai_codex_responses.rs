//! Port of senpi packages/ai/src/api/openai-codex-responses.ts.
//!
//! The Codex Responses wire API: an SSE (or cached-WebSocket) transport in front of the shared
//! Responses processor. This module builds the Codex request body, maps the Codex event vocabulary
//! onto the shared Responses events, and owns the retry/cache-affinity/compression behavior.

use std::collections::{BTreeMap, HashMap};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::task::{Context as TaskContext, Poll};
use std::time::Duration;

use bytes::Bytes;
use futures::Stream;
use serde_json::{Map, Value, json};

use crate::api::constrained_sampling::create_grammar_tool_input_properties;
use crate::api::openai_prompt_cache::{
    apply_chat_gpt_subscription_cache_affinity_headers, clamp_openai_prompt_cache_key,
};
use crate::api::openai_responses::{
    CachedWebSocket, CachedWebSocketConnection, ParseWebSocketOptions,
    SESSION_WEBSOCKET_CACHE_TTL_MS, TungsteniteWebSocket, WebSocketFailureSlot,
    WebSocketStreamFailure, close_websocket_silently, is_websocket_reusable, parse_web_socket,
};
use crate::api::openai_responses_shared::{
    ConvertResponsesMessagesOptions, ConvertResponsesToolsOptions, ResponsesStreamError,
    ResponsesStreamOptions, convert_responses_messages, convert_responses_tools, get_done_reason,
    process_responses_stream,
};
use crate::api::simple_options::{
    OPENAI_RESPONSES_RESERVED_BODY_KEYS, apply_extra_body, build_base_options, clamp_max_for_openai,
};
use crate::models::{clamp_thinking_level, supports_max, supports_xhigh};
use crate::types::{
    AssistantMessage, AssistantMessageEvent, CacheRetention, Context, ErrorReason, Model,
    ModelThinkingLevel, ProviderHeaders, ProviderResponse, SimpleStreamOptions, StopReason,
    StreamOptions, ThinkingLevel, Transport, Usage,
};
use crate::utils::abort::AbortSignal;
use crate::utils::deferred_tools::split_deferred_tools;
use crate::utils::diagnostics::{
    Thrown, append_assistant_message_diagnostic, create_assistant_message_diagnostic, now_ms,
};
use crate::utils::error_body::{
    SdkErrorShape, ThrownProviderError, format_provider_error, normalize_provider_error,
};
use crate::utils::event_stream::{
    AssistantMessageEventStream, create_assistant_message_event_stream,
};
use crate::utils::headers::headers_to_record;
use crate::utils::lazy::error_stream;
use crate::utils::retry_hint::{
    RetryHintInput, append_retry_after_ms_marker, extract_429_retry_after_ms,
};
use crate::utils::uuid::uuidv7;
use crate::wire_identity::get_wire_identity;

pub mod fallback_state;
pub mod provider_diagnostic;
pub mod reasoning;
pub mod usage_limit;

use fallback_state::{
    ChatGptSubscriptionWebSocketDebugStats, is_web_socket_sse_fallback_active,
    record_web_socket_failure, record_web_socket_sse_fallback,
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

const CODEX_RESPONSE_STATUSES: [&str; 6] = [
    "completed",
    "incomplete",
    "failed",
    "cancelled",
    "queued",
    "in_progress",
];

/// `OpenAICodexResponsesOptions`, read off the provider-neutral options bag.
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
    options
        .extra
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// A thrown Codex error carrying the backend's own code, mirroring `CodexApiError`.
#[derive(Debug, Clone, PartialEq)]
pub struct CodexApiError {
    pub message: String,
    pub code: Option<String>,
    pub payload: Option<Value>,
    /// The Codex-classified diagnostic (`attachProviderDiagnostic`), when the failure named one.
    pub provider_diagnostic: Option<Box<crate::types::ProviderDiagnostic>>,
}

/// `CodexProtocolError`.
#[derive(Debug, Clone, PartialEq)]
pub struct CodexProtocolError {
    pub message: String,
    pub payload: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CodexError {
    Api(Box<CodexApiError>),
    Protocol(CodexProtocolError),
    Provider(Box<crate::api::openai_responses::ResponsesApiError>),
    WebSocketClose(crate::api::websocket_transport_failure::WebSocketCloseError),
    /// A native transport failure whose typed source was classified before being formatted away
    /// (`transportProviderDiagnosticFromError`).
    Transport {
        message: String,
        diagnostic: Option<Box<crate::types::ProviderDiagnostic>>,
    },
    /// A non-Codex thrown value (network failure, timeout, abort, ...).
    Other(String),
}

impl CodexError {
    fn message(&self) -> String {
        match self {
            CodexError::Api(error) => error.message.clone(),
            CodexError::Protocol(error) => error.message.clone(),
            CodexError::Provider(error) => error.to_string(),
            CodexError::WebSocketClose(error) => error.to_string(),
            CodexError::Transport { message, .. } => message.clone(),
            CodexError::Other(message) => message.clone(),
        }
    }
}

impl From<CodexApiError> for CodexError {
    fn from(error: CodexApiError) -> Self {
        CodexError::Api(Box::new(error))
    }
}

impl From<crate::api::openai_responses::ResponsesApiError> for CodexError {
    fn from(error: crate::api::openai_responses::ResponsesApiError) -> Self {
        Self::Provider(Box::new(error))
    }
}

/// `buildRequestBody`.
pub fn build_request_body(
    model: &Model,
    context: &Context,
    options: &CodexResponsesOptions,
    cache_retention: Option<CacheRetention>,
    grammar_tool_input_properties: &BTreeMap<String, String>,
    extra_body: Option<&Map<String, Value>>,
) -> Result<Map<String, Value>, String> {
    let requested_reasoning_effort = options.reasoning_effort.as_deref();
    let mapped_reasoning_effort: Option<Option<String>> = requested_reasoning_effort
        .and_then(ModelThinkingLevel::parse)
        .and_then(|level| {
            model
                .thinking_level_map
                .as_ref()
                .and_then(|map| map.get(&level).cloned())
        });
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
        &CODEX_TOOL_CALL_PROVIDERS
            .iter()
            .map(|provider| (*provider).to_owned())
            .collect(),
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
        json!(
            context
                .system_prompt
                .clone()
                .filter(|prompt| !prompt.is_empty())
                .unwrap_or_else(|| String::from("You are a helpful assistant."))
        ),
    );
    body.insert(String::from("input"), Value::Array(messages));
    body.insert(
        String::from("text"),
        json!({ "verbosity": options.text_verbosity.clone().unwrap_or_else(|| String::from("low")) }),
    );
    body.insert(
        String::from("include"),
        json!(["reasoning.encrypted_content"]),
    );
    let prompt_cache_key = if cache_retention == Some(CacheRetention::None) {
        None
    } else {
        crate::api::openai_prompt_cache::derive_openai_prompt_cache_key(
            &model.id,
            &crate::types::transcript::normalize_context(context),
        )
    };
    body.insert(String::from("prompt_cache_key"), json!(prompt_cache_key));
    body.insert(
        String::from("tool_choice"),
        json!(
            options
                .tool_choice
                .clone()
                .unwrap_or_else(|| String::from("auto"))
        ),
    );
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
    let thinking_off = model
        .thinking_level_map
        .as_ref()
        .and_then(|map| map.get(&ModelThinkingLevel::Off));
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
    let raw = base_url
        .filter(|base_url| !base_url.trim().is_empty())
        .unwrap_or(DEFAULT_CODEX_BASE_URL);
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
pub fn resolve_codex_service_tier(
    response_service_tier: Option<&str>,
    request_service_tier: Option<&str>,
) -> Option<String> {
    if response_service_tier == Some("default")
        && matches!(
            request_service_tier,
            Some("flex") | Some("priority") | Some("fast") | Some("ultrafast")
        )
    {
        return request_service_tier.map(str::to_owned);
    }
    response_service_tier
        .or(request_service_tier)
        .map(str::to_owned)
}

/// `getServiceTierCostMultiplier` for ChatGPT plan usage, not purchased API credits.
pub fn get_codex_service_tier_cost_multiplier(model: &Model, service_tier: Option<&str>) -> f64 {
    match service_tier {
        Some("ultrafast") => {
            let id = model.upstream_model_id.as_deref().unwrap_or(&model.id);
            if ["gpt-6-astra", "gpt-6.1-sol"].contains(&id) {
                8.0
            } else {
                1.0
            }
        }
        Some("flex") => 0.5,
        Some("priority") | Some("fast") => 2.5,
        _ => 1.0,
    }
}

/// `applyServiceTierPricing` for the Codex API.
pub fn apply_codex_service_tier_pricing(
    usage: &mut Usage,
    service_tier: Option<&str>,
    model: &Model,
) {
    let multiplier = get_codex_service_tier_cost_multiplier(model, service_tier);
    if multiplier == 1.0 {
        return;
    }
    usage.cost.input *= multiplier;
    usage.cost.output *= multiplier;
    usage.cost.cache_read *= multiplier;
    usage.cost.cache_write *= multiplier;
    usage.cost.total =
        usage.cost.input + usage.cost.output + usage.cost.cache_read + usage.cost.cache_write;
}

/// `normalizeCodexStatus`: an unrecognized status is dropped, not passed through.
fn normalize_codex_status(status: Option<&Value>) -> Option<String> {
    let status = status?.as_str()?;
    CODEX_RESPONSE_STATUSES
        .contains(&status)
        .then(|| status.to_owned())
}

fn as_record(value: &Value) -> Option<&Map<String, Value>> {
    value.as_object()
}

fn get_string(value: Option<&Value>) -> Option<String> {
    value?.as_str().map(str::to_owned)
}

fn get_codex_event_error(event: &Map<String, Value>) -> Option<&Map<String, Value>> {
    let response = event.get("response").and_then(as_record);
    event.get("error").and_then(as_record).or_else(|| {
        response
            .and_then(|response| response.get("error"))
            .and_then(as_record)
    })
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
    match extract_429_retry_after_ms(
        &RetryHintInput {
            status: None,
            headers: None,
            body_text: &payload,
        },
        None,
    ) {
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
    as_record(event)
        .map(get_codex_event_error_code)
        .unwrap_or_default()
}

/// `getChatGptSubscriptionWebSocketDebugStats`.
pub fn get_chat_gpt_subscription_web_socket_debug_stats(
    session_id: &str,
) -> Option<ChatGptSubscriptionWebSocketDebugStats> {
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
    stats.last_input_items = request_body
        .get("input")
        .and_then(Value::as_array)
        .map_or(0, |input| input.len() as u64);
    match request_body
        .get("previous_response_id")
        .and_then(Value::as_str)
    {
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
        serde_json::to_string(&value.cloned().unwrap_or(Value::Array(Vec::new())))
            .unwrap_or_default()
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
    let current_input = body
        .get("input")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut baseline = last_request_body
        .get("input")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    baseline.extend(last_response_items.iter().cloned());
    if current_input.len() < baseline.len() {
        return None;
    }
    let prefix = &current_input[..baseline.len()];
    if !response_inputs_equal(
        Some(&Value::Array(prefix.to_vec())),
        Some(&Value::Array(baseline.clone())),
    ) {
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
    next.insert(
        String::from("previous_response_id"),
        json!(last_response_id),
    );
    next.insert(String::from("input"), Value::Array(delta));
    Some(next)
}

/// `parseErrorResponse`.
pub async fn parse_error_response(
    status: u16,
    status_text: &str,
    raw: &str,
) -> (String, Option<String>) {
    let mut message = if raw.is_empty() {
        status_text.to_owned()
    } else {
        raw.to_owned()
    };
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
        let code_is_usage_limit = [
            "usage_limit_reached",
            "usage_not_included",
            "rate_limit_exceeded",
        ]
        .iter()
        .any(|needle| code.to_lowercase().contains(needle));
        if code_is_usage_limit || status == 429 {
            let plan = error
                .get("plan_type")
                .and_then(Value::as_str)
                .map(|plan| format!(" ({} plan)", plan.to_lowercase()))
                .unwrap_or_default();
            let minutes = error
                .get("resets_at")
                .and_then(Value::as_f64)
                .map(|resets_at| {
                    ((resets_at * 1000.0 - now_ms() as f64) / 60_000.0)
                        .round()
                        .max(0.0) as i64
                });
            let when = minutes
                .map(|minutes| format!(" Try again in ~{minutes} min."))
                .unwrap_or_default();
            friendly_message = Some(
                format!("You have hit your ChatGPT usage limit{plan}.{when}")
                    .trim()
                    .to_owned(),
            );
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
    static OS_RELEASE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    OS_RELEASE
        .get_or_init(|| {
            std::process::Command::new("uname")
                .arg("-r")
                .output()
                .ok()
                .filter(|output| output.status.success())
                .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
                .unwrap_or_default()
        })
        .clone()
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
    headers.insert(
        String::from("Authorization"),
        Some(format!("Bearer {token}")),
    );
    match account_id {
        Some(account_id) => {
            headers.insert(
                String::from("chatgpt-account-id"),
                Some(account_id.to_owned()),
            );
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
        let Ok(name) = reqwest::header::HeaderName::from_bytes(name.as_bytes()) else {
            continue;
        };
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
    headers.insert(
        reqwest::header::ACCEPT,
        reqwest::header::HeaderValue::from_static("text/event-stream"),
    );
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
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
        reqwest::header::HeaderValue::from_static(
            crate::api::openai_responses::OPENAI_BETA_RESPONSES_WEBSOCKETS,
        ),
    );
    apply_chat_gpt_subscription_cache_affinity_headers(&mut headers, Some(request_id));
    headers
}

/// `CachedWebSocketContinuationState`.
#[derive(Debug, Clone, PartialEq)]
pub struct CachedWebSocketContinuationState {
    pub last_request_body: Map<String, Value>,
    pub last_response_id: String,
    pub last_response_items: Vec<Value>,
}

/// The Codex `CachedWebSocketConnection`: one live socket per session and account, parked between
/// requests and carrying the `previous_response_id` continuation.
pub struct CodexCachedWebSocketConnection {
    socket: Arc<TungsteniteWebSocket>,
    busy: AtomicBool,
    created_at: u64,
    idle_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    continuation: Mutex<Option<CachedWebSocketContinuationState>>,
}

impl CodexCachedWebSocketConnection {
    fn new(socket: Arc<TungsteniteWebSocket>) -> Self {
        Self {
            socket,
            busy: AtomicBool::new(true),
            created_at: u64::try_from(now_ms()).unwrap_or_default(),
            idle_task: Mutex::new(None),
            continuation: Mutex::new(None),
        }
    }

    fn busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }

    fn set_continuation(&self, continuation: Option<CachedWebSocketContinuationState>) {
        *self
            .continuation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = continuation;
    }
}

type CodexWebSocketSessionCache =
    HashMap<String, HashMap<String, Arc<CodexCachedWebSocketConnection>>>;

/// `websocketSessionCache`: session id -> account id -> connection.
static WEBSOCKET_SESSION_CACHE: LazyLock<Mutex<CodexWebSocketSessionCache>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn websocket_session_cache() -> std::sync::MutexGuard<'static, CodexWebSocketSessionCache> {
    WEBSOCKET_SESSION_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn remove_session_web_socket(
    session_id: &str,
    account_id: &str,
    entry: &Arc<CodexCachedWebSocketConnection>,
) {
    let mut cache = websocket_session_cache();
    if let Some(account_entries) = cache.get_mut(session_id) {
        if account_entries
            .get(account_id)
            .is_some_and(|cached| Arc::ptr_eq(cached, entry))
        {
            account_entries.remove(account_id);
        }
        if account_entries.is_empty() {
            cache.remove(session_id);
        }
    }
}

/// `evictSessionWebSocket`.
fn evict_session_web_socket(
    session_id: &str,
    account_id: &str,
    entry: &Arc<CodexCachedWebSocketConnection>,
    reason: &str,
) {
    unpark_session_web_socket(entry);
    close_websocket_silently(entry.socket.as_ref(), 1000, reason);
    remove_session_web_socket(session_id, account_id, entry);
}

/// `unparkSessionWebSocket`.
fn unpark_session_web_socket(entry: &CodexCachedWebSocketConnection) {
    if let Some(task) = entry
        .idle_task
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
    {
        task.abort();
    }
}

/// `parkSessionWebSocket`: parks a connection between requests. The idle TTL is the only eviction
/// the cache had; a server or network close while parked must evict immediately, because the next
/// request would otherwise write into a dead socket and wait out the whole stream-start watchdog.
fn park_session_web_socket(
    session_id: String,
    account_id: String,
    entry: Arc<CodexCachedWebSocketConnection>,
) {
    unpark_session_web_socket(&entry);
    entry.busy.store(false, Ordering::SeqCst);
    let parked = entry.clone();
    let task = tokio::spawn(async move {
        let reason = tokio::select! {
            () = tokio::time::sleep(Duration::from_millis(SESSION_WEBSOCKET_CACHE_TTL_MS)) => "idle_timeout",
            () = parked.socket.closed() => "parked_close",
        };
        if parked.busy() {
            return;
        }
        evict_session_web_socket(&session_id, &account_id, &parked, reason);
    });
    *entry
        .idle_task
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(task);
}

/// The `{ socket, entry, reused, release }` `acquireWebSocket` returns.
struct CodexWebSocketLease {
    socket: Arc<TungsteniteWebSocket>,
    /// The cached entry with its session and account keys; `None` for a one-shot socket.
    entry: Option<(String, String, Arc<CodexCachedWebSocketConnection>)>,
    reused: bool,
}

impl CodexWebSocketLease {
    fn entry(&self) -> Option<&Arc<CodexCachedWebSocketConnection>> {
        self.entry.as_ref().map(|(_, _, entry)| entry)
    }

    fn release(self, keep: bool) {
        let Some((session_id, account_id, entry)) = self.entry else {
            close_websocket_silently(self.socket.as_ref(), 1000, "done");
            return;
        };
        if !keep || !is_websocket_reusable(entry.socket.as_ref()) {
            close_websocket_silently(entry.socket.as_ref(), 1000, "done");
            unpark_session_web_socket(&entry);
            remove_session_web_socket(&session_id, &account_id, &entry);
            return;
        }
        park_session_web_socket(session_id, account_id, entry);
    }
}

/// `acquireWebSocket`.
async fn acquire_web_socket(
    url: &str,
    headers: &BTreeMap<String, String>,
    session_id: Option<&str>,
    account_id: &str,
    signal: Option<&AbortSignal>,
    connect_timeout_ms: Option<u64>,
) -> Result<CodexWebSocketLease, CodexError> {
    let Some(session_id) = session_id else {
        let socket = super::openai_responses::connect_web_socket_typed(
            url,
            headers,
            signal,
            connect_timeout_ms,
        )
        .await
        .map_err(CodexError::from)?;
        return Ok(CodexWebSocketLease {
            socket,
            entry: None,
            reused: false,
        });
    };

    let cached = websocket_session_cache()
        .get(session_id)
        .and_then(|entries| entries.get(account_id))
        .cloned();
    if let Some(cached) = cached {
        unpark_session_web_socket(&cached);
        if !cached.busy() && is_web_socket_session_expired(cached.created_at) {
            close_websocket_silently(cached.socket.as_ref(), 1000, "connection_age_limit");
            remove_session_web_socket(session_id, account_id, &cached);
        } else if !cached.busy() && is_websocket_reusable(cached.socket.as_ref()) {
            cached.busy.store(true, Ordering::SeqCst);
            return Ok(CodexWebSocketLease {
                socket: cached.socket.clone(),
                entry: Some((session_id.to_owned(), account_id.to_owned(), cached)),
                reused: true,
            });
        } else if cached.busy() {
            let socket = super::openai_responses::connect_web_socket_typed(
                url,
                headers,
                signal,
                connect_timeout_ms,
            )
            .await
            .map_err(CodexError::from)?;
            return Ok(CodexWebSocketLease {
                socket,
                entry: None,
                reused: false,
            });
        } else {
            close_websocket_silently(cached.socket.as_ref(), 1000, "done");
            remove_session_web_socket(session_id, account_id, &cached);
        }
    }

    let socket =
        super::openai_responses::connect_web_socket_typed(url, headers, signal, connect_timeout_ms)
            .await
            .map_err(CodexError::from)?;
    let entry = Arc::new(CodexCachedWebSocketConnection::new(socket.clone()));
    websocket_session_cache()
        .entry(session_id.to_owned())
        .or_default()
        .insert(account_id.to_owned(), entry.clone());
    Ok(CodexWebSocketLease {
        socket,
        entry: Some((session_id.to_owned(), account_id.to_owned(), entry)),
        reused: false,
    })
}

/// `buildCachedWebSocketRequestBody` over a cached entry: a stale or mismatched continuation is
/// dropped and the full body sent.
fn build_entry_cached_web_socket_request_body(
    entry: &CodexCachedWebSocketConnection,
    body: &Map<String, Value>,
) -> Map<String, Value> {
    let mut continuation = entry
        .continuation
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(state) = continuation.as_ref() else {
        return body.clone();
    };
    match build_cached_web_socket_request_body(
        body,
        Some(&state.last_request_body),
        Some(state.last_response_id.as_str()).filter(|id| !id.is_empty()),
        Some(state.last_response_items.as_slice()),
    ) {
        Some(next) => next,
        None => {
            *continuation = None;
            body.clone()
        }
    }
}

/// senpi's `onStart` callback state: `websocketStarted` for this attempt and the request-wide
/// `startEmitted`.
#[derive(Clone)]
struct WebSocketStart {
    started: Arc<AtomicBool>,
    start_emitted: Arc<AtomicBool>,
}

/// `startWebSocketOutputOnFirstEvent`: emits `start` when the first mapped event arrives. A Codex
/// error event is thrown by `mapCodexEvents` before it is yielded, so it does not count.
struct StartOnFirstEvent {
    inner: Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    adapter_state: Arc<Mutex<CodexStreamAdapterState>>,
    start: Option<(
        WebSocketStart,
        AssistantMessage,
        AssistantMessageEventStream,
    )>,
}

impl Stream for StartOnFirstEvent {
    type Item = Result<Bytes, reqwest::Error>;

    fn poll_next(self: Pin<&mut Self>, context: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let polled = this.inner.as_mut().poll_next(context);
        if let Poll::Ready(Some(Ok(_))) = &polled {
            let failed = this
                .adapter_state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .error
                .is_some();
            if !failed && let Some((start, partial, sink)) = this.start.take() {
                start.started.store(true, Ordering::SeqCst);
                if !start.start_emitted.swap(true, Ordering::SeqCst) {
                    sink.push(AssistantMessageEvent::Start { partial });
                }
            }
        }
        polled
    }
}

/// One `response.create` round trip over an acquired socket.
#[allow(clippy::too_many_arguments)]
async fn run_web_socket_request(
    socket: &Arc<TungsteniteWebSocket>,
    request_body: &Map<String, Value>,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    model: &Model,
    on_start: &WebSocketStart,
    idle_timeout_ms: Option<u64>,
    signal: Option<AbortSignal>,
    grammar_tool_input_properties: &BTreeMap<String, String>,
    codex_options: &CodexResponsesOptions,
    options: &StreamOptions,
) -> Result<(), CodexError> {
    let mut frame = Map::new();
    frame.insert(String::from("type"), json!("response.create"));
    frame.extend(request_body.clone());
    let events = socket
        .begin_request(Value::Object(frame).to_string())
        .await
        .map_err(CodexError::Other)?;
    let failed: WebSocketFailureSlot = Arc::default();
    let body = parse_web_socket(
        socket.clone(),
        events,
        signal,
        ParseWebSocketOptions {
            completion_types: &["response.completed", "response.done", "response.incomplete"],
            invalid_json_prefix: Some("Invalid Codex WebSocket JSON"),
            idle_timeout_ms,
        },
        failed.clone(),
    );
    let adapter_state = Arc::new(Mutex::new(CodexStreamAdapterState::default()));
    let adapter = CodexEventStream {
        body,
        buffer: String::new(),
        done: false,
        state: adapter_state.clone(),
    };
    let started = StartOnFirstEvent {
        inner: Box::pin(adapter),
        adapter_state: adapter_state.clone(),
        start: Some((on_start.clone(), output.clone(), stream.clone())),
    };
    let stream_options = ResponsesStreamOptions {
        service_tier: codex_options.service_tier.as_deref(),
        on_provider_stream_event: options.on_provider_stream_event.as_ref(),
        async_on_provider_stream_event: options.async_on_provider_stream_event.as_ref(),
        resolve_service_tier: Some(&resolve_codex_service_tier),
        grammar_tool_input_properties,
        apply_service_tier_pricing: Some(&|usage: &mut Usage, service_tier: Option<&str>| {
            apply_codex_service_tier_pricing(usage, service_tier, model)
        }),
    };
    let processed =
        process_responses_stream(Box::pin(started), output, stream, model, &stream_options).await;
    let adapter_error = adapter_state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .error
        .take();
    if let Some(error) = adapter_error {
        return Err(error);
    }
    let failure = failed
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    if let Some(failure) = failure {
        return Err(match failure {
            WebSocketStreamFailure::Close(error) => CodexError::WebSocketClose(error),
            WebSocketStreamFailure::DiagnosedTransport {
                message,
                diagnostic,
            } => CodexError::from(super::openai_responses::ResponsesApiError::Provider {
                message,
                diagnostic,
                error_cause: None,
            }),
            WebSocketStreamFailure::Protocol { message, payload } => {
                CodexError::Protocol(CodexProtocolError {
                    message,
                    payload: Some(Value::String(payload)),
                })
            }
            other => CodexError::Other(other.message()),
        });
    }
    processed.map_err(|error| CodexError::Other(error.message))
}

/// `processWebSocketStream`.
#[allow(clippy::too_many_arguments)]
async fn process_web_socket_stream(
    url: &str,
    body: &Map<String, Value>,
    headers: &reqwest::header::HeaderMap,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    model: &Model,
    on_start: &WebSocketStart,
    idle_timeout_ms: Option<u64>,
    websocket_connect_timeout_ms: Option<u64>,
    cache_session_id: Option<&str>,
    account_id: &str,
    grammar_tool_input_properties: &BTreeMap<String, String>,
    options: &StreamOptions,
    codex_options: &CodexResponsesOptions,
) -> Result<(), CodexError> {
    let signal = options.request.signal.clone();
    let lease = acquire_web_socket(
        url,
        &headers_to_record(headers),
        cache_session_id,
        account_id,
        signal.as_ref(),
        websocket_connect_timeout_ms,
    )
    .await?;
    let use_cached_context = matches!(
        options.transport,
        Some(Transport::WebsocketCached | Transport::Auto)
    );
    // ChatGPT Codex Responses rejects `store: true` ("Store must be set to false"). WebSocket
    // continuation still works via connection-scoped previous_response_id state.
    let full_body = body;
    let mut request_body = match lease.entry() {
        Some(entry) if use_cached_context => {
            build_entry_cached_web_socket_request_body(entry, full_body)
        }
        _ => full_body.clone(),
    };
    let mut recovered_stale_previous_response = false;
    let result = loop {
        if let Some(session_id) = cache_session_id {
            let reused = lease.reused && !recovered_stale_previous_response;
            fallback_state::update_web_socket_debug_stats(session_id, |stats| {
                record_web_socket_request_stats(
                    Some(stats),
                    &request_body,
                    reused,
                    use_cached_context,
                );
            });
        }
        match run_web_socket_request(
            &lease.socket,
            &request_body,
            output,
            stream,
            model,
            on_start,
            idle_timeout_ms,
            signal.clone(),
            grammar_tool_input_properties,
            codex_options,
            options,
        )
        .await
        {
            Ok(()) => break Ok(()),
            Err(error) => {
                if !recovered_stale_previous_response
                    && let Some(entry) = lease.entry()
                    && request_body
                        .get("previous_response_id")
                        .and_then(Value::as_str)
                        .is_some_and(|id| !id.is_empty())
                    && is_codex_previous_response_stale(&error)
                    && !signal.as_ref().is_some_and(AbortSignal::aborted)
                {
                    entry.set_continuation(None);
                    request_body = full_body.clone();
                    recovered_stale_previous_response = true;
                    continue;
                }
                break Err(error);
            }
        }
    };

    let mut keep_connection = true;
    match &result {
        Ok(()) => {
            if signal.as_ref().is_some_and(AbortSignal::aborted) {
                keep_connection = false;
            } else if use_cached_context
                && let Some(entry) = lease.entry()
                && let Some(response_id) = output.response_id.clone()
            {
                let context = Context {
                    active_tool_names: None,
                    system_prompt: None,
                    messages: vec![crate::types::Message::Assistant(Box::new(output.clone()))],
                    tools: None,
                };
                let response_items: Vec<Value> = convert_responses_messages(
                    model,
                    &context,
                    &CODEX_TOOL_CALL_PROVIDERS
                        .iter()
                        .map(|provider| (*provider).to_owned())
                        .collect(),
                    &ConvertResponsesMessagesOptions {
                        include_system_prompt: Some(false),
                        grammar_tool_input_properties: grammar_tool_input_properties.clone(),
                        ..ConvertResponsesMessagesOptions::default()
                    },
                )
                .into_iter()
                .filter(|item| {
                    !matches!(
                        item.get("type").and_then(Value::as_str),
                        Some("function_call_output" | "custom_tool_call_output")
                    )
                })
                .collect();
                entry.set_continuation(Some(CachedWebSocketContinuationState {
                    last_request_body: full_body.clone(),
                    last_response_id: response_id,
                    last_response_items: response_items,
                }));
            }
        }
        Err(_) => {
            if let Some(entry) = lease.entry() {
                entry.set_continuation(None);
            }
            keep_connection = false;
        }
    }
    lease.release(keep_connection);
    result
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

/// `closeChatGptSubscriptionWebSocketSessions`.
pub fn close_chat_gpt_subscription_web_socket_sessions(session_id: Option<&str>) {
    let close_entry = |entry: &Arc<CodexCachedWebSocketConnection>| {
        unpark_session_web_socket(entry);
        close_websocket_silently(entry.socket.as_ref(), 1000, "debug_close");
    };
    let mut cache = websocket_session_cache();
    if let Some(session_id) = session_id {
        for entry in cache
            .remove(session_id)
            .into_iter()
            .flat_map(HashMap::into_values)
        {
            close_entry(&entry);
        }
        drop(cache);
        fallback_state::clear_web_socket_fallback_state(Some(session_id));
        return;
    }
    for entry in cache.drain().flat_map(|(_, entries)| entries.into_values()) {
        close_entry(&entry);
    }
    drop(cache);
    fallback_state::clear_web_socket_fallback_state(None);
}

/// The codex session-resource cleanup senpi registers at module load
/// (`registerSessionResourceCleanup(closeChatGptSubscriptionWebSocketSessions)`). A Rust module has
/// no load hook, so the host registers this through [`register_session_resource_cleanup`].
pub fn codex_session_resource_cleanup(session_id: Option<&str>) -> Result<(), String> {
    close_chat_gpt_subscription_web_socket_sessions(session_id);
    Ok(())
}

/// `registerSessionResourceCleanup` for the codex WebSocket sessions.
pub fn register_session_resource_cleanup() -> impl FnOnce() {
    crate::session_resources::register_session_resource_cleanup(Arc::new(
        codex_session_resource_cleanup,
    ))
}

/// The Codex SSE adapter state shared with the driver: a Codex-level failure raised while mapping
/// the event stream (senpi throws these out of `mapCodexEvents`).
#[derive(Default)]
struct CodexStreamAdapterState {
    error: Option<CodexError>,
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
                        return Poll::Ready(Some(Ok(Bytes::from(synthetic_error_frame(
                            &this.state,
                        )))));
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
                    let mut state = this
                        .state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    if state.error.is_none() {
                        state.error = Some(CodexError::Transport {
                            message: error.to_string(),
                            diagnostic:
                                crate::utils::provider_diagnostic_carrier::diagnostic_from_transport(
                                    &error,
                                )
                                .map(Box::new),
                        });
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
        .as_ref()
        .map(CodexError::message)
        .unwrap_or_default();
    let frame = json!({ "type": "error", "code": "", "message": message }).to_string();
    format!("data: {frame}\n\n").into_bytes()
}

/// `parseSSE` + `mapCodexEvents` for one frame: `Ok(None)` when the frame carries no event,
/// `Err(())` when a Codex-level failure was recorded (the caller emits the terminal frame).
fn map_codex_frame(
    chunk: &str,
    state: &Arc<Mutex<CodexStreamAdapterState>>,
) -> Result<Option<Bytes>, ()> {
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
    let event = match serde_json::from_str::<Value>(data) {
        Ok(event) => event,
        Err(error) => {
            return fail(
                state,
                CodexError::Protocol(CodexProtocolError {
                    message: format!("Invalid Codex SSE JSON: {error}"),
                    payload: Some(Value::String(data.to_owned())),
                }),
            );
        }
    };
    let Some(event_type) = event.get("type").and_then(Value::as_str).map(str::to_owned) else {
        return Ok(None);
    };
    let Some(event) = event.as_object().cloned() else {
        return Ok(None);
    };

    match event_type.as_str() {
        "error" => {
            let code = get_codex_event_error_code(&event);
            let message = get_codex_event_error_message(&event);
            let fallback = serde_json::to_string(&Value::Object(event.clone())).unwrap_or_default();
            let base =
                usage_limit::describe_chat_gpt_usage_limit_event(&event).unwrap_or_else(|| {
                    format!(
                        "Codex error: {}",
                        if !message.is_empty() {
                            message
                        } else if !code.is_empty() {
                            code.clone()
                        } else {
                            fallback
                        }
                    )
                });
            let diagnostic = provider_diagnostic::codex_event_provider_diagnostic(&event, now_ms());
            fail(
                state,
                CodexError::Api(Box::new(CodexApiError {
                    message: error_text_with_marker(base, &event),
                    code: (!code.is_empty()).then_some(code),
                    payload: Some(Value::Object(event)),
                    provider_diagnostic: diagnostic.map(Box::new),
                })),
            )
        }
        "response.failed" => {
            let code = get_codex_event_error_code(&event);
            let message = get_codex_event_error_message(&event);
            let mut with_error = event.clone();
            if let Some(error) = get_codex_event_error(&event) {
                with_error.insert(String::from("error"), Value::Object(error.clone()));
            } else {
                with_error.remove("error");
            }
            let base = usage_limit::describe_chat_gpt_usage_limit_event(&with_error)
                .unwrap_or_else(|| {
                    if message.is_empty() {
                        String::from("Codex response failed")
                    } else {
                        message
                    }
                });
            let diagnostic = provider_diagnostic::codex_event_provider_diagnostic(&event, now_ms());
            fail(
                state,
                CodexError::Api(Box::new(CodexApiError {
                    message: error_text_with_marker(base, &event),
                    code: Some(code),
                    payload: Some(Value::Object(event)),
                    provider_diagnostic: diagnostic.map(Box::new),
                })),
            )
        }
        "response.done" | "response.completed" | "response.incomplete" => {
            let response = event.get("response").cloned().unwrap_or(Value::Null);
            let mut normalized = response.as_object().cloned().unwrap_or_default();
            if let Some(status) = normalize_codex_status(normalized.get("status")) {
                normalized.insert(String::from("status"), json!(status));
            } else {
                normalized.remove("status");
            }
            let frame =
                json!({ "type": "response.completed", "response": Value::Object(normalized) })
                    .to_string();
            let mut state_guard = state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state_guard.error = None;
            Ok(Some(Bytes::from(format!("data: {frame}\n\n"))))
        }
        _ => Ok(Some(Bytes::from(format!(
            "data: {}\n\n",
            Value::Object(event)
        )))),
    }
}

fn fail(
    state: &Arc<Mutex<CodexStreamAdapterState>>,
    error: CodexError,
) -> Result<Option<Bytes>, ()> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .error = Some(error);
    Err(())
}

/// `stream`.
pub fn stream(
    model: &Model,
    context: &Context,
    options: Option<StreamOptions>,
) -> AssistantMessageEventStream {
    let resolved_context = crate::types::transcript::resolve_provider_context(context, false);
    let context = &resolved_context;
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
        failure_source: None,
        timing: None,
        prompt_cache_breakpoint: None,
        prompt_cache_diagnostics: None,
        thinking_level: None,
        provider_diagnostic: None,
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
        error_cause: None,
        abort_source: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: now_ms(),
    }
}

async fn run_stream(
    model: Model,
    context: Context,
    options: StreamOptions,
    sink: AssistantMessageEventStream,
) {
    let mut output = create_output(&model);
    let codex_options = CodexResponsesOptions::from_stream_options(&options);
    if let Err(error) = drive(
        &model,
        &context,
        &options,
        &codex_options,
        &mut output,
        &sink,
    )
    .await
    {
        output.stop_reason = if options
            .request
            .signal
            .as_ref()
            .is_some_and(|signal| signal.aborted())
        {
            StopReason::Aborted
        } else {
            StopReason::Error
        };
        output.error_message = Some(format_codex_error(&error));
        if output.stop_reason != StopReason::Aborted {
            match &error {
                CodexError::Provider(error) => {
                    super::openai_responses::apply_responses_failure(&mut output, error)
                }
                CodexError::Api(error) => {
                    output.provider_diagnostic = error.provider_diagnostic.as_deref().cloned();
                    if let Some(payload) = &error.payload {
                        output.error_cause = crate::api::github_copilot_errors::usage_limit_cause(
                            &model.provider,
                            None,
                            payload,
                        );
                    }
                }
                CodexError::Transport { diagnostic, .. } => {
                    output.provider_diagnostic = diagnostic.as_deref().cloned();
                }
                CodexError::Protocol(_) | CodexError::WebSocketClose(_) | CodexError::Other(_) => {}
            }
        }
        sink.push(AssistantMessageEvent::Error {
            reason: if output.stop_reason == StopReason::Aborted {
                ErrorReason::Aborted
            } else {
                ErrorReason::Error
            },
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
            CodexError::Api(api) => api
                .payload
                .as_ref()
                .and_then(|payload| payload.get("status"))
                .cloned(),
            _ => None,
        },
        ..SdkErrorShape::default()
    };
    format_provider_error(
        &normalize_provider_error(&ThrownProviderError::Error(Box::new(shape))),
        None,
    )
}

async fn drive(
    model: &Model,
    context: &Context,
    options: &StreamOptions,
    codex_options: &CodexResponsesOptions,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
) -> Result<(), CodexError> {
    let resolved_context = crate::types::transcript::resolve_provider_context(context, false);
    let context = &resolved_context;
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
    let mut body = build_request_body(
        model,
        context,
        codex_options,
        options.cache_retention,
        &grammar_tool_input_properties,
        options.extra_body.as_ref(),
    )
    .map_err(CodexError::Other)?;
    if let Some(next) = options
        .request
        .apply_payload_hook(&Value::Object(body.clone()), model, None)
        .await
        .map_err(CodexError::Other)?
    {
        body = next.as_object().cloned().unwrap_or_default();
    }

    let websocket_request_id = cache_session_id
        .clone()
        .unwrap_or_else(|| uuidv7(None).unwrap_or_default());
    let sse_headers = build_sse_headers(
        model.headers.as_ref(),
        options.request.headers.as_ref(),
        account_id.as_deref(),
        &api_key,
        cache_session_id.as_deref(),
    );
    let websocket_headers = build_web_socket_headers(
        model.headers.as_ref(),
        options.request.headers.as_ref(),
        account_id.as_deref(),
        &api_key,
        &websocket_request_id,
    );
    let body_json = Value::Object(body.clone()).to_string();
    let http_timeout_ms = normalize_timeout_ms(options.request.timeout_ms)?;
    let websocket_connect_timeout_ms = normalize_timeout_ms(
        options
            .websocket_connect_timeout_ms
            .or(Some(DEFAULT_WEBSOCKET_CONNECT_TIMEOUT_MS)),
    )?;
    let transport = options.transport.unwrap_or(Transport::Auto);
    let start_emitted = Arc::new(AtomicBool::new(false));
    let websocket_disabled_for_session = transport != Transport::Sse
        && is_web_socket_sse_fallback_active(cache_session_id.as_deref());
    if websocket_disabled_for_session {
        record_web_socket_sse_fallback(cache_session_id.as_deref());
    }

    if transport != Transport::Sse && !websocket_disabled_for_session {
        let websocket_url = resolve_codex_web_socket_url(Some(model.base_url.as_str()));
        let websocket_account_id = account_id.clone().unwrap_or_else(|| api_key.clone());
        let signal = options.request.signal.clone();
        let mut retried_web_socket_connection_limit = false;
        let mut retried_missing_web_socket_continuation = false;
        loop {
            let on_start = WebSocketStart {
                started: Arc::new(AtomicBool::new(false)),
                start_emitted: start_emitted.clone(),
            };
            let result = process_web_socket_stream(
                &websocket_url,
                &body,
                &websocket_headers,
                output,
                stream,
                model,
                &on_start,
                http_timeout_ms,
                websocket_connect_timeout_ms,
                cache_session_id.as_deref(),
                &websocket_account_id,
                &grammar_tool_input_properties,
                options,
                codex_options,
            )
            .await
            .and_then(|()| {
                if signal.as_ref().is_some_and(AbortSignal::aborted) {
                    return Err(CodexError::Other(String::from("Request was aborted")));
                }
                assert_successful_output(output)
            });
            let error = match result {
                Ok(()) => {
                    stream.push(AssistantMessageEvent::Done {
                        reason: get_done_reason(output.stop_reason),
                        message: output.clone(),
                    });
                    stream.end(None);
                    return Ok(());
                }
                Err(error) => error,
            };
            let websocket_started = on_start.started.load(Ordering::SeqCst);
            let aborted = signal.as_ref().is_some_and(AbortSignal::aborted);
            let connection_limit_before_start =
                !websocket_started && is_web_socket_connection_limit_reached_error(&error);
            let previous_response_not_found = is_previous_response_not_found_error(&error);
            if !aborted && previous_response_not_found && !retried_missing_web_socket_continuation {
                retried_missing_web_socket_continuation = true;
                continue;
            }
            if !aborted && connection_limit_before_start && !retried_web_socket_connection_limit {
                retried_web_socket_connection_limit = true;
                continue;
            }
            if aborted || (is_codex_non_transport_error(&error) && !connection_limit_before_start) {
                return Err(error);
            }
            let transport_name = match transport {
                Transport::Sse => "sse",
                Transport::Websocket => "websocket",
                Transport::WebsocketCached => "websocket-cached",
                Transport::Auto => "auto",
            };
            let mut details = Map::new();
            details.insert(String::from("configuredTransport"), json!(transport_name));
            if !websocket_started {
                details.insert(String::from("fallbackTransport"), json!("sse"));
            }
            details.insert(String::from("eventsEmitted"), json!(websocket_started));
            details.insert(
                String::from("phase"),
                json!(if websocket_started {
                    "after_message_stream_start"
                } else {
                    "before_message_stream_start"
                }),
            );
            details.insert(String::from("requestBytes"), json!(body_json.len()));
            let thrown = Thrown::error("Error", error.message());
            append_assistant_message_diagnostic(
                &mut output.diagnostics,
                create_assistant_message_diagnostic(
                    "provider_transport_failure",
                    &thrown,
                    Some(details),
                ),
            );
            record_web_socket_failure(cache_session_id.as_deref(), &thrown);
            if websocket_started {
                return Err(error);
            }
            record_web_socket_sse_fallback(cache_session_id.as_deref());
            break;
        }
    }

    let mut sse_headers = sse_headers;
    let compressed_body =
        zstd::bulk::compress(body_json.as_bytes(), REQUEST_COMPRESSION_ZSTD_LEVEL).ok();
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
            Some(timeout_ms) if timeout_ms > 0 => {
                match tokio::time::timeout(Duration::from_millis(timeout_ms), request.send()).await
                {
                    Ok(result) => result,
                    Err(_) => {
                        let message =
                            format!("Codex SSE response headers timed out after {timeout_ms}ms");
                        last_error = Some(CodexError::Other(message.clone()));
                        if attempt < max_retries {
                            if sleep(BASE_DELAY_MS * 2u64.pow(attempt), signal.as_ref())
                                .await
                                .is_err()
                            {
                                return Err(CodexError::Other(String::from("Request was aborted")));
                            }
                            continue;
                        }
                        return Err(CodexError::Other(message));
                    }
                }
            }
            _ => request.send().await,
        };
        let response_value = match send {
            Ok(response) => response,
            Err(error) => {
                let message = error.to_string();
                if signal.as_ref().is_some_and(|signal| signal.aborted()) {
                    return Err(CodexError::Other(String::from("Request was aborted")));
                }
                let error =
                    CodexError::from(super::openai_responses::responses_transport_error(error));
                last_error = Some(error.clone());
                if attempt < max_retries && !message.contains("usage limit") {
                    let delay = BASE_DELAY_MS * 2u64.pow(attempt);
                    if sleep(delay, signal.as_ref()).await.is_err() {
                        return Err(CodexError::Other(String::from("Request was aborted")));
                    }
                    continue;
                }
                return Err(error);
            }
        };

        options
            .request
            .apply_response_hook(
                &ProviderResponse {
                    status: response_value.status().as_u16(),
                    headers: headers_to_record(response_value.headers()),
                },
                model,
            )
            .await
            .map_err(CodexError::Other)?;

        if response_value.status().is_success() {
            response = Some(response_value);
            break;
        }

        let status = response_value.status().as_u16();
        let headers = response_value.headers().clone();
        let error_text = response_value.text().await.unwrap_or_default();
        if attempt < max_retries && is_retryable_error(status, &error_text) {
            let retry_after_delay_ms = extract_429_retry_after_ms(
                &RetryHintInput {
                    status: Some(429),
                    headers: Some(&headers),
                    body_text: "",
                },
                None,
            );
            let delay_ms = match retry_after_delay_ms {
                Some(delay_ms) => {
                    validate_retry_delay_ms(delay_ms, options.request.max_retry_delay_ms)?
                }
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
                &RetryHintInput {
                    status: Some(429),
                    headers: Some(&headers),
                    body_text: &error_text,
                },
                None,
            );
            if let Some(hint_ms) = hint_ms {
                error_message = append_retry_after_ms_marker(&error_message, hint_ms);
            }
        }
        // senpi throws this into the attempt's `catch`, which retries any error but a retry-delay
        // rejection or a usage-limit message while attempts remain.
        let error = CodexError::from(super::openai_responses::ResponsesApiError::Provider {
            message: error_message.clone(),
            diagnostic: provider_diagnostic::codex_http_provider_diagnostic(
                status,
                &error_text,
                now_ms(),
            ),
            error_cause: serde_json::from_str::<Value>(&error_text)
                .ok()
                .and_then(|payload| {
                    crate::api::github_copilot_errors::usage_limit_cause(
                        &model.provider,
                        Some(status),
                        &payload,
                    )
                }),
        });
        last_error = Some(error.clone());
        if attempt < max_retries && !error_message.contains("usage limit") {
            if sleep(BASE_DELAY_MS * 2u64.pow(attempt), signal.as_ref())
                .await
                .is_err()
            {
                return Err(CodexError::Other(String::from("Request was aborted")));
            }
            continue;
        }
        return Err(error);
    }

    let Some(response) = response else {
        return Err(
            last_error.unwrap_or_else(|| CodexError::Other(String::from("Failed after retries")))
        );
    };

    if !start_emitted.swap(true, Ordering::SeqCst) {
        stream.push(AssistantMessageEvent::Start {
            partial: output.clone(),
        });
    }
    let adapter_state = Arc::new(Mutex::new(CodexStreamAdapterState::default()));
    let provenance: super::openai_responses::ResponsesFailureSlot = Arc::default();
    let adapter = CodexEventStream {
        body: super::openai_responses::observe_responses_transport(
            Box::pin(response.bytes_stream()),
            provenance.clone(),
        ),
        buffer: String::new(),
        done: false,
        state: adapter_state.clone(),
    };
    let service_tier = codex_options.service_tier.as_deref();
    let stream_options = ResponsesStreamOptions {
        service_tier,
        on_provider_stream_event: options.on_provider_stream_event.as_ref(),
        async_on_provider_stream_event: options.async_on_provider_stream_event.as_ref(),
        resolve_service_tier: Some(&resolve_codex_service_tier),
        grammar_tool_input_properties: &grammar_tool_input_properties,
        apply_service_tier_pricing: Some(&|usage: &mut Usage, service_tier: Option<&str>| {
            apply_codex_service_tier_pricing(usage, service_tier, model)
        }),
    };
    let processed =
        process_responses_stream(Box::pin(adapter), output, stream, model, &stream_options).await;
    let adapter_error = adapter_state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .error
        .clone();
    if let Some(error) = adapter_error {
        return Err(match error {
            CodexError::Other(message) => CodexError::from(
                super::openai_responses::responses_stream_error(message, &provenance),
            ),
            CodexError::Transport {
                message,
                diagnostic: None,
            } => CodexError::from(super::openai_responses::responses_stream_error(
                message,
                &provenance,
            )),
            error => error,
        });
    }
    processed.map_err(|error: ResponsesStreamError| CodexError::Other(error.message))?;

    if signal.as_ref().is_some_and(|signal| signal.aborted()) {
        return Err(CodexError::Other(String::from("Request was aborted")));
    }
    assert_successful_output(output)?;
    stream.push(AssistantMessageEvent::Done {
        reason: get_done_reason(output.stop_reason),
        message: output.clone(),
    });
    stream.end(None);
    Ok(())
}

fn assert_successful_output(output: &AssistantMessage) -> Result<(), CodexError> {
    if output.stop_reason == StopReason::Pending {
        return Err(CodexError::Other(String::from(
            "Codex stream ended without a stop reason",
        )));
    }
    if output.stop_reason == StopReason::Error || output.stop_reason == StopReason::Aborted {
        return Err(CodexError::Other(
            output
                .error_message
                .clone()
                .unwrap_or_else(|| String::from("An unknown error occurred")),
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
    [
        "ratelimit",
        "rate limit",
        "rate_limit",
        "overloaded",
        "service unavailable",
        "serviceunavailable",
        "upstream connect",
        "connection refused",
    ]
    .iter()
    .any(|needle| {
        lowered
            .replace([' ', '.', '-', '_'], "")
            .contains(&needle.replace([' ', '.', '-', '_'], ""))
    })
}

/// `validateRetryDelayMs`.
fn validate_retry_delay_ms(
    delay_ms: u64,
    max_retry_delay_ms: Option<u64>,
) -> Result<u64, CodexError> {
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
pub fn stream_simple(
    model: &Model,
    context: &Context,
    options: Option<SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    let resolved_context = crate::types::transcript::resolve_provider_context(context, false);
    let context = &resolved_context;
    let simple = options.unwrap_or_default();
    let api_key = simple
        .stream
        .request
        .api_key
        .clone()
        .filter(|key| !key.is_empty());
    let Some(api_key) = api_key else {
        return error_stream(
            model,
            &format!("No API key for provider: {}", model.provider),
        );
    };
    let mut stream_options = match build_base_options(model, context, Some(&simple), Some(&api_key))
    {
        Ok(stream_options) => stream_options,
        Err(error) => return error_stream(model, &error.to_string()),
    };
    if let Some(tool_choice) = simple.tool_choice {
        stream_options.extra.insert(
            String::from("toolChoice"),
            serde_json::to_value(tool_choice).unwrap_or(Value::Null),
        );
    }
    if let Some(service_tier) = simple.service_tier {
        let tier = match service_tier {
            crate::types::ServiceTierPreference::Auto => "auto",
            crate::types::ServiceTierPreference::Flex => "flex",
            crate::types::ServiceTierPreference::Priority => "priority",
            crate::types::ServiceTierPreference::Ultrafast => "ultrafast",
        };
        stream_options
            .extra
            .insert(String::from("serviceTier"), json!(tier));
    }
    let clamped_reasoning = simple
        .reasoning
        .map(|reasoning| clamp_thinking_level(model, reasoning.into()));
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
        stream_options
            .extra
            .insert(String::from("reasoningEffort"), json!(reasoning_effort));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::openai_responses::connect_web_socket;
    use futures::{SinkExt, StreamExt};
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_async;
    use tokio_tungstenite::tungstenite::Message;

    #[test]
    fn websocket_url_converts_http_schemes() {
        assert_eq!(
            resolve_codex_web_socket_url(Some("https://example.test")),
            "wss://example.test/codex/responses"
        );
        assert_eq!(
            resolve_codex_web_socket_url(Some("http://example.test/codex")),
            "ws://example.test/codex/responses"
        );
    }

    #[test]
    fn codex_failed_event_retains_structured_usage_payload() {
        // Given the backend's structured terminal error after any partial content.
        let state = Arc::new(Mutex::new(CodexStreamAdapterState::default()));
        let frame = format!(
            "data: {}",
            json!({"type":"response.failed","response":{"error":{"code":"usage_limit_reached","message":"opaque"}}})
        );
        // When the wire adapter maps the failure.
        assert!(map_codex_frame(&frame, &state).is_err());
        let error = state.lock().expect("adapter").error.clone().expect("error");
        // Then structured scope can be projected without consulting the formatted message.
        let CodexError::Api(error) = error else {
            panic!("API error");
        };
        assert_eq!(
            crate::api::github_copilot_errors::usage_limit_cause(
                "chatgpt-subscription",
                None,
                error.payload.as_ref().expect("payload")
            ),
            Some(crate::types::AssistantErrorCause::UsageLimit {
                scope: crate::types::UsageLimitScope::Account
            })
        );
    }

    #[test]
    fn websocket_headers_use_websocket_beta_and_affinity() {
        let headers = build_web_socket_headers(None, None, None, "token", "request-id");
        assert_eq!(
            headers
                .get("openai-beta")
                .and_then(|value| value.to_str().ok()),
            Some("responses_websockets=2026-02-06")
        );
        assert!(headers.get("accept").is_none());
        assert!(headers.get("content-type").is_none());
    }

    #[tokio::test]
    async fn websocket_event_parser_maps_completion_frames() {
        let frames = vec![Bytes::from(format!(
            "data: {}\n\n",
            json!({"type":"response.completed","response":{"status":"completed"}})
        ))];
        let state = Arc::new(Mutex::new(CodexStreamAdapterState::default()));
        let adapter = CodexEventStream {
            body: Box::pin(futures::stream::iter(frames.into_iter().map(Ok))),
            buffer: String::new(),
            done: false,
            state,
        };
        let collected: Vec<_> = adapter.collect().await;
        assert_eq!(collected.len(), 1);
        assert!(
            String::from_utf8_lossy(collected[0].as_ref().expect("frame"))
                .contains("response.completed")
        );
    }

    async fn serve_codex_socket(frames: Vec<String>) -> (String, tokio::task::JoinHandle<Value>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind websocket server");
        let address = listener.local_addr().expect("websocket server address");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept websocket");
            let mut socket = accept_async(stream).await.expect("websocket handshake");
            let request = match socket
                .next()
                .await
                .expect("request frame")
                .expect("request frame is valid")
            {
                Message::Text(text) => {
                    serde_json::from_str::<Value>(text.as_str()).expect("request JSON")
                }
                other => panic!("unexpected request frame {other:?}"),
            };
            for frame in frames {
                socket
                    .send(Message::Text(frame.into()))
                    .await
                    .expect("send frame");
            }
            socket.close(None).await.expect("send closing handshake");
            request
        });
        (format!("ws://{address}"), server)
    }

    async fn read_live(
        socket: Arc<TungsteniteWebSocket>,
        request: &str,
    ) -> (Vec<String>, Option<WebSocketStreamFailure>) {
        let events = socket
            .begin_request(request.to_owned())
            .await
            .expect("send request");
        let failed: WebSocketFailureSlot = Arc::default();
        let body = parse_web_socket(
            socket,
            events,
            None,
            ParseWebSocketOptions {
                completion_types: &["response.completed", "response.done", "response.incomplete"],
                invalid_json_prefix: Some("Invalid Codex WebSocket JSON"),
                idle_timeout_ms: Some(5_000),
            },
            failed.clone(),
        );
        let frames: Vec<String> =
            tokio::time::timeout(Duration::from_secs(10), body.collect::<Vec<_>>())
                .await
                .expect("bounded websocket read")
                .into_iter()
                .map(|frame| String::from_utf8_lossy(&frame.expect("frame")).into_owned())
                .collect();
        let failure = failed.lock().expect("failure slot").take();
        (frames, failure)
    }

    #[tokio::test]
    async fn live_websocket_stream_yields_each_frame_until_completion() {
        let (url, server) = serve_codex_socket(vec![
            json!({"type":"response.output_text.delta","delta":"hi"}).to_string(),
            json!({"type":"response.completed","response":{"status":"completed"}}).to_string(),
        ])
        .await;
        let socket = connect_web_socket(
            &url,
            &BTreeMap::new(),
            None,
            Some(DEFAULT_WEBSOCKET_CONNECT_TIMEOUT_MS),
        )
        .await
        .expect("connect");
        let (frames, failure) = read_live(socket, "{\"type\":\"response.create\"}").await;
        assert_eq!(
            server.await.expect("server task")["type"],
            "response.create"
        );
        assert_eq!(failure, None);
        assert_eq!(frames.len(), 2);
        assert!(frames[0].contains("response.output_text.delta"));
        assert!(frames[1].contains("response.completed"));
    }

    #[tokio::test]
    async fn live_websocket_stream_reports_close_before_completion() {
        let (url, server) = serve_codex_socket(vec![
            json!({"type":"response.output_text.delta","delta":"hi"}).to_string(),
        ])
        .await;
        let socket = connect_web_socket(
            &url,
            &BTreeMap::new(),
            None,
            Some(DEFAULT_WEBSOCKET_CONNECT_TIMEOUT_MS),
        )
        .await
        .expect("connect");
        let (frames, failure) = read_live(socket, "{}").await;
        server.await.expect("server task");
        assert_eq!(frames.len(), 1);
        assert!(
            matches!(failure, Some(WebSocketStreamFailure::Close(_))),
            "{failure:?}"
        );
    }

    #[tokio::test]
    async fn codex_error_frames_carry_the_backend_code() {
        let state = Arc::new(Mutex::new(CodexStreamAdapterState::default()));
        let frame = format!(
            "data: {}",
            json!({"type":"error","error":{"code":"previous_response_not_found","message":"gone"}})
        );
        assert!(map_codex_frame(&frame, &state).is_err());
        let error = state
            .lock()
            .expect("state")
            .error
            .clone()
            .expect("recorded error");
        assert!(is_previous_response_not_found_error(&error));
        assert!(is_codex_non_transport_error(&error));
    }
}
