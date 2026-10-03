//! Port of senpi packages/ai/src/api/azure-openai-responses.ts.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Map, Value};

use crate::api::constrained_sampling::create_grammar_tool_input_properties;
use crate::api::openai_prompt_cache::clamp_openai_prompt_cache_key;
use crate::api::openai_responses_shared::{
    convert_responses_messages, convert_responses_tools, get_done_reason, process_responses_stream,
    ConvertResponsesMessagesOptions, ConvertResponsesToolsOptions, ResponsesStreamOptions,
};
use crate::api::simple_options::build_base_options;
use crate::models::{clamp_thinking_level, supports_max};
use crate::types::{
    AssistantMessage, AssistantMessageEvent, CacheRetention, Context, ErrorReason, Model, ModelThinkingLevel,
    ProviderHeaders, ProviderResponse, SimpleStreamOptions, StopReason, StreamOptions, ThinkingLevel, Usage,
};
use crate::utils::error_body::{format_provider_error, normalize_provider_error, SdkErrorShape, ThrownProviderError};
use crate::utils::event_stream::{create_assistant_message_event_stream, AssistantMessageEventStream};
use crate::utils::headers::headers_to_record;
use crate::utils::lazy::error_stream;
use crate::utils::pi_user_agent::get_pi_user_agent;
use crate::utils::provider_env::get_provider_env_value;
use crate::utils::provider_retry::{
    retry_provider_request, ProviderRetryError, ProviderRetryOptions,
};

use super::openai_responses::ResponsesApiError;

pub const DEFAULT_AZURE_API_VERSION: &str = "v1";
pub const AZURE_TOOL_CALL_PROVIDERS: [&str; 4] =
    ["openai", "chatgpt-subscription", "opencode", "azure-openai-responses"];
pub const AZURE_OPENAI_RESPONSES_MIN_OUTPUT_TOKENS: u64 = 16;
const DEFAULT_TIMEOUT_MS: u64 = 60_000;

/// `parseDeploymentNameMap`.
pub fn parse_deployment_name_map(value: Option<&str>) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let Some(value) = value else { return map };
    for entry in value.split(',') {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut parts = trimmed.splitn(2, '=');
        let model_id = parts.next().unwrap_or_default();
        let deployment_name = parts.next().unwrap_or_default();
        if model_id.trim().is_empty() || deployment_name.trim().is_empty() {
            continue;
        }
        map.insert(model_id.trim().to_owned(), deployment_name.trim().to_owned());
    }
    map
}

/// `resolveDeploymentName`.
pub fn resolve_deployment_name(model: &Model, options: &StreamOptions) -> String {
    if let Some(deployment_name) = option_str(options, "azureDeploymentName") {
        return deployment_name;
    }
    let mapped = parse_deployment_name_map(
        get_provider_env_value("AZURE_OPENAI_DEPLOYMENT_NAME_MAP", options.request.env.as_ref()).as_deref(),
    )
    .get(&model.id)
    .cloned();
    mapped.unwrap_or_else(|| model.id.clone())
}

fn option_str(options: &StreamOptions, key: &str) -> Option<String> {
    match options.extra.get(key) {
        Some(Value::String(value)) => Some(value.clone()),
        _ => None,
    }
}

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

/// `formatAzureOpenAIError`.
pub fn format_azure_openai_error(error: &ResponsesApiError) -> String {
    let thrown = match error {
        ResponsesApiError::Http { status_code, body } => ThrownProviderError::Error(Box::new(SdkErrorShape {
            message: error.to_string(),
            status_code: Some(json!(status_code)),
            body: (!body.trim().is_empty()).then(|| json!(body)),
            ..SdkErrorShape::default()
        })),
        ResponsesApiError::Message(message) => {
            ThrownProviderError::Error(Box::new(SdkErrorShape { message: message.clone(), ..SdkErrorShape::default() }))
        }
    };
    format_provider_error(&normalize_provider_error(&thrown), Some("Azure OpenAI API error"))
}

/// `normalizeAzureBaseUrl`.
pub fn normalize_azure_base_url(base_url: &str) -> Result<String, String> {
    let trimmed = base_url.trim().trim_end_matches('/');
    let mut url = url::Url::parse(trimmed)
        .map_err(|_| format!("Invalid Azure OpenAI base URL: {base_url}"))?;
    let host = url.host_str().unwrap_or_default().to_owned();
    let is_azure_host = host.ends_with(".openai.azure.com")
        || host.ends_with(".cognitiveservices.azure.com")
        || host.ends_with(".ai.azure.com");
    let normalized_path = url.path().trim_end_matches('/').to_owned();
    if is_azure_host
        && matches!(normalized_path.as_str(), "" | "/" | "/openai" | "/openai/v1/responses")
    {
        url.set_path("/openai/v1");
        url.set_query(None);
    }
    Ok(url.to_string().trim_end_matches('/').to_owned())
}

/// `buildDefaultBaseUrl`.
pub fn build_default_base_url(resource_name: &str) -> String {
    format!("https://{resource_name}.openai.azure.com/openai/v1")
}

/// `resolveAzureConfig`.
pub fn resolve_azure_config(
    model: &Model,
    options: &StreamOptions,
) -> Result<(String, String), String> {
    let env = options.request.env.as_ref();
    let api_version = option_str(options, "azureApiVersion")
        .or_else(|| get_provider_env_value("AZURE_OPENAI_API_VERSION", env))
        .unwrap_or_else(|| DEFAULT_AZURE_API_VERSION.to_owned());

    let base_url = option_str(options, "azureBaseUrl")
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            get_provider_env_value("AZURE_OPENAI_BASE_URL", env)
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        });
    let resource_name = option_str(options, "azureResourceName")
        .or_else(|| get_provider_env_value("AZURE_OPENAI_RESOURCE_NAME", env));

    let mut resolved_base_url = base_url;
    if resolved_base_url.is_none()
        && let Some(resource_name) = resource_name.as_deref()
        && !resource_name.is_empty()
    {
        resolved_base_url = Some(build_default_base_url(resource_name));
    }
    if resolved_base_url.is_none() && !model.base_url.is_empty() {
        resolved_base_url = Some(model.base_url.clone());
    }
    let Some(resolved_base_url) = resolved_base_url else {
        return Err(String::from(
            "Azure OpenAI base URL is required. Set AZURE_OPENAI_BASE_URL or AZURE_OPENAI_RESOURCE_NAME, or pass azureBaseUrl, azureResourceName, or model.baseUrl.",
        ));
    };
    Ok((normalize_azure_base_url(&resolved_base_url)?, api_version))
}

/// `createClient`.
pub fn build_azure_headers(
    model: &Model,
    api_key: &str,
    options_headers: Option<&ProviderHeaders>,
) -> reqwest::header::HeaderMap {
    let mut headers: BTreeMap<String, Option<String>> = BTreeMap::new();
    headers.insert("User-Agent".into(), Some(get_pi_user_agent()));
    for (name, value) in model.headers.iter().flatten() {
        headers.insert(name.clone(), Some(value.clone()));
    }
    for (name, value) in options_headers.iter().flat_map(|headers| headers.iter()) {
        match value {
            Some(value) => {
                headers.insert(name.clone(), Some(value.clone()));
            }
            None => {
                headers.remove(name);
            }
        }
    }
    if !headers.keys().any(|name| name.eq_ignore_ascii_case("api-key")) {
        headers.insert("api-key".into(), Some(api_key.to_owned()));
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
    header_map.insert(reqwest::header::CONTENT_TYPE, reqwest::header::HeaderValue::from_static("application/json"));
    header_map
}

/// `buildParams`.
pub fn build_params(
    model: &Model,
    context: &Context,
    options: &StreamOptions,
    deployment_name: &str,
    grammar_tool_input_properties: &BTreeMap<String, String>,
) -> Result<Map<String, Value>, String> {
    let reasoning_summary = reasoning_summary(options);
    let summary_is_truthy = matches!(&reasoning_summary, Some(Some(value)) if !value.is_empty());
    let requested_reasoning_effort = option_str(options, "reasoningEffort")
        .or_else(|| summary_is_truthy.then(|| String::from("medium")));
    let thinking_level_map = model.thinking_level_map.clone();
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
        &AZURE_TOOL_CALL_PROVIDERS.iter().map(|provider| (*provider).to_owned()).collect::<BTreeSet<String>>(),
        &ConvertResponsesMessagesOptions {
            grammar_tool_input_properties: grammar_tool_input_properties.clone(),
            ..ConvertResponsesMessagesOptions::default()
        },
    );

    let mut params = Map::new();
    params.insert("model".into(), json!(deployment_name));
    params.insert("input".into(), Value::Array(messages));
    params.insert("stream".into(), json!(true));
    if options.cache_retention.or(model.cache_retention) != Some(CacheRetention::None)
        && let Some(key) = clamp_openai_prompt_cache_key(options.session_id.as_deref())
    {
        params.insert("prompt_cache_key".into(), json!(key));
    }
    params.insert("store".into(), json!(false));

    if let Some(max_tokens) = options.max_tokens {
        params.insert("max_output_tokens".into(), json!(max_tokens.max(AZURE_OPENAI_RESPONSES_MIN_OUTPUT_TOKENS)));
    }
    if let Some(temperature) = options.temperature {
        params.insert("temperature".into(), json!(temperature));
    }
    if context.tools.as_ref().is_some_and(|tools| !tools.is_empty()) {
        let compat = model.compat.as_ref().map(|compat| compat.openai_responses()).unwrap_or_default();
        let tools = convert_responses_tools(
            context.tools.as_deref().unwrap_or_default(),
            &ConvertResponsesToolsOptions {
                supports_strict_mode: Some(compat.supports_strict_mode.unwrap_or(true)),
                supports_openai_grammar_tools: Some(compat.supports_openai_grammar_tools.unwrap_or(false)),
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
            && !matches!(thinking_level_map.as_ref().and_then(|map| map.get(&ModelThinkingLevel::Off)), Some(None))
        {
            let off = thinking_level_map
                .as_ref()
                .and_then(|map| map.get(&ModelThinkingLevel::Off).cloned())
                .flatten()
                .unwrap_or_else(|| String::from("none"));
            params.insert("reasoning".into(), json!({ "effort": off }));
        }
    }

    if let Some(sampling_params) = options.sampling_params.as_ref() {
        for (key, value) in sampling_params {
            params.insert(key.clone(), value.clone());
        }
    }
    Ok(params)
}

fn create_output(model: &Model) -> AssistantMessage {
    AssistantMessage {
        content: Vec::new(),
        api: String::from("azure-openai-responses"),
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
        output.error_message = Some(format_azure_openai_error(&error));
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
    let deployment_name = resolve_deployment_name(model, options);
    let Some(api_key) = options.request.api_key.clone().filter(|key| !key.is_empty()) else {
        return Err(ResponsesApiError::Message(format!("No API key for provider: {}", model.provider)));
    };
    let compat = model.compat.as_ref().map(|compat| compat.openai_responses()).unwrap_or_default();
    let grammar_tool_input_properties = create_grammar_tool_input_properties(
        context.tools.as_deref(),
        compat.supports_openai_grammar_tools.unwrap_or(false),
    )
    .map_err(ResponsesApiError::Message)?;

    let mut params = build_params(model, context, options, &deployment_name, &grammar_tool_input_properties)
        .map_err(ResponsesApiError::Message)?;
    if let Some(next) = options.request.apply_payload_hook(&Value::Object(params.clone()), model, None)
        .await.map_err(ResponsesApiError::Message)?
    {
        params = next.as_object().cloned().unwrap_or_default();
    }

    let (base_url, api_version) = resolve_azure_config(model, options).map_err(ResponsesApiError::Message)?;
    let url = format!("{}/responses?api-version={api_version}", base_url.trim_end_matches('/'));
    let headers = build_azure_headers(model, &api_key, options.request.headers.as_ref());
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

    process_responses_stream(
        Box::pin(response.bytes_stream()),
        output,
        sink,
        model,
        &ResponsesStreamOptions {
            service_tier: None,
            grammar_tool_input_properties: &grammar_tool_input_properties,
            apply_service_tier_pricing: None,
        },
    )
    .await
    .map_err(|error| ResponsesApiError::Message(error.message))?;

    if signal.as_ref().is_some_and(|signal| signal.aborted()) {
        return Err(ResponsesApiError::Message(String::from("Request was aborted")));
    }
    if output.stop_reason == StopReason::Pending {
        return Err(ResponsesApiError::Message(String::from(
            "Azure OpenAI Responses stream ended without a stop reason",
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
    let Some(api_key) = simple.stream.request.api_key.clone().filter(|key| !key.is_empty()) else {
        return error_stream(model, &format!("No API key for provider: {}", model.provider));
    };
    let mut stream_options = match build_base_options(model, context, Some(&simple), Some(&api_key)) {
        Ok(stream_options) => stream_options,
        Err(error) => return error_stream(model, &error.to_string()),
    };
    if let Some(tool_choice) = simple.tool_choice {
        stream_options.extra.insert("toolChoice".into(), serde_json::to_value(tool_choice).unwrap_or(Value::Null));
    }
    let clamped_reasoning = simple.reasoning.map(|reasoning| clamp_thinking_level(model, reasoning.into()));
    let reasoning_effort = match clamped_reasoning {
        None | Some(ModelThinkingLevel::Off) => None,
        Some(ModelThinkingLevel::Max) if supports_max(model) => Some(String::from("max")),
        Some(ModelThinkingLevel::Max) => Some(String::from("high")),
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
            Some(ModelThinkingLevel::from(thinking).as_str().to_owned())
        }
    };
    if let Some(reasoning_effort) = reasoning_effort {
        stream_options.extra.insert("reasoningEffort".into(), json!(reasoning_effort));
    }
    stream(model, context, Some(stream_options))
}
