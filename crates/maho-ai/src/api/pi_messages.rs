//! Port of senpi packages/ai/src/api/pi-messages.ts.
//!
//! Streams pi's own message protocol directly to a backend: the request is a single POST of
//! `{ model, context, options }` to `<baseUrl>/messages`, the response is an SSE stream of
//! serialized assistant-message events plus a terminal `done`/`error` event.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::types::{
    AssistantMessage, AssistantMessageEvent, CacheRetention, ContentBlock, Context, ErrorReason,
    Message, Model, ProviderEnv, StopReason, StreamOptions, TextContent, ThinkingContent, ToolCall,
    Usage,
};
use crate::utils::diagnostics::{
    AssistantMessageDiagnostic, DiagnosticCode, Thrown, append_assistant_message_diagnostic,
    create_assistant_message_diagnostic, now_ms,
};
use crate::utils::event_stream::{
    AssistantMessageEventStream, create_assistant_message_event_stream,
};
use crate::utils::headers::{headers_to_record, provider_headers_to_record};
use crate::utils::json_parse::parse_streaming_json;
use crate::utils::provider_env::get_provider_env_value;

/// Serialized assistant-message event as sent by a pi-messages backend.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PiMessagesEvent {
    Start,
    TextStart {
        #[serde(rename = "contentIndex")]
        content_index: usize,
    },
    TextDelta {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        delta: String,
    },
    TextEnd {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        content: String,
        #[serde(
            rename = "contentSignature",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        content_signature: Option<String>,
    },
    ThinkingStart {
        #[serde(rename = "contentIndex")]
        content_index: usize,
    },
    ThinkingDelta {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        delta: String,
    },
    ThinkingEnd {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        content: String,
        #[serde(
            rename = "contentSignature",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        content_signature: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        redacted: Option<bool>,
    },
    ToolcallStart {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        id: String,
        #[serde(rename = "toolName")]
        tool_name: String,
    },
    ToolcallDelta {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        delta: String,
    },
    ToolcallEnd {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        #[serde(rename = "toolCall")]
        tool_call: ToolCall,
    },
    Done {
        reason: String,
        usage: Usage,
        #[serde(
            rename = "responseId",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        response_id: Option<String>,
        #[serde(
            rename = "providerThinkingLevel",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        provider_thinking_level: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rewrite: Option<PiMessagesRewriteImpact>,
    },
    Error {
        reason: String,
        usage: Usage,
        #[serde(
            rename = "errorMessage",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        error_message: Option<String>,
        #[serde(
            rename = "responseId",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        response_id: Option<String>,
        #[serde(
            rename = "providerThinkingLevel",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        provider_thinking_level: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rewrite: Option<PiMessagesRewriteImpact>,
    },
}

/// Impact summary of a server-side message rewrite (e.g. a gateway policy).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiMessagesRewriteImpact {
    pub policy_id: String,
    pub policy_version: u64,
    pub changed: bool,
    pub token_count_change: i64,
    pub message_count_change: i64,
    pub system_prompt_changed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct PiMessagesResponseError {
    pub message: String,
    pub code: Option<String>,
    pub diagnostic_details: Map<String, Value>,
}

impl PiMessagesResponseError {
    pub fn new(
        message: String,
        code: Option<String>,
        diagnostic_details: Map<String, Value>,
    ) -> Self {
        Self {
            message,
            code,
            diagnostic_details,
        }
    }
}

fn parse_pi_messages_error_body(body: &str) -> Option<Map<String, Value>> {
    let parsed: Value = serde_json::from_str(body).ok()?;
    let object = parsed.as_object()?;
    let error = object.get("error")?;
    if error.is_object() {
        Some(object.clone())
    } else {
        None
    }
}

fn truncate_diagnostic_string(value: &str) -> String {
    const MAX_LENGTH: usize = 8192;
    let mut units = value.encode_utf16();
    let prefix: Vec<u16> = units.by_ref().take(MAX_LENGTH).collect();
    if units.next().is_some() {
        format!("{}\u{2026}", String::from_utf16_lossy(&prefix))
    } else {
        value.to_owned()
    }
}

fn format_pi_messages_response_error(
    status: u16,
    status_text: &str,
    body: &str,
    error_body: Option<&Map<String, Value>>,
) -> String {
    let error = error_body
        .and_then(|body| body.get("error"))
        .and_then(Value::as_object);
    let message = error
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str);
    let code = error
        .and_then(|error| error.get("code"))
        .and_then(Value::as_str);
    let suffix = message.unwrap_or(body);
    let code_suffix = code.map(|code| format!(" ({code})")).unwrap_or_default();
    format!("{status} {status_text}: {suffix}{code_suffix}")
}

fn create_pi_messages_response_error(
    model: &Model,
    url: &str,
    status: u16,
    status_text: &str,
    body: &str,
) -> PiMessagesResponseError {
    let error_body = parse_pi_messages_error_body(body);
    let code = error_body
        .as_ref()
        .and_then(|body| body.get("error"))
        .and_then(Value::as_object)
        .and_then(|error| error.get("code"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let mut details = Map::new();
    details.insert("version".into(), Value::from(1));
    details.insert("provider".into(), Value::from(model.provider.clone()));
    details.insert("model".into(), Value::from(model.id.clone()));
    details.insert("url".into(), Value::from(url));
    details.insert("status".into(), Value::from(status));
    details.insert("statusText".into(), Value::from(status_text));
    details.insert(
        "error".into(),
        error_body
            .as_ref()
            .and_then(|body| body.get("error"))
            .cloned()
            .unwrap_or(Value::Null),
    );
    details.insert(
        "body".into(),
        if error_body.is_some() {
            Value::Null
        } else {
            Value::from(truncate_diagnostic_string(body))
        },
    );
    details.insert("timestampMs".into(), Value::from(now_ms()));
    PiMessagesResponseError::new(
        format_pi_messages_response_error(status, status_text, body, error_body.as_ref()),
        code,
        details,
    )
}

fn create_empty_usage() -> Usage {
    Usage::default()
}

fn append_rewrite_diagnostic(
    message: &mut AssistantMessage,
    rewrite: Option<&PiMessagesRewriteImpact>,
) {
    let Some(rewrite) = rewrite else { return };
    let mut details = Map::new();
    details.insert("policyId".into(), Value::from(rewrite.policy_id.clone()));
    details.insert("policyVersion".into(), Value::from(rewrite.policy_version));
    details.insert("changed".into(), Value::from(rewrite.changed));
    details.insert(
        "tokenCountChange".into(),
        Value::from(rewrite.token_count_change),
    );
    details.insert(
        "messageCountChange".into(),
        Value::from(rewrite.message_count_change),
    );
    details.insert(
        "systemPromptChanged".into(),
        Value::from(rewrite.system_prompt_changed),
    );
    append_assistant_message_diagnostic(
        &mut message.diagnostics,
        AssistantMessageDiagnostic {
            kind: "pi_messages_rewrite".into(),
            timestamp: now_ms(),
            error: None,
            details: Some(details),
        },
    );
}

fn ensure_slot(content: &mut Vec<ContentBlock>, index: usize) {
    while content.len() <= index {
        content.push(ContentBlock::Text(TextContent::default()));
    }
}

struct EventConverter {
    model: Model,
    partial: AssistantMessage,
    tool_json: std::collections::HashMap<usize, String>,
}

impl EventConverter {
    fn new(model: &Model) -> Self {
        Self {
            model: model.clone(),
            partial: AssistantMessage {
                failure_source: None,
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
                usage: create_empty_usage(),
                stop_reason: StopReason::Pending,
                stop_details: None,
                deferred: None,
                error_message: None,
                abort_source: None,
                raw_stop_reason: None,
                end_turn: None,
                timestamp: now_ms(),
            },
            tool_json: std::collections::HashMap::new(),
        }
    }

    fn convert(&mut self, event: &PiMessagesEvent) -> AssistantMessageEvent {
        let _ = &self.model;
        match event {
            PiMessagesEvent::Done {
                reason,
                usage,
                response_id,
                provider_thinking_level,
                rewrite,
            } => {
                self.partial.stop_reason = parse_stop_reason(reason);
                self.partial.usage = *usage;
                self.partial.response_id = response_id.clone();
                if let Some(level) = provider_thinking_level {
                    self.partial.provider_thinking_level = Some(level.clone());
                }
                append_rewrite_diagnostic(&mut self.partial, rewrite.as_ref());
                AssistantMessageEvent::Done {
                    reason: match self.partial.stop_reason {
                        StopReason::Length => crate::types::DoneReason::Length,
                        StopReason::ToolUse => crate::types::DoneReason::ToolUse,
                        StopReason::Deferred => crate::types::DoneReason::Deferred,
                        _ => crate::types::DoneReason::Stop,
                    },
                    message: self.partial.clone(),
                }
            }
            PiMessagesEvent::Error {
                reason,
                usage,
                error_message,
                response_id,
                provider_thinking_level,
                rewrite,
            } => {
                self.partial.stop_reason = parse_stop_reason(reason);
                self.partial.usage = *usage;
                self.partial.error_message = error_message.clone();
                self.partial.response_id = response_id.clone();
                if let Some(level) = provider_thinking_level {
                    self.partial.provider_thinking_level = Some(level.clone());
                }
                append_rewrite_diagnostic(&mut self.partial, rewrite.as_ref());
                AssistantMessageEvent::Error {
                    reason: if self.partial.stop_reason == StopReason::Aborted {
                        ErrorReason::Aborted
                    } else {
                        ErrorReason::Error
                    },
                    error: self.partial.clone(),
                }
            }
            PiMessagesEvent::Start => partial_event(&self.partial, |partial| {
                AssistantMessageEvent::Start { partial }
            }),
            PiMessagesEvent::TextStart { content_index } => {
                ensure_slot(&mut self.partial.content, *content_index);
                self.partial.content[*content_index] = ContentBlock::text("");
                partial_event(&self.partial, |partial| AssistantMessageEvent::TextStart {
                    content_index: *content_index,
                    partial,
                })
            }
            PiMessagesEvent::TextDelta {
                content_index,
                delta,
            } => {
                if let Some(ContentBlock::Text(text)) = self.partial.content.get_mut(*content_index)
                {
                    text.text.push_str(delta);
                }
                partial_event(&self.partial, |partial| AssistantMessageEvent::TextDelta {
                    content_index: *content_index,
                    delta: delta.clone(),
                    partial,
                })
            }
            PiMessagesEvent::TextEnd {
                content_index,
                content,
                content_signature,
            } => {
                if let Some(ContentBlock::Text(text)) = self.partial.content.get_mut(*content_index)
                {
                    text.text = content.clone();
                    text.text_signature = content_signature.clone();
                }
                partial_event(&self.partial, |partial| AssistantMessageEvent::TextEnd {
                    content_index: *content_index,
                    content: content.clone(),
                    partial,
                })
            }
            PiMessagesEvent::ThinkingStart { content_index } => {
                ensure_slot(&mut self.partial.content, *content_index);
                self.partial.content[*content_index] = ContentBlock::Thinking(ThinkingContent {
                    thinking: String::new(),
                    ..ThinkingContent::default()
                });
                partial_event(&self.partial, |partial| {
                    AssistantMessageEvent::ThinkingStart {
                        content_index: *content_index,
                        partial,
                    }
                })
            }
            PiMessagesEvent::ThinkingDelta {
                content_index,
                delta,
            } => {
                if let Some(ContentBlock::Thinking(thinking)) =
                    self.partial.content.get_mut(*content_index)
                {
                    thinking.thinking.push_str(delta);
                }
                partial_event(&self.partial, |partial| {
                    AssistantMessageEvent::ThinkingDelta {
                        content_index: *content_index,
                        delta: delta.clone(),
                        partial,
                    }
                })
            }
            PiMessagesEvent::ThinkingEnd {
                content_index,
                content,
                content_signature,
                redacted,
            } => {
                if let Some(ContentBlock::Thinking(thinking)) =
                    self.partial.content.get_mut(*content_index)
                {
                    thinking.thinking = content.clone();
                    thinking.thinking_signature = content_signature.clone();
                    thinking.redacted = *redacted;
                }
                partial_event(&self.partial, |partial| {
                    AssistantMessageEvent::ThinkingEnd {
                        content_index: *content_index,
                        content: content.clone(),
                        partial,
                    }
                })
            }
            PiMessagesEvent::ToolcallStart {
                content_index,
                id,
                tool_name,
            } => {
                ensure_slot(&mut self.partial.content, *content_index);
                self.partial.content[*content_index] = ContentBlock::ToolCall(ToolCall {
                    id: id.clone(),
                    name: tool_name.clone(),
                    arguments: Map::new(),
                    incomplete: None,
                    error_message: None,
                    thought_signature: None,
                    namespace: None,
                });
                self.tool_json.insert(*content_index, String::new());
                partial_event(&self.partial, |partial| {
                    AssistantMessageEvent::ToolcallStart {
                        content_index: *content_index,
                        partial,
                    }
                })
            }
            PiMessagesEvent::ToolcallDelta {
                content_index,
                delta,
            } => {
                let json = format!(
                    "{}{delta}",
                    self.tool_json
                        .get(content_index)
                        .cloned()
                        .unwrap_or_default()
                );
                self.tool_json.insert(*content_index, json.clone());
                let parsed = parse_streaming_json(Some(&json));
                if let Some(ContentBlock::ToolCall(call)) =
                    self.partial.content.get_mut(*content_index)
                {
                    call.arguments = parsed.as_object().cloned().unwrap_or_default();
                }
                partial_event(&self.partial, |partial| {
                    AssistantMessageEvent::ToolcallDelta {
                        content_index: *content_index,
                        delta: delta.clone(),
                        partial,
                    }
                })
            }
            PiMessagesEvent::ToolcallEnd {
                content_index,
                tool_call,
            } => {
                self.tool_json.remove(content_index);
                if let Some(ContentBlock::ToolCall(call)) =
                    self.partial.content.get_mut(*content_index)
                {
                    *call = tool_call.clone();
                }
                partial_event(&self.partial, |partial| {
                    AssistantMessageEvent::ToolcallEnd {
                        content_index: *content_index,
                        tool_call: tool_call.clone(),
                        partial,
                    }
                })
            }
        }
    }
}

fn partial_event(
    partial: &AssistantMessage,
    build: impl FnOnce(AssistantMessage) -> AssistantMessageEvent,
) -> AssistantMessageEvent {
    build(partial.clone())
}

fn parse_stop_reason(reason: &str) -> StopReason {
    match reason {
        "stop" => StopReason::Stop,
        "length" => StopReason::Length,
        "toolUse" => StopReason::ToolUse,
        "aborted" => StopReason::Aborted,
        "deferred" => StopReason::Deferred,
        _ => StopReason::Error,
    }
}

fn parse_pi_messages_event(raw: &str) -> Result<Option<Value>, PiMessagesError> {
    let Some(data) = raw
        .split('\n')
        .find(|line| line.starts_with("data:"))
        .map(|line| line[5..].trim())
    else {
        return Ok(None);
    };
    if data.is_empty() || data == "[DONE]" {
        return Ok(None);
    }
    serde_json::from_str(data)
        .map(Some)
        .map_err(|error| PiMessagesError::Message(error.to_string()))
}

fn create_error_event(
    model: &Model,
    error: &PiMessagesError,
    aborted: bool,
) -> AssistantMessageEvent {
    let reason = if aborted {
        StopReason::Aborted
    } else {
        StopReason::Error
    };
    let mut assistant_message = AssistantMessage {
        failure_source: None,
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
        usage: create_empty_usage(),
        stop_reason: reason,
        stop_details: None,
        deferred: None,
        error_message: Some(error.message()),
        abort_source: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: now_ms(),
    };

    if !aborted && let PiMessagesError::Response(response) = error {
        {
            let thrown = Thrown::Error {
                name: "PiMessagesResponseError".into(),
                message: response.message.clone(),
                stack: None,
                code: response.code.clone().map(DiagnosticCode::Text),
            };
            append_assistant_message_diagnostic(
                &mut assistant_message.diagnostics,
                create_assistant_message_diagnostic(
                    "pi_messages_response_failure",
                    &thrown,
                    Some(response.diagnostic_details.clone()),
                ),
            );
        }
    }

    AssistantMessageEvent::Error {
        reason: if aborted {
            ErrorReason::Aborted
        } else {
            ErrorReason::Error
        },
        error: assistant_message,
    }
}

#[derive(Debug)]
enum PiMessagesError {
    Message(String),
    Response(PiMessagesResponseError),
}

impl PiMessagesError {
    fn message(&self) -> String {
        match self {
            PiMessagesError::Message(message) => message.clone(),
            PiMessagesError::Response(response) => response.message.clone(),
        }
    }
}

fn resolve_cache_retention(
    cache_retention: Option<CacheRetention>,
    env: Option<&ProviderEnv>,
) -> Option<CacheRetention> {
    if cache_retention.is_some() {
        return cache_retention;
    }
    // Backend defaults apply when unset; only the legacy env opt-in is mapped.
    if get_provider_env_value("PI_CACHE_RETENTION", env).as_deref() == Some("long") {
        Some(CacheRetention::Long)
    } else {
        None
    }
}

fn without_prompt_cache_breakpoint(message: &AssistantMessage) -> Value {
    let mut value = serde_json::to_value(message).expect("assistant message serialization");
    if message.prompt_cache_breakpoint == Some(true)
        && let Some(object) = value.as_object_mut()
    {
        object.remove("promptCacheBreakpoint");
    }
    value
}

fn context_payload(context: &Context) -> Value {
    let messages: Vec<Value> = context
        .messages
        .iter()
        .map(|message| match message {
            Message::ToolResult(result) => {
                let content: Vec<Value> = result
                    .content
                    .iter()
                    .map(|part| match part {
                        ContentBlock::Text(text) => {
                            let mut mapped = Map::new();
                            mapped.insert("type".into(), Value::from("text"));
                            mapped.insert("text".into(), Value::from(text.text.clone()));
                            if let Some(signature) = &text.text_signature {
                                mapped
                                    .insert("textSignature".into(), Value::from(signature.clone()));
                            }
                            Value::Object(mapped)
                        }
                        other => serde_json::to_value(other).unwrap_or(Value::Null),
                    })
                    .collect();
                json!({
                    "role": "toolResult",
                    "toolCallId": result.tool_call_id,
                    "toolName": result.tool_name,
                    "content": content,
                    "isError": result.is_error,
                    "timestamp": result.timestamp,
                })
            }
            Message::Assistant(assistant) => without_prompt_cache_breakpoint(assistant),
            other => serde_json::to_value(other).unwrap_or(Value::Null),
        })
        .collect();
    let mut payload = Map::new();
    if let Some(system_prompt) = &context.system_prompt {
        payload.insert("systemPrompt".into(), Value::from(system_prompt.clone()));
    }
    payload.insert("messages".into(), Value::Array(messages));
    if let Some(tools) = &context.tools {
        payload.insert(
            "tools".into(),
            serde_json::to_value(tools).unwrap_or(Value::Null),
        );
    }
    Value::Object(payload)
}

fn option_value(options: &StreamOptions, key: &str) -> Option<Value> {
    options
        .extra
        .get(key)
        .cloned()
        .filter(|value| !value.is_null())
}

pub fn stream(
    model: &Model,
    context: &Context,
    options: Option<StreamOptions>,
) -> AssistantMessageEventStream {
    let resolved_context = crate::types::transcript::resolve_provider_context(context, false);
    let context = &resolved_context;
    let event_stream = create_assistant_message_event_stream();
    let model = model.clone();
    let context = context.clone();
    let options = options.unwrap_or_default();
    let sink = event_stream.clone();
    tokio::spawn(async move {
        run_stream(model, context, options, sink).await;
    });
    event_stream
}

async fn run_stream(
    model: Model,
    context: Context,
    options: StreamOptions,
    sink: AssistantMessageEventStream,
) {
    let mut converter = EventConverter::new(&model);

    let outcome = drive(&model, &context, &options, &mut converter, &sink).await;
    if let Err(error) = outcome {
        let aborted = options
            .request
            .signal
            .as_ref()
            .is_some_and(|signal| signal.aborted());
        sink.push(create_error_event(&model, &error, aborted));
    }
}

async fn drive(
    model: &Model,
    context: &Context,
    options: &StreamOptions,
    converter: &mut EventConverter,
    sink: &AssistantMessageEventStream,
) -> Result<(), PiMessagesError> {
    let resolved_context = crate::types::transcript::resolve_provider_context(context, false);
    let context = &resolved_context;
    let Some(api_key) = options
        .request
        .api_key
        .clone()
        .filter(|key| !key.is_empty())
    else {
        return Err(PiMessagesError::Message(format!(
            "No API key provided for provider \"{}\"",
            model.provider
        )));
    };

    let base = model.base_url.trim_end_matches('/');
    let mut url = format!("{base}/messages");
    if option_value(options, "debug").and_then(|value| value.as_bool()) == Some(true) {
        url.push_str("?debug=1");
    }

    let mut payload = json!({
        "model": model.id,
        "context": context_payload(context),
        "options": {
            "temperature": options.temperature,
            "maxTokens": options.max_tokens,
            "reasoning": option_value(options, "reasoning"),
            "cacheRetention": resolve_cache_retention(options.cache_retention, options.request.env.as_ref())
                .map(|retention| serde_json::to_value(retention).unwrap_or(Value::Null)),
            "sessionId": options.session_id,
            "toolChoice": option_value(options, "toolChoice"),
        },
    });

    if let Some(next) = options
        .request
        .apply_payload_hook(&payload, model, None)
        .await
        .map_err(PiMessagesError::Message)?
    {
        payload = next;
    }

    let client = options.request.fetch.clone().unwrap_or_default();
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        reqwest::header::HeaderValue::from_str(&format!("Bearer {api_key}"))
            .map_err(|error| PiMessagesError::Message(error.to_string()))?,
    );
    headers.insert(
        reqwest::header::ACCEPT,
        reqwest::header::HeaderValue::from_static("text/event-stream"),
    );
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    if let Some(overrides) = provider_headers_to_record(options.request.headers.as_ref()) {
        for (name, value) in overrides {
            if let (Ok(name), Ok(value)) = (
                reqwest::header::HeaderName::from_bytes(name.as_bytes()),
                reqwest::header::HeaderValue::from_str(&value),
            ) {
                headers.insert(name, value);
            }
        }
    }

    let request = client.post(&url).headers(headers).json(&payload);
    let response = match &options.request.signal {
        Some(signal) => tokio::select! {
            result = request.send() => result,
            _ = signal.cancelled() => return Err(PiMessagesError::Message("Request was aborted".into())),
        },
        None => request.send().await,
    }
    .map_err(|error| PiMessagesError::Message(error.to_string()))?;

    options
        .request
        .apply_response_hook(
            &crate::types::ProviderResponse {
                status: response.status().as_u16(),
                headers: headers_to_record(response.headers()),
            },
            model,
        )
        .await
        .map_err(PiMessagesError::Message)?;

    if !response.status().is_success() {
        let status = response.status().as_u16();
        let status_text = response
            .status()
            .canonical_reason()
            .unwrap_or_default()
            .to_owned();
        let body = match &options.request.signal {
            Some(signal) => tokio::select! {
                biased;
                () = signal.cancelled() => return Err(PiMessagesError::Message("Request was aborted".into())),
                result = response.text() => result,
            },
            None => response.text().await,
        }.map_err(|error| PiMessagesError::Message(error.to_string()))?;
        return Err(PiMessagesError::Response(
            create_pi_messages_response_error(model, &url, status, &status_text, &body),
        ));
    }

    read_pi_messages_events(response.bytes_stream(), model, options, converter, sink).await
}

async fn read_pi_messages_events<S, E>(
    mut body: S,
    model: &Model,
    options: &StreamOptions,
    converter: &mut EventConverter,
    sink: &AssistantMessageEventStream,
) -> Result<(), PiMessagesError>
where
    S: futures::Stream<Item = Result<bytes::Bytes, E>> + Unpin,
    E: std::fmt::Display,
{
    use futures::StreamExt;
    // Decode complete SSE frames, not transport chunks: UTF-8 sequences and CRLF pairs
    // may both span chunks. Lossy decoding matches TextDecoder's replacement behavior.
    let mut buffer = Vec::new();
    loop {
        let chunk = match &options.request.signal {
            Some(signal) => tokio::select! {
                biased;
                () = signal.cancelled() => return Err(PiMessagesError::Message("Request was aborted".into())),
                chunk = body.next() => chunk,
            },
            None => body.next().await,
        };
        let Some(chunk) = chunk else { break };
        let chunk = chunk.map_err(|error| PiMessagesError::Message(error.to_string()))?;
        buffer.extend_from_slice(&chunk);
        while let Some(split) = buffer
            .windows(2)
            .position(|pair| pair == b"\n\n")
            .map(|index| index + 2)
            .into_iter()
            .chain(
                buffer
                    .windows(3)
                    .position(|triple| triple == b"\n\r\n")
                    .map(|index| index + 3),
            )
            .min()
        {
            let raw = String::from_utf8_lossy(&buffer[..split]).replace("\r\n", "\n");
            if forward_pi_messages_event(&raw, model, options, converter, sink).await? {
                return Ok(());
            }
            buffer.drain(..split);
        }
    }

    let raw = String::from_utf8_lossy(&buffer).replace("\r\n", "\n");
    if !raw.trim().is_empty()
        && forward_pi_messages_event(&raw, model, options, converter, sink).await?
    {
        return Ok(());
    }

    Err(PiMessagesError::Message(format!(
        "{} stream ended without a terminal event",
        model.provider
    )))
}

async fn forward_pi_messages_event(
    raw: &str,
    model: &Model,
    options: &StreamOptions,
    converter: &mut EventConverter,
    sink: &AssistantMessageEventStream,
) -> Result<bool, PiMessagesError> {
    let Some(value) = parse_pi_messages_event(raw)? else {
        return Ok(false);
    };
    if let Some(hook) = &options.on_provider_stream_event {
        hook(&value, model);
    }
    if let Some(hook) = &options.async_on_provider_stream_event {
        hook(value.clone(), model.clone())
            .await
            .map_err(PiMessagesError::Message)?;
    }
    let event = serde_json::from_value::<PiMessagesEvent>(value)
        .map_err(|error| PiMessagesError::Message(error.to_string()))?;
    let converted = converter.convert(&event);
    let terminal = matches!(
        converted,
        AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. }
    );
    sink.push(converted);
    Ok(terminal)
}

pub fn stream_simple(
    model: &Model,
    context: &Context,
    options: Option<crate::types::SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    let resolved_context = crate::types::transcript::resolve_provider_context(context, false);
    let context = &resolved_context;
    let simple = options.unwrap_or_default();
    let mut stream_options = simple.stream;
    if let Some(reasoning) = simple.reasoning {
        stream_options.extra.insert(
            "reasoning".into(),
            serde_json::to_value(reasoning).unwrap_or(Value::Null),
        );
    }
    if let Some(tool_choice) = simple.tool_choice {
        stream_options.extra.insert(
            "toolChoice".into(),
            serde_json::to_value(tool_choice).unwrap_or(Value::Null),
        );
    }
    stream(model, context, Some(stream_options))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn converter_model() -> Model {
        crate::models_generated::get_builtin_provider_models("anthropic").expect("models")[0]
            .clone()
    }

    #[test]
    fn assistant_cache_hint_is_request_local_and_other_fields_are_preserved() {
        let model = converter_model();
        for hint in [None, Some(false), Some(true)] {
            let mut message = EventConverter::new(&model).partial;
            message.prompt_cache_breakpoint = hint;
            message.content = vec![ContentBlock::text("stable")];
            let original = serde_json::to_value(&message).expect("message");
            let context = Context {
                messages: vec![Message::Assistant(Box::new(message.clone()))],
                ..Context::default()
            };
            let actual = context_payload(&context)["messages"][0].clone();
            let mut expected = original;
            if hint == Some(true) {
                expected
                    .as_object_mut()
                    .expect("object")
                    .remove("promptCacheBreakpoint");
            }
            assert_eq!(actual, expected);
            assert_eq!(message.prompt_cache_breakpoint, hint);
        }
    }

    #[test]
    fn converts_text_and_tool_call_events_like_the_ts_converter() {
        let model = converter_model();
        let mut converter = EventConverter::new(&model);
        converter.convert(&PiMessagesEvent::Start);
        let event = converter.convert(&PiMessagesEvent::TextStart { content_index: 0 });
        assert!(matches!(
            event,
            AssistantMessageEvent::TextStart {
                content_index: 0,
                ..
            }
        ));
        converter.convert(&PiMessagesEvent::TextDelta {
            content_index: 0,
            delta: "Hello".into(),
        });
        let event = converter.convert(&PiMessagesEvent::TextEnd {
            content_index: 0,
            content: "Hello, there!".into(),
            content_signature: None,
        });
        let AssistantMessageEvent::TextEnd {
            content, partial, ..
        } = event
        else {
            panic!("text_end")
        };
        assert_eq!(content, "Hello, there!");
        assert_eq!(partial.content[0], ContentBlock::text("Hello, there!"));

        converter.convert(&PiMessagesEvent::ToolcallStart {
            content_index: 1,
            id: "call".into(),
            tool_name: "t".into(),
        });
        converter.convert(&PiMessagesEvent::ToolcallDelta {
            content_index: 1,
            delta: "{\"a\":".into(),
        });
        let event = converter.convert(&PiMessagesEvent::ToolcallEnd {
            content_index: 1,
            tool_call: ToolCall {
                id: "call".into(),
                name: "t".into(),
                arguments: [("a".to_owned(), Value::from(1))].into_iter().collect(),
                incomplete: None,
                error_message: None,
                thought_signature: None,
                namespace: None,
            },
        });
        let AssistantMessageEvent::ToolcallEnd { tool_call, .. } = event else {
            panic!("toolcall_end")
        };
        assert_eq!(tool_call.arguments.get("a"), Some(&Value::from(1)));
    }

    #[test]
    fn done_events_carry_usage_and_response_id() {
        let model = converter_model();
        let mut converter = EventConverter::new(&model);
        let event = converter.convert(&PiMessagesEvent::Done {
            reason: "stop".into(),
            usage: Usage {
                input: 3,
                output: 4,
                ..Usage::default()
            },
            response_id: Some("resp".into()),
            provider_thinking_level: None,
            rewrite: None,
        });
        let AssistantMessageEvent::Done { message, .. } = event else {
            panic!("done")
        };
        assert_eq!(message.usage.input, 3);
        assert_eq!(message.response_id.as_deref(), Some("resp"));
        assert_eq!(message.stop_reason, StopReason::Stop);
    }

    #[test]
    fn parses_only_data_lines_and_skips_done_sentinels() {
        assert_eq!(
            parse_pi_messages_event("data: [DONE]").expect("sentinel"),
            None
        );
        assert_eq!(
            parse_pi_messages_event("event: x\n").expect("no data"),
            None
        );
        assert_eq!(
            parse_pi_messages_event("data: {\"type\":\"start\"}").expect("event"),
            Some(json!({ "type": "start" }))
        );
    }

    #[test]
    fn diagnostic_truncation_counts_utf16_without_slicing_utf8() {
        let value = format!("{}z", "\u{1f600}".repeat(4096));
        let truncated = truncate_diagnostic_string(&value);
        assert_eq!(truncated, format!("{}\u{2026}", "\u{1f600}".repeat(4096)));
        let split_surrogate = format!("{}\u{1f600}", "x".repeat(8191));
        assert_eq!(
            truncate_diagnostic_string(&split_surrogate),
            format!("{}\u{fffd}\u{2026}", "x".repeat(8191))
        );
    }

    #[tokio::test]
    async fn bytewise_chunks_preserve_utf8_crlf_and_unterminated_terminal_event() {
        let model = converter_model();
        let mut converter = EventConverter::new(&model);
        let sink = create_assistant_message_event_stream();
        let done = serde_json::to_string(&PiMessagesEvent::Done {
            reason: "stop".into(),
            usage: Usage::default(),
            response_id: None,
            provider_thinking_level: None,
            rewrite: None,
        })
        .expect("done JSON");
        let wire = format!(
            "data: {{\"type\":\"text_start\",\"contentIndex\":0}}\r\n\r\ndata: {{\"type\":\"text_delta\",\"contentIndex\":0,\"delta\":\"\u{1f600}\u{e9}\"}}\r\n\r\ndata: {done}"
        );
        let chunks = futures::stream::iter(
            wire.bytes()
                .map(|byte| Ok::<_, std::io::Error>(bytes::Bytes::from(vec![byte]))),
        );
        read_pi_messages_events(
            chunks,
            &model,
            &StreamOptions::default(),
            &mut converter,
            &sink,
        )
        .await
        .expect("stream");
        assert_eq!(
            converter.partial.content,
            vec![ContentBlock::text("\u{1f600}\u{e9}")]
        );
        assert_eq!(converter.partial.stop_reason, StopReason::Stop);
    }

    #[tokio::test]
    async fn malformed_json_fails_instead_of_skipping_to_terminal_event() {
        let model = converter_model();
        let mut converter = EventConverter::new(&model);
        let sink = create_assistant_message_event_stream();
        let done = serde_json::to_string(&PiMessagesEvent::Done {
            reason: "stop".into(),
            usage: Usage::default(),
            response_id: None,
            provider_thinking_level: None,
            rewrite: None,
        })
        .expect("done JSON");
        let chunks = futures::stream::iter([Ok::<_, std::io::Error>(bytes::Bytes::from(format!(
            "data: {{broken}}\n\ndata: {done}\n\n"
        )))]);
        let result = read_pi_messages_events(
            chunks,
            &model,
            &StreamOptions::default(),
            &mut converter,
            &sink,
        )
        .await;
        assert!(matches!(result, Err(PiMessagesError::Message(_))));
    }

    #[tokio::test]
    async fn provider_observer_is_awaited_before_conversion_and_preserves_raw_fields() {
        let model = converter_model();
        let mut converter = EventConverter::new(&model);
        let sink = create_assistant_message_event_stream();
        let (entered_tx, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
        let release = std::sync::Arc::new(tokio::sync::Notify::new());
        let hook_release = release.clone();
        let options = StreamOptions {
            async_on_provider_stream_event: Some(std::sync::Arc::new(
                move |value, observed_model| {
                    let entered_tx = entered_tx.clone();
                    let hook_release = hook_release.clone();
                    Box::pin(async move {
                        let released = hook_release.notified();
                        tokio::pin!(released);
                        released.as_mut().enable();
                        entered_tx
                            .send((value, observed_model.id))
                            .expect("observer notification");
                        released.await;
                        Err("observer failure".into())
                    })
                },
            )),
            ..StreamOptions::default()
        };
        let observe = forward_pi_messages_event(
            "data: {\"type\":\"text_start\",\"contentIndex\":0,\"opaque\":17}",
            &model,
            &options,
            &mut converter,
            &sink,
        );
        let trigger = async {
            let (value, id) = entered_rx.recv().await.expect("observer entered");
            assert_eq!(value["opaque"], 17);
            assert_eq!(id, model.id);
            release.notify_one();
        };
        let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(observe, trigger)
        })
        .await
        .expect("bounded observer");
        assert!(matches!(result, Err(PiMessagesError::Message(_))));
        assert!(converter.partial.content.is_empty());
    }

    #[tokio::test]
    async fn cancellation_interrupts_success_and_error_body_reads_and_reports_aborted() {
        for status in [200_u16, 400] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("listener");
            let address = listener.local_addr().expect("address");
            let app = axum::Router::new().route(
                "/messages",
                axum::routing::post(move || async move {
                    axum::http::Response::builder()
                        .status(status)
                        .body(axum::body::Body::from_stream(futures::stream::pending::<
                            Result<bytes::Bytes, std::io::Error>,
                        >()))
                        .expect("response")
                }),
            );
            let server =
                tokio::spawn(async move { axum::serve(listener, app).await.expect("server") });
            let controller = crate::utils::abort::AbortController::new();
            let (headers_tx, mut headers_rx) = tokio::sync::mpsc::unbounded_channel();
            let mut model = converter_model();
            model.base_url = format!("http://{address}");
            let mut options = StreamOptions::default();
            options.request.api_key = Some("test".into());
            options.request.signal = Some(controller.signal());
            options.request.on_response = Some(std::sync::Arc::new(move |_, _| {
                headers_tx.send(()).expect("headers");
            }));
            let sink = stream(&model, &Context::default(), Some(options));
            let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                headers_rx.recv().await.expect("response headers received");
                controller.abort(None);
                sink.result().await.expect("terminal message")
            })
            .await;
            server.abort();
            let message = result.expect("bounded cancellation");
            assert_eq!(message.stop_reason, StopReason::Aborted);
        }
    }

    #[test]
    fn response_errors_format_status_message_and_code() {
        let model = converter_model();
        let error = create_pi_messages_response_error(
            &model,
            "http://127.0.0.1/messages",
            400,
            "Bad Request",
            "{\"error\":{\"message\":\"nope\",\"code\":\"E1\"}}",
        );
        assert_eq!(error.message, "400 Bad Request: nope (E1)");
        assert_eq!(error.code.as_deref(), Some("E1"));
        assert_eq!(
            error.diagnostic_details.get("status"),
            Some(&Value::from(400))
        );
    }

    #[test]
    fn cache_retention_env_opt_in_is_mapped() {
        let env: ProviderEnv = [("PI_CACHE_RETENTION".to_owned(), "long".to_owned())]
            .into_iter()
            .collect();
        assert_eq!(
            resolve_cache_retention(None, Some(&env)),
            Some(CacheRetention::Long)
        );
        assert_eq!(resolve_cache_retention(None, None), None);
        assert_eq!(
            resolve_cache_retention(Some(CacheRetention::None), Some(&env)),
            Some(CacheRetention::None)
        );
    }
}
