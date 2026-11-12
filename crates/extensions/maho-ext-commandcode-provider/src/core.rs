use crate::contract::{CcResult, GENERATE_MAX_TOKENS_CAP, ProviderEnv};
use maho_ai::types::{
    AssistantMessageEvent, AssistantMessageEventStream, DoneReason, ErrorReason, StopReason,
    TextContent, ThinkingContent, ToolCall, Usage,
};
use maho_ai::utils::event_stream::create_assistant_message_event_stream;
use serde_json::{Value, json};

pub fn parse_stream_event_line(line: &str) -> Option<Value> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with(':') || trimmed.starts_with("event:") {
        return None;
    }
    let data = trimmed.strip_prefix("data:").unwrap_or(trimmed).trim();
    if data.is_empty() || data == "[DONE]" {
        return None;
    }
    serde_json::from_str(data).ok()
}

pub fn finish_reason(raw: Option<&str>) -> DoneReason {
    match raw {
        Some("tool-calls" | "tool_calls") => DoneReason::ToolUse,
        Some("length" | "max_tokens" | "max-tokens" | "max_output_tokens") => DoneReason::Length,
        _ => DoneReason::Stop,
    }
}

pub fn finish_reason_from_event(raw: Option<&str>, finish: Option<&str>) -> DoneReason {
    finish_reason(raw.or(finish))
}

pub fn upstream_failure_reason(raw: &str) -> bool {
    let raw = raw.trim();
    ["network", "connection", "upstream"].iter().any(|prefix| {
        raw.get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
            && {
                let rest = &raw[prefix.len()..];
                let rest = rest
                    .strip_prefix(['-', '_', ' ', '\t', '\n', '\r'])
                    .unwrap_or("");
                rest.eq_ignore_ascii_case("error")
            }
    })
}

pub fn usage_from_event(total_usage: &Value) -> (u64, u64, u64, u64) {
    let details = total_usage.get("inputTokenDetails");
    let input_total = total_usage
        .get("inputTokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cache_read = details
        .and_then(|v| v.get("cacheReadTokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cache_write = details
        .and_then(|v| v.get("cacheWriteTokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let input = details
        .and_then(|v| v.get("noCacheTokens"))
        .and_then(Value::as_u64)
        .unwrap_or_else(|| {
            input_total
                .saturating_sub(cache_read)
                .saturating_sub(cache_write)
        });
    (
        input,
        total_usage
            .get("outputTokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_read,
        cache_write,
    )
}

pub fn http_error_text(status: u16, detail: &str) -> String {
    format!("Command Code API error {status}: {detail}")
}

pub fn generate_request_body(
    model: &maho_ai::model::Model,
    context: &maho_ai::types::Context,
    env: &ProviderEnv,
    max_tokens: u64,
) -> Value {
    use chrono::Utc;
    let options = maho_ai::types::SimpleStreamOptions::default();
    let thread_id = uuid::Uuid::new_v4();
    let mut params = json!({
        "model": model.id,
        "messages": crate::converters::messages_to_cc(&context.messages, true).unwrap_or_default(),
        "tools": crate::converters::tools_to_json(context.tools.as_deref().unwrap_or_default()),
        "system": crate::converters::system_prompt_to_text(&json!(context.system_prompt)),
        "max_tokens": max_tokens.min(model.max_tokens).min(GENERATE_MAX_TOKENS_CAP),
        "stream": true
    });
    if let Some(temperature) = options.stream.temperature {
        params["temperature"] = json!(temperature);
    }
    let reasoning = if model.reasoning {
        options.reasoning.and_then(|level| {
            model
                .thinking_level_map
                .as_ref()
                .and_then(|map| map.get(&maho_ai::types::ModelThinkingLevel::from(level)))
                .and_then(|effort| effort.as_ref())
                .map(|effort| format!("{effort:?}").to_lowercase())
                .filter(|s| s != "off")
        })
    } else {
        None
    };
    if let Some(effort) = reasoning {
        params["reasoning_effort"] = json!(effort);
    }
    json!({"config":{"workingDir":env.cwd,"date":Utc::now().format("%Y-%m-%d").to_string(),"environment":format!("{}-{}, Node.js rust-port", std::env::consts::OS, std::env::consts::ARCH),"structure":[],"isGitRepo":false,"currentBranch":"","mainBranch":"","gitStatus":"","recentCommits":[]},"memory":null,"taste":null,"skills":null,"params":params,"threadId":thread_id.to_string()})
}

const GENERATE_BASE_RETRY_DELAY_MS: u64 = 500;
const GENERATE_MAX_RETRY_DELAY_MS: u64 = 60_000;

pub fn is_retryable_status(status: u16) -> bool {
    status == 429 || (500..600).contains(&status)
}

pub fn retry_after_seconds(raw: Option<&str>) -> Option<f64> {
    let raw = raw?;
    if let Ok(seconds) = raw.trim().parse::<f64>()
        && seconds.is_finite()
        && seconds >= 0.0
    {
        return Some(seconds);
    }
    chrono::DateTime::parse_from_rfc2822(raw).ok().map(|at| {
        let delta = at.timestamp_millis() - chrono::Utc::now().timestamp_millis();
        (delta.max(0) as f64) / 1000.0
    })
}

fn generate_max_tokens(model: &maho_ai::model::Model, max_tokens: Option<u64>) -> u64 {
    max_tokens
        .unwrap_or(model.max_tokens)
        .min(model.max_tokens)
        .min(GENERATE_MAX_TOKENS_CAP)
}

fn mapped_reasoning_effort(
    model: &maho_ai::model::Model,
    reasoning: Option<maho_ai::types::ThinkingLevel>,
) -> Option<String> {
    let level = reasoning?;
    if !model.reasoning {
        return None;
    }
    let map = model.thinking_level_map.as_ref()?;
    let mapped = map.get(&maho_ai::types::ModelThinkingLevel::from(level))?;
    mapped
        .clone()
        .filter(|effort| effort != "off" && !effort.is_empty())
}

fn is_uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok()
}

struct GenerateOutput {
    message: maho_ai::types::AssistantMessage,
    text_index: Option<usize>,
    thinking_index: Option<usize>,
    tools: std::collections::HashMap<String, (usize, String)>,
    finished: bool,
}

impl GenerateOutput {
    fn new(model: &maho_ai::model::Model) -> Self {
        Self {
            message: maho_ai::types::AssistantMessage {
                failure_source: None,
                credential_served_recently: None,
                timing: None,
                error_cause: None,
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
                abort_source: None,
                raw_stop_reason: None,
                end_turn: None,
                timestamp: maho_ai::utils::diagnostics::now_ms(),
            },
            text_index: None,
            thinking_index: None,
            tools: std::collections::HashMap::new(),
            finished: false,
        }
    }

    fn end_text(&mut self, sink: &AssistantMessageEventStream) {
        let Some(index) = self.text_index.take() else {
            return;
        };
        let content = match self.message.content.get(index) {
            Some(maho_ai::types::ContentBlock::Text(text)) => text.text.clone(),
            _ => String::new(),
        };
        sink.push(AssistantMessageEvent::TextEnd {
            content_index: index,
            content,
            partial: self.message.clone(),
        });
    }

    fn end_thinking(&mut self, sink: &AssistantMessageEventStream) {
        let Some(index) = self.thinking_index.take() else {
            return;
        };
        let content = match self.message.content.get(index) {
            Some(maho_ai::types::ContentBlock::Thinking(thinking)) => thinking.thinking.clone(),
            _ => String::new(),
        };
        sink.push(AssistantMessageEvent::ThinkingEnd {
            content_index: index,
            content,
            partial: self.message.clone(),
        });
    }

    fn handle(&mut self, event: &Value, sink: &AssistantMessageEventStream) {
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match kind {
            "text-delta" => {
                self.end_thinking(sink);
                let delta = event
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let index = match self.text_index {
                    Some(index) => index,
                    None => {
                        self.message
                            .content
                            .push(maho_ai::types::ContentBlock::Text(TextContent {
                                text: String::new(),
                                audience: None,
                                text_signature: None,
                            }));
                        let index = self.message.content.len() - 1;
                        self.text_index = Some(index);
                        sink.push(AssistantMessageEvent::TextStart {
                            content_index: index,
                            partial: self.message.clone(),
                        });
                        index
                    }
                };
                if let Some(maho_ai::types::ContentBlock::Text(text)) =
                    self.message.content.get_mut(index)
                {
                    text.text.push_str(&delta);
                }
                sink.push(AssistantMessageEvent::TextDelta {
                    content_index: index,
                    delta,
                    partial: self.message.clone(),
                });
            }
            "reasoning-start" => self.end_text(sink),
            "reasoning-delta" => {
                self.end_text(sink);
                let delta = event
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let index = match self.thinking_index {
                    Some(index) => {
                        if let Some(maho_ai::types::ContentBlock::Thinking(thinking)) =
                            self.message.content.get_mut(index)
                        {
                            thinking.thinking.push_str(&delta);
                        }
                        index
                    }
                    None => {
                        self.message
                            .content
                            .push(maho_ai::types::ContentBlock::Thinking(ThinkingContent {
                                thinking: delta.clone(),
                                ..Default::default()
                            }));
                        let index = self.message.content.len() - 1;
                        self.thinking_index = Some(index);
                        sink.push(AssistantMessageEvent::ThinkingStart {
                            content_index: index,
                            partial: self.message.clone(),
                        });
                        index
                    }
                };
                sink.push(AssistantMessageEvent::ThinkingDelta {
                    content_index: index,
                    delta,
                    partial: self.message.clone(),
                });
            }
            "reasoning-end" => self.end_thinking(sink),
            "tool-input-start" => {
                self.end_text(sink);
                self.end_thinking(sink);
                let Some(id) = event
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                else {
                    return;
                };
                if self.tools.contains_key(id) {
                    return;
                }
                self.message
                    .content
                    .push(maho_ai::types::ContentBlock::ToolCall(ToolCall {
                        id: id.to_owned(),
                        name: event
                            .get("toolName")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        arguments: Default::default(),
                        incomplete: None,
                        error_message: None,
                        thought_signature: None,
                        namespace: None,
                    }));
                let index = self.message.content.len() - 1;
                self.tools.insert(id.to_owned(), (index, String::new()));
                sink.push(AssistantMessageEvent::ToolcallStart {
                    content_index: index,
                    partial: self.message.clone(),
                });
            }
            "tool-input-delta" => {
                let (Some(id), Some(delta)) = (
                    event.get("id").and_then(Value::as_str),
                    event.get("delta").and_then(Value::as_str),
                ) else {
                    return;
                };
                let Some((index, partial)) = self.tools.get_mut(id) else {
                    return;
                };
                partial.push_str(delta);
                let arguments = serde_json::from_str::<Value>(partial)
                    .ok()
                    .and_then(|value| value.as_object().cloned())
                    .unwrap_or_default();
                let index = *index;
                if let Some(maho_ai::types::ContentBlock::ToolCall(call)) =
                    self.message.content.get_mut(index)
                {
                    call.arguments = arguments;
                }
                sink.push(AssistantMessageEvent::ToolcallDelta {
                    content_index: index,
                    delta: delta.to_owned(),
                    partial: self.message.clone(),
                });
            }
            "tool-call" => {
                self.end_text(sink);
                self.end_thinking(sink);
                let id = event
                    .get("toolCallId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let arguments = event
                    .get("input")
                    .or_else(|| event.get("args"))
                    .or_else(|| event.get("arguments"))
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                let index = match self.tools.remove(&id) {
                    Some((index, _)) => {
                        if let Some(maho_ai::types::ContentBlock::ToolCall(call)) =
                            self.message.content.get_mut(index)
                        {
                            if let Some(name) = event.get("toolName").and_then(Value::as_str) {
                                call.name = name.to_owned();
                            }
                            call.arguments = arguments;
                        }
                        index
                    }
                    None => {
                        self.message
                            .content
                            .push(maho_ai::types::ContentBlock::ToolCall(ToolCall {
                                id,
                                name: event
                                    .get("toolName")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_owned(),
                                arguments,
                                incomplete: None,
                                error_message: None,
                                thought_signature: None,
                                namespace: None,
                            }));
                        let index = self.message.content.len() - 1;
                        sink.push(AssistantMessageEvent::ToolcallStart {
                            content_index: index,
                            partial: self.message.clone(),
                        });
                        index
                    }
                };
                if let Some(maho_ai::types::ContentBlock::ToolCall(call)) =
                    self.message.content.get(index)
                {
                    sink.push(AssistantMessageEvent::ToolcallEnd {
                        content_index: index,
                        tool_call: call.clone(),
                        partial: self.message.clone(),
                    });
                }
            }
            "finish" => {
                if let Some(raw) = event
                    .get("rawFinishReason")
                    .and_then(Value::as_str)
                    .filter(|raw| upstream_failure_reason(raw))
                {
                    self.message.stop_reason = StopReason::Error;
                    self.message.error_message = Some(format!(
                        "Provider finished with reason \"{raw}\" — upstream connection failed mid-stream"
                    ));
                    self.finished = true;
                    return;
                }
                if let Some(total) = event.get("totalUsage") {
                    let (input, output, cache_read, cache_write) = usage_from_event(total);
                    self.message.usage.input = input;
                    self.message.usage.output = output;
                    self.message.usage.cache_read = cache_read;
                    self.message.usage.cache_write = cache_write;
                    self.message.usage.total_tokens = input + output + cache_read + cache_write;
                }
                self.message.stop_reason =
                    map_stop_reason(event.get("finishReason").and_then(Value::as_str));
                self.finished = true;
            }
            _ => {}
        }
    }
}

fn map_stop_reason(raw: Option<&str>) -> StopReason {
    match finish_reason(raw) {
        maho_ai::types::DoneReason::ToolUse => StopReason::ToolUse,
        maho_ai::types::DoneReason::Length => StopReason::Length,
        maho_ai::types::DoneReason::Stop => StopReason::Stop,
        _ => StopReason::Stop,
    }
}

fn request_body(
    model: &maho_ai::model::Model,
    context: &maho_ai::types::Context,
    env: &ProviderEnv,
    options: Option<&maho_ai::types::SimpleStreamOptions>,
    thread_id: &str,
) -> CcResult<Value> {
    let stream = options.map(|options| &options.stream);
    let allow_images = crate::catalog::input_modalities_for_model(&model.id)
        .contains(&maho_ai::types::InputModality::Image);
    let messages = crate::converters::messages_to_cc(&context.messages, allow_images)?;
    let mut params = json!({
        "model": model.id,
        "messages": messages,
        "tools": crate::converters::tools_to_json(context.tools.as_deref().unwrap_or_default()),
        "system": crate::converters::system_prompt_to_text(&json!(context.system_prompt)),
        "max_tokens": generate_max_tokens(model, stream.and_then(|stream| stream.max_tokens)),
        "stream": true
    });
    if let Some(temperature) = stream.and_then(|stream| stream.temperature) {
        params["temperature"] = json!(temperature);
    }
    if let Some(effort) =
        mapped_reasoning_effort(model, options.and_then(|options| options.reasoning))
    {
        params["reasoning_effort"] = json!(effort);
    }
    Ok(json!({
        "config": {
            "workingDir": env.cwd,
            "date": chrono::Utc::now().format("%Y-%m-%d").to_string(),
            "environment": format!("{}-{}, Node.js rust-port", std::env::consts::OS, std::env::consts::ARCH),
            "structure": [],
            "isGitRepo": false,
            "currentBranch": "",
            "mainBranch": "",
            "gitStatus": "",
            "recentCommits": []
        },
        "memory": null,
        "taste": null,
        "skills": null,
        "params": params,
        "threadId": thread_id
    }))
}

pub fn stream_generate(
    model: &maho_ai::model::Model,
    context: &maho_ai::types::Context,
    options: Option<maho_ai::types::SimpleStreamOptions>,
    env: &ProviderEnv,
) -> AssistantMessageEventStream {
    let stream = create_assistant_message_event_stream();
    let output_stream = stream.clone();
    let model = model.clone();
    let context = context.clone();
    let env = env.clone();
    tokio::spawn(async move {
        let result = run_generate(&model, &context, options, &env, &output_stream).await;
        if let Err(failure) = result {
            output_stream.push(AssistantMessageEvent::Error {
                reason: failure.reason,
                error: failure.message,
            });
        }
        output_stream.end(None);
    });
    stream
}

struct GenerateFailure {
    reason: ErrorReason,
    message: maho_ai::types::AssistantMessage,
}

fn failure(model: &maho_ai::model::Model, reason: ErrorReason, detail: String) -> GenerateFailure {
    let mut message = GenerateOutput::new(model).message;
    message.stop_reason = match reason {
        ErrorReason::Aborted => StopReason::Aborted,
        ErrorReason::Error => StopReason::Error,
    };
    message.error_message = Some(detail);
    GenerateFailure { reason, message }
}

async fn run_generate(
    model: &maho_ai::model::Model,
    context: &maho_ai::types::Context,
    options: Option<maho_ai::types::SimpleStreamOptions>,
    env: &ProviderEnv,
    sink: &AssistantMessageEventStream,
) -> Result<(), GenerateFailure> {
    let mut output = GenerateOutput::new(model);
    let api_key = options
        .as_ref()
        .and_then(|options| options.stream.request.api_key.clone())
        .filter(|key| !crate::api_key::is_placeholder(key))
        .or_else(|| crate::api_key::get_configured_api_key(env));
    let Some(api_key) = api_key else {
        return Err(failure(
            model,
            ErrorReason::Error,
            "No Command Code API key. Run /login and select Command Code, set COMMAND_CODE_API_KEY (or legacy COMMANDCODE_API_KEY), or configure ~/.commandcode/auth.json, ~/.pi/agent/auth.json or ~/.omp/agent/auth.json".to_owned(),
        ));
    };
    let signal = options
        .as_ref()
        .and_then(|options| options.stream.request.signal.clone());
    if signal
        .as_ref()
        .is_some_and(maho_ai::utils::abort::AbortSignal::aborted)
    {
        return Err(failure(
            model,
            ErrorReason::Aborted,
            "Request aborted".to_owned(),
        ));
    }
    sink.push(AssistantMessageEvent::Start {
        partial: output.message.clone(),
    });
    let thread_id = options
        .as_ref()
        .and_then(|options| options.stream.session_id.clone())
        .filter(|session| is_uuid(session))
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let body = request_body(model, context, env, options.as_ref(), &thread_id)
        .map_err(|detail| failure(model, ErrorReason::Error, detail))?;
    let body = match options
        .as_ref()
        .and_then(|options| options.stream.request.on_payload.as_ref())
    {
        Some(hook) => hook(&body, model, None).unwrap_or(body),
        None => body,
    };
    let url = format!("{}/alpha/generate", env.legacy_api_base());
    let client = maho_ai::utils::http_client::shared_http_client();
    let body_text = body.to_string();
    let max_retries = options
        .as_ref()
        .and_then(|options| options.stream.request.max_retries)
        .unwrap_or(0);
    let max_retry_delay_ms = options
        .as_ref()
        .and_then(|options| options.stream.request.max_retry_delay_ms)
        .unwrap_or(GENERATE_MAX_RETRY_DELAY_MS);
    let mut attempt = 0;
    let mut buffer = String::new();
    loop {
        let response = client
            .post(&url)
            .bearer_auth(&api_key)
            .header("Content-Type", "application/json")
            .header(
                "x-command-code-version",
                crate::catalog::COMMAND_CODE_CLI_VERSION,
            )
            .header("x-cli-environment", "production")
            .header("x-project-slug", crate::contract::project_slug(&env.cwd))
            .header("x-taste-learning", "true")
            .header("User-Agent", "cli")
            .header(
                "x-session-id",
                options
                    .as_ref()
                    .and_then(|options| options.stream.session_id.clone())
                    .unwrap_or_else(|| thread_id.clone()),
            )
            .body(body_text.clone())
            .send()
            .await
            .map_err(|error| {
                failure(
                    model,
                    ErrorReason::Error,
                    crate::overflow::redact_error_text(&error.to_string()),
                )
            })?;
        let status = response.status().as_u16();
        if is_retryable_status(status) && attempt < max_retries {
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| retry_after_seconds(Some(value)));
            let wait_ms = retry_after
                .map(|seconds| (seconds * 1000.0) as u64)
                .unwrap_or_else(|| {
                    (GENERATE_BASE_RETRY_DELAY_MS << attempt.min(10)).min(max_retry_delay_ms)
                });
            if wait_ms > max_retry_delay_ms && retry_after.is_some() {
                return Err(failure(
                    model,
                    ErrorReason::Error,
                    format!(
                        "Retry-After delay {}s exceeds max {max_retry_delay_ms}ms",
                        retry_after.unwrap_or_default()
                    ),
                ));
            }
            attempt += 1;
            if wait_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(wait_ms)).await;
            }
            continue;
        }
        if !response.status().is_success() {
            let raw = response.text().await.unwrap_or_default();
            let detail = serde_json::from_str::<Value>(&raw)
                .ok()
                .map(|value| crate::overflow::command_code_error_message(&value))
                .filter(|message| !message.is_empty())
                .unwrap_or_else(|| {
                    crate::overflow::redact_error_text(&raw)
                        .chars()
                        .take(500)
                        .collect()
                });
            let detail = if detail.is_empty() {
                "Provider returned an error".to_owned()
            } else {
                detail
            };
            return Err(failure(
                model,
                ErrorReason::Error,
                crate::overflow::redact_error_text(&http_error_text(status, &detail)),
            ));
        }
        let mut body_stream = response.bytes_stream();
        use futures::StreamExt;
        while let Some(chunk) = body_stream.next().await {
            if signal
                .as_ref()
                .is_some_and(maho_ai::utils::abort::AbortSignal::aborted)
            {
                return Err(failure(
                    model,
                    ErrorReason::Aborted,
                    "Request aborted".to_owned(),
                ));
            }
            let chunk = chunk.map_err(|error| {
                failure(
                    model,
                    ErrorReason::Error,
                    crate::overflow::redact_error_text(&error.to_string()),
                )
            })?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(newline) = buffer.find('\n') {
                let line: String = buffer.drain(..=newline).collect();
                if let Some(event) = parse_stream_event_line(&line) {
                    output.handle(&event, sink);
                    if output.finished {
                        break;
                    }
                }
            }
            if output.finished {
                break;
            }
        }
        break;
    }
    if !output.finished
        && let Some(event) = parse_stream_event_line(&buffer)
    {
        output.handle(&event, sink);
    }
    if !output.finished {
        return Err(failure(
            model,
            ErrorReason::Error,
            "Stream ended unexpectedly before completion (no finish event) — response was truncated".to_owned(),
        ));
    }
    if output.message.stop_reason == StopReason::Error
        && output
            .message
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("upstream connection failed"))
    {
        let detail = output.message.error_message.clone().unwrap_or_default();
        return Err(failure(model, ErrorReason::Error, detail));
    }
    output.end_text(sink);
    output.end_thinking(sink);
    crate::cost::calculate_cost(
        &crate::pricing::model_cost(&model.id),
        &mut output.message.usage,
    );
    sink.push(AssistantMessageEvent::Done {
        reason: match output.message.stop_reason {
            StopReason::Length => DoneReason::Length,
            StopReason::ToolUse => DoneReason::ToolUse,
            _ => DoneReason::Stop,
        },
        message: output.message,
    });
    Ok(())
}

#[cfg(test)]
mod generate_tests {
    use super::*;

    #[test]
    fn only_429_and_5xx_are_retryable() {
        assert!(is_retryable_status(429));
        assert!(is_retryable_status(500));
        assert!(is_retryable_status(503));
        assert!(!is_retryable_status(400));
        assert!(!is_retryable_status(403));
        assert!(!is_retryable_status(600));
    }

    #[test]
    fn retry_after_reads_seconds() {
        assert_eq!(retry_after_seconds(Some("12")), Some(12.0));
        assert_eq!(retry_after_seconds(Some("-1")), None);
        assert_eq!(retry_after_seconds(None), None);
    }

    #[test]
    fn max_tokens_caps_at_the_generate_ceiling() {
        let model: maho_ai::model::Model = serde_json::from_value(json!({
            "id": "claude-opus-5", "name": "x", "api": "anthropic-messages", "provider": "commandcode",
            "baseUrl": "", "reasoning": true, "input": ["text"],
            "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
            "contextWindow": 1, "maxTokens": 100_000
        }))
        .expect("model");
        assert_eq!(generate_max_tokens(&model, None), GENERATE_MAX_TOKENS_CAP);
        assert_eq!(generate_max_tokens(&model, Some(10)), 10);
    }

    #[test]
    fn a_text_delta_opens_and_extends_one_block() {
        let model: maho_ai::model::Model = serde_json::from_value(json!({
            "id": "claude-opus-5", "name": "x", "api": "anthropic-messages", "provider": "commandcode",
            "baseUrl": "", "reasoning": true, "input": ["text"],
            "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
            "contextWindow": 1, "maxTokens": 10
        }))
        .expect("model");
        let sink = create_assistant_message_event_stream();
        let mut output = GenerateOutput::new(&model);
        output.handle(&json!({"type": "text-delta", "text": "he"}), &sink);
        output.handle(&json!({"type": "text-delta", "text": "llo"}), &sink);
        output.handle(&json!({"type": "finish", "finishReason": "stop"}), &sink);
        assert_eq!(output.message.content.len(), 1);
        match &output.message.content[0] {
            maho_ai::types::ContentBlock::Text(text) => assert_eq!(text.text, "hello"),
            other => panic!("unexpected block {other:?}"),
        }
        assert!(output.finished);
        assert_eq!(output.message.stop_reason, StopReason::Stop);
    }

    #[test]
    fn a_finish_event_fills_usage_and_reason() {
        let model: maho_ai::model::Model = serde_json::from_value(json!({
            "id": "claude-opus-5", "name": "x", "api": "anthropic-messages", "provider": "commandcode",
            "baseUrl": "", "reasoning": true, "input": ["text"],
            "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
            "contextWindow": 1, "maxTokens": 10
        }))
        .expect("model");
        let sink = create_assistant_message_event_stream();
        let mut output = GenerateOutput::new(&model);
        output.handle(
            &json!({
                "type": "finish",
                "finishReason": "tool-calls",
                "totalUsage": {"inputTokens": 10, "outputTokens": 4,
                    "inputTokenDetails": {"cacheReadTokens": 2, "cacheWriteTokens": 0}}
            }),
            &sink,
        );
        assert_eq!(output.message.usage.input, 8);
        assert_eq!(output.message.usage.output, 4);
        assert_eq!(output.message.usage.cache_read, 2);
        assert_eq!(output.message.usage.total_tokens, 14);
        assert_eq!(output.message.stop_reason, StopReason::ToolUse);
    }

    #[test]
    fn an_upstream_failure_finish_marks_the_message_failed() {
        let model: maho_ai::model::Model = serde_json::from_value(json!({
            "id": "claude-opus-5", "name": "x", "api": "anthropic-messages", "provider": "commandcode",
            "baseUrl": "", "reasoning": true, "input": ["text"],
            "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
            "contextWindow": 1, "maxTokens": 10
        }))
        .expect("model");
        let sink = create_assistant_message_event_stream();
        let mut output = GenerateOutput::new(&model);
        output.handle(
            &json!({"type": "finish", "rawFinishReason": "upstream_error"}),
            &sink,
        );
        assert_eq!(output.message.stop_reason, StopReason::Error);
        assert!(
            output
                .message
                .error_message
                .as_deref()
                .is_some_and(|message| message.contains("upstream connection failed"))
        );
    }
}
