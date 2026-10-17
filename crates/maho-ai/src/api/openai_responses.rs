//! Port of senpi packages/ai/src/api/openai-responses.ts.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use bytes::Bytes;
use futures::{SinkExt, Stream, StreamExt};
use serde_json::{json, Map, Value};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Message as WebSocketMessage};

use crate::api::cloudflare::{is_cloudflare_provider, resolve_cloudflare_base_url};
use crate::api::constrained_sampling::create_grammar_tool_input_properties;
use crate::api::github_copilot_headers::{build_copilot_dynamic_headers, has_copilot_vision_input};
use crate::api::openai_client_auth::resolve_openai_client_auth;
use crate::api::openai_prompt_cache::clamp_openai_prompt_cache_key;
use crate::api::openai_responses_shared::{
    convert_responses_messages, convert_responses_tools, get_done_reason, process_responses_stream,
    ConvertResponsesMessagesOptions, ConvertResponsesToolsOptions, ResponsesDeferredToolsMode,
    ResponsesStreamOptions,
};
use crate::api::simple_options::{
    apply_extra_body, build_base_options, clamp_max_for_openai, OPENAI_RESPONSES_RESERVED_BODY_KEYS,
};
use crate::api::websocket_liveness::{
    start_websocket_liveness, LivenessCapableSocket, WebSocketLivenessMonitor, DEFAULT_WEBSOCKET_LIVENESS_POLICY,
};
use crate::api::websocket_transport_failure::{
    create_web_socket_transport_failure, WebSocketCloseEvent, WebSocketErrorEvent, WebSocketTransportFailure,
    WEBSOCKET_ERROR_CLOSE_GRACE_MS,
};
use crate::models::{clamp_thinking_level, infer_openai_thinking_level_map, supports_max, supports_xhigh};
use crate::openai_responses_compat::SessionAffinityFormat;
use crate::types::{
    AssistantMessage, AssistantMessageEvent, CacheRetention, Context, ErrorReason, Model, ModelThinkingLevel,
    ProviderEnv, ProviderHeaders, ProviderResponse, ServiceTierPreference, SimpleStreamOptions, StopReason,
    StreamOptions, ThinkingLevel, Transport, Usage,
};
use crate::utils::abort::AbortSignal;
use crate::utils::deferred_tools::split_deferred_tools;
use crate::utils::error_body::{
    format_provider_error, normalize_provider_error, SdkErrorShape, ThrownProviderError,
};
use crate::utils::event_stream::{create_assistant_message_event_stream, AssistantMessageEventStream};
use crate::utils::headers::headers_to_record;
use crate::utils::lazy::error_stream;
use crate::utils::pi_user_agent::get_pi_user_agent;
use crate::utils::provider_env::get_provider_env_value;
use crate::utils::provider_retry::{
    retry_provider_request, ProviderErrorStatus, ProviderRequestError, ProviderRetryError, ProviderRetryOptions,
};

pub const OPENAI_TOOL_CALL_PROVIDERS: [&str; 3] = ["openai", "chatgpt-subscription", "opencode"];
pub const OPENAI_BETA_RESPONSES_WEBSOCKETS: &str = "responses_websockets=2026-02-06";
pub const OPENAI_WEB_SEARCH_SOURCES_INCLUDE: &str = "web_search_call.action.sources";
pub const SESSION_WEBSOCKET_CACHE_TTL_MS: u64 = 5 * 60 * 1000;
pub const OPENAI_RESPONSES_MIN_OUTPUT_TOKENS: u64 = 16;
const DEFAULT_TIMEOUT_MS: u64 = 60_000;

/// `resolveCacheRetention`.
pub fn resolve_cache_retention(cache_retention: Option<CacheRetention>, env: Option<&ProviderEnv>) -> CacheRetention {
    if let Some(cache_retention) = cache_retention {
        return cache_retention;
    }
    if get_provider_env_value("PI_CACHE_RETENTION", env).as_deref() == Some("long") {
        return CacheRetention::Long;
    }
    CacheRetention::Short
}

/// `detectSessionAffinityFormat`.
pub fn detect_session_affinity_format(model: &Model) -> SessionAffinityFormat {
    if model.provider == "openrouter" || model.base_url.contains("openrouter.ai") {
        SessionAffinityFormat::Openrouter
    } else {
        SessionAffinityFormat::Openai
    }
}

/// `Required<OpenAIResponsesCompat>` as `getCompat` resolves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponsesCompat {
    pub supports_developer_role: bool,
    pub session_affinity_format: SessionAffinityFormat,
    pub supports_long_cache_retention: bool,
    pub supports_web_socket: bool,
    pub supports_remote_compaction_v2: bool,
    pub supports_web_search_preview: bool,
    pub web_search_tool_type: String,
    pub supports_image_generation: bool,
    pub supports_strict_mode: bool,
    pub supports_openai_grammar_tools: bool,
    pub supports_additional_tools: bool,
    pub supports_tool_search: bool,
    pub supports_explicit_prompt_cache_mode: bool,
    pub supports_max_output_tokens: bool,
}

/// `isOpenAIResponsesNativeEndpoint`.
pub fn is_openai_responses_native_endpoint(model: &Model, env: Option<&ProviderEnv>) -> bool {
    let base_url = if is_cloudflare_provider(&model.provider) {
        resolve_cloudflare_base_url(model, env)
    } else {
        model.base_url.clone()
    };
    let base_url = if base_url.is_empty() { String::from("https://api.openai.com/v1") } else { base_url };
    url::Url::parse(&base_url).ok().and_then(|url| url.host_str().map(str::to_owned)).as_deref()
        == Some("api.openai.com")
}

/// `getCompat`.
pub fn get_compat(model: &Model, env: Option<&ProviderEnv>) -> ResponsesCompat {
    let compat = model.compat.as_ref().map(|compat| compat.openai_responses()).unwrap_or_default();
    let is_native_endpoint = is_openai_responses_native_endpoint(model, env);
    ResponsesCompat {
        supports_developer_role: compat.supports_developer_role.unwrap_or(true),
        session_affinity_format: compat
            .session_affinity_format
            .unwrap_or_else(|| detect_session_affinity_format(model)),
        supports_long_cache_retention: compat.supports_long_cache_retention.unwrap_or(true),
        supports_web_socket: compat.supports_web_socket.unwrap_or(is_native_endpoint),
        supports_remote_compaction_v2: compat.supports_remote_compaction_v2.unwrap_or(is_native_endpoint),
        supports_web_search_preview: compat.supports_web_search_preview.unwrap_or(is_native_endpoint),
        web_search_tool_type: compat.web_search_tool_type.unwrap_or_else(|| "web_search_preview".to_owned()),
        supports_image_generation: compat.supports_image_generation.unwrap_or(is_native_endpoint),
        supports_strict_mode: compat.supports_strict_mode.unwrap_or(false),
        supports_openai_grammar_tools: compat.supports_openai_grammar_tools.unwrap_or(false),
        supports_additional_tools: compat.supports_additional_tools.unwrap_or(false),
        supports_tool_search: compat.supports_tool_search.unwrap_or(false),
        supports_explicit_prompt_cache_mode: compat.supports_explicit_prompt_cache_mode.unwrap_or(false),
        supports_max_output_tokens: compat.supports_max_output_tokens.unwrap_or(true),
    }
}

/// `getPromptCacheRetention`.
pub fn get_prompt_cache_retention(
    compat: &ResponsesCompat,
    cache_retention: CacheRetention,
) -> Option<&'static str> {
    (cache_retention == CacheRetention::Long
        && compat.supports_long_cache_retention
        && !compat.supports_explicit_prompt_cache_mode)
        .then_some("24h")
}

/// `getPromptCacheOptions`.
pub fn get_prompt_cache_options(compat: &ResponsesCompat, cache_retention: CacheRetention) -> Option<Value> {
    if !compat.supports_explicit_prompt_cache_mode {
        return None;
    }
    if cache_retention == CacheRetention::None {
        return Some(json!({ "mode": "explicit" }));
    }
    if cache_retention == CacheRetention::Long && compat.supports_long_cache_retention {
        return Some(json!({ "ttl": "30m" }));
    }
    None
}

fn is_open_ai_web_search_preview_tool(value: &Value) -> bool {
    value.as_object().is_some_and(|object| {
        matches!(
            object.get("type").and_then(Value::as_str),
            Some("web_search_preview") | Some("web_search_preview_2025_03_11") | Some("web_search")
        )
    })
}

/// `sanitizeUnsupportedNativeTools`.
pub fn sanitize_unsupported_native_tools(params: Map<String, Value>, compat: &ResponsesCompat) -> Map<String, Value> {
    if compat.supports_web_search_preview {
        return params;
    }
    let mut sanitized = params;

    if let Some(Value::Array(tools)) = sanitized.get("tools")
        && tools.len() != tools.iter().filter(|tool| !is_open_ai_web_search_preview_tool(tool)).count()
    {
        let tools: Vec<Value> =
            tools.iter().filter(|tool| !is_open_ai_web_search_preview_tool(tool)).cloned().collect();
        if tools.is_empty() {
            sanitized.remove("tools");
        } else {
            sanitized.insert("tools".into(), Value::Array(tools));
        }
    }
    if let Some(Value::Array(include)) = sanitized.get("include")
        && include.iter().any(|value| value.as_str() == Some(OPENAI_WEB_SEARCH_SOURCES_INCLUDE))
    {
        let include: Vec<Value> = include
            .iter()
            .filter(|value| value.as_str() != Some(OPENAI_WEB_SEARCH_SOURCES_INCLUDE))
            .cloned()
            .collect();
        if include.is_empty() {
            sanitized.remove("include");
        } else {
            sanitized.insert("include".into(), Value::Array(include));
        }
    }
    if sanitized.get("tool_choice").is_some_and(is_open_ai_web_search_preview_tool) {
        sanitized.remove("tool_choice");
    }
    sanitized
}

/// The api's thrown-error shape: a senpi `Error` (optionally an SDK HTTP error).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResponsesApiError {
    Message(String),
    Http { status_code: u16, body: String },
}

impl std::fmt::Display for ResponsesApiError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResponsesApiError::Message(message) => write!(formatter, "{message}"),
            ResponsesApiError::Http { status_code, body } => {
                if body.trim().is_empty() {
                    write!(formatter, "{status_code} status code (no body)")
                } else {
                    write!(formatter, "{body}")
                }
            }
        }
    }
}

impl ProviderRequestError for ResponsesApiError {
    fn provider_status(&self) -> Option<ProviderErrorStatus<'_>> {
        match self {
            ResponsesApiError::Http { status_code, .. } => {
                Some(ProviderErrorStatus { status: Some(*status_code), headers: None })
            }
            ResponsesApiError::Message(_) => None,
        }
    }
}

/// `formatOpenAIResponsesError`.
pub fn format_openai_responses_error(error: &ResponsesApiError) -> String {
    let thrown = match error {
        ResponsesApiError::Http { status_code, body } => {
            let shape = SdkErrorShape {
                message: error.to_string(),
                status_code: Some(json!(status_code)),
                body: (!body.trim().is_empty()).then(|| json!(body)),
                ..SdkErrorShape::default()
            };
            ThrownProviderError::Error(Box::new(shape))
        }
        ResponsesApiError::Message(message) => {
            ThrownProviderError::Error(Box::new(SdkErrorShape { message: message.clone(), ..SdkErrorShape::default() }))
        }
    };
    format_provider_error(&normalize_provider_error(&thrown), Some("OpenAI API error"))
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

fn option_str(options: &StreamOptions, key: &str) -> Option<String> {
    match options.extra.get(key) {
        Some(Value::String(value)) => Some(value.clone()),
        _ => None,
    }
}

/// `OpenAIResponsesOptions.reasoningSummary`, which distinguishes an explicit `null` from absent.
fn reasoning_summary(options: &StreamOptions) -> Option<Option<String>> {
    // An empty string is kept: senpi's `||` treats it as absent for the summary default but as
    // falsy for the `medium` fallback, so the two consumers must see the raw value.
    match options.extra.get("reasoningSummary") {
        None => None,
        Some(Value::Null) => Some(None),
        Some(Value::String(value)) => Some(Some(value.clone())),
        Some(_) => Some(None),
    }
}

fn responses_base_url(model: &Model, env: Option<&ProviderEnv>) -> String {
    let base_url = if is_cloudflare_provider(&model.provider) {
        resolve_cloudflare_base_url(model, env)
    } else {
        model.base_url.clone()
    };
    if base_url.is_empty() { String::from("https://api.openai.com/v1") } else { base_url }
}

/// The request headers the OpenAI client is built with (`createClient`).
pub fn build_responses_headers(
    model: &Model,
    context: &Context,
    api_key: &str,
    options_headers: Option<&ProviderHeaders>,
    session_id: Option<&str>,
    compat: &ResponsesCompat,
) -> reqwest::header::HeaderMap {
    let mut headers: BTreeMap<String, Option<String>> = BTreeMap::new();
    headers.insert("User-Agent".into(), Some(get_pi_user_agent()));
    for (name, value) in model.headers.iter().flatten() {
        headers.insert(name.clone(), Some(value.clone()));
    }
    if model.provider == "github-copilot" {
        let has_images = has_copilot_vision_input(&context.messages);
        for (name, value) in build_copilot_dynamic_headers(&context.messages, has_images) {
            headers.insert(name, Some(value));
        }
    }
    if let Some(session_id) = session_id {
        match compat.session_affinity_format {
            SessionAffinityFormat::Openrouter => {
                headers.insert("x-session-id".into(), Some(session_id.to_owned()));
            }
            SessionAffinityFormat::Openai => {
                headers.insert("session_id".into(), Some(session_id.to_owned()));
                headers.insert("x-client-request-id".into(), Some(session_id.to_owned()));
            }
            SessionAffinityFormat::OpenaiNosession => {
                headers.insert("x-client-request-id".into(), Some(session_id.to_owned()));
            }
        }
    }
    let mut suppress_default_authorization = false;
    for (name, value) in options_headers.iter().flat_map(|headers| headers.iter()) {
        match value {
            Some(value) => {
                headers.insert(name.clone(), Some(value.clone()));
            }
            None => {
                if name.eq_ignore_ascii_case("authorization") {
                    suppress_default_authorization = true;
                }
                headers.remove(name);
            }
        }
    }
    let has_authorization = headers
        .iter()
        .any(|(name, value)| name.eq_ignore_ascii_case("authorization") && value.is_some());
    if !suppress_default_authorization && !has_authorization {
        headers.insert("Authorization".into(), Some(format!("Bearer {api_key}")));
    }

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
    header_map.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    header_map
}

/// `buildParams`.
pub fn build_params(
    model: &Model,
    context: &Context,
    options: &StreamOptions,
    compat: &ResponsesCompat,
    grammar_tool_input_properties: &BTreeMap<String, String>,
) -> Result<Map<String, Value>, String> {
    let deferred_tools_mode = if compat.supports_additional_tools {
        Some(ResponsesDeferredToolsMode::AdditionalTools)
    } else if compat.supports_tool_search {
        Some(ResponsesDeferredToolsMode::ToolSearch)
    } else {
        None
    };
    let tool_placement = split_deferred_tools(context, deferred_tools_mode.is_some(), None);
    let reasoning_summary = reasoning_summary(options);
    let summary_is_truthy = matches!(&reasoning_summary, Some(Some(value)) if !value.is_empty());
    let requested_reasoning_effort = option_str(options, "reasoningEffort")
        .or_else(|| summary_is_truthy.then(|| String::from("medium")));
    let thinking_level_map = infer_openai_thinking_level_map(model);
    let mapped_reasoning_effort: Option<Option<String>> = requested_reasoning_effort
        .as_deref()
        .and_then(ModelThinkingLevel::parse)
        .and_then(|level| thinking_level_map.as_ref().and_then(|map| map.get(&level).cloned()));
    let reasoning_effort = match mapped_reasoning_effort.clone() {
        Some(value) => value,
        None => requested_reasoning_effort.clone(),
    };
    let reasoning_requested = reasoning_effort.is_some();
    let reasoning_unavailable = matches!(mapped_reasoning_effort, Some(None));

    let messages = convert_responses_messages(
        model,
        context,
        &OPENAI_TOOL_CALL_PROVIDERS.iter().map(|provider| (*provider).to_owned()).collect::<BTreeSet<String>>(),
        &ConvertResponsesMessagesOptions {
            preserve_thinking: Some(reasoning_requested),
            grammar_tool_input_properties: grammar_tool_input_properties.clone(),
            deferred_tools: tool_placement.deferred.clone(),
            deferred_tools_mode,
            tool_options: ConvertResponsesToolsOptions {
                supports_strict_mode: Some(compat.supports_strict_mode),
                supports_openai_grammar_tools: Some(compat.supports_openai_grammar_tools),
                ..ConvertResponsesToolsOptions::default()
            },
            ..ConvertResponsesMessagesOptions::default()
        },
    );

    let cache_retention = resolve_cache_retention(
        options.cache_retention.or(model.cache_retention),
        options.request.env.as_ref(),
    );
    let mut params = Map::new();
    params.insert("model".into(), json!(model.id));
    params.insert("input".into(), Value::Array(messages));
    params.insert("stream".into(), json!(true));
    if cache_retention != CacheRetention::None
        && let Some(key) = clamp_openai_prompt_cache_key(options.session_id.as_deref())
    {
        params.insert("prompt_cache_key".into(), json!(key));
    }
    if let Some(retention) = get_prompt_cache_retention(compat, cache_retention) {
        params.insert("prompt_cache_retention".into(), json!(retention));
    }
    if let Some(cache_options) = get_prompt_cache_options(compat, cache_retention) {
        params.insert("prompt_cache_options".into(), cache_options);
    }
    params.insert("store".into(), json!(false));

    if let Some(max_tokens) = options.max_tokens
        && compat.supports_max_output_tokens
    {
        params.insert("max_output_tokens".into(), json!(max_tokens.max(OPENAI_RESPONSES_MIN_OUTPUT_TOKENS)));
    }
    if let Some(temperature) = options.temperature {
        params.insert("temperature".into(), json!(temperature));
    }
    if let Some(service_tier) = option_str(options, "serviceTier") {
        params.insert("service_tier".into(), json!(service_tier));
    }
    if !tool_placement.immediate.is_empty() {
        let tools = convert_responses_tools(
            &tool_placement.immediate,
            &ConvertResponsesToolsOptions {
                supports_strict_mode: Some(compat.supports_strict_mode),
                supports_openai_grammar_tools: Some(compat.supports_openai_grammar_tools),
                ..ConvertResponsesToolsOptions::default()
            },
        )?;
        params.insert("tools".into(), Value::Array(tools));
    }
    if let Some(tool_choice) = options.extra.get("toolChoice") {
        params.insert("tool_choice".into(), tool_choice.clone());
    }

    if model.reasoning {
        if reasoning_requested {
            let mut reasoning = Map::new();
            reasoning.insert("effort".into(), json!(reasoning_effort.clone().unwrap_or_default()));
            if !matches!(reasoning_summary, Some(None)) {
                let summary = reasoning_summary
                    .clone()
                    .flatten()
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| String::from("auto"));
                reasoning.insert("summary".into(), json!(summary));
            }
            params.insert("reasoning".into(), Value::Object(reasoning));
            params.insert("include".into(), json!(["reasoning.encrypted_content"]));
        } else if !reasoning_unavailable
            && model.provider != "github-copilot"
            && !matches!(thinking_level_map.as_ref().and_then(|map| map.get(&ModelThinkingLevel::Off)), Some(None))
        {
            let off = thinking_level_map
                .as_ref()
                .and_then(|map| map.get(&ModelThinkingLevel::Off).cloned())
                .flatten()
                .unwrap_or_else(|| String::from("none"));
            params.insert("reasoning".into(), json!({ "effort": off }));
        }
        if model.provider == "xai" {
            params.insert("include".into(), json!(["reasoning.encrypted_content"]));
        }
    }

    apply_extra_body(&mut params, options.extra_body.as_ref(), &OPENAI_RESPONSES_RESERVED_BODY_KEYS);
    if let Some(sampling_params) = options.sampling_params.as_ref() {
        for (key, value) in sampling_params {
            params.insert(key.clone(), value.clone());
        }
    }
    Ok(params)
}

/// `getServiceTierCostMultiplier`.
pub fn get_service_tier_cost_multiplier(model: &Model, service_tier: Option<&str>) -> f64 {
    match service_tier {
        // OpenAI prices GPT-6 Astra and GPT-6.1 Sol Ultrafast at 6x Standard on every
        // token class and context tier. Any other model keeps its base rate.
        Some("ultrafast") => {
            let id = model.upstream_model_id.as_deref().unwrap_or(&model.id);
            if ["gpt-6-astra", "gpt-6.1-sol"].contains(&id) { 6.0 } else { 1.0 }
        }
        Some("flex") => 0.5,
        Some("priority") | Some("fast") => if model.id == "gpt-5.5" { 2.5 } else { 2.0 },
        _ => 1.0,
    }
}

/// `applyServiceTierPricing`.
pub fn apply_service_tier_pricing(usage: &mut Usage, service_tier: Option<&str>, model: &Model) {
    let multiplier = get_service_tier_cost_multiplier(model, service_tier);
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

/// `resolveOpenAIResponsesWebSocketUrl`.
pub fn resolve_openai_responses_websocket_url(model: &Model, env: Option<&ProviderEnv>) -> String {
    let base_url = responses_base_url(model, env);
    let Ok(mut url) = url::Url::parse(&base_url) else { return base_url };
    let path = url.path().to_owned();
    if !path.ends_with("/responses") {
        url.set_path(&format!("{}/responses", path.trim_end_matches('/')));
    }
    let scheme = match url.scheme() {
        "https" => "wss",
        "http" => "ws",
        _ => return url.to_string(),
    }
    .to_owned();
    let _ = url.set_scheme(&scheme);
    url.to_string()
}

/// `buildWebSocketHeaders`.
pub fn build_websocket_headers(
    model: &Model,
    context: &Context,
    api_key: &str,
    options_headers: Option<&ProviderHeaders>,
    session_id: Option<&str>,
    env: Option<&ProviderEnv>,
) -> BTreeMap<String, String> {
    let mut headers: BTreeMap<String, String> = BTreeMap::new();
    for (name, value) in model.headers.iter().flatten() {
        headers.insert(name.clone(), value.clone());
    }
    let mut suppress_default_authorization = false;
    if model.provider == "github-copilot" {
        let has_images = has_copilot_vision_input(&context.messages);
        for (name, value) in build_copilot_dynamic_headers(&context.messages, has_images) {
            headers.insert(name, value);
        }
    }
    for (name, value) in options_headers.iter().flat_map(|headers| headers.iter()) {
        match value {
            Some(value) => {
                headers.insert(name.clone(), value.clone());
            }
            None => {
                if name.eq_ignore_ascii_case("authorization") {
                    suppress_default_authorization = true;
                }
                headers.remove(name);
            }
        }
    }
    if !suppress_default_authorization
        && !headers.keys().any(|name| name.eq_ignore_ascii_case("authorization"))
    {
        headers.insert("Authorization".into(), format!("Bearer {api_key}"));
    }
    if let Some(session_id) = session_id {
        let compat = get_compat(model, env);
        if compat.session_affinity_format == SessionAffinityFormat::Openai {
            headers.insert("session_id".into(), session_id.to_owned());
        }
        match compat.session_affinity_format {
            SessionAffinityFormat::Openai | SessionAffinityFormat::OpenaiNosession => {
                headers.insert("x-client-request-id".into(), session_id.to_owned());
            }
            SessionAffinityFormat::Openrouter => {
                headers.insert("x-session-id".into(), session_id.to_owned());
            }
        }
    }
    headers.retain(|name, _| {
        !matches!(
            name.to_ascii_lowercase().as_str(),
            "accept" | "content-type" | "openai-beta"
        )
    });
    headers.insert("openai-beta".into(), OPENAI_BETA_RESPONSES_WEBSOCKETS.to_owned());
    headers
}

pub trait CachedWebSocket: Send + Sync {
    /// `readyState`; `None` mirrors senpi's `undefined` (a socket that never reported one).
    fn ready_state(&self) -> Option<u8>;
    fn close(&self, code: u16, reason: &str);
}

/// `isWebSocketReusable`.
pub fn is_websocket_reusable(socket: &dyn CachedWebSocket) -> bool {
    matches!(socket.ready_state(), None | Some(1))
}

pub(crate) fn close_websocket_silently(socket: &dyn CachedWebSocket, code: u16, reason: &str) {
    // senpi wraps this in `try {} catch {}`; a socket that refuses to close must not abort the
    // expiry sweep.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| socket.close(code, reason)));
}

/// `CachedWebSocketConnection`.
pub struct CachedWebSocketConnection {
    pub socket: Arc<dyn CachedWebSocket>,
    /// The live transport behind `socket` when the entry was opened by [`connect_web_socket`].
    transport: Option<Arc<TungsteniteWebSocket>>,
    busy: AtomicBool,
    /// The armed idle-expiry timer; re-arming aborts the previous one so each release leaves at
    /// most one timer task per entry.
    idle_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl CachedWebSocketConnection {
    /// A freshly acquired connection is busy until its holder releases it.
    pub fn new(socket: Arc<dyn CachedWebSocket>) -> Self {
        Self { socket, transport: None, busy: AtomicBool::new(true), idle_task: Mutex::new(None) }
    }

    /// A freshly connected transport, busy until its holder releases it.
    pub fn from_transport(socket: Arc<TungsteniteWebSocket>) -> Self {
        Self { socket: socket.clone(), transport: Some(socket), busy: AtomicBool::new(true), idle_task: Mutex::new(None) }
    }

    pub fn busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }

    pub fn set_busy(&self, busy: bool) {
        self.busy.store(busy, Ordering::SeqCst);
    }
}

static WEBSOCKET_SESSION_CACHE: LazyLock<Mutex<HashMap<String, Arc<CachedWebSocketConnection>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// `getOpenAIResponsesWebSocketCacheSize` (diagnostics).
pub fn get_openai_responses_websocket_cache_size() -> usize {
    WEBSOCKET_SESSION_CACHE.lock().expect("websocket session cache lock").len()
}

/// The `websocketSessionCache.set` half of senpi's `acquireWebSocket`.
pub fn cache_websocket_session(session_id: &str, entry: Arc<CachedWebSocketConnection>) {
    WEBSOCKET_SESSION_CACHE
        .lock()
        .expect("websocket session cache lock")
        .insert(session_id.to_owned(), entry);
}

fn remove_websocket_session(session_id: &str) {
    WEBSOCKET_SESSION_CACHE.lock().expect("websocket session cache lock").remove(session_id);
}

fn remove_websocket_session_entry(session_id: &str, entry: &Arc<CachedWebSocketConnection>) {
    let mut cache = WEBSOCKET_SESSION_CACHE.lock().expect("websocket session cache lock");
    if cache.get(session_id).is_some_and(|cached| Arc::ptr_eq(cached, entry)) {
        cache.remove(session_id);
    }
}

/// `scheduleSessionWebSocketExpiry`: arms the idle-expiry for a cached session socket. A fire while
/// the entry is busy must not strand the entry: a live busy socket is re-checked on the next tick,
/// while a dead one is dropped immediately because nothing can release it.
pub fn schedule_session_websocket_expiry(session_id: &str, entry: Arc<CachedWebSocketConnection>) {
    let session_id = session_id.to_owned();
    let armed = entry.clone();
    let task = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(SESSION_WEBSOCKET_CACHE_TTL_MS)).await;
            if entry.busy() {
                if is_websocket_reusable(entry.socket.as_ref()) {
                    continue;
                }
                close_websocket_silently(entry.socket.as_ref(), 1000, "idle_timeout_dead");
                remove_websocket_session(&session_id);
                return;
            }
            close_websocket_silently(entry.socket.as_ref(), 1000, "idle_timeout");
            remove_websocket_session(&session_id);
            return;
        }
    });
    let previous = armed.idle_task.lock().unwrap_or_else(std::sync::PoisonError::into_inner).replace(task);
    if let Some(previous) = previous { previous.abort(); }
}

/// WHATWG `readyState` values the native socket reports.
pub const WEBSOCKET_OPEN: u8 = 1;
pub const WEBSOCKET_CLOSING: u8 = 2;
pub const WEBSOCKET_CLOSED: u8 = 3;

/// One inbound WebSocket event as senpi's `message`/`ping`/`pong`/`error`/`close` listeners observe it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebSocketEvent {
    /// A decoded text (or binary) message.
    Message(String),
    /// A ping or pong control frame (liveness activity).
    Activity,
    Error(String),
    Close(WebSocketCloseEvent),
}

enum WebSocketCommand {
    Send(String),
    Ping(String),
    Close(u16, String),
}

/// The inbound event queue one request holds while it reads the socket.
pub type WebSocketEventReceiver = tokio::sync::OwnedMutexGuard<tokio::sync::mpsc::UnboundedReceiver<WebSocketEvent>>;

/// The runtime WebSocket senpi drives through listeners: a tokio-tungstenite connection owned by a
/// background task, fed commands and draining frames into an event queue.
pub struct TungsteniteWebSocket {
    commands: tokio::sync::mpsc::UnboundedSender<WebSocketCommand>,
    events: Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<WebSocketEvent>>>,
    ready_state: tokio::sync::watch::Receiver<u8>,
}

type TungsteniteStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

impl TungsteniteWebSocket {
    fn spawn(socket: TungsteniteStream) -> Arc<Self> {
        let (commands, command_rx) = tokio::sync::mpsc::unbounded_channel();
        let (event_tx, events) = tokio::sync::mpsc::unbounded_channel();
        let (state_tx, ready_state) = tokio::sync::watch::channel(WEBSOCKET_OPEN);
        tokio::spawn(run_web_socket(socket, command_rx, event_tx, state_tx));
        Arc::new(Self { commands, events: Arc::new(tokio::sync::Mutex::new(events)), ready_state })
    }

    /// Takes the inbound queue for one request, dropping frames that arrived while the connection was
    /// parked (senpi has no listener attached then), and sends `text`.
    pub async fn begin_request(&self, text: String) -> Result<WebSocketEventReceiver, String> {
        let mut events = self.events.clone().lock_owned().await;
        while events.try_recv().is_ok() {}
        self.commands
            .send(WebSocketCommand::Send(text))
            .map_err(|_| String::from("WebSocket is not open"))?;
        Ok(events)
    }

    /// Resolves once the connection reached `CLOSED`.
    pub async fn closed(&self) {
        let mut ready_state = self.ready_state.clone();
        let _ = ready_state.wait_for(|state| *state == WEBSOCKET_CLOSED).await;
    }
}

impl CachedWebSocket for TungsteniteWebSocket {
    fn ready_state(&self) -> Option<u8> {
        Some(*self.ready_state.borrow())
    }

    fn close(&self, code: u16, reason: &str) {
        let _ = self.commands.send(WebSocketCommand::Close(code, reason.to_owned()));
    }
}

impl LivenessCapableSocket for TungsteniteWebSocket {
    fn has_ping(&self) -> bool {
        true
    }

    fn ping(&self, data: &str) -> Result<(), String> {
        self.commands
            .send(WebSocketCommand::Ping(data.to_owned()))
            .map_err(|_| String::from("WebSocket is not open"))
    }
}

async fn run_web_socket(
    mut socket: TungsteniteStream,
    mut commands: tokio::sync::mpsc::UnboundedReceiver<WebSocketCommand>,
    events: tokio::sync::mpsc::UnboundedSender<WebSocketEvent>,
    ready_state: tokio::sync::watch::Sender<u8>,
) {
    let abnormal_close = WebSocketCloseEvent { code: Some(1006), reason: None, was_clean: Some(false) };
    loop {
        tokio::select! {
            command = commands.recv() => {
                let result = match command {
                    Some(WebSocketCommand::Send(text)) => socket.send(WebSocketMessage::Text(text.into())).await,
                    Some(WebSocketCommand::Ping(data)) => {
                        socket.send(WebSocketMessage::Ping(Bytes::from(data.into_bytes()))).await
                    }
                    Some(WebSocketCommand::Close(code, reason)) => {
                        ready_state.send_replace(WEBSOCKET_CLOSING);
                        let frame = CloseFrame { code: CloseCode::from(code), reason: reason.into() };
                        // A close on an already-closing socket is ignored, as `close()` is in the browser.
                        let _ = socket.close(Some(frame)).await;
                        Ok(())
                    }
                    None => {
                        let _ = socket.close(None).await;
                        ready_state.send_replace(WEBSOCKET_CLOSED);
                        return;
                    }
                };
                if let Err(error) = result {
                    let _ = events.send(WebSocketEvent::Error(error.to_string()));
                }
            }
            message = socket.next() => match message {
                Some(Ok(WebSocketMessage::Text(text))) => {
                    let _ = events.send(WebSocketEvent::Message(text.as_str().to_owned()));
                }
                Some(Ok(WebSocketMessage::Binary(data))) => {
                    let _ = events.send(WebSocketEvent::Message(String::from_utf8_lossy(&data).into_owned()));
                }
                Some(Ok(WebSocketMessage::Ping(_) | WebSocketMessage::Pong(_))) => {
                    let _ = events.send(WebSocketEvent::Activity);
                }
                Some(Ok(WebSocketMessage::Close(frame))) => {
                    ready_state.send_replace(WEBSOCKET_CLOSED);
                    let reason = frame.as_ref().map(|frame| frame.reason.as_str().to_owned()).filter(|reason| !reason.is_empty());
                    let _ = events.send(WebSocketEvent::Close(WebSocketCloseEvent {
                        code: frame.as_ref().map(|frame| u16::from(frame.code)),
                        reason,
                        was_clean: Some(true),
                    }));
                    return;
                }
                Some(Ok(WebSocketMessage::Frame(_))) => {}
                Some(Err(error)) => {
                    ready_state.send_replace(WEBSOCKET_CLOSED);
                    let _ = events.send(WebSocketEvent::Error(error.to_string()));
                    let _ = events.send(WebSocketEvent::Close(abnormal_close.clone()));
                    return;
                }
                None => {
                    ready_state.send_replace(WEBSOCKET_CLOSED);
                    let _ = events.send(WebSocketEvent::Close(abnormal_close.clone()));
                    return;
                }
            }
        }
    }
}

/// Resolves when `signal` aborts; never when there is no signal.
pub(crate) async fn wait_for_abort(signal: Option<&AbortSignal>) {
    match signal {
        Some(signal) => signal.cancelled().await,
        None => std::future::pending::<()>().await,
    }
}

/// `connectWebSocket`: opens the socket with the request headers, honoring the abort signal and an
/// optional connect timeout (`0`/absent disables it).
pub async fn connect_web_socket(
    url: &str,
    headers: &BTreeMap<String, String>,
    signal: Option<&AbortSignal>,
    connect_timeout_ms: Option<u64>,
) -> Result<Arc<TungsteniteWebSocket>, String> {
    let mut request = url.into_client_request().map_err(|error| error.to_string())?;
    for (name, value) in headers {
        if let (Ok(name), Ok(value)) =
            (http::HeaderName::from_bytes(name.as_bytes()), http::HeaderValue::from_str(value))
        {
            request.headers_mut().insert(name, value);
        }
    }
    let connect = async {
        let connect = tokio_tungstenite::connect_async(request);
        match connect_timeout_ms {
            Some(ms) if ms > 0 => match tokio::time::timeout(Duration::from_millis(ms), connect).await {
                Ok(result) => result.map_err(|error| error.to_string()),
                Err(_) => Err(format!("WebSocket connect timeout after {ms}ms")),
            },
            _ => connect.await.map_err(|error| error.to_string()),
        }
    };
    let (socket, _) = tokio::select! {
        biased;
        () = wait_for_abort(signal) => return Err(String::from("Request was aborted")),
        result = connect => result?,
    };
    Ok(TungsteniteWebSocket::spawn(socket))
}

/// A failure `parseWebSocket` throws once its queue drains.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebSocketStreamFailure {
    Aborted,
    Transport(String),
    /// Invalid JSON on a strict (Codex) stream.
    Protocol { message: String, payload: String },
}

impl WebSocketStreamFailure {
    pub fn message(&self) -> String {
        match self {
            WebSocketStreamFailure::Aborted => String::from("Request was aborted"),
            WebSocketStreamFailure::Transport(message) => message.clone(),
            WebSocketStreamFailure::Protocol { message, .. } => message.clone(),
        }
    }
}

/// Where the parsed stream records the failure that ended it (the byte stream itself can only end).
pub type WebSocketFailureSlot = Arc<Mutex<Option<WebSocketStreamFailure>>>;

/// The per-API differences between senpi's two `parseWebSocket` generators.
#[derive(Debug, Clone, Copy)]
pub struct ParseWebSocketOptions {
    /// Event types that complete the response.
    pub completion_types: &'static [&'static str],
    /// `Some(prefix)` turns invalid JSON into a protocol failure (Codex); `None` skips the frame.
    pub invalid_json_prefix: Option<&'static str>,
    /// The per-wait idle timeout (`0`/absent disables it).
    pub idle_timeout_ms: Option<u64>,
}

struct ParseWebSocketState {
    socket: Arc<TungsteniteWebSocket>,
    events: WebSocketEventReceiver,
    failures: tokio::sync::mpsc::UnboundedReceiver<WebSocketStreamFailure>,
    liveness: WebSocketLivenessMonitor,
    transport_failure: WebSocketTransportFailure,
    signal: Option<AbortSignal>,
    options: ParseWebSocketOptions,
    failed: WebSocketFailureSlot,
    saw_completion: bool,
    done: bool,
}

impl ParseWebSocketState {
    fn fail(&mut self, failure: WebSocketStreamFailure) {
        self.done = true;
        let mut failed = self.failed.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if failed.is_none() {
            *failed = Some(failure);
        }
    }
}

impl Drop for ParseWebSocketState {
    fn drop(&mut self) {
        self.liveness.stop();
        self.transport_failure.dispose();
    }
}

enum ParseWebSocketStep {
    Failure(WebSocketStreamFailure),
    Event(Option<WebSocketEvent>),
    Idle,
}

/// `parseWebSocket`: the live event stream for one request, re-encoded as SSE frames for the shared
/// Responses processor. Each frame is yielded as it arrives; a failure (abort, liveness, idle
/// timeout, transport error/close, invalid JSON) ends the stream and is recorded in `failed`.
pub fn parse_web_socket(
    socket: Arc<TungsteniteWebSocket>,
    events: WebSocketEventReceiver,
    signal: Option<AbortSignal>,
    options: ParseWebSocketOptions,
    failed: WebSocketFailureSlot,
) -> Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>> {
    let (failure_tx, failures) = tokio::sync::mpsc::unbounded_channel();
    let liveness_socket = socket.clone();
    let liveness_tx = failure_tx.clone();
    let liveness = start_websocket_liveness(
        socket.clone(),
        move |error| {
            let _ = liveness_tx.send(WebSocketStreamFailure::Transport(error.to_string()));
            close_websocket_silently(liveness_socket.as_ref(), 1000, "liveness_timeout");
        },
        DEFAULT_WEBSOCKET_LIVENESS_POLICY,
    );
    let transport_failure = create_web_socket_transport_failure(
        move |error| {
            let _ = failure_tx.send(WebSocketStreamFailure::Transport(error));
        },
        WEBSOCKET_ERROR_CLOSE_GRACE_MS,
    );
    let state = ParseWebSocketState {
        socket,
        events,
        failures,
        liveness,
        transport_failure,
        signal,
        options,
        failed,
        saw_completion: false,
        done: false,
    };
    Box::pin(futures::stream::unfold(state, |mut state| async move {
        let frame = next_web_socket_frame(&mut state).await?;
        Some((Ok::<Bytes, reqwest::Error>(frame), state))
    }))
}

async fn next_web_socket_frame(state: &mut ParseWebSocketState) -> Option<Bytes> {
    loop {
        if state.done {
            return None;
        }
        if state.signal.as_ref().is_some_and(AbortSignal::aborted) {
            state.fail(WebSocketStreamFailure::Aborted);
            return None;
        }
        let idle_timeout_ms = state.options.idle_timeout_ms.filter(|ms| *ms > 0);
        let idle = async move {
            match idle_timeout_ms {
                Some(ms) => tokio::time::sleep(Duration::from_millis(ms)).await,
                None => std::future::pending::<()>().await,
            }
        };
        let step = tokio::select! {
            biased;
            () = wait_for_abort(state.signal.as_ref()) => ParseWebSocketStep::Failure(WebSocketStreamFailure::Aborted),
            Some(failure) = state.failures.recv() => ParseWebSocketStep::Failure(failure),
            event = state.events.recv() => ParseWebSocketStep::Event(event),
            () = idle => ParseWebSocketStep::Idle,
        };
        match step {
            ParseWebSocketStep::Failure(failure) => {
                state.fail(failure);
                return None;
            }
            ParseWebSocketStep::Idle => {
                let ms = idle_timeout_ms.unwrap_or_default();
                close_websocket_silently(state.socket.as_ref(), 1000, "idle_timeout");
                state.fail(WebSocketStreamFailure::Transport(format!("WebSocket idle timeout after {ms}ms")));
                return None;
            }
            ParseWebSocketStep::Event(None) => {
                // The connection task is gone without a close event: report it as an abnormal close.
                state.transport_failure.on_close(&WebSocketCloseEvent {
                    code: Some(1006),
                    reason: None,
                    was_clean: Some(false),
                });
                let failure = state.failures.try_recv().unwrap_or_else(|_| {
                    WebSocketStreamFailure::Transport(String::from("WebSocket stream closed before response.completed"))
                });
                state.fail(failure);
                return None;
            }
            ParseWebSocketStep::Event(Some(WebSocketEvent::Message(text))) => {
                state.liveness.note_activity();
                if text.is_empty() {
                    continue;
                }
                match serde_json::from_str::<Value>(&text) {
                    Ok(parsed) => {
                        let event_type = parsed.get("type").and_then(Value::as_str).unwrap_or_default();
                        if state.options.completion_types.contains(&event_type) {
                            state.saw_completion = true;
                            state.done = true;
                        }
                        return Some(Bytes::from(format!("data: {parsed}\n\n")));
                    }
                    Err(error) => {
                        if let Some(prefix) = state.options.invalid_json_prefix {
                            state.fail(WebSocketStreamFailure::Protocol {
                                message: format!("{prefix}: {error}"),
                                payload: text,
                            });
                            return None;
                        }
                    }
                }
            }
            ParseWebSocketStep::Event(Some(WebSocketEvent::Activity)) => {
                state.liveness.note_activity();
            }
            ParseWebSocketStep::Event(Some(WebSocketEvent::Error(message))) => {
                state
                    .transport_failure
                    .on_error(&WebSocketErrorEvent { message: Some(message), nested_message: None });
            }
            ParseWebSocketStep::Event(Some(WebSocketEvent::Close(close))) => {
                if state.saw_completion {
                    state.transport_failure.dispose();
                    state.done = true;
                    return None;
                }
                state.transport_failure.on_close(&close);
            }
        }
    }
}

/// The connection `acquireWebSocket` hands out, plus how to give it back.
struct ResponsesWebSocketLease {
    socket: Arc<TungsteniteWebSocket>,
    entry: Option<(String, Arc<CachedWebSocketConnection>)>,
}

impl ResponsesWebSocketLease {
    fn release(self, keep: bool) {
        let Some((session_id, entry)) = self.entry else {
            close_websocket_silently(self.socket.as_ref(), 1000, "done");
            return;
        };
        if !keep || !is_websocket_reusable(entry.socket.as_ref()) {
            close_websocket_silently(entry.socket.as_ref(), 1000, "done");
            remove_websocket_session_entry(&session_id, &entry);
            return;
        }
        entry.set_busy(false);
        schedule_session_websocket_expiry(&session_id, entry);
    }
}

/// `acquireWebSocket`.
async fn acquire_web_socket(
    url: &str,
    headers: &BTreeMap<String, String>,
    session_id: Option<&str>,
    signal: Option<&AbortSignal>,
) -> Result<ResponsesWebSocketLease, String> {
    let Some(session_id) = session_id else {
        let socket = connect_web_socket(url, headers, signal, None).await?;
        return Ok(ResponsesWebSocketLease { socket, entry: None });
    };

    let cached = WEBSOCKET_SESSION_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(session_id)
        .cloned();
    if let Some(cached) = cached {
        if !cached.busy()
            && is_websocket_reusable(cached.socket.as_ref())
            && let Some(socket) = cached.transport.clone()
        {
            cached.set_busy(true);
            return Ok(ResponsesWebSocketLease { socket, entry: Some((session_id.to_owned(), cached)) });
        }
        if !cached.busy() {
            close_websocket_silently(cached.socket.as_ref(), 1000, "done");
            remove_websocket_session_entry(session_id, &cached);
        }
    }

    let socket = connect_web_socket(url, headers, signal, None).await?;
    let entry = Arc::new(CachedWebSocketConnection::from_transport(socket.clone()));
    cache_websocket_session(session_id, entry.clone());
    Ok(ResponsesWebSocketLease { socket, entry: Some((session_id.to_owned(), entry)) })
}

/// `processWebSocketStream`. `websocket_started` is senpi's `onStart` flag: set once the request
/// frame went out, after which a failure must not fall back to HTTP.
#[allow(clippy::too_many_arguments)]
async fn process_web_socket_stream(
    url: &str,
    params: &Map<String, Value>,
    headers: &BTreeMap<String, String>,
    output: &mut AssistantMessage,
    sink: &AssistantMessageEventStream,
    model: &Model,
    websocket_started: &mut bool,
    cache_session_id: Option<&str>,
    grammar_tool_input_properties: &BTreeMap<String, String>,
    options: &StreamOptions,
) -> Result<(), ResponsesApiError> {
    let signal = options.request.signal.clone();
    let lease = acquire_web_socket(url, headers, cache_session_id, signal.as_ref())
        .await
        .map_err(ResponsesApiError::Message)?;
    let result = async {
        let mut frame = Map::new();
        frame.insert("type".into(), json!("response.create"));
        frame.extend(params.clone());
        let events = lease
            .socket
            .begin_request(Value::Object(frame).to_string())
            .await
            .map_err(ResponsesApiError::Message)?;
        *websocket_started = true;
        options
            .request
            .apply_response_hook(&ProviderResponse { status: 101, headers: BTreeMap::new() }, model)
            .await
            .map_err(ResponsesApiError::Message)?;
        sink.push(AssistantMessageEvent::Start { partial: output.clone() });
        let failed: WebSocketFailureSlot = Arc::default();
        let body = parse_web_socket(
            lease.socket.clone(),
            events,
            signal.clone(),
            ParseWebSocketOptions {
                completion_types: &["response.completed", "response.incomplete"],
                invalid_json_prefix: None,
                idle_timeout_ms: None,
            },
            failed.clone(),
        );
        let service_tier = option_str(options, "serviceTier");
        let stream_options = ResponsesStreamOptions {
            service_tier: service_tier.as_deref(),
            grammar_tool_input_properties,
            apply_service_tier_pricing: Some(&|usage: &mut Usage, service_tier: Option<&str>| {
                apply_service_tier_pricing(usage, service_tier, model)
            }),
        };
        let processed = process_responses_stream(body, output, sink, model, &stream_options).await;
        let failure = failed.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take();
        if let Some(failure) = failure {
            return Err(ResponsesApiError::Message(failure.message()));
        }
        processed.map_err(|error| ResponsesApiError::Message(error.message))
    }
    .await;
    lease.release(false);
    result
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

async fn run_stream(model: Model, context: Context, options: StreamOptions, sink: AssistantMessageEventStream) {
    let mut output = create_output(&model);
    if let Err(error) = drive(&model, &context, &options, &mut output, &sink).await {
        output.stop_reason = if options.request.signal.as_ref().is_some_and(|signal| signal.aborted()) {
            StopReason::Aborted
        } else {
            StopReason::Error
        };
        output.error_message = Some(format_openai_responses_error(&error));
        sink.push(AssistantMessageEvent::Error {
            reason: if output.stop_reason == StopReason::Aborted { ErrorReason::Aborted } else { ErrorReason::Error },
            error: output.clone(),
        });
        sink.end(None);
    }
}

async fn drive(
    model: &Model,
    context: &Context,
    options: &StreamOptions,
    output: &mut AssistantMessage,
    sink: &AssistantMessageEventStream,
) -> Result<(), ResponsesApiError> {
    let env = options.request.env.as_ref();
    let client_auth = resolve_openai_client_auth(
        &model.provider,
        options.request.api_key.as_deref(),
        options.request.headers.as_ref(),
    )
    .map_err(ResponsesApiError::Message)?;
    let cache_retention = resolve_cache_retention(options.cache_retention, env);
    let cache_session_id = (cache_retention != CacheRetention::None)
        .then(|| options.session_id.clone())
        .flatten();
    let compat = get_compat(model, env);
    let grammar_tool_input_properties = create_grammar_tool_input_properties(
        context.tools.as_deref(),
        compat.supports_openai_grammar_tools,
    )
    .map_err(ResponsesApiError::Message)?;

    let mut params = build_params(model, context, options, &compat, &grammar_tool_input_properties)
        .map_err(ResponsesApiError::Message)?;
    if let Some(next) = options.request.apply_payload_hook(&Value::Object(params.clone()), model, None)
        .await.map_err(ResponsesApiError::Message)?
    {
        params = next.as_object().cloned().unwrap_or_default();
    }
    params = sanitize_unsupported_native_tools(params, &compat);
    let transport = options.transport.unwrap_or(Transport::Sse);
    if transport != Transport::Sse && compat.supports_web_socket {
        let mut websocket_started = false;
        let websocket_headers = build_websocket_headers(
            model,
            context,
            &client_auth.api_key,
            client_auth.headers.as_ref(),
            cache_session_id.as_deref(),
            env,
        );
        let result = process_web_socket_stream(
            &resolve_openai_responses_websocket_url(model, env),
            &params,
            &websocket_headers,
            output,
            sink,
            model,
            &mut websocket_started,
            cache_session_id.as_deref(),
            &grammar_tool_input_properties,
            options,
        )
        .await
        .and_then(|()| {
            if options.request.signal.as_ref().is_some_and(|signal| signal.aborted()) {
                return Err(ResponsesApiError::Message(String::from("Request was aborted")));
            }
            Ok(())
        });
        match result {
            Ok(()) => {
                sink.push(AssistantMessageEvent::Done {
                    reason: get_done_reason(output.stop_reason),
                    message: output.clone(),
                });
                sink.end(None);
                return Ok(());
            }
            Err(error) => {
                if transport == Transport::Websocket || websocket_started {
                    return Err(error);
                }
            }
        }
    }

    let base_url = responses_base_url(model, env);
    let url = format!("{}/responses", base_url.trim_end_matches('/'));
    let headers = build_responses_headers(
        model,
        context,
        &client_auth.api_key,
        client_auth.headers.as_ref(),
        cache_session_id.as_deref(),
        &compat,
    );
    let client = options.request.fetch.clone().unwrap_or_default();
    let body = Value::Object(params).to_string();
    let timeout_ms = options.request.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
    let signal = options.request.signal.clone();

    let attempt_client = client.clone();
    let attempt_url = url.clone();
    let attempt_headers = headers.clone();
    let attempt_body = body.clone();
    let attempt = move || {
        let client = attempt_client.clone();
        let url = attempt_url.clone();
        let headers = attempt_headers.clone();
        let body = attempt_body.clone();
        async move {
            let response = tokio::time::timeout(
                std::time::Duration::from_millis(timeout_ms),
                client.post(&url).headers(headers).body(body).send(),
            )
            .await
            .map_err(|_| ResponsesApiError::Message(String::from("Request was aborted")))?
            .map_err(|error| ResponsesApiError::Message(error.to_string()))?;
            if !response.status().is_success() {
                let status_code = response.status().as_u16();
                let body = response.text().await.unwrap_or_default();
                return Err(ResponsesApiError::Http { status_code, body });
            }
            Ok(response)
        }
    };

    let retry_options = ProviderRetryOptions {
        max_retries: options.request.max_retries,
        max_retry_delay_ms: options.request.max_retry_delay_ms,
        signal: signal.clone(),
    };
    let response = match retry_provider_request(attempt, &retry_options).await {
        Ok(response) => response,
        Err(ProviderRetryError::Request(error)) => return Err(error),
        Err(ProviderRetryError::RetryDelay { message, .. }) => return Err(ResponsesApiError::Message(message)),
        Err(ProviderRetryError::Aborted) => return Err(ResponsesApiError::Message(String::from("Request was aborted"))),
    };

    options.request.apply_response_hook(
            &ProviderResponse { status: response.status().as_u16(), headers: headers_to_record(response.headers()) },
            model,
        ).await.map_err(ResponsesApiError::Message)?;
    sink.push(AssistantMessageEvent::Start { partial: output.clone() });

    let service_tier = option_str(options, "serviceTier");
    let stream_options = ResponsesStreamOptions {
        service_tier: service_tier.as_deref(),
        grammar_tool_input_properties: &grammar_tool_input_properties,
        apply_service_tier_pricing: Some(&|usage: &mut Usage, service_tier: Option<&str>| {
            apply_service_tier_pricing(usage, service_tier, model)
        }),
    };
    process_responses_stream(Box::pin(response.bytes_stream()), output, sink, model, &stream_options)
        .await
        .map_err(|error| ResponsesApiError::Message(error.message))?;

    if signal.as_ref().is_some_and(|signal| signal.aborted()) {
        return Err(ResponsesApiError::Message(String::from("Request was aborted")));
    }
    if output.stop_reason == StopReason::Pending {
        return Err(ResponsesApiError::Message(String::from(
            "OpenAI Responses stream ended without a stop reason",
        )));
    }
    if output.stop_reason == StopReason::Aborted || output.stop_reason == StopReason::Error {
        return Err(ResponsesApiError::Message(
            output.error_message.clone().unwrap_or_else(|| String::from("An unknown error occurred")),
        ));
    }
    sink.push(AssistantMessageEvent::Done { reason: get_done_reason(output.stop_reason), message: output.clone() });
    sink.end(None);
    Ok(())
}

/// `streamSimple`.
pub fn stream_simple(
    model: &Model,
    context: &Context,
    options: Option<SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    let simple = options.unwrap_or_default();
    if let Err(error) = resolve_openai_client_auth(
        &model.provider,
        simple.stream.request.api_key.as_deref(),
        simple.stream.request.headers.as_ref(),
    ) {
        return error_stream(model, &error);
    }
    let api_key = simple.stream.request.api_key.clone();
    let mut stream_options = match build_base_options(model, context, Some(&simple), api_key.as_deref()) {
        Ok(stream_options) => stream_options,
        Err(error) => return error_stream(model, &error.to_string()),
    };
    if let Some(tool_choice) = simple.tool_choice {
        stream_options.extra.insert("toolChoice".into(), serde_json::to_value(tool_choice).unwrap_or(Value::Null));
    }
    if let Some(service_tier) = simple.service_tier {
        let tier = match service_tier {
            ServiceTierPreference::Auto => "auto",
            ServiceTierPreference::Flex => "flex",
            ServiceTierPreference::Priority => "priority",
            ServiceTierPreference::Ultrafast => "ultrafast",
        };
        stream_options.extra.insert("serviceTier".into(), json!(tier));
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
        stream_options.extra.insert("reasoningEffort".into(), json!(reasoning_effort));
    }
    stream(model, context, Some(stream_options))
}
