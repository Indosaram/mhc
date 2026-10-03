//! Port of senpi packages/ai/src/api/google-generative-ai.ts.
//!
//! The TS module drives the `@google/genai` client; this port issues the same wire protocol
//! itself over `reqwest` (the SDK is a JavaScript package with no Rust analogue, the same trade the
//! other wire-API ports in this crate make). Everything the TS module decides *around* the SDK is
//! ported verbatim: the payload it builds, the request URL and headers, the retry loop, the SSE
//! decoder, the event order and the error texts. The SDK-internal identity headers
//! (`x-goog-api-client`, its own `User-Agent` label) are the one omission: the adapter overrides
//! the user agent with pi's own.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use futures::StreamExt;
use serde_json::{json, Map, Value};

use crate::api::google_shared::{
    convert_messages, convert_tools, is_thinking_part, js_json_stringify, js_truthy, map_stop_reason,
    post_stream_generate_content, request_body, resolve_google_function_calling_mode,
    resolve_google_thinking_level, retain_thought_signature, retry_google_request, supports_google_strict_tool_sampling,
    to_provider_native_content, ConvertMessagesOptions, GoogleApiThinkingLevel, GoogleAuth, GoogleClientConfig,
    GoogleRequestError, GoogleSseDecoder, GoogleStreamRequest, ResolvedGoogleThinkingLevel,
};
use crate::api::simple_options::{apply_extra_body, build_base_options, GOOGLE_RESERVED_BODY_KEYS};
use crate::models::{calculate_cost, clamp_thinking_level};
use crate::types::{
    AssistantMessage, AssistantMessageEvent, ContentBlock, Context, DoneReason, ErrorReason, Model, ModelThinkingLevel,
    SimpleStreamOptions, StopReason, StreamOptions, TextContent, ThinkingBudgets, ThinkingContent,
    ToolCall, ToolChoice, Usage,
};
use crate::utils::abort::AbortSignal;
use crate::utils::error_body::{format_provider_error, normalize_provider_error, SdkErrorShape, ThrownProviderError};
use crate::utils::event_stream::{create_assistant_message_event_stream, AssistantMessageEventStream};
use crate::utils::headers::provider_headers_to_record;
use crate::utils::pi_user_agent::get_pi_user_agent;
use crate::utils::provider_retry::{ProviderRetryError, ProviderRetryOptions};
use crate::utils::sanitize_unicode::sanitize_surrogates;

/// `GoogleOptions.toolChoice`, carried in `StreamOptions::extra` under its TS name.
pub const TOOL_CHOICE_KEY: &str = "toolChoice";
/// `GoogleOptions.thinking`, carried in `StreamOptions::extra` under its TS name.
pub const THINKING_KEY: &str = "thinking";

/// `GoogleOptions.thinking`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GoogleThinkingOptions {
    pub enabled: bool,
    /// -1 for dynamic, 0 to disable.
    pub budget_tokens: Option<i64>,
    pub level: Option<GoogleApiThinkingLevel>,
}

impl GoogleThinkingOptions {
    pub fn disabled() -> Self {
        Self { enabled: false, budget_tokens: None, level: None }
    }

    pub fn to_value(self) -> Value {
        let mut map = Map::new();
        map.insert("enabled".into(), Value::Bool(self.enabled));
        if let Some(budget_tokens) = self.budget_tokens {
            map.insert("budgetTokens".into(), Value::from(budget_tokens));
        }
        if let Some(level) = self.level {
            map.insert("level".into(), Value::String(level.as_str().into()));
        }
        Value::Object(map)
    }

    pub fn from_value(value: &Value) -> Option<Self> {
        let map = value.as_object()?;
        Some(Self {
            enabled: map.get("enabled").and_then(Value::as_bool).unwrap_or(false),
            budget_tokens: map.get("budgetTokens").and_then(Value::as_i64),
            level: map.get("level").and_then(Value::as_str).and_then(parse_google_thinking_level),
        })
    }
}

fn parse_google_thinking_level(value: &str) -> Option<GoogleApiThinkingLevel> {
    match value {
        "THINKING_LEVEL_UNSPECIFIED" => Some(GoogleApiThinkingLevel::ThinkingLevelUnspecified),
        "MINIMAL" => Some(GoogleApiThinkingLevel::Minimal),
        "LOW" => Some(GoogleApiThinkingLevel::Low),
        "MEDIUM" => Some(GoogleApiThinkingLevel::Medium),
        "HIGH" => Some(GoogleApiThinkingLevel::High),
        _ => None,
    }
}

pub fn option_tool_choice(options: &StreamOptions) -> Option<String> {
    options.extra.get(TOOL_CHOICE_KEY).and_then(Value::as_str).map(str::to_owned)
}

pub fn option_thinking(options: &StreamOptions) -> Option<GoogleThinkingOptions> {
    options.extra.get(THINKING_KEY).and_then(GoogleThinkingOptions::from_value)
}

fn tool_choice_str(tool_choice: ToolChoice) -> &'static str {
    match tool_choice {
        ToolChoice::Auto => "auto",
        ToolChoice::None => "none",
    }
}

/// Counter for generating unique tool call IDs.
static TOOL_CALL_COUNTER: AtomicU64 = AtomicU64::new(0);

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
    let api_key = simple.stream.request.api_key.clone().filter(|key| !key.is_empty());
    let Some(api_key) = api_key else {
        return crate::utils::lazy::error_stream(model, &format!("No API key for provider: {}", model.provider));
    };

    let mut base = match build_base_options(model, context, Some(&simple), Some(&api_key)) {
        Ok(base) => base,
        Err(error) => return crate::utils::lazy::error_stream(model, &error.to_string()),
    };
    if let Some(tool_choice) = simple.tool_choice {
        base.extra.insert(TOOL_CHOICE_KEY.into(), Value::String(tool_choice_str(tool_choice).into()));
    }

    // `reasoning` is typed as ThinkingLevel, but runtime callers can hand "off"
    // through, and Gemini 3 maps null "off" so a post-clamp check cannot see it.
    // Thinking-off takes the disabled wire form, never an enabled one.
    let Some(reasoning) = simple.reasoning else {
        return stream(model, context, Some(with_thinking(base, GoogleThinkingOptions::disabled())));
    };

    let clamped_reasoning = clamp_thinking_level(model, reasoning.into());
    if clamped_reasoning == ModelThinkingLevel::Off {
        // Only non-reasoning models clamp every request to "off".
        return stream(model, context, Some(with_thinking(base, GoogleThinkingOptions::disabled())));
    }
    let resolved_level = match resolve_google_thinking_level(model, clamped_reasoning) {
        Ok(level) => level,
        Err(message) => return crate::utils::lazy::error_stream(model, &message),
    };

    if is_gemini_3_pro_model(model) || is_gemini_3_flash_model(model) || is_gemma_4_model(model) {
        let thinking = GoogleThinkingOptions {
            enabled: true,
            budget_tokens: None,
            level: Some(get_thinking_level(resolved_level, model)),
        };
        return stream(model, context, Some(with_thinking(base, thinking)));
    }

    let thinking = GoogleThinkingOptions {
        enabled: true,
        budget_tokens: Some(get_google_budget(model, resolved_level, simple.thinking_budgets.as_ref())),
        level: None,
    };
    stream(model, context, Some(with_thinking(base, thinking)))
}

fn with_thinking(mut options: StreamOptions, thinking: GoogleThinkingOptions) -> StreamOptions {
    options.extra.insert(THINKING_KEY.into(), thinking.to_value());
    options
}

pub fn create_client(
    model: &Model,
    api_key: Option<&str>,
    options_headers: Option<&BTreeMap<String, String>>,
) -> GoogleClientConfig {
    let mut headers: BTreeMap<String, String> = BTreeMap::new();
    headers.insert("User-Agent".into(), get_pi_user_agent());
    if let Some(model_headers) = &model.headers {
        headers.extend(model_headers.clone());
    }
    if let Some(options_headers) = options_headers {
        headers.extend(options_headers.clone());
    }

    let (base_url, api_version) = if model.base_url.is_empty() {
        (None, None)
    } else {
        // baseUrl already includes the version path, don't append.
        (Some(model.base_url.clone()), Some(String::new()))
    };

    GoogleClientConfig {
        vertexai: false,
        api_key: api_key.map(str::to_owned),
        project: None,
        location: None,
        api_version,
        base_url,
        base_url_resource_scope_collection: false,
        google_auth_key_filename: None,
        headers: (!headers.is_empty()).then_some(headers),
    }
}

pub fn build_params(
    model: &Model,
    context: &Context,
    options: &StreamOptions,
) -> Result<Value, GoogleRequestError> {
    let thinking = option_thinking(options);
    let contents = convert_messages(
        model,
        context,
        ConvertMessagesOptions { preserve_thinking: Some(thinking.is_some_and(|thinking| thinking.enabled)) },
    );

    let mut generation_config = Map::new();
    if let Some(temperature) = options.temperature {
        generation_config.insert("temperature".into(), json!(temperature));
    }
    if let Some(max_tokens) = options.max_tokens {
        generation_config.insert("maxOutputTokens".into(), json!(max_tokens));
    }

    let supports_strict_mode = supports_google_strict_tool_sampling(&model.id);
    let function_calling_mode = match context.tools.as_ref().filter(|tools| !tools.is_empty()) {
        Some(tools) => resolve_google_function_calling_mode(
            tools,
            option_tool_choice(options).as_deref(),
            supports_strict_mode,
        )
        .map_err(GoogleRequestError::Message)?,
        None => None,
    };

    let mut config = Map::new();
    for (key, value) in generation_config {
        config.insert(key, value);
    }
    if let Some(system_prompt) = &context.system_prompt {
        config.insert("systemInstruction".into(), Value::String(sanitize_surrogates(system_prompt)));
    }
    if let Some(tools) = context.tools.as_ref().filter(|tools| !tools.is_empty()) {
        let declarations = convert_tools(tools, false, supports_strict_mode).map_err(GoogleRequestError::Message)?;
        config.insert("tools".into(), Value::Array(declarations.unwrap_or_default()));
    }
    if let Some(mode) = function_calling_mode {
        config.insert(
            "toolConfig".into(),
            json!({ "functionCallingConfig": { "mode": mode.as_str() } }),
        );
    }

    match thinking {
        Some(thinking) if thinking.enabled && model.reasoning => {
            let mut thinking_config = Map::new();
            thinking_config.insert("includeThoughts".into(), Value::Bool(true));
            if let Some(level) = thinking.level {
                thinking_config.insert("thinkingLevel".into(), Value::String(level.as_str().into()));
            } else if let Some(budget_tokens) = thinking.budget_tokens {
                thinking_config.insert("thinkingBudget".into(), Value::from(budget_tokens));
            }
            config.insert("thinkingConfig".into(), Value::Object(thinking_config));
        }
        Some(thinking) if model.reasoning && !thinking.enabled => {
            config.insert("thinkingConfig".into(), get_disabled_thinking_config(model));
        }
        _ => {}
    }

    if let Some(signal) = &options.request.signal
        && signal.aborted()
    {
        return Err(GoogleRequestError::Message("Request aborted".into()));
    }

    apply_extra_body(&mut config, options.extra_body.as_ref(), &GOOGLE_RESERVED_BODY_KEYS);

    Ok(json!({
        "model": model.id,
        "contents": contents,
        "config": Value::Object(config),
    }))
}

fn is_gemma_4_model(model: &Model) -> bool {
    let id = model.id.to_lowercase();
    id.contains("gemma-4") || id.contains("gemma4")
}

fn is_gemini_3_pro_model(model: &Model) -> bool {
    is_gemini_3_pro_id(&model.id)
}

fn is_gemini_3_pro_id(model_id: &str) -> bool {
    let id = model_id.to_lowercase();
    let Some(rest) = id.strip_prefix("gemini-3") else { return false };
    let rest = match rest.strip_prefix('.') {
        Some(rest) => rest.trim_start_matches(|character: char| character.is_ascii_digit()),
        None => rest,
    };
    rest.starts_with("-pro")
}

fn is_gemini_3_flash_model(model: &Model) -> bool {
    let id = model.id.to_lowercase();
    if let Some(rest) = id.strip_prefix("gemini-3") {
        let rest = match rest.strip_prefix('.') {
            Some(rest) => rest.trim_start_matches(|character: char| character.is_ascii_digit()),
            None => rest,
        };
        if rest.starts_with("-flash") {
            return true;
        }
    }
    id == "gemini-flash-latest" || id == "gemini-flash-lite-latest"
}

fn get_disabled_thinking_config(model: &Model) -> Value {
    // Google docs: Gemini 3.1 Pro cannot disable thinking, and Gemini 3 Flash / Flash-Lite
    // do not support full thinking-off either. For Gemini 3 models, use the lowest supported
    // thinkingLevel without includeThoughts so hidden thinking remains invisible to pi.
    if is_gemini_3_pro_model(model) {
        return json!({ "thinkingLevel": GoogleApiThinkingLevel::Low.as_str() });
    }
    if is_gemini_3_flash_model(model) {
        return json!({ "thinkingLevel": GoogleApiThinkingLevel::Minimal.as_str() });
    }
    if is_gemma_4_model(model) {
        return json!({ "thinkingLevel": GoogleApiThinkingLevel::Minimal.as_str() });
    }

    // Gemini 2.x supports disabling via thinkingBudget = 0.
    json!({ "thinkingBudget": 0 })
}

fn get_thinking_level(effort: ResolvedGoogleThinkingLevel, model: &Model) -> GoogleApiThinkingLevel {
    if is_gemini_3_pro_model(model) {
        match effort {
            ResolvedGoogleThinkingLevel::Minimal | ResolvedGoogleThinkingLevel::Low => {
                return GoogleApiThinkingLevel::Low
            }
            ResolvedGoogleThinkingLevel::Medium | ResolvedGoogleThinkingLevel::High => {
                return GoogleApiThinkingLevel::High
            }
        }
    }
    if is_gemma_4_model(model) {
        match effort {
            ResolvedGoogleThinkingLevel::Minimal | ResolvedGoogleThinkingLevel::Low => {
                return GoogleApiThinkingLevel::Minimal
            }
            ResolvedGoogleThinkingLevel::Medium | ResolvedGoogleThinkingLevel::High => {
                return GoogleApiThinkingLevel::High
            }
        }
    }
    match effort {
        ResolvedGoogleThinkingLevel::Minimal => GoogleApiThinkingLevel::Minimal,
        ResolvedGoogleThinkingLevel::Low => GoogleApiThinkingLevel::Low,
        ResolvedGoogleThinkingLevel::Medium => GoogleApiThinkingLevel::Medium,
        ResolvedGoogleThinkingLevel::High => GoogleApiThinkingLevel::High,
    }
}

pub fn get_google_budget(
    model: &Model,
    level: ResolvedGoogleThinkingLevel,
    custom_budgets: Option<&ThinkingBudgets>,
) -> i64 {
    if let Some(budget) = custom_budgets.and_then(|budgets| budget_for_level(budgets, level)) {
        return budget;
    }

    if model.id.contains("2.5-pro") {
        return match level {
            ResolvedGoogleThinkingLevel::Minimal => 128,
            ResolvedGoogleThinkingLevel::Low => 2048,
            ResolvedGoogleThinkingLevel::Medium => 8192,
            ResolvedGoogleThinkingLevel::High => 32768,
        };
    }

    if model.id.contains("2.5-flash-lite") {
        return match level {
            ResolvedGoogleThinkingLevel::Minimal => 512,
            ResolvedGoogleThinkingLevel::Low => 2048,
            ResolvedGoogleThinkingLevel::Medium => 8192,
            ResolvedGoogleThinkingLevel::High => 24576,
        };
    }

    if model.id.contains("2.5-flash") {
        return match level {
            ResolvedGoogleThinkingLevel::Minimal => 128,
            ResolvedGoogleThinkingLevel::Low => 2048,
            ResolvedGoogleThinkingLevel::Medium => 8192,
            ResolvedGoogleThinkingLevel::High => 24576,
        };
    }

    -1
}

fn budget_for_level(budgets: &ThinkingBudgets, level: ResolvedGoogleThinkingLevel) -> Option<i64> {
    let budget = match level {
        ResolvedGoogleThinkingLevel::Minimal => budgets.minimal,
        ResolvedGoogleThinkingLevel::Low => budgets.low,
        ResolvedGoogleThinkingLevel::Medium => budgets.medium,
        ResolvedGoogleThinkingLevel::High => budgets.high,
    };
    budget.and_then(|budget| i64::try_from(budget).ok())
}

fn create_output(model: &Model) -> AssistantMessage {
    AssistantMessage {
        content: Vec::new(),
        api: "google-generative-ai".into(),
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

impl GoogleRequestError {
    /// The SDK error shape `normalizeProviderError` probes.
    pub fn to_thrown(&self) -> ThrownProviderError {
        match self {
            GoogleRequestError::Sdk { message, status, .. } => ThrownProviderError::Error(Box::new(SdkErrorShape {
                message: message.clone(),
                status: status.map(|status| json!(status)),
                ..SdkErrorShape::default()
            })),
            GoogleRequestError::Message(message) => ThrownProviderError::Error(Box::new(SdkErrorShape {
                message: message.clone(),
                ..SdkErrorShape::default()
            })),
        }
    }
}

pub fn stream_error_message(error: &GoogleRequestError) -> String {
    format_provider_error(&normalize_provider_error(&error.to_thrown()), None)
}

async fn run_stream(model: Model, context: Context, options: StreamOptions, sink: AssistantMessageEventStream) {
    let mut output = create_output(&model);
    let outcome = drive(&model, &context, &options, &mut output, &sink).await;
    if let Err(error) = outcome {
        let aborted = options.request.signal.as_ref().is_some_and(AbortSignal::aborted);
        output.stop_reason = if aborted { StopReason::Aborted } else { StopReason::Error };
        output.error_message = Some(stream_error_message(&error));
        sink.push(AssistantMessageEvent::Error {
            reason: if aborted { ErrorReason::Aborted } else { ErrorReason::Error },
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
) -> Result<(), GoogleRequestError> {
    if options.request.fetch.is_some() {
        return Err(GoogleRequestError::Message(
            "Custom fetch is not supported by the Google Generative AI adapter".into(),
        ));
    }
    let Some(api_key) = options.request.api_key.clone().filter(|key| !key.is_empty()) else {
        return Err(GoogleRequestError::Message(format!("No API key for provider: {}", model.provider)));
    };

    let client = create_client(model, Some(&api_key), provider_headers_to_record(options.request.headers.as_ref()).as_ref());
    let mut params = build_params(model, context, options)?;
    if let Some(next_params) = options.request.apply_payload_hook(&params, model, None)
        .await.map_err(GoogleRequestError::Message)?
    {
        params = next_params;
    }

    let url = client.stream_generate_content_url(&model.id);
    let headers = client.headers.clone();
    let auth = Some(GoogleAuth::ApiKey(api_key.clone()));
    let http = reqwest::Client::new();
    let body = request_body(&params, false)?;
    let response = retry_google_request(
        move || {
            let http = http.clone();
            let url = url.clone();
            let headers = headers.clone();
            let auth = auth.clone();
            let body = body.clone();
            async move {
                post_stream_generate_content(&http, GoogleStreamRequest { url, headers: headers.as_ref(), auth, body })
                    .await
            }
        },
        &ProviderRetryOptions {
            max_retries: options.request.max_retries,
            max_retry_delay_ms: options.request.max_retry_delay_ms,
            signal: options.request.signal.clone(),
        },
    )
    .await
    .map_err(map_retry_error)?;

    sink.push(AssistantMessageEvent::Start { partial: output.clone() });
    let mut state = StreamState::default();
    let mut body = response.bytes_stream();
    while let Some(chunk) = body.next().await {
        if options.request.signal.as_ref().is_some_and(AbortSignal::aborted) {
            return Err(GoogleRequestError::Message("Request was aborted".into()));
        }
        let chunk = chunk.map_err(|error| GoogleRequestError::Message(error.to_string()))?;
        for chunk in state.decoder.push_bytes(&chunk)? {
            handle_chunk(model, &chunk, output, &mut state, sink)?;
        }
    }
    for chunk in state.decoder.finish()? {
        handle_chunk(model, &chunk, output, &mut state, sink)?;
    }

    if let Some(current_block) = state.current_block {
        push_block_end(current_block, output, sink);
    }

    if options.request.signal.as_ref().is_some_and(AbortSignal::aborted) {
        return Err(GoogleRequestError::Message("Request was aborted".into()));
    }

    if output.stop_reason == StopReason::Pending {
        return Err(GoogleRequestError::Message("Google stream ended without a finish reason".into()));
    }
    if matches!(output.stop_reason, StopReason::Aborted | StopReason::Error) {
        let error_message = output
            .raw_stop_reason
            .clone()
            .map(|reason| format!("Provider stopped with: {reason}"))
            .unwrap_or_else(|| "An unknown error occurred".to_owned());
        return Err(GoogleRequestError::Message(error_message));
    }

    sink.push(AssistantMessageEvent::Done {
        reason: done_reason(output.stop_reason),
        message: output.clone(),
    });
    sink.end(Some(output.clone()));
    Ok(())
}

fn map_retry_error(error: ProviderRetryError<GoogleRequestError>) -> GoogleRequestError {
    match error {
        ProviderRetryError::Request(error) => error,
        ProviderRetryError::RetryDelay { message, .. } => GoogleRequestError::Message(message),
        ProviderRetryError::Aborted => GoogleRequestError::Message("Request was aborted".into()),
    }
}

fn done_reason(stop_reason: StopReason) -> DoneReason {
    match stop_reason {
        StopReason::Length => DoneReason::Length,
        StopReason::ToolUse => DoneReason::ToolUse,
        StopReason::Deferred => DoneReason::Deferred,
        _ => DoneReason::Stop,
    }
}

/// The streaming state the TS closure keeps in locals (`currentBlock` and the two metadata
/// emission flags).
#[derive(Default)]
struct StreamState {
    current_block: Option<CurrentBlock>,
    grounding_metadata_emitted: bool,
    url_context_metadata_emitted: bool,
    decoder: GoogleSseDecoder,
}

/// `currentBlock` points at the block inside `output.content`; the TS code mutates that object in
/// place, so the port keeps its index and mutates through it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CurrentBlock {
    index: usize,
    kind: CurrentBlockKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CurrentBlockKind {
    Text,
    Thinking,
}

fn block_index(output: &AssistantMessage) -> usize {
    output.content.len().saturating_sub(1)
}

fn push_block_end(current_block: CurrentBlock, output: &AssistantMessage, sink: &AssistantMessageEventStream) {
    match current_block.kind {
        CurrentBlockKind::Text => {
            let content = match output.content.get(current_block.index) {
                Some(ContentBlock::Text(text)) => text.text.clone(),
                _ => String::new(),
            };
            sink.push(AssistantMessageEvent::TextEnd { content_index: block_index(output), content, partial: output.clone() });
        }
        CurrentBlockKind::Thinking => {
            let content = match output.content.get(current_block.index) {
                Some(ContentBlock::Thinking(thinking)) => thinking.thinking.clone(),
                _ => String::new(),
            };
            sink.push(AssistantMessageEvent::ThinkingEnd {
                content_index: block_index(output),
                content,
                partial: output.clone(),
            });
        }
    }
}

fn handle_chunk(
    model: &Model,
    chunk: &Value,
    output: &mut AssistantMessage,
    state: &mut StreamState,
    sink: &AssistantMessageEventStream,
) -> Result<(), GoogleRequestError> {
    // @google/genai documents GenerateContentResponse.responseId as an output-only field
    // used to identify each response. Keep the first non-empty one from the stream.
    if output.response_id.as_ref().is_none_or(|response_id| response_id.is_empty())
        && let Some(response_id) = chunk.get("responseId").and_then(Value::as_str)
    {
        output.response_id = Some(response_id.to_owned());
    }

    let candidate = chunk.get("candidates").and_then(Value::as_array).and_then(|candidates| candidates.first());
    if let Some(parts) = candidate
        .and_then(|candidate| candidate.get("content"))
        .and_then(|content| content.get("parts"))
        .and_then(Value::as_array)
    {
        for part in parts {
            let Some(part) = part.as_object() else { continue };
            let mut handled_as_known_part = false;
            if let Some(text_value) = part.get("text") {
                handled_as_known_part = true;
                let is_thinking = is_thinking_part(part);
                let needs_new_block = match state.current_block {
                    None => true,
                    Some(current) => {
                        (is_thinking && current.kind != CurrentBlockKind::Thinking)
                            || (!is_thinking && current.kind != CurrentBlockKind::Text)
                    }
                };
                if needs_new_block {
                    if let Some(current_block) = state.current_block {
                        push_block_end(current_block, output, sink);
                    }
                    let index = output.content.len();
                    if is_thinking {
                        output.content.push(ContentBlock::Thinking(ThinkingContent::default()));
                        state.current_block = Some(CurrentBlock { index, kind: CurrentBlockKind::Thinking });
                        sink.push(AssistantMessageEvent::ThinkingStart { content_index: block_index(output), partial: output.clone() });
                    } else {
                        output.content.push(ContentBlock::Text(TextContent::default()));
                        state.current_block = Some(CurrentBlock { index, kind: CurrentBlockKind::Text });
                        sink.push(AssistantMessageEvent::TextStart { content_index: block_index(output), partial: output.clone() });
                    }
                }

                let text = crate::utils::diagnostics::js_string(text_value);
                let incoming_signature = part.get("thoughtSignature").and_then(Value::as_str);
                let current_block = state.current_block.expect("a block was opened above");
                match current_block.kind {
                    CurrentBlockKind::Thinking => {
                        if let Some(ContentBlock::Thinking(thinking)) = output.content.get_mut(current_block.index) {
                            thinking.thinking.push_str(&text);
                            thinking.thinking_signature =
                                retain_thought_signature(thinking.thinking_signature.take(), incoming_signature);
                        }
                        sink.push(AssistantMessageEvent::ThinkingDelta {
                            content_index: block_index(output),
                            delta: text,
                            partial: output.clone(),
                        });
                    }
                    CurrentBlockKind::Text => {
                        if let Some(ContentBlock::Text(block)) = output.content.get_mut(current_block.index) {
                            block.text.push_str(&text);
                            block.text_signature =
                                retain_thought_signature(block.text_signature.take(), incoming_signature);
                        }
                        sink.push(AssistantMessageEvent::TextDelta {
                            content_index: block_index(output),
                            delta: text,
                            partial: output.clone(),
                        });
                    }
                }
            }

            if js_truthy(part.get("functionCall")) {
                handled_as_known_part = true;
                if let Some(current_block) = state.current_block.take() {
                    push_block_end(current_block, output, sink);
                }

                let function_call = part.get("functionCall").and_then(Value::as_object).cloned().unwrap_or_default();
                let provided_id = function_call.get("id").and_then(Value::as_str).filter(|id| !id.is_empty());
                let name = function_call.get("name").and_then(Value::as_str);
                // Generate unique ID if not provided or if it's a duplicate
                let needs_new_id = provided_id.is_none_or(|id| {
                    output
                        .content
                        .iter()
                        .any(|block| matches!(block, ContentBlock::ToolCall(tool_call) if tool_call.id == id))
                });
                let tool_call_id = if needs_new_id {
                    let counter = TOOL_CALL_COUNTER.fetch_add(1, Ordering::SeqCst) + 1;
                    format!("{}_{}_{}", name.unwrap_or("undefined"), crate::utils::diagnostics::now_ms(), counter)
                } else {
                    provided_id.unwrap_or_default().to_owned()
                };

                let arguments = function_call
                    .get("args")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                let tool_call = ToolCall {
                    id: tool_call_id,
                    name: name.unwrap_or_default().to_owned(),
                    arguments,
                    incomplete: None,
                    error_message: None,
                    thought_signature: part
                        .get("thoughtSignature")
                        .and_then(Value::as_str)
                        .filter(|signature| !signature.is_empty())
                        .map(str::to_owned),
                    namespace: None,
                };

                output.content.push(ContentBlock::ToolCall(tool_call.clone()));
                sink.push(AssistantMessageEvent::ToolcallStart { content_index: block_index(output), partial: output.clone() });
                sink.push(AssistantMessageEvent::ToolcallDelta {
                    content_index: block_index(output),
                    delta: js_json_stringify(&Value::Object(tool_call.arguments.clone())),
                    partial: output.clone(),
                });
                sink.push(AssistantMessageEvent::ToolcallEnd {
                    content_index: block_index(output),
                    tool_call,
                    partial: output.clone(),
                });
            }

            if !handled_as_known_part {
                output.content.push(ContentBlock::ProviderNative(to_provider_native_content(part)));
            }
        }
    }

    if let Some(grounding_metadata) = candidate.and_then(|candidate| candidate.get("groundingMetadata"))
        && !state.grounding_metadata_emitted
    {
        output.content.push(ContentBlock::ProviderNative(crate::types::ProviderNativeContent {
            subtype: "groundingMetadata".into(),
            raw: grounding_metadata.clone(),
        }));
        state.grounding_metadata_emitted = true;
    }

    if let Some(url_context_metadata) = candidate.and_then(|candidate| candidate.get("urlContextMetadata"))
        && !state.url_context_metadata_emitted
    {
        output.content.push(ContentBlock::ProviderNative(crate::types::ProviderNativeContent {
            subtype: "urlContextMetadata".into(),
            raw: url_context_metadata.clone(),
        }));
        state.url_context_metadata_emitted = true;
    }

    if let Some(finish_reason) = candidate.and_then(|candidate| candidate.get("finishReason")).and_then(Value::as_str) {
        output.raw_stop_reason = Some(finish_reason.to_owned());
        output.stop_reason = map_stop_reason(finish_reason).map_err(GoogleRequestError::Message)?;
        if output.stop_reason == StopReason::Stop
            && output.content.iter().any(|block| matches!(block, ContentBlock::ToolCall(_)))
        {
            output.stop_reason = StopReason::ToolUse;
        }
    }

    if let Some(usage_metadata) = chunk.get("usageMetadata") {
        let prompt_tokens = usage_metadata.get("promptTokenCount").and_then(Value::as_u64).unwrap_or(0);
        let cached_tokens = usage_metadata.get("cachedContentTokenCount").and_then(Value::as_u64).unwrap_or(0);
        let candidate_tokens = usage_metadata.get("candidatesTokenCount").and_then(Value::as_u64).unwrap_or(0);
        let thought_tokens = usage_metadata.get("thoughtsTokenCount").and_then(Value::as_u64).unwrap_or(0);
        output.usage = Usage {
            input: prompt_tokens.saturating_sub(cached_tokens),
            output: candidate_tokens + thought_tokens,
            cache_read: cached_tokens,
            cache_write: 0,
            cache_write_1h: None,
            reasoning: Some(thought_tokens),
            total_tokens: usage_metadata.get("totalTokenCount").and_then(Value::as_u64).unwrap_or(0),
            cost: Default::default(),
        };
        calculate_cost(model, &mut output.usage);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gemini_3_family_detection_matches_the_ts_regexes() {
        let model = |id: &str| {
            let mut model = crate::models_generated::get_builtin_model("google", "gemini-2.5-flash")
                .expect("models")
                .clone();
            model.id = id.to_owned();
            model
        };
        assert!(is_gemini_3_pro_model(&model("gemini-3-pro-preview")));
        assert!(is_gemini_3_pro_model(&model("gemini-3.1-pro-preview")));
        assert!(!is_gemini_3_pro_model(&model("gemini-3-flash-preview")));
        assert!(is_gemini_3_flash_model(&model("gemini-3-flash-preview")));
        assert!(is_gemini_3_flash_model(&model("gemini-3.1-flash-lite")));
        assert!(is_gemini_3_flash_model(&model("gemini-flash-latest")));
        assert!(!is_gemini_3_flash_model(&model("gemini-2.5-flash")));
        assert!(is_gemma_4_model(&model("gemma-4-27b")));
        assert!(!is_gemma_4_model(&model("gemma-3-27b")));
    }

    #[test]
    fn google_budgets_follow_the_model_families() {
        let model = |id: &str| {
            let mut model = crate::models_generated::get_builtin_model("google", "gemini-2.5-flash")
                .expect("models")
                .clone();
            model.id = id.to_owned();
            model
        };
        assert_eq!(get_google_budget(&model("gemini-2.5-pro"), ResolvedGoogleThinkingLevel::High, None), 32768);
        assert_eq!(get_google_budget(&model("gemini-2.5-flash"), ResolvedGoogleThinkingLevel::High, None), 24576);
        assert_eq!(get_google_budget(&model("gemini-2.5-flash-lite"), ResolvedGoogleThinkingLevel::Minimal, None), 512);
        assert_eq!(get_google_budget(&model("gemini-3-flash"), ResolvedGoogleThinkingLevel::High, None), -1);
        let custom = ThinkingBudgets { high: Some(1234), ..ThinkingBudgets::default() };
        assert_eq!(get_google_budget(&model("gemini-3-flash"), ResolvedGoogleThinkingLevel::High, Some(&custom)), 1234);
    }

    #[test]
    fn create_client_mirrors_the_ts_http_options() {
        let model = crate::models_generated::get_builtin_model("google", "gemini-2.5-flash").expect("models");
        let client = create_client(model, Some("test-api-key"), None);
        assert_eq!(client.base_url.as_deref(), Some(model.base_url.as_str()));
        assert_eq!(client.api_version.as_deref(), Some(""));
        assert_eq!(client.api_key.as_deref(), Some("test-api-key"));
        assert_eq!(client.headers.as_ref().and_then(|headers| headers.get("User-Agent")).map(String::as_str), Some(get_pi_user_agent().as_str()));
        assert_eq!(
            client.stream_generate_content_url(&model.id),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
        );
    }
}
