//! Port of senpi packages/ai/src/api/openai-completions.ts.
//!
//! `ChatTransport` is this lane's port of the SDK boundary senpi's own tests replace
//! (`vi.mock("openai")`): `ReqwestTransport` is the production HTTP implementation, and tests
//! inject a recording/failing transport exactly where the TS tests inject the mocked SDK.

mod auth_headers;
mod client_auth;
mod constrained_sampling;
mod context_room;
mod copilot_headers;
mod lazy;
mod prompt_cache;
mod simple_options;
mod transform_messages;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};

use futures::StreamExt;
use regex::Regex;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::{json, Map, Value};

use crate::models::{calculate_cost, clamp_thinking_level, infer_openai_thinking_level_map, supports_max, supports_xhigh};
use crate::types::{
    AssistantMessage, AssistantMessageEvent, AssistantMessageEventStream, CacheRetention, Context, ContentBlock,
    DoneReason, ErrorReason, Message, Model, ModelThinkingLevel, ProviderEnv, ProviderHeaders, ProviderStreams,
    SimpleStreamOptions, StopReason, StreamOptions, TextContent, ThinkingBudgets, ThinkingContent,
    ThinkingLevelMap, ThinkingTokenBudgetField, Tool, ToolCall, Usage, UserContent,
};
use crate::utils::abort::AbortSignal;
use crate::utils::diagnostics::now_ms;
use crate::utils::error_body::{
    format_provider_error, normalize_provider_error, safe_json_stringify, SdkErrorShape, ThrownProviderError,
};
use crate::utils::hash::short_hash;
use crate::utils::headers::headers_to_record;
use crate::utils::json_parse::parse_streaming_json;
use crate::utils::pi_user_agent::get_pi_user_agent;
use crate::utils::provider_env::get_provider_env_value;
use crate::utils::provider_retry::{
    retry_provider_stream_request, ProviderErrorStatus, ProviderRequestError, ProviderRetryError, ProviderRetryOptions,
};
use crate::utils::sanitize_unicode::sanitize_surrogates;
use crate::utils::tool_choice_fallback::{is_forced_tool_choice_unsupported_error, omit_tool_choice_param, HttpFailure};
use crate::utils::tool_schema_compat::{normalize_tool_parameters_for_moonshot, normalize_tool_parameters_for_openai_compat};

pub use crate::utils::prompt_cache_ttl::{
    get_openai_completions_compat as get_compat, ResolvedOpenAICompletionsCompat,
};

use client_auth::resolve_openai_client_auth;
use constrained_sampling::{
    append_grammar_tool_input_json_delta, create_grammar_tool_input_properties, get_grammar_tool_input,
    get_json_schema_tool_parameters, resolve_grammar_constrained_sampling, resolve_json_schema_strict_sampling,
    GrammarToolInputJsonBuffer,
};
use copilot_headers::{build_copilot_dynamic_headers, has_copilot_vision_input};
use prompt_cache::clamp_openai_prompt_cache_key;
use simple_options::{
    apply_extra_body, build_base_options, clamp_max_for_openai, clamp_thinking_budget_to_answer_room,
    thinking_budget_for_level, OPENAI_COMPLETIONS_RESERVED_BODY_KEYS,
};
use transform_messages::{transform_messages, TransformMessagesOptions};

const KIMI_K3_THINKING_LEVEL_MAP: [(ModelThinkingLevel, Option<&str>); 7] = [
    (ModelThinkingLevel::Off, None),
    (ModelThinkingLevel::Minimal, None),
    (ModelThinkingLevel::Low, Some("low")),
    (ModelThinkingLevel::Medium, None),
    (ModelThinkingLevel::High, Some("high")),
    (ModelThinkingLevel::Xhigh, None),
    (ModelThinkingLevel::Max, Some("max")),
];

const DEEPSEEK_THINKING_LEVEL_MAP: [(ModelThinkingLevel, Option<&str>); 6] = [
    (ModelThinkingLevel::Minimal, Some("high")),
    (ModelThinkingLevel::Low, Some("high")),
    (ModelThinkingLevel::Medium, Some("high")),
    (ModelThinkingLevel::High, Some("high")),
    (ModelThinkingLevel::Xhigh, Some("max")),
    (ModelThinkingLevel::Max, Some("max")),
];

const OPENROUTER_DEEPSEEK_THINKING_LEVEL_MAP: [(ModelThinkingLevel, Option<&str>); 6] = [
    (ModelThinkingLevel::Minimal, Some("high")),
    (ModelThinkingLevel::Low, Some("high")),
    (ModelThinkingLevel::Medium, Some("high")),
    (ModelThinkingLevel::High, Some("high")),
    (ModelThinkingLevel::Xhigh, Some("high")),
    (ModelThinkingLevel::Max, Some("high")),
];

const MIMO_THINKING_LEVEL_MAP: [(ModelThinkingLevel, Option<&str>); 6] = [
    (ModelThinkingLevel::Minimal, Some("low")),
    (ModelThinkingLevel::Low, Some("low")),
    (ModelThinkingLevel::Medium, Some("medium")),
    (ModelThinkingLevel::High, Some("high")),
    (ModelThinkingLevel::Xhigh, Some("high")),
    (ModelThinkingLevel::Max, None),
];

const OLLAMA_THINKING_LEVEL_MAP: [(ModelThinkingLevel, Option<&str>); 7] = [
    (ModelThinkingLevel::Off, Some("none")),
    (ModelThinkingLevel::Minimal, None),
    (ModelThinkingLevel::Low, Some("low")),
    (ModelThinkingLevel::Medium, Some("medium")),
    (ModelThinkingLevel::High, Some("high")),
    (ModelThinkingLevel::Xhigh, None),
    (ModelThinkingLevel::Max, Some("high")),
];

fn to_thinking_level_map(entries: &[(ModelThinkingLevel, Option<&str>)]) -> ThinkingLevelMap {
    entries
        .iter()
        .map(|(level, value)| (*level, value.map(str::to_owned)))
        .collect()
}

static KIMI_K3_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|[/:-])kimi-k3(?:$|[/.:_-])").expect("kimi k3 pattern"));
static MIMO_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bmimo\b").expect("mimo pattern"));
static GLM_5X_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|[/:-])glm-5\.[23](?:$|[/.:_-])").expect("glm pattern"));
static GLM_53_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|[/:-])glm-5\.3(?:-(?:flash|highspeed))?(?:$|[/.:_])").expect("glm 5.3 pattern")
});
static PIPELINE_SEPARATORS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^a-zA-Z0-9_-]").expect("id sanitize pattern"));

#[derive(Default)]
pub struct OpenAiCompletionsOptions {
    pub stream: StreamOptions,
    pub tool_choice: Option<Value>,
    pub reasoning_effort: Option<ModelThinkingLevel>,
    pub thinking_budgets: Option<ThinkingBudgets>,
}


#[derive(Debug, Clone, Default)]
pub struct ConvertCompletionsMessagesOptions {
    pub preserve_thinking: Option<bool>,
    pub grammar_tool_input_properties: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone)]
pub struct OpenAiCompletionsError {
    pub message: String,
    pub status: Option<u16>,
    pub headers: Option<Box<HeaderMap>>,
    pub body: Option<Value>,
    pub provider_error: bool,
}

impl OpenAiCompletionsError {
    pub fn protocol(message: impl Into<String>) -> Self {
        Self { message: message.into(), status: None, headers: None, body: None, provider_error: false }
    }

    pub fn transport(message: impl Into<String>) -> Self {
        let message = message.into();
        let provider_error = message == crate::utils::provider_retry::DIGITALOCEAN_STREAM_FAILURE_MESSAGE;
        Self { message, status: None, headers: None, body: None, provider_error }
    }

    pub fn http(message: impl Into<String>, status: u16, headers: HeaderMap) -> Self {
        Self { message: message.into(), status: Some(status), headers: Some(Box::new(headers)), body: None, provider_error: true }
    }

    fn as_thrown(&self) -> ThrownProviderError {
        ThrownProviderError::Error(Box::new(SdkErrorShape {
            message: self.message.clone(),
            status: self.status.map(|status| json!(status)),
            error: self.body.clone().map(crate::utils::error_body::SdkFieldValue::Plain),
            ..SdkErrorShape::default()
        }))
    }
}

impl std::fmt::Display for OpenAiCompletionsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl ProviderRequestError for OpenAiCompletionsError {
    fn provider_status(&self) -> Option<ProviderErrorStatus<'_>> {
        if !self.provider_error {
            return None;
        }
        Some(ProviderErrorStatus { status: self.status, headers: self.headers.as_deref() })
    }
}

pub struct ChatStreamResponse {
    pub stream: futures::stream::BoxStream<'static, Result<Value, OpenAiCompletionsError>>,
    pub status: u16,
    pub headers: BTreeMap<String, String>,
}

use std::collections::BTreeMap;

pub struct ChatRequest {
    pub url: String,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
    pub timeout_ms: Option<u64>,
    pub signal: Option<AbortSignal>,
}

pub trait ChatTransport: Send + Sync {
    fn create_chat_completion(
        &self,
        request: ChatRequest,
    ) -> crate::types::BoxFuture<'static, Result<ChatStreamResponse, OpenAiCompletionsError>>;
}

pub struct ReqwestTransport {
    client: reqwest::Client,
}

struct Abort;

impl ReqwestTransport {
    pub fn new(client: reqwest::Client) -> Self {
        Self { client }
    }
}

impl ChatTransport for ReqwestTransport {
    fn create_chat_completion(
        &self,
        request: ChatRequest,
    ) -> crate::types::BoxFuture<'static, Result<ChatStreamResponse, OpenAiCompletionsError>> {
        let client = self.client.clone();
        Box::pin(async move {
            let builder = client.post(&request.url).headers(request.headers).body(request.body);
            let send = builder.send();
            let with_abort = async {
                match &request.signal {
                    Some(signal) => tokio::select! {
                        biased;
                        () = signal.cancelled() => Err(Abort),
                        result = send => result.map_err(|_| Abort),
                    },
                    None => send.await.map_err(|_| Abort),
                }
            };
            let response = match request.timeout_ms {
                Some(timeout_ms) => {
                    match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), with_abort).await {
                        Ok(response) => response,
                        Err(_) => return Err(OpenAiCompletionsError::protocol("Request timed out.")),
                    }
                }
                None => with_abort.await,
            };
            let response = match response {
                Ok(response) => response,
                Err(Abort) => {
                    if request.signal.as_ref().is_some_and(AbortSignal::aborted) {
                        return Err(OpenAiCompletionsError::protocol("Request was aborted."));
                    }
                    return Err(OpenAiCompletionsError::protocol("Request timed out."));
                }
            };
            let status = response.status().as_u16();
            let headers = response.headers().clone();
            if !response.status().is_success() {
                let body = response.text().await.unwrap_or_default();
                return Err(http_error(status, &headers, &body));
            }
            Ok(ChatStreamResponse {
                stream: sse_chunks(response),
                status,
                headers: headers_to_record(&headers),
            })
        })
    }
}

fn http_error(status: u16, headers: &HeaderMap, body: &str) -> OpenAiCompletionsError {
    let parsed: Option<Value> = serde_json::from_str(body).ok();
    let error_value = parsed.as_ref().and_then(|value| value.get("error")).cloned();
    let message = error_value
        .as_ref()
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{status} status code (no body)"));
    let mut error = OpenAiCompletionsError::http(message, status, headers.clone());
    error.provider_error = true;
    if let Some(value) = error_value.filter(|value| value.as_object().is_some_and(|map| !map.is_empty())) {
        error.body = Some(value);
    }
    error
}

struct SseState {
    response: reqwest::Response,
    buffer: Vec<u8>,
    pending: String,
    done: bool,
    failed: bool,
}

fn sse_chunks(response: reqwest::Response) -> futures::stream::BoxStream<'static, Result<Value, OpenAiCompletionsError>> {
    futures::stream::unfold(
        SseState { response, buffer: Vec::new(), pending: String::new(), done: false, failed: false },
        |mut state| async move {
            if state.done {
                return None;
            }
            loop {
                if let Some(index) = state.buffer.iter().position(|byte| *byte == b'\n') {
                    let line: Vec<u8> = state.buffer.drain(..=index).collect();
                    let line = String::from_utf8_lossy(&line[..line.len() - 1]).trim_end_matches('\r').to_owned();
                    if line.is_empty() {
                        if state.pending.is_empty() {
                            continue;
                        }
                        let data = std::mem::take(&mut state.pending);
                        if data == "[DONE]" {
                            return None;
                        }
                        return Some((parse_chunk(&data), state));
                    }
                    if let Some(rest) = line.strip_prefix("data:") {
                        if !state.pending.is_empty() {
                            state.pending.push('\n');
                        }
                        state.pending.push_str(rest.trim_start());
                    }
                    continue;
                }
                if state.failed {
                    return None;
                }
                match state.response.chunk().await {
                    Ok(Some(chunk)) => state.buffer.extend_from_slice(&chunk),
                    Ok(None) => {
                        state.failed = true;
                        if state.pending.is_empty() {
                            return None;
                        }
                        let data = std::mem::take(&mut state.pending);
                        return Some((parse_chunk(&data), state));
                    }
                    Err(error) => {
                        state.failed = true;
                        let item = Err(OpenAiCompletionsError::transport(error.to_string()));
                        return Some((item, state));
                    }
                }
            }
        },
    )
    .boxed()
}
fn parse_chunk(data: &str) -> Result<Value, OpenAiCompletionsError> {
    match serde_json::from_str::<Value>(data) {
        Ok(value) => Ok(value),
        Err(error) => Err(OpenAiCompletionsError::protocol(error.to_string())),
    }
}

fn create_client_headers(
    model: &Model,
    context: &Context,
    api_key: &str,
    options_headers: Option<&ProviderHeaders>,
    session_id: Option<&str>,
    compat: &ResolvedOpenAICompletionsCompat,
) -> HeaderMap {
    let mut headers: ProviderHeaders = ProviderHeaders::new();
    headers.insert("User-Agent".to_owned(), Some(get_pi_user_agent()));
    if let Some(model_headers) = &model.headers {
        for (name, value) in model_headers {
            headers.insert(name.clone(), Some(value.clone()));
        }
    }
    if model.provider == "github-copilot" {
        let has_images = has_copilot_vision_input(&context.messages);
        for (name, value) in build_copilot_dynamic_headers(&context.messages, has_images) {
            headers.insert(name, Some(value));
        }
    }
    if let Some(session_id) = session_id.filter(|_| compat.send_session_affinity_headers) {
        if compat.session_affinity_format == crate::types::SessionAffinityFormat::Openrouter {
            headers.insert("x-session-id".to_owned(), Some(session_id.to_owned()));
        } else {
            if compat.session_affinity_format == crate::types::SessionAffinityFormat::Openai {
                headers.insert("session_id".to_owned(), Some(session_id.to_owned()));
            }
            headers.insert("x-client-request-id".to_owned(), Some(session_id.to_owned()));
            headers.insert("x-session-affinity".to_owned(), Some(session_id.to_owned()));
        }
    }
    if let Some(options_headers) = options_headers {
        for (name, value) in options_headers {
            headers.insert(name.clone(), value.clone());
        }
    }
    if !api_key.is_empty() {
        headers.entry("Authorization".to_owned()).or_insert_with(|| Some(format!("Bearer {api_key}")));
    }

    let mut map = HeaderMap::new();
    for (name, value) in headers {
        let Ok(name) = HeaderName::from_bytes(name.as_bytes()) else { continue };
        let Some(value) = value else {
            map.remove(name);
            continue;
        };
        if let Ok(value) = HeaderValue::from_str(&value) {
            map.insert(name, value);
        }
    }
    map
}

fn get_thinking_level_map(model: &Model, compat: &ResolvedOpenAICompletionsCompat) -> Option<ThinkingLevelMap> {
    if let Some(inferred) = infer_openai_thinking_level_map(model) {
        return Some(inferred);
    }

    let id = model.id.to_lowercase();
    let is_kimi_k3 = id == "k3" || id.starts_with("k3-") || KIMI_K3_RE.is_match(&id);
    let is_deep_seek = id.contains("deepseek");
    let is_mi_mo = MIMO_RE.is_match(&id);
    let is_glm_5x = GLM_5X_RE.is_match(&id);

    if model.provider == "ollama" {
        return Some(to_thinking_level_map(&OLLAMA_THINKING_LEVEL_MAP));
    }
    if is_kimi_k3 {
        return Some(to_thinking_level_map(&KIMI_K3_THINKING_LEVEL_MAP));
    }
    if compat.thinking_format == crate::types::ThinkingFormat::Openrouter && is_deep_seek {
        return Some(to_thinking_level_map(&OPENROUTER_DEEPSEEK_THINKING_LEVEL_MAP));
    }
    if (compat.thinking_format == crate::types::ThinkingFormat::Openai
        || compat.thinking_format == crate::types::ThinkingFormat::Openrouter)
        && is_mi_mo
    {
        return Some(to_thinking_level_map(&MIMO_THINKING_LEVEL_MAP));
    }
    if is_deep_seek {
        return Some(to_thinking_level_map(&DEEPSEEK_THINKING_LEVEL_MAP));
    }
    if is_glm_5x {
        if compat.thinking_format == crate::types::ThinkingFormat::Zai {
            return Some(to_thinking_level_map(&DEEPSEEK_THINKING_LEVEL_MAP));
        }
        if compat.thinking_format == crate::types::ThinkingFormat::Openrouter {
            return Some([(ModelThinkingLevel::Xhigh, Some("xhigh".to_owned()))].into_iter().collect());
        }
        return Some([(ModelThinkingLevel::Max, Some("max".to_owned()))].into_iter().collect());
    }

    None
}

fn resolve_reasoning_effort(thinking_level_map: Option<&ThinkingLevelMap>, level: ModelThinkingLevel) -> Option<String> {
    match thinking_level_map.and_then(|map| map.get(&level)) {
        Some(Some(mapped)) => Some(mapped.clone()),
        Some(None) => None,
        None => Some(level.as_str().to_owned()),
    }
}

fn has_tool_history(messages: &[Message]) -> bool {
    messages.iter().any(|message| match message {
        Message::ToolResult(_) => true,
        Message::Assistant(assistant) => assistant.content.iter().any(|block| matches!(block, ContentBlock::ToolCall(_))),
        _ => false,
    })
}

fn get_deferred_tool_names(messages: &[Message]) -> HashSet<String> {
    let mut names = HashSet::new();
    for message in messages {
        if let Message::ToolResult(result) = message {
            for name in result.added_tool_names.iter().flatten() {
                names.insert(name.clone());
            }
        }
    }
    names
}

fn get_tools_by_name(tools: Option<&[Tool]>, names: &HashSet<String>) -> Vec<Tool> {
    let Some(tools) = tools else { return Vec::new() };
    let by_name: HashMap<&str, &Tool> = tools.iter().map(|tool| (tool.name.as_str(), tool)).collect();
    names.iter().filter_map(|name| by_name.get(name.as_str()).map(|tool| (*tool).clone())).collect()
}

fn get_openrouter_raw_metadata(error: &OpenAiCompletionsError) -> Option<String> {
    let metadata = error.body.as_ref()?.get("metadata")?;
    metadata.get("raw")?.as_str().map(str::to_owned)
}

fn is_reasoning_detail(detail: &Value) -> bool {
    let Some(detail) = detail.as_object() else { return false };
    let valid_common = detail.get("id").is_none_or(|value| value.is_null() || value.is_string())
        && detail.get("format").is_none_or(Value::is_string)
        && detail.get("index").is_none_or(Value::is_number);
    if !valid_common {
        return false;
    }
    match detail.get("type").and_then(Value::as_str) {
        Some("reasoning.summary") => detail.get("summary").is_some_and(Value::is_string),
        Some("reasoning.encrypted") => detail.get("data").is_some_and(Value::is_string),
        Some("reasoning.text") => {
            detail.get("text").is_some_and(Value::is_string)
                && detail.get("signature").is_none_or(|value| value.is_null() || value.is_string())
        }
        _ => false,
    }
}

fn parse_openai_reasoning_details(signature: Option<&str>) -> Option<Vec<Value>> {
    let signature = signature?;
    let parsed: Value = serde_json::from_str(signature).ok()?;
    let array = parsed.as_array()?;
    (!array.is_empty() && array.iter().all(is_reasoning_detail)).then(|| array.clone())
}

fn parse_legacy_encrypted_reasoning_detail(signature: Option<&str>) -> Option<Value> {
    let signature = signature?;
    let parsed: Value = serde_json::from_str(signature).ok()?;
    let detail = parsed.as_object()?;
    (is_reasoning_detail(&parsed)
        && detail.get("type").and_then(Value::as_str) == Some("reasoning.encrypted")
        && detail.get("id").and_then(Value::as_str).is_some_and(|id| !id.is_empty())
        && detail.get("data").and_then(Value::as_str).is_some_and(|data| !data.is_empty()))
    .then_some(parsed)
}

fn fill_missing_common_reasoning_detail_fields(target: &mut Map<String, Value>, source: &Map<String, Value>) {
    if !target.contains_key("id")
        && let Some(id) = source.get("id") {
            target.insert("id".to_owned(), id.clone());
        }
    if target.get("format").is_none_or(Value::is_null)
        && let Some(format) = source.get("format") {
            target.insert("format".to_owned(), format.clone());
        }
    if !target.contains_key("index")
        && let Some(index) = source.get("index") {
            target.insert("index".to_owned(), index.clone());
        }
}

fn append_openai_reasoning_detail(details: &mut Vec<Value>, detail: &Value) {
    let detail_type = detail.get("type").and_then(Value::as_str);
    if detail_type == Some("reasoning.text")
        && let Some(last) = details.last_mut().and_then(Value::as_object_mut)
            && last.get("type").and_then(Value::as_str) == Some("reasoning.text") {
                let appended = format!(
                    "{}{}",
                    last.get("text").and_then(Value::as_str).unwrap_or_default(),
                    detail.get("text").and_then(Value::as_str).unwrap_or_default()
                );
                last.insert("text".to_owned(), Value::String(appended));
                if last.get("signature").is_none_or(Value::is_null)
                    && let Some(signature) = detail.get("signature").filter(|value| !value.is_null()) {
                        last.insert("signature".to_owned(), signature.clone());
                    }
                let source = detail.as_object().cloned().unwrap_or_default();
                fill_missing_common_reasoning_detail_fields(last, &source);
                return;
            }
    if detail_type == Some("reasoning.summary")
        && let Some(last) = details.last_mut().and_then(Value::as_object_mut)
            && last.get("type").and_then(Value::as_str) == Some("reasoning.summary") {
                let appended = format!(
                    "{}{}",
                    last.get("summary").and_then(Value::as_str).unwrap_or_default(),
                    detail.get("summary").and_then(Value::as_str).unwrap_or_default()
                );
                last.insert("summary".to_owned(), Value::String(appended));
                let source = detail.as_object().cloned().unwrap_or_default();
                fill_missing_common_reasoning_detail_fields(last, &source);
                return;
            }
    details.push(detail.clone());
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

fn is_forced_openai_completions_tool_choice(tool_choice: Option<&Value>) -> bool {
    matches!(tool_choice, Some(value) if value != "auto" && value != "none")
}

fn resolve_thinking_token_budget_field(
    compat: &ResolvedOpenAICompletionsCompat,
) -> Option<ThinkingTokenBudgetField> {
    if let Some(field) = compat.thinking_token_budget_field {
        return Some(field);
    }
    if compat.supports_thinking_token_budget == Some(true) {
        return Some(ThinkingTokenBudgetField::ThinkingTokenBudget);
    }
    None
}

fn resolve_clamped_thinking_budget(
    model: &Model,
    options: Option<&OpenAiCompletionsOptions>,
    params: &Map<String, Value>,
) -> Option<u64> {
    let effort = options?.reasoning_effort?;
    if !model.reasoning {
        return None;
    }
    let ceiling = params
        .get("max_tokens")
        .or_else(|| params.get("max_completion_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(model.max_tokens);
    let budget = clamp_thinking_budget_to_answer_room(
        thinking_budget_for_level(effort, options.and_then(|options| options.thinking_budgets.as_ref())),
        ceiling,
    );
    (budget > 0).then_some(budget)
}

fn resolve_chat_template_kwarg_value(
    model: &Model,
    options: Option<&OpenAiCompletionsOptions>,
    compat: &ResolvedOpenAICompletionsCompat,
    value: &Value,
    thinking_budget: Option<u64>,
) -> Option<Value> {
    let Some(object) = value.as_object() else {
        return Some(value.clone());
    };

    let reasoning_effort = options.and_then(|options| options.reasoning_effort);
    if reasoning_effort.is_none() && object.get("omitWhenOff").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    match object.get("$var").and_then(Value::as_str) {
        Some("thinking.enabled") => return Some(Value::Bool(reasoning_effort.is_some())),
        Some("thinking.budget") => return thinking_budget.map(|budget| json!(budget)),
        _ => {}
    }

    let thinking_level_map = get_thinking_level_map(model, compat);
    let mapped = match reasoning_effort {
        Some(effort) => thinking_level_map.as_ref().and_then(|map| map.get(&effort)),
        None => thinking_level_map.as_ref().and_then(|map| map.get(&ModelThinkingLevel::Off)),
    };
    match mapped {
        Some(Some(mapped)) => Some(Value::String(mapped.clone())),
        Some(None) => None,
        None => reasoning_effort.map(|effort| Value::String(effort.as_str().to_owned())),
    }
}

fn build_chat_template_values(
    model: &Model,
    options: Option<&OpenAiCompletionsOptions>,
    compat: &ResolvedOpenAICompletionsCompat,
    values: &Map<String, Value>,
    thinking_budget: Option<u64>,
) -> Option<Map<String, Value>> {
    let mut resolved_values = Map::new();
    for (key, value) in values {
        if let Some(resolved) = resolve_chat_template_kwarg_value(model, options, compat, value, thinking_budget) {
            resolved_values.insert(key.clone(), resolved);
        }
    }
    (!resolved_values.is_empty()).then_some(resolved_values)
}

fn get_compat_cache_control(
    compat: &ResolvedOpenAICompletionsCompat,
    cache_retention: CacheRetention,
) -> Option<Value> {
    if compat.cache_control_format != Some(crate::types::CacheControlFormat::Anthropic)
        || cache_retention == CacheRetention::None
    {
        return None;
    }
    let ttl = (cache_retention == CacheRetention::Long && compat.supports_long_cache_retention).then_some("1h");
    let mut cache_control = Map::new();
    cache_control.insert("type".to_owned(), Value::String("ephemeral".to_owned()));
    if let Some(ttl) = ttl {
        cache_control.insert("ttl".to_owned(), Value::String(ttl.to_owned()));
    }
    Some(Value::Object(cache_control))
}

fn add_cache_control_to_text_content(message: &mut Map<String, Value>, cache_control: &Value) -> bool {
    match message.get("content").cloned() {
        Some(Value::String(content)) => {
            if content.is_empty() {
                return false;
            }
            let mut part = Map::new();
            part.insert("type".to_owned(), Value::String("text".to_owned()));
            part.insert("text".to_owned(), Value::String(content));
            part.insert("cache_control".to_owned(), cache_control.clone());
            message.insert("content".to_owned(), Value::Array(vec![Value::Object(part)]));
            true
        }
        Some(Value::Array(parts)) => {
            let mut parts = parts;
            let index = parts
                .iter()
                .enumerate()
                .rev()
                .find(|(_, part)| part.get("type").and_then(Value::as_str) == Some("text"))
                .map(|(index, _)| index);
            let Some(index) = index else { return false };
            if let Some(Value::Object(part)) = parts.get_mut(index) {
                part.insert("cache_control".to_owned(), cache_control.clone());
            }
            message.insert("content".to_owned(), Value::Array(parts));
            true
        }
        _ => false,
    }
}

fn apply_anthropic_cache_control(messages: &mut [Value], tools: Option<&mut Vec<Value>>, cache_control: &Value) {
    for message in messages.iter_mut() {
        let role = message.get("role").and_then(Value::as_str);
        if matches!(role, Some("system" | "developer")) {
            if let Some(object) = message.as_object_mut() {
                add_cache_control_to_text_content(object, cache_control);
            }
            break;
        }
    }

    if let Some(tools) = tools
        && let Some(last) = tools.last_mut().and_then(Value::as_object_mut) {
            last.insert("cache_control".to_owned(), cache_control.clone());
        }

    for message in messages.iter_mut().rev() {
        let role = message.get("role").and_then(Value::as_str);
        if matches!(role, Some("user" | "assistant" | "tool"))
            && let Some(object) = message.as_object_mut()
                && add_cache_control_to_text_content(object, cache_control) {
                    return;
                }
    }
}

fn normalize_tool_call_id(id: &str) -> String {
    if id.contains('|') {
        let separator_index = id.find('|').unwrap_or(0);
        let call_id = PIPELINE_SEPARATORS_RE.replace_all(&id[..separator_index], "_").into_owned();
        let item_id = PIPELINE_SEPARATORS_RE.replace_all(&id[separator_index + 1..], "_").into_owned();
        let combined_id = if item_id.is_empty() { call_id.clone() } else { format!("{call_id}_{item_id}") };
        if combined_id.len() <= 40 {
            return combined_id;
        }
        let hash = short_hash(id);
        let hash = &hash[..hash.len().min(8)];
        let prefix_len = 40usize.saturating_sub(hash.len()).saturating_sub(1).max(1);
        let prefix = &call_id[..call_id.len().min(prefix_len)];
        return format!("{prefix}_{hash}");
    }

    let sanitized_id = {
        let replaced = PIPELINE_SEPARATORS_RE.replace_all(id, "_").into_owned();
        if replaced.is_empty() { "tool_call".to_owned() } else { replaced }
    };
    if sanitized_id == id && sanitized_id.len() <= 40 {
        return id.to_owned();
    }

    let hash = short_hash(id);
    let hash = &hash[..hash.len().min(8)];
    let prefix_len = 40usize.saturating_sub(hash.len()).saturating_sub(1).max(1);
    let prefix = &sanitized_id[..sanitized_id.len().min(prefix_len)];
    format!("{prefix}_{hash}")
}

pub fn convert_messages(
    model: &Model,
    context: &Context,
    compat: &ResolvedOpenAICompletionsCompat,
    options: &ConvertCompletionsMessagesOptions,
) -> Result<Vec<Value>, String> {
    let mut params: Vec<Value> = Vec::new();

    let normalize = |id: &str, _model: &Model, _source: &AssistantMessage| normalize_tool_call_id(id);
    let transformed_messages = transform_messages(
        &context.messages,
        model,
        Some(&normalize),
        &TransformMessagesOptions {
            preserve_thinking: options.preserve_thinking,
            normalize_same_model_tool_call_ids: Some(true),
            ..TransformMessagesOptions::default()
        },
    );

    if let Some(system_prompt) = &context.system_prompt {
        let use_developer_role = model.reasoning && compat.supports_developer_role;
        let mut message = Map::new();
        message.insert(
            "role".to_owned(),
            Value::String(if use_developer_role { "developer" } else { "system" }.to_owned()),
        );
        message.insert("content".to_owned(), Value::String(sanitize_surrogates(system_prompt)));
        params.push(Value::Object(message));
    }

    let mut last_role: Option<String> = None;
    let mut index = 0usize;

    while index < transformed_messages.len() {
        let message = &transformed_messages[index];
        if compat.requires_assistant_after_tool_result && last_role.as_deref() == Some("toolResult") && message.role() == "user"
        {
            params.push(json!({ "role": "assistant", "content": "I have processed the tool results." }));
        }

        match message {
            Message::User(user) => {
                match &user.content {
                    UserContent::Text(text) => {
                        params.push(json!({ "role": "user", "content": sanitize_surrogates(text) }));
                    }
                    UserContent::Blocks(blocks) => {
                        let mut content: Vec<Value> = Vec::new();
                        for block in blocks {
                            match block {
                                ContentBlock::Text(text) => {
                                    content.push(json!({ "type": "text", "text": sanitize_surrogates(&text.text) }));
                                }
                                ContentBlock::Image(image) => {
                                    content.push(json!({
                                        "type": "image_url",
                                        "image_url": { "url": format!("data:{};base64,{}", image.mime_type, image.data) },
                                    }));
                                }
                                _ => {}
                            }
                        }
                        if content.is_empty() {
                            index += 1;
                            continue;
                        }
                        params.push(json!({ "role": "user", "content": content }));
                    }
                }
                last_role = Some("user".to_owned());
            }
            Message::Assistant(assistant) => {
                let mut assistant_message = Map::new();
                assistant_message.insert("role".to_owned(), Value::String("assistant".to_owned()));
                assistant_message.insert(
                    "content".to_owned(),
                    if compat.requires_assistant_after_tool_result { Value::String(String::new()) } else { Value::Null },
                );

                let text_blocks: Vec<&TextContent> = assistant
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text(text) if !text.text.trim().is_empty() => Some(text),
                        _ => None,
                    })
                    .collect();
                let assistant_text: String = text_blocks.iter().map(|block| sanitize_surrogates(&block.text)).collect();

                let thinking_blocks: Vec<&ThinkingContent> = assistant
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Thinking(thinking) => Some(thinking),
                        _ => None,
                    })
                    .collect();
                let tool_calls: Vec<&ToolCall> = assistant
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::ToolCall(tool_call) => Some(tool_call),
                        _ => None,
                    })
                    .collect();

                let signed_reasoning_details = thinking_blocks
                    .iter()
                    .filter_map(|block| parse_openai_reasoning_details(block.thinking_signature.as_deref()))
                    .next();
                let legacy_reasoning_details: Vec<Value> = tool_calls
                    .iter()
                    .filter_map(|tool_call| parse_legacy_encrypted_reasoning_detail(tool_call.thought_signature.as_deref()))
                    .collect();
                let preserved_reasoning_details = signed_reasoning_details.or_else(|| {
                    (!legacy_reasoning_details.is_empty()).then_some(legacy_reasoning_details)
                });

                let non_empty_thinking_blocks: Vec<&ThinkingContent> = thinking_blocks
                    .iter()
                    .filter(|block| !block.thinking.trim().is_empty())
                    .copied()
                    .collect();
                if !non_empty_thinking_blocks.is_empty() {
                    if compat.requires_thinking_as_text {
                        let thinking_text = non_empty_thinking_blocks
                            .iter()
                            .map(|block| sanitize_surrogates(&block.thinking))
                            .collect::<Vec<_>>()
                            .join("\n\n");
                        let mut content = vec![json!({ "type": "text", "text": thinking_text })];
                        for block in &text_blocks {
                            content.push(json!({ "type": "text", "text": sanitize_surrogates(&block.text) }));
                        }
                        assistant_message.insert("content".to_owned(), Value::Array(content));
                    } else {
                        if !assistant_text.is_empty() {
                            assistant_message.insert("content".to_owned(), Value::String(assistant_text.clone()));
                        }
                        let mut signature = non_empty_thinking_blocks[0].thinking_signature.clone();
                        if model.provider == "opencode-go" && signature.as_deref() == Some("reasoning") {
                            signature = Some("reasoning_content".to_owned());
                        }
                        if let Some(signature) = signature.filter(|signature| !signature.is_empty()) {
                            let thinking = non_empty_thinking_blocks
                                .iter()
                                .map(|block| block.thinking.clone())
                                .collect::<Vec<_>>()
                                .join("\n");
                            assistant_message.insert(signature, Value::String(thinking));
                        }
                    }
                } else if !assistant_text.is_empty() {
                    assistant_message.insert("content".to_owned(), Value::String(assistant_text));
                }

                if !tool_calls.is_empty() {
                    let mut calls = Vec::new();
                    for tool_call in &tool_calls {
                        let custom_input_property = options
                            .grammar_tool_input_properties
                            .as_ref()
                            .and_then(|properties| properties.get(&tool_call.name));
                        if let Some(property) = custom_input_property {
                            calls.push(json!({
                                "id": tool_call.id,
                                "type": "custom",
                                "custom": {
                                    "name": tool_call.name,
                                    "input": sanitize_surrogates(
                                        &get_grammar_tool_input(&tool_call.name, &tool_call.arguments, property)
                                            .unwrap_or_default(),
                                    ),
                                },
                            }));
                            continue;
                        }
                        calls.push(json!({
                            "id": tool_call.id,
                            "type": "function",
                            "function": {
                                "name": tool_call.name,
                                "arguments": safe_json_stringify(&Value::Object(tool_call.arguments.clone())),
                            },
                        }));
                    }
                    assistant_message.insert("tool_calls".to_owned(), Value::Array(calls));
                }
                if let Some(details) = preserved_reasoning_details {
                    assistant_message.insert("reasoning_details".to_owned(), Value::Array(details));
                }
                if compat.requires_reasoning_content_on_assistant_messages
                    && model.reasoning
                    && !assistant_message.contains_key("reasoning_content")
                {
                    assistant_message.insert("reasoning_content".to_owned(), Value::String(String::new()));
                }
                let has_content = match assistant_message.get("content") {
                    Some(Value::String(text)) => !text.is_empty(),
                    Some(Value::Array(items)) => !items.is_empty(),
                    _ => false,
                };
                if !has_content && !assistant_message.contains_key("tool_calls") {
                    index += 1;
                    continue;
                }
                params.push(Value::Object(assistant_message));
                last_role = Some("assistant".to_owned());
            }
            Message::ToolResult(_) => {
                let mut image_blocks: Vec<Value> = Vec::new();
                let mut deferred_tool_names: HashSet<String> = HashSet::new();
                let mut next = index;
                while next < transformed_messages.len() {
                    let Message::ToolResult(tool_message) = &transformed_messages[next] else { break };
                    let text_result = tool_message
                        .content
                        .iter()
                        .filter_map(|block| match block {
                            ContentBlock::Text(text) => Some(text.text.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    let has_images = tool_message.content.iter().any(|block| matches!(block, ContentBlock::Image(_)));
                    let has_text = !text_result.is_empty();
                    let tool_result_text =
                        if has_text { text_result } else if has_images { "(see attached image)".to_owned() } else { "(no tool output)".to_owned() };
                    let mut tool_result_message = Map::new();
                    tool_result_message.insert("role".to_owned(), Value::String("tool".to_owned()));
                    tool_result_message.insert("content".to_owned(), Value::String(sanitize_surrogates(&tool_result_text)));
                    tool_result_message.insert("tool_call_id".to_owned(), Value::String(tool_message.tool_call_id.clone()));
                    if compat.requires_tool_result_name && !tool_message.tool_name.is_empty() {
                        tool_result_message.insert("name".to_owned(), Value::String(tool_message.tool_name.clone()));
                    }
                    params.push(Value::Object(tool_result_message));

                    if compat.deferred_tools_mode == Some(crate::types::DeferredToolsMode::Kimi) {
                        for name in tool_message.added_tool_names.iter().flatten() {
                            deferred_tool_names.insert(name.clone());
                        }
                    }

                    if has_images && model.input.contains(&crate::types::InputModality::Image) {
                        for block in &tool_message.content {
                            if let ContentBlock::Image(image) = block {
                                image_blocks.push(json!({
                                    "type": "image_url",
                                    "image_url": { "url": format!("data:{};base64,{}", image.mime_type, image.data) },
                                }));
                            }
                        }
                    }
                    next += 1;
                }
                index = next - 1;

                if !image_blocks.is_empty() {
                    if compat.requires_assistant_after_tool_result {
                        params.push(json!({ "role": "assistant", "content": "I have processed the tool results." }));
                    }
                    let mut content = vec![json!({ "type": "text", "text": "Attached image(s) from tool result:" })];
                    content.extend(image_blocks);
                    params.push(json!({ "role": "user", "content": content }));
                    last_role = Some("user".to_owned());
                } else {
                    last_role = Some("toolResult".to_owned());
                }

                if !deferred_tool_names.is_empty() {
                    let deferred_tools = get_tools_by_name(context.tools.as_deref(), &deferred_tool_names);
                    if !deferred_tools.is_empty() {
                        params.push(json!({ "role": "system", "tools": convert_tools(&deferred_tools, compat)? }));
                    }
                }
            }
            Message::ConfigurationUpdate(_) => {
                last_role = Some(message.role().to_owned());
            }
        }

        index += 1;
    }

    Ok(params)
}

fn convert_tools(tools: &[Tool], compat: &ResolvedOpenAICompletionsCompat) -> Result<Vec<Value>, String> {
    let mut converted = Vec::with_capacity(tools.len());
    for tool in tools {
        converted.push(convert_tool(tool, compat)?);
    }
    Ok(converted)
}

fn convert_tool(tool: &Tool, compat: &ResolvedOpenAICompletionsCompat) -> Result<Value, String> {
    if let Some(grammar) = resolve_grammar_constrained_sampling(tool, compat.supports_openai_grammar_tools)
        .unwrap_or(None)
    {
        return Ok(json!({
            "type": "custom",
            "custom": {
                "name": tool.name,
                "description": tool.description,
                "format": {
                    "type": "grammar",
                    "grammar": { "syntax": grammar.format, "definition": grammar.definition },
                },
            },
        }));
    }
    if tool.freeform.is_some() {
        return Err("Freeform tools cannot be sent to OpenAI Chat Completions; use Responses API".to_owned());
    }

    let strict = resolve_json_schema_strict_sampling(tool, compat.supports_strict_mode).unwrap_or(None);
    let schema_parameters =
        get_json_schema_tool_parameters(tool, strict).unwrap_or_else(|_| tool.parameters.clone());
    let schema_parameters = schema_parameters.as_object().cloned().unwrap_or_default();
    let normalized_parameters = if compat.tool_schema_flavor == Some(crate::types::ToolSchemaFlavor::MoonshotMfjs) {
        normalize_tool_parameters_for_moonshot(&schema_parameters)
    } else {
        normalize_tool_parameters_for_openai_compat(&schema_parameters)
    };

    let mut function = Map::new();
    function.insert("name".to_owned(), Value::String(tool.name.clone()));
    function.insert("description".to_owned(), Value::String(tool.description.clone()));
    function.insert("parameters".to_owned(), Value::Object(normalized_parameters));
    if compat.supports_strict_mode {
        function.insert("strict".to_owned(), Value::Bool(strict == Some(true)));
    }
    Ok(json!({ "type": "function", "function": Value::Object(function) }))
}


fn normalize_request_tool_schemas(params: Map<String, Value>, compat: &ResolvedOpenAICompletionsCompat) -> Map<String, Value> {
    let Some(tools) = params.get("tools").and_then(Value::as_array).cloned() else { return params };
    let mut next = params;
    let normalized = tools
        .into_iter()
        .map(|tool| {
            let is_function = tool.get("type").and_then(Value::as_str) == Some("function");
            let parameters = tool.get("function").and_then(|function| function.get("parameters"));
            if !is_function || parameters.is_none_or(Value::is_null) {
                return tool;
            }
            let parameters = parameters.and_then(Value::as_object).cloned().unwrap_or_default();
            let normalized_parameters = if compat.tool_schema_flavor == Some(crate::types::ToolSchemaFlavor::MoonshotMfjs) {
                normalize_tool_parameters_for_moonshot(&parameters)
            } else {
                normalize_tool_parameters_for_openai_compat(&parameters)
            };
            let mut function = tool.get("function").and_then(Value::as_object).cloned().unwrap_or_default();
            function.insert("parameters".to_owned(), Value::Object(normalized_parameters));
            let mut next_tool = tool.clone();
            if let Some(object) = next_tool.as_object_mut() {
                object.insert("function".to_owned(), Value::Object(function));
            }
            next_tool
        })
        .collect();
    next.insert("tools".to_owned(), Value::Array(normalized));
    next
}

fn parse_chunk_usage(raw_usage: &Value, model: &Model) -> Usage {
    let prompt_tokens = raw_usage.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0);
    let details = raw_usage.get("prompt_tokens_details");
    let cache_read_tokens = details
        .and_then(|details| details.get("cached_tokens"))
        .and_then(Value::as_u64)
        .or_else(|| raw_usage.get("prompt_cache_hit_tokens").and_then(Value::as_u64))
        .or_else(|| raw_usage.get("cached_tokens").and_then(Value::as_u64))
        .unwrap_or(0);
    let cache_write_tokens = details
        .and_then(|details| details.get("cache_write_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);

    let input = prompt_tokens.saturating_sub(cache_read_tokens).saturating_sub(cache_write_tokens);
    let output_tokens = raw_usage.get("completion_tokens").and_then(Value::as_u64).unwrap_or(0);
    let reasoning = raw_usage
        .get("completion_tokens_details")
        .and_then(|details| details.get("reasoning_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);

    let mut usage = Usage {
        input,
        output: output_tokens,
        cache_read: cache_read_tokens,
        cache_write: cache_write_tokens,
        cache_write_1h: None,
        reasoning: Some(reasoning),
        total_tokens: input + output_tokens + cache_read_tokens + cache_write_tokens,
        cost: Default::default(),
    };
    calculate_cost(model, &mut usage);
    usage
}

fn map_stop_reason(reason: Option<&str>) -> (StopReason, Option<String>) {
    let Some(reason) = reason else { return (StopReason::Stop, None) };
    match reason {
        "stop" | "end" => (StopReason::Stop, None),
        "length" => (StopReason::Length, None),
        "function_call" | "tool_calls" => (StopReason::ToolUse, None),
        "content_filter" => (StopReason::Error, Some("Provider finish_reason: content_filter".to_owned())),
        "network_error" => (StopReason::Error, Some("Provider finish_reason: network_error".to_owned())),
        other => (StopReason::Error, Some(format!("Provider finish_reason: {other}"))),
    }
}

fn build_params(
    model: &Model,
    context: &Context,
    options: Option<&OpenAiCompletionsOptions>,
    compat: &ResolvedOpenAICompletionsCompat,
    cache_retention: CacheRetention,
    grammar_tool_input_properties: &HashMap<String, String>,
) -> Result<Map<String, Value>, String> {
    let messages = convert_messages(
        model,
        context,
        compat,
        &ConvertCompletionsMessagesOptions {
            preserve_thinking: Some(options.is_some_and(|options| options.reasoning_effort.is_some())),
            grammar_tool_input_properties: Some(grammar_tool_input_properties.clone()),
        },
    )?;
    let cache_control = get_compat_cache_control(compat, cache_retention);
    let thinking_level_map = get_thinking_level_map(model, compat);

    let mut params = Map::new();
    params.insert("model".to_owned(), Value::String(model.id.clone()));
    params.insert("messages".to_owned(), Value::Array(messages));
    params.insert("stream".to_owned(), Value::Bool(true));
    if ((model.base_url.contains("api.openai.com") && cache_retention != CacheRetention::None)
        || (cache_retention == CacheRetention::Long && compat.supports_long_cache_retention)
        || (compat.supports_prompt_cache_key == Some(true) && cache_retention != CacheRetention::None))
        && let Some(key) = clamp_openai_prompt_cache_key(options.and_then(|options| options.stream.session_id.as_deref()))
    {
        params.insert("prompt_cache_key".to_owned(), Value::String(key));
    }
    if cache_retention == CacheRetention::Long && compat.supports_long_cache_retention {
        params.insert("prompt_cache_retention".to_owned(), Value::String("24h".to_owned()));
    }
    if compat.send_session_affinity_headers
        && compat.session_affinity_format == crate::types::SessionAffinityFormat::Openrouter
        && cache_retention != CacheRetention::None
        && let Some(session_id) = options.and_then(|options| options.stream.session_id.clone()) {
            params.insert("session_id".to_owned(), Value::String(session_id));
        }

    if compat.supports_usage_in_streaming {
        params.insert("stream_options".to_owned(), json!({ "include_usage": true }));
    }
    if compat.supports_store {
        params.insert("store".to_owned(), Value::Bool(false));
    }

    if let Some(max_tokens) = options.and_then(|options| options.stream.max_tokens).filter(|max_tokens| *max_tokens > 0) {
        let field = if compat.max_tokens_field == crate::types::MaxTokensField::MaxTokens {
            "max_tokens"
        } else {
            "max_completion_tokens"
        };
        params.insert(field.to_owned(), json!(max_tokens));
    }
    if let Some(temperature) = options.and_then(|options| options.stream.temperature) {
        params.insert("temperature".to_owned(), json!(temperature));
    }

    let deferred_tool_names = if compat.deferred_tools_mode == Some(crate::types::DeferredToolsMode::Kimi) {
        get_deferred_tool_names(&context.messages)
    } else {
        HashSet::new()
    };
    let active_tools: Vec<Tool> = context
        .tools
        .iter()
        .flatten()
        .filter(|tool| !deferred_tool_names.contains(&tool.name))
        .cloned()
        .collect();
    if !active_tools.is_empty() {
        params.insert("tools".to_owned(), Value::Array(convert_tools(&active_tools, compat)?));
        if compat.zai_tool_stream {
            params.insert("tool_stream".to_owned(), Value::Bool(true));
        }
    } else if has_tool_history(&context.messages) {
        params.insert("tools".to_owned(), Value::Array(Vec::new()));
    }

    if let Some(cache_control) = &cache_control {
        let mut tools = params.get("tools").and_then(Value::as_array).cloned();
        if let Some(messages) = params.get_mut("messages").and_then(Value::as_array_mut) {
            apply_anthropic_cache_control(messages, tools.as_mut(), cache_control);
        }
        if let Some(tools) = tools {
            params.insert("tools".to_owned(), Value::Array(tools));
        }
    }

    if let Some(tool_choice) = options.and_then(|options| options.tool_choice.clone()) {
        params.insert("tool_choice".to_owned(), tool_choice);
    }
    if let Some(priority) = compat.vllm_priority {
        params.insert("priority".to_owned(), json!(priority));
    }

    let thinking_token_budget_field = resolve_thinking_token_budget_field(compat);
    let thinking_budget = resolve_clamped_thinking_budget(model, options, &params);
    let reasoning_effort = options.and_then(|options| options.reasoning_effort);
    let thinking_format = compat.thinking_format;

    if thinking_format == crate::types::ThinkingFormat::Zai && model.reasoning {
        let is_glm_53 = GLM_53_RE.is_match(&model.id.to_lowercase());
        let thinking = if reasoning_effort.is_some() || is_glm_53 {
            json!({ "type": "enabled", "clear_thinking": false })
        } else {
            json!({ "type": "disabled" })
        };
        params.insert("thinking".to_owned(), thinking);
        if reasoning_effort.is_some() && compat.supports_reasoning_effort
            && let Some(effort) = reasoning_effort.and_then(|effort| resolve_reasoning_effort(thinking_level_map.as_ref(), effort)) {
                params.insert("reasoning_effort".to_owned(), Value::String(effort));
            }
    } else if thinking_format == crate::types::ThinkingFormat::Qwen && model.reasoning {
        params.insert("enable_thinking".to_owned(), Value::Bool(reasoning_effort.is_some()));
        if reasoning_effort.is_some() && compat.supports_reasoning_effort
            && let Some(effort) = reasoning_effort.and_then(|effort| resolve_reasoning_effort(thinking_level_map.as_ref(), effort)) {
                params.insert("reasoning_effort".to_owned(), Value::String(effort));
            }
    } else if thinking_format == crate::types::ThinkingFormat::QwenChatTemplate && model.reasoning {
        params.insert(
            "chat_template_kwargs".to_owned(),
            json!({ "enable_thinking": reasoning_effort.is_some(), "preserve_thinking": true }),
        );
    } else if thinking_format == crate::types::ThinkingFormat::ChatTemplate && model.reasoning {
        if let Some(values) = build_chat_template_values(
            model,
            options,
            compat,
            &compat.chat_template_kwargs,
            thinking_budget,
        ) {
            params.insert("chat_template_kwargs".to_owned(), Value::Object(values));
        }
    } else if thinking_format == crate::types::ThinkingFormat::Baseten && model.reasoning {
        let values = compat.chat_template_args.clone().unwrap_or_default();
        if let Some(values) = build_chat_template_values(model, options, compat, &values, thinking_budget) {
            params.insert("chat_template_args".to_owned(), Value::Object(values));
        }
        if compat.supports_reasoning_effort {
            let mapped = match reasoning_effort {
                Some(effort) => thinking_level_map.as_ref().and_then(|map| map.get(&effort)),
                None => thinking_level_map.as_ref().and_then(|map| map.get(&ModelThinkingLevel::Off)),
            };
            let effort = match mapped {
                Some(Some(mapped)) => Some(mapped.clone()),
                Some(None) => None,
                None => reasoning_effort.map(|effort| effort.as_str().to_owned()),
            };
            if let Some(effort) = effort {
                params.insert("reasoning_effort".to_owned(), Value::String(effort));
            }
        }
    } else if thinking_format == crate::types::ThinkingFormat::Deepseek && model.reasoning {
        if reasoning_effort.is_some() {
            params.insert("thinking".to_owned(), json!({ "type": "enabled" }));
            if compat.supports_reasoning_effort
                && let Some(effort) = reasoning_effort.and_then(|effort| resolve_reasoning_effort(thinking_level_map.as_ref(), effort)) {
                    params.insert("reasoning_effort".to_owned(), Value::String(effort));
                }
        } else if compat.supports_disabled_thinking
            && thinking_level_map.as_ref().and_then(|map| map.get(&ModelThinkingLevel::Off)) != Some(&None)
        {
            params.insert("thinking".to_owned(), json!({ "type": "disabled" }));
        }
    } else if thinking_format == crate::types::ThinkingFormat::Openrouter && model.reasoning {
        if reasoning_effort.is_some() {
            if let Some(effort) = reasoning_effort.and_then(|effort| resolve_reasoning_effort(thinking_level_map.as_ref(), effort)) {
                params.insert("reasoning".to_owned(), json!({ "effort": effort }));
            }
        } else if thinking_level_map.as_ref().and_then(|map| map.get(&ModelThinkingLevel::Off)) != Some(&None) {
            let off = thinking_level_map
                .as_ref()
                .and_then(|map| map.get(&ModelThinkingLevel::Off))
                .and_then(|value| value.clone())
                .unwrap_or_else(|| "none".to_owned());
            params.insert("reasoning".to_owned(), json!({ "effort": off }));
        }
    } else if thinking_format == crate::types::ThinkingFormat::AntLing && model.reasoning && reasoning_effort.is_some() {
        let effort = reasoning_effort
            .and_then(|effort| thinking_level_map.as_ref().and_then(|map| map.get(&effort)))
            .and_then(|value| value.clone());
        if let Some(effort) = effort {
            params.insert("reasoning".to_owned(), json!({ "effort": effort }));
        }
    } else if thinking_format == crate::types::ThinkingFormat::Together && model.reasoning {
        params.insert("reasoning".to_owned(), json!({ "enabled": reasoning_effort.is_some() }));
        if reasoning_effort.is_some() && compat.supports_reasoning_effort
            && let Some(effort) = reasoning_effort.and_then(|effort| resolve_reasoning_effort(thinking_level_map.as_ref(), effort)) {
                params.insert("reasoning_effort".to_owned(), Value::String(effort));
            }
    } else if thinking_format == crate::types::ThinkingFormat::StringThinking && model.reasoning {
        if reasoning_effort.is_some() {
            if let Some(effort) = reasoning_effort.and_then(|effort| resolve_reasoning_effort(thinking_level_map.as_ref(), effort)) {
                params.insert("thinking".to_owned(), Value::String(effort));
            }
        } else if thinking_level_map.as_ref().and_then(|map| map.get(&ModelThinkingLevel::Off)) != Some(&None) {
            let off = thinking_level_map
                .as_ref()
                .and_then(|map| map.get(&ModelThinkingLevel::Off))
                .and_then(|value| value.clone())
                .unwrap_or_else(|| "none".to_owned());
            params.insert("thinking".to_owned(), Value::String(off));
        }
    } else if reasoning_effort.is_some() && model.reasoning && compat.supports_reasoning_effort {
        if let Some(effort) = reasoning_effort.and_then(|effort| resolve_reasoning_effort(thinking_level_map.as_ref(), effort)) {
            params.insert("reasoning_effort".to_owned(), Value::String(effort));
        }
    } else if reasoning_effort.is_none() && model.reasoning && compat.supports_reasoning_effort {
        let off = thinking_level_map
            .as_ref()
            .and_then(|map| map.get(&ModelThinkingLevel::Off))
            .and_then(|value| value.clone());
        if let Some(off) = off {
            params.insert("reasoning_effort".to_owned(), Value::String(off));
        }
    }

    if let (Some(field), Some(budget)) = (thinking_token_budget_field, thinking_budget) {
        let key = match field {
            ThinkingTokenBudgetField::ThinkingTokenBudget => "thinking_token_budget",
            ThinkingTokenBudgetField::ThinkingBudget => "thinking_budget",
            ThinkingTokenBudgetField::ThinkingBudgetTokens => "thinking_budget_tokens",
        };
        params.insert(key.to_owned(), json!(budget));
    }

    if let Some(compat_view) = &model.compat {
        if let Some(routing) = compat_view.get("openRouterRouting") {
            params.insert("provider".to_owned(), routing.clone());
        }
        if let Some(venice) = compat_view.get("veniceParameters") {
            params.insert("venice_parameters".to_owned(), venice.clone());
        }
        if let Some(routing) = compat_view.get("vercelGatewayRouting") {
            let only = routing.get("only").cloned().filter(|value| !value.is_null());
            let order = routing.get("order").cloned().filter(|value| !value.is_null());
            if only.is_some() || order.is_some() {
                let mut gateway = Map::new();
                if let Some(only) = only {
                    gateway.insert("only".to_owned(), only);
                }
                if let Some(order) = order {
                    gateway.insert("order".to_owned(), order);
                }
                params.insert("providerOptions".to_owned(), json!({ "gateway": gateway }));
            }
        }
    }

    apply_extra_body(
        &mut params,
        options.and_then(|options| options.stream.extra_body.as_ref()),
        &OPENAI_COMPLETIONS_RESERVED_BODY_KEYS,
    );

    if let Some(sampling_params) = options.and_then(|options| options.stream.sampling_params.as_ref()) {
        for (key, value) in sampling_params {
            params.insert(key.clone(), value.clone());
        }
    }

    Ok(params)
}

enum StreamBlock {
    Text(TextContent),
    Thinking(ThinkingContent),
    ToolCall(StreamingToolCall),
}

impl StreamBlock {
    fn to_content(&self) -> ContentBlock {
        match self {
            StreamBlock::Text(text) => ContentBlock::Text(text.clone()),
            StreamBlock::Thinking(thinking) => ContentBlock::Thinking(thinking.clone()),
            StreamBlock::ToolCall(block) => ContentBlock::ToolCall(ToolCall {
                id: block.id.clone(),
                name: block.name.clone(),
                arguments: block.arguments.clone(),
                incomplete: None,
                error_message: None,
                thought_signature: None,
                namespace: None,
            }),
        }
    }
}

struct CustomToolInput {
    property: String,
    json_buffer: GrammarToolInputJsonBuffer,
}

struct StreamingToolCall {
    id: String,
    name: String,
    arguments: Map<String, Value>,
    partial_args: Option<String>,
    custom_input: Option<CustomToolInput>,
    stream_index: Option<usize>,
}

fn get_custom_tool_call_input(block: &StreamingToolCall) -> String {
    let Some(property) = block.custom_input.as_ref().map(|custom| custom.property.as_str()) else {
        return String::new();
    };
    block.arguments.get(property).and_then(Value::as_str).unwrap_or_default().to_owned()
}

fn append_custom_tool_call_input(
    block: &mut StreamingToolCall,
    next_input: &str,
    close: bool,
) -> Result<Option<String>, String> {
    let Some(custom) = block.custom_input.as_mut() else { return Ok(None) };
    let property = custom.property.clone();
    let delta = append_grammar_tool_input_json_delta(&mut custom.json_buffer, &property, next_input, close)?;
    let mut arguments = Map::new();
    arguments.insert(property, Value::String(next_input.to_owned()));
    block.arguments = arguments;
    Ok(delta)
}

#[derive(Default)]
struct StreamState {
    blocks: Vec<StreamBlock>,
    text_block: Option<usize>,
    thinking_block: Option<usize>,
    active_block: Option<usize>,
    defer_mixed_events: bool,
    has_finish_reason: bool,
    deferred_text_deltas: Vec<String>,
    deferred_thinking_deltas: Vec<String>,
    deferred_tool_call_deltas: HashMap<usize, Vec<String>>,
    tool_call_blocks_by_index: HashMap<usize, usize>,
    tool_call_blocks_by_id: HashMap<String, usize>,
}

impl StreamState {
    fn sync(&self, output: &mut AssistantMessage) {
        output.content = self.blocks.iter().map(StreamBlock::to_content).collect();
    }

    fn finish_block(&mut self, index: usize, output: &mut AssistantMessage, stream: &AssistantMessageEventStream) {
        let mut text_end: Option<String> = None;
        let mut thinking_end: Option<String> = None;
        let mut tool_delta: Option<String> = None;
        let mut tool_end: Option<ToolCall> = None;

        match self.blocks.get_mut(index) {
            Some(StreamBlock::Text(text)) => text_end = Some(text.text.clone()),
            Some(StreamBlock::Thinking(thinking)) => thinking_end = Some(thinking.thinking.clone()),
            Some(StreamBlock::ToolCall(block)) => {
                if block.custom_input.is_some() {
                    let next_input = get_custom_tool_call_input(block);
                    tool_delta = append_custom_tool_call_input(block, &next_input, true).unwrap_or(None);
                } else {
                    block.arguments = parse_streaming_json(block.partial_args.as_deref())
                        .as_object()
                        .cloned()
                        .unwrap_or_default();
                }
                block.partial_args = None;
                block.custom_input = None;
                block.stream_index = None;
                tool_end = Some(ToolCall {
                    id: block.id.clone(),
                    name: block.name.clone(),
                    arguments: block.arguments.clone(),
                    incomplete: None,
                    error_message: None,
                    thought_signature: None,
                    namespace: None,
                });
            }
            None => return,
        }

        if let Some(content) = text_end {
            self.sync(output);
            stream.push(AssistantMessageEvent::TextEnd { content_index: index, content, partial: output.clone() });
        }
        if let Some(content) = thinking_end {
            self.sync(output);
            stream.push(AssistantMessageEvent::ThinkingEnd { content_index: index, content, partial: output.clone() });
        }
        if let Some(delta) = tool_delta {
            self.sync(output);
            stream.push(AssistantMessageEvent::ToolcallDelta { content_index: index, delta, partial: output.clone() });
        }
        if let Some(tool_call) = tool_end {
            self.sync(output);
            stream.push(AssistantMessageEvent::ToolcallEnd {
                content_index: index,
                tool_call,
                partial: output.clone(),
            });
        }
    }

    fn finish_active_block(&mut self, output: &mut AssistantMessage, stream: &AssistantMessageEventStream) {
        let Some(active) = self.active_block.take() else { return };
        self.finish_block(active, output, stream);
    }

    fn ensure_text_block(&mut self, output: &mut AssistantMessage, stream: &AssistantMessageEventStream) -> usize {
        if let Some(index) = self.text_block {
            return index;
        }
        if !self.defer_mixed_events {
            self.finish_active_block(output, stream);
            self.thinking_block = None;
        }
        let index = self.blocks.len();
        self.blocks.push(StreamBlock::Text(TextContent::default()));
        self.text_block = Some(index);
        if !self.defer_mixed_events {
            self.active_block = Some(index);
            self.sync(output);
            stream.push(AssistantMessageEvent::TextStart { content_index: index, partial: output.clone() });
        }
        index
    }

    fn ensure_thinking_block(
        &mut self,
        thinking_signature: &str,
        output: &mut AssistantMessage,
        stream: &AssistantMessageEventStream,
    ) -> usize {
        if let Some(index) = self.thinking_block {
            return index;
        }
        if !self.defer_mixed_events {
            self.finish_active_block(output, stream);
            self.text_block = None;
        }
        let index = self.blocks.len();
        self.blocks.push(StreamBlock::Thinking(ThinkingContent {
            thinking: String::new(),
            thinking_signature: Some(thinking_signature.to_owned()),
            ..ThinkingContent::default()
        }));
        self.thinking_block = Some(index);
        if !self.defer_mixed_events {
            self.active_block = Some(index);
            self.sync(output);
            stream.push(AssistantMessageEvent::ThinkingStart { content_index: index, partial: output.clone() });
        }
        index
    }

    fn ensure_tool_call_block(
        &mut self,
        tool_call: &Value,
        grammar_tool_input_properties: &HashMap<String, String>,
        output: &mut AssistantMessage,
        stream: &AssistantMessageEventStream,
    ) -> usize {
        let stream_index = tool_call.get("index").and_then(Value::as_u64).map(|index| index as usize);
        let name = tool_call
            .get("function")
            .and_then(|function| function.get("name"))
            .and_then(Value::as_str)
            .or_else(|| tool_call.get("custom").and_then(|custom| custom.get("name")).and_then(Value::as_str))
            .unwrap_or_default();
        let tool_call_id = tool_call.get("id").and_then(Value::as_str);

        let mut index = stream_index.and_then(|stream_index| self.tool_call_blocks_by_index.get(&stream_index).copied());
        if index.is_none()
            && let Some(id) = tool_call_id {
                index = self.tool_call_blocks_by_id.get(id).copied();
            }

        let index = match index {
            Some(index) => index,
            None => {
                if !self.defer_mixed_events {
                    self.finish_active_block(output, stream);
                    self.text_block = None;
                    self.thinking_block = None;
                }
                let custom_input_property = if tool_call.get("custom").is_some()
                    && tool_call.get("function").is_none()
                {
                    Some(grammar_tool_input_properties.get(name).cloned().unwrap_or_else(|| "input".to_owned()))
                } else {
                    None
                };
                let mut arguments = Map::new();
                if let Some(property) = &custom_input_property {
                    arguments.insert(property.clone(), Value::String(String::new()));
                }
                let block = StreamingToolCall {
                    id: tool_call_id.unwrap_or_default().to_owned(),
                    name: name.to_owned(),
                    arguments,
                    partial_args: custom_input_property.is_none().then(String::new),
                    custom_input: custom_input_property.map(|property| CustomToolInput {
                        property,
                        json_buffer: GrammarToolInputJsonBuffer::default(),
                    }),
                    stream_index,
                };
                let new_index = self.blocks.len();
                self.blocks.push(StreamBlock::ToolCall(block));
                if let Some(stream_index) = stream_index {
                    self.tool_call_blocks_by_index.insert(stream_index, new_index);
                }
                if let Some(id) = tool_call_id.filter(|id| !id.is_empty()) {
                    self.tool_call_blocks_by_id.insert(id.to_owned(), new_index);
                }
                if !self.defer_mixed_events {
                    self.active_block = Some(new_index);
                    self.sync(output);
                    stream.push(AssistantMessageEvent::ToolcallStart { content_index: new_index, partial: output.clone() });
                }
                new_index
            }
        };

        if let Some(stream_index) = stream_index {
            if let Some(StreamBlock::ToolCall(block)) = self.blocks.get_mut(index)
                && block.stream_index.is_none() {
                    block.stream_index = Some(stream_index);
                }
            self.tool_call_blocks_by_index.insert(stream_index, index);
        }
        if let Some(id) = tool_call_id.filter(|id| !id.is_empty()) {
            self.tool_call_blocks_by_id.insert(id.to_owned(), index);
        }
        if let Some(StreamBlock::ToolCall(block)) = self.blocks.get_mut(index)
            && block.name.is_empty() && !name.is_empty() {
                block.name = name.to_owned();
            }
        if tool_call.get("custom").is_some() && tool_call.get("function").is_none() {
            let missing_custom_input = matches!(self.blocks.get(index), Some(StreamBlock::ToolCall(block)) if block.custom_input.is_none());
            if missing_custom_input
                && let Some(StreamBlock::ToolCall(block)) = self.blocks.get_mut(index) {
                    let property = grammar_tool_input_properties
                        .get(&block.name)
                        .cloned()
                        .unwrap_or_else(|| "input".to_owned());
                    let mut arguments = Map::new();
                    arguments.insert(property.clone(), Value::String(String::new()));
                    block.arguments = arguments;
                    block.custom_input = Some(CustomToolInput {
                        property,
                        json_buffer: GrammarToolInputJsonBuffer::default(),
                    });
                    block.partial_args = None;
                }
        }
        index
    }

    fn flush_deferred_blocks(&mut self, output: &mut AssistantMessage, stream: &AssistantMessageEventStream) {
        let text_deltas = self.deferred_text_deltas.clone();
        let thinking_deltas = self.deferred_thinking_deltas.clone();
        let tool_call_deltas = self.deferred_tool_call_deltas.clone();
        for index in 0..self.blocks.len() {
            match self.blocks.get(index) {
                Some(StreamBlock::Text(_)) => {
                    self.sync(output);
                    stream.push(AssistantMessageEvent::TextStart { content_index: index, partial: output.clone() });
                    for delta in &text_deltas {
                        stream.push(AssistantMessageEvent::TextDelta {
                            content_index: index,
                            delta: delta.clone(),
                            partial: output.clone(),
                        });
                    }
                }
                Some(StreamBlock::Thinking(_)) => {
                    self.sync(output);
                    stream.push(AssistantMessageEvent::ThinkingStart { content_index: index, partial: output.clone() });
                    for delta in &thinking_deltas {
                        stream.push(AssistantMessageEvent::ThinkingDelta {
                            content_index: index,
                            delta: delta.clone(),
                            partial: output.clone(),
                        });
                    }
                }
                Some(StreamBlock::ToolCall(_)) => {
                    self.sync(output);
                    stream.push(AssistantMessageEvent::ToolcallStart { content_index: index, partial: output.clone() });
                    for delta in tool_call_deltas.get(&index).cloned().unwrap_or_default() {
                        stream.push(AssistantMessageEvent::ToolcallDelta {
                            content_index: index,
                            delta,
                            partial: output.clone(),
                        });
                    }
                }
                None => continue,
            }
            self.finish_block(index, output, stream);
        }
    }
}

fn default_transport(options: Option<&OpenAiCompletionsOptions>) -> Arc<dyn ChatTransport> {
    let client = options
        .and_then(|options| options.stream.request.fetch.clone())
        .unwrap_or_default();
    Arc::new(ReqwestTransport::new(client))
}

pub fn stream(
    model: &Model,
    context: &Context,
    options: Option<OpenAiCompletionsOptions>,
) -> AssistantMessageEventStream {
    let transport = default_transport(options.as_ref());
    stream_with_transport(model, context, options, transport)
}

pub fn stream_with_transport(
    model: &Model,
    context: &Context,
    options: Option<OpenAiCompletionsOptions>,
    transport: Arc<dyn ChatTransport>,
) -> AssistantMessageEventStream {
    let stream = AssistantMessageEventStream::assistant();
    let output = AssistantMessage {
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
    };
    let model = model.clone();
    let context = context.clone();
    let target = stream.clone();
    tokio::spawn(async move {
        let mut output = output;
        match run_stream_inner(&model, &context, options.as_ref(), transport, &target, &mut output).await {
            Ok(()) => {}
            Err(error) => {
                let aborted = options
                    .as_ref()
                    .and_then(|options| options.stream.request.signal.as_ref())
                    .is_some_and(AbortSignal::aborted);
                output.stop_reason = if aborted { StopReason::Aborted } else { StopReason::Error };
                let normalized = normalize_provider_error(&error.as_thrown());
                let mut message = format_provider_error(&normalized, None);
                if let Some(raw_metadata) = get_openrouter_raw_metadata(&error)
                    && !message.contains(&raw_metadata) {
                        message.push('\n');
                        message.push_str(&raw_metadata);
                    }
                output.error_message = Some(message);
                let reason = if output.stop_reason == StopReason::Aborted {
                    ErrorReason::Aborted
                } else {
                    ErrorReason::Error
                };
                target.push(AssistantMessageEvent::Error { reason, error: output.clone() });
                target.end(Some(output));
            }
        }
    });
    stream
}

async fn create_request(
    transport: &Arc<dyn ChatTransport>,
    url: &str,
    headers: &HeaderMap,
    params: &Map<String, Value>,
    timeout_ms: Option<u64>,
    signal: Option<AbortSignal>,
) -> Result<ChatStreamResponse, OpenAiCompletionsError> {
    let send = |params: &Map<String, Value>| {
        let request = ChatRequest {
            url: url.to_owned(),
            headers: headers.clone(),
            body: serde_json::to_vec(&Value::Object(params.clone())).unwrap_or_default(),
            timeout_ms,
            signal: signal.clone(),
        };
        transport.create_chat_completion(request)
    };
    match send(params).await {
        Ok(response) => Ok(response),
        Err(error) => {
            let failure = HttpFailure { status: error.status, message: error.message.clone() };
            let forced = is_forced_openai_completions_tool_choice(params.get("tool_choice"));
            if is_forced_tool_choice_unsupported_error(&failure, forced) {
                return send(&omit_tool_choice_param(params)).await;
            }
            Err(error)
        }
    }
}

async fn run_stream_inner(
    model: &Model,
    context: &Context,
    options: Option<&OpenAiCompletionsOptions>,
    transport: Arc<dyn ChatTransport>,
    stream: &AssistantMessageEventStream,
    output: &mut AssistantMessage,
) -> Result<(), OpenAiCompletionsError> {
    let request = options.map(|options| &options.stream.request);
    let signal = request.and_then(|request| request.signal.clone());
    let client_auth = resolve_openai_client_auth(
        &model.provider,
        request.and_then(|request| request.api_key.as_deref()),
        request.and_then(|request| request.headers.as_ref()),
    )
    .map_err(OpenAiCompletionsError::protocol)?;
    let compat = get_compat(model);
    let grammar_tool_input_properties =
        create_grammar_tool_input_properties(context.tools.as_deref(), compat.supports_openai_grammar_tools)
            .map_err(OpenAiCompletionsError::protocol)?;
    let grammar_properties: HashMap<String, String> = grammar_tool_input_properties
        .iter()
        .filter_map(|(name, value)| value.as_str().map(|property| (name.clone(), property.to_owned())))
        .collect();
    let cache_retention = resolve_cache_retention(
        options.and_then(|options| options.stream.cache_retention).or(model.cache_retention),
        request.and_then(|request| request.env.as_ref()),
    );
    let cache_session_id = (cache_retention != CacheRetention::None)
        .then(|| options.and_then(|options| options.stream.session_id.clone()))
        .flatten();
    let headers = create_client_headers(
        model,
        context,
        &client_auth.api_key,
        client_auth.headers.as_ref(),
        cache_session_id.as_deref(),
        &compat,
    );

    let mut params = build_params(model, context, options, &compat, cache_retention, &grammar_properties)
        .map_err(OpenAiCompletionsError::protocol)?;
    if let Some(request) = request
        && let Some(next) = request.apply_payload_hook(&Value::Object(params.clone()), model, None)
            .await.map_err(OpenAiCompletionsError::protocol)?
            && let Some(object) = next.as_object() {
                params = object.clone();
            }
    let params = normalize_request_tool_schemas(params, &compat);

    let url = format!("{}/chat/completions", model.base_url.trim_end_matches('/'));
    let timeout_ms = request.and_then(|request| request.timeout_ms);
    let response_options = request.cloned();
    let retry_options = ProviderRetryOptions {
        max_retries: request.and_then(|request| request.max_retries),
        max_retry_delay_ms: request.and_then(|request| request.max_retry_delay_ms),
        signal: signal.clone(),
    };
    let (chunk_stream, _metadata) = {
        let transport = transport.clone();
        let url = url.clone();
        let headers = headers.clone();
        let params = params.clone();
        let model = model.clone();
        let retry_signal = signal.clone();
        retry_provider_stream_request(
            move || {
                let transport = transport.clone();
                let url = url.clone();
                let headers = headers.clone();
                let params = params.clone();
                let model = model.clone();
                let response_options = response_options.clone();
                let signal = retry_signal.clone();
                async move {
                    let response = create_request(&transport, &url, &headers, &params, timeout_ms, signal).await?;
                    if let Some(options) = response_options {
                        options.apply_response_hook(
                            &crate::types::ProviderResponse { status: response.status, headers: response.headers.clone() },
                            &model,
                        ).await.map_err(OpenAiCompletionsError::protocol)?;
                    }
                    Ok((response.stream, (response.status, response.headers)))
                }
            },
            &retry_options,
        )
        .await
        .map_err(|error| match error {
            ProviderRetryError::Request(error) => error,
            ProviderRetryError::RetryDelay { message, .. } => OpenAiCompletionsError::protocol(message),
            ProviderRetryError::Aborted => OpenAiCompletionsError::protocol("Request was aborted."),
        })?
    };

    stream.push(AssistantMessageEvent::Start { partial: output.clone() });

    let mut state = StreamState::default();
    let mut chunks = chunk_stream;
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk?;
        if !chunk.is_object() {
            continue;
        }

        if output.response_id.as_deref().unwrap_or_default().is_empty()
            && let Some(id) = chunk.get("id").and_then(Value::as_str)
                && !id.is_empty() {
                    output.response_id = Some(id.to_owned());
                }
        if let Some(chunk_model) = chunk.get("model").and_then(Value::as_str)
            && !chunk_model.is_empty() && chunk_model != model.id
                && output.response_model.is_none() {
                    output.response_model = Some(chunk_model.to_owned());
                }
        if let Some(usage) = chunk.get("usage").filter(|usage| !usage.is_null()) {
            output.usage = parse_chunk_usage(usage, model);
        }

        let Some(choice) = chunk.get("choices").and_then(Value::as_array).and_then(|choices| choices.first()) else {
            continue;
        };

        if chunk.get("usage").is_none()
            && let Some(choice_usage) = choice.get("usage").filter(|usage| !usage.is_null()) {
                output.usage = parse_chunk_usage(choice_usage, model);
            }

        if let Some(finish_reason) = choice.get("finish_reason").and_then(Value::as_str) {
            output.raw_stop_reason = Some(finish_reason.to_owned());
            let (stop_reason, error_message) = map_stop_reason(Some(finish_reason));
            output.stop_reason = stop_reason;
            if let Some(error_message) = error_message {
                output.error_message = Some(error_message);
            }
            state.has_finish_reason = true;
        }

        if let Some(delta) = choice.get("delta").filter(|delta| !delta.is_null()) {
            let content_delta = delta
                .get("content")
                .and_then(Value::as_str)
                .filter(|content| !content.is_empty())
                .map(str::to_owned);
            let reasoning_fields = ["reasoning_content", "reasoning", "reasoning_text"];
            let found_reasoning_field = reasoning_fields.iter().find(|field| {
                delta.get(**field).and_then(Value::as_str).is_some_and(|value| !value.is_empty())
            });
            if state.blocks.is_empty() && content_delta.is_some() && found_reasoning_field.is_some() {
                state.defer_mixed_events = true;
            }

            if let Some(content_delta) = content_delta {
                let index = state.ensure_text_block(output, stream);
                if let Some(StreamBlock::Text(text)) = state.blocks.get_mut(index) {
                    text.text.push_str(&content_delta);
                }
                if state.defer_mixed_events {
                    state.deferred_text_deltas.push(content_delta);
                } else {
                    state.sync(output);
                    stream.push(AssistantMessageEvent::TextDelta {
                        content_index: index,
                        delta: content_delta,
                        partial: output.clone(),
                    });
                }
            }

            if let Some(field) = found_reasoning_field {
                let delta_value = delta.get(*field).and_then(Value::as_str).unwrap_or_default().to_owned();
                if !delta_value.is_empty() {
                    let thinking_signature = if model.provider == "opencode-go" && *field == "reasoning" {
                        "reasoning_content"
                    } else {
                        field
                    };
                    let index = state.ensure_thinking_block(thinking_signature, output, stream);
                    if let Some(StreamBlock::Thinking(thinking)) = state.blocks.get_mut(index) {
                        thinking.thinking.push_str(&delta_value);
                    }
                    if state.defer_mixed_events {
                        state.deferred_thinking_deltas.push(delta_value);
                    } else {
                        state.sync(output);
                        stream.push(AssistantMessageEvent::ThinkingDelta {
                            content_index: index,
                            delta: delta_value,
                            partial: output.clone(),
                        });
                    }
                }
            }

            if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for tool_call in tool_calls {
                    let index = state.ensure_tool_call_block(tool_call, &grammar_properties, output, stream);
                    let tool_call_id = tool_call.get("id").and_then(Value::as_str);
                    if let Some(StreamBlock::ToolCall(block)) = state.blocks.get_mut(index) {
                        if block.id.is_empty()
                            && let Some(id) = tool_call_id {
                                block.id = id.to_owned();
                            }
                        let name = tool_call
                            .get("function")
                            .and_then(|function| function.get("name"))
                            .and_then(Value::as_str)
                            .or_else(|| {
                                tool_call.get("custom").and_then(|custom| custom.get("name")).and_then(Value::as_str)
                            });
                        if block.name.is_empty()
                            && let Some(name) = name {
                                block.name = name.to_owned();
                            }
                    }
                    if let Some(id) = tool_call_id.filter(|id| !id.is_empty()) {
                        state.tool_call_blocks_by_id.insert(id.to_owned(), index);
                    }

                    let mut delta_text = String::new();
                    if let Some(arguments) = tool_call
                        .get("function")
                        .and_then(|function| function.get("arguments"))
                        .and_then(Value::as_str)
                        .filter(|arguments| !arguments.is_empty())
                    {
                        delta_text = arguments.to_owned();
                        if let Some(StreamBlock::ToolCall(block)) = state.blocks.get_mut(index) {
                            let mut partial = block.partial_args.clone().unwrap_or_default();
                            partial.push_str(arguments);
                            block.partial_args = Some(partial.clone());
                            block.arguments = parse_streaming_json(Some(partial.as_str()))
                                .as_object()
                                .cloned()
                                .unwrap_or_default();
                        }
                    } else if let Some(custom_input) = tool_call
                        .get("custom")
                        .and_then(|custom| custom.get("input"))
                        .and_then(Value::as_str)
                        && let Some(StreamBlock::ToolCall(block)) = state.blocks.get_mut(index) {
                            let next_input = format!("{}{}", get_custom_tool_call_input(block), custom_input);
                            delta_text = append_custom_tool_call_input(block, &next_input, false)
                                .unwrap_or(None)
                                .unwrap_or_default();
                        }
                    if state.defer_mixed_events {
                        state.deferred_tool_call_deltas.entry(index).or_default().push(delta_text);
                    } else {
                        state.sync(output);
                        stream.push(AssistantMessageEvent::ToolcallDelta {
                            content_index: index,
                            delta: delta_text,
                            partial: output.clone(),
                        });
                    }
                }
            }

            if let Some(reasoning_details) = delta.get("reasoning_details").and_then(Value::as_array) {
                for detail in reasoning_details {
                    if !is_reasoning_detail(detail) {
                        continue;
                    }
                    let index = state.ensure_thinking_block("", output, stream);
                    if let Some(StreamBlock::Thinking(thinking)) = state.blocks.get_mut(index) {
                        let mut preserved_details =
                            parse_openai_reasoning_details(thinking.thinking_signature.as_deref()).unwrap_or_default();
                        append_openai_reasoning_detail(&mut preserved_details, detail);
                        thinking.thinking_signature = Some(safe_json_stringify(&Value::Array(preserved_details)));
                    }
                }
            }
        }
    }

    if state.defer_mixed_events {
        state.flush_deferred_blocks(output, stream);
    } else {
        state.finish_active_block(output, stream);
    }
    if signal.as_ref().is_some_and(AbortSignal::aborted) {
        return Err(OpenAiCompletionsError::protocol("Request was aborted"));
    }
    if output.stop_reason == StopReason::Aborted {
        return Err(OpenAiCompletionsError::protocol("Request was aborted"));
    }
    if !state.has_finish_reason && !compat.supports_finish_reason {
        output.stop_reason = if output.content.iter().any(|block| matches!(block, ContentBlock::ToolCall(_))) {
            StopReason::ToolUse
        } else {
            StopReason::Stop
        };
    }
    if output.stop_reason == StopReason::Error {
        return Err(OpenAiCompletionsError::protocol(
            output.error_message.clone().unwrap_or_else(|| "Provider returned an error stop reason".to_owned()),
        ));
    }
    if (compat.supports_finish_reason && !state.has_finish_reason) || output.stop_reason == StopReason::Pending {
        return Err(OpenAiCompletionsError::protocol("Stream ended without finish_reason"));
    }

    let reason = match output.stop_reason {
        StopReason::Stop => DoneReason::Stop,
        StopReason::Length => DoneReason::Length,
        StopReason::ToolUse => DoneReason::ToolUse,
        StopReason::Deferred => DoneReason::Deferred,
        StopReason::Pending | StopReason::Error | StopReason::Aborted => DoneReason::Stop,
    };
    stream.push(AssistantMessageEvent::Done { reason, message: output.clone() });
    stream.end(Some(output.clone()));
    Ok(())
}

pub fn stream_simple(
    model: &Model,
    context: &Context,
    options: Option<SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    stream_simple_with_transport(model, context, options, None)
}

pub fn stream_simple_with_transport(
    model: &Model,
    context: &Context,
    options: Option<SimpleStreamOptions>,
    transport: Option<Arc<dyn ChatTransport>>,
) -> AssistantMessageEventStream {
    let api_key = options.as_ref().and_then(|options| options.stream.request.api_key.as_deref());
    if let Err(message) = resolve_openai_client_auth(
        &model.provider,
        api_key,
        options.as_ref().and_then(|options| options.stream.request.headers.as_ref()),
    ) {
        return crate::utils::lazy::error_stream(model, &message);
    }

    let base = match build_base_options(model, context, options.as_ref(), api_key) {
        Ok(base) => base,
        Err(error) => return crate::utils::lazy::error_stream(model, &error.to_string()),
    };
    let tool_choice = options
        .as_ref()
        .and_then(|options| options.stream.extra.get("toolChoice").cloned())
        .or_else(|| {
            options.as_ref().and_then(|options| options.tool_choice).map(|tool_choice| match tool_choice {
                crate::types::ToolChoice::Auto => Value::String("auto".to_owned()),
                crate::types::ToolChoice::None => Value::String("none".to_owned()),
            })
        });

    let compat = get_compat(model);
    let thinking_level_map = get_thinking_level_map(model, &compat);
    let thinking_model = if thinking_level_map == model.thinking_level_map {
        model.clone()
    } else {
        Model { thinking_level_map, ..model.clone() }
    };
    let clamped_reasoning = match options.as_ref().and_then(|options| options.reasoning) {
        Some(reasoning) => Some(clamp_thinking_level(&thinking_model, ModelThinkingLevel::from(reasoning))),
        None if model.id.contains("gpt-6-astra") => Some(ModelThinkingLevel::Off),
        None => None,
    };
    let normalized_reasoning = match clamped_reasoning {
        Some(ModelThinkingLevel::Off) if model.id.contains("gpt-6-astra") => Some(ModelThinkingLevel::Low),
        other => other,
    };
    let reasoning_effort = match normalized_reasoning {
        Some(ModelThinkingLevel::Off) | None => None,
        Some(ModelThinkingLevel::Max) if supports_max(&thinking_model) => Some(ModelThinkingLevel::Max),
        Some(level) => Some(clamp_max_for_openai(level, supports_xhigh(&thinking_model))),
    };

    let options = OpenAiCompletionsOptions {
        stream: base,
        tool_choice,
        reasoning_effort,
        thinking_budgets: options.and_then(|options| options.thinking_budgets),
    };
    match transport {
        Some(transport) => stream_with_transport(model, context, Some(options), transport),
        None => stream(model, context, Some(options)),
    }
}

pub struct OpenAiCompletionsApi;

pub fn open_ai_completions_api() -> Arc<dyn ProviderStreams> {
    lazy::lazy_api(Arc::new(|| Ok(Arc::new(OpenAiCompletionsApi) as Arc<dyn ProviderStreams>)))
}

impl ProviderStreams for OpenAiCompletionsApi {
    fn stream(&self, model: &Model, context: &Context, options: Option<StreamOptions>) -> AssistantMessageEventStream {
        stream(
            model,
            context,
            Some(OpenAiCompletionsOptions { stream: options.unwrap_or_default(), ..OpenAiCompletionsOptions::default() }),
        )
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        stream_simple(model, context, options)
    }
}
