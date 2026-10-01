//! Port of senpi packages/agent/src/proxy.ts.
//!
//! Proxy stream function for apps that route LLM calls through a server.
//! The server manages auth and proxies requests to LLM providers.

use std::collections::BTreeMap;

use futures::StreamExt;
use maho_ai::model::Model;
use maho_ai::types::{
    AssistantMessage, AssistantMessageEvent, ContentBlock, Context, ErrorReason, StopReason, TextContent,
    ThinkingContent, ToolCall, Usage,
};
use maho_ai::utils::abort::AbortSignal;
use maho_ai::utils::event_stream::{AssistantMessageEventStream, create_assistant_message_event_stream};
use maho_ai::utils::json_parse::parse_streaming_json;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::assistant_terminal_state::now_ms;

/// Proxy event types - server sends these with partial field stripped to reduce bandwidth.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum ProxyAssistantMessageEvent {
    Start,
    TextStart {
        content_index: usize,
    },
    TextDelta {
        content_index: usize,
        delta: String,
    },
    TextEnd {
        content_index: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content_signature: Option<String>,
    },
    ThinkingStart {
        content_index: usize,
    },
    ThinkingDelta {
        content_index: usize,
        delta: String,
    },
    ThinkingEnd {
        content_index: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content_signature: Option<String>,
    },
    ToolcallStart {
        content_index: usize,
        id: String,
        tool_name: String,
    },
    ToolcallDelta {
        content_index: usize,
        delta: String,
    },
    /// Servers SHOULD send the final ToolCall; flagged incomplete calls emit no argument deltas, so
    /// delta reconstruction alone cannot represent them, and metadata such as `namespace` only
    /// exists on the final call.
    ToolcallEnd {
        content_index: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_call: Option<ToolCall>,
    },
    Done {
        reason: StopReason,
        usage: Usage,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_thinking_level: Option<String>,
    },
    Error {
        reason: ErrorReason,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_message: Option<String>,
        usage: Usage,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_thinking_level: Option<String>,
    },
}

/// `ProxySerializableStreamOptions`.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxySerializableStreamOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampling_params: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<maho_ai::types::ThinkingLevel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_selection: Option<maho_ai::types::ThinkingSelection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_retention: Option<maho_ai::types::CacheRetention>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<maho_ai::types::ProviderHeaders>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<maho_ai::types::Transport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_budgets: Option<maho_ai::types::ThinkingBudgets>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_retry_delay_ms: Option<u64>,
}

impl ProxySerializableStreamOptions {
    /// The subset of a simple request the proxy accepts, taken field-for-field.
    pub fn from_simple(options: &maho_ai::types::SimpleStreamOptions) -> Self {
        Self {
            temperature: options.stream.temperature,
            sampling_params: options.stream.sampling_params.clone(),
            max_tokens: options.stream.max_tokens,
            reasoning: options.reasoning,
            thinking_selection: options.thinking_selection.clone(),
            cache_retention: options.stream.cache_retention,
            session_id: options.stream.session_id.clone(),
            headers: options.stream.request.headers.clone(),
            metadata: options.stream.metadata.clone(),
            transport: options.stream.transport,
            thinking_budgets: options.thinking_budgets.clone(),
            max_retry_delay_ms: options.stream.request.max_retry_delay_ms,
        }
    }
}

/// `ProxyStreamOptions`.
#[derive(Debug, Clone, Default)]
pub struct ProxyStreamOptions {
    pub serializable: ProxySerializableStreamOptions,
    /// Local abort signal for the proxy request
    pub signal: Option<AbortSignal>,
    /// Auth token for the proxy server
    pub auth_token: String,
    /// Proxy server URL (e.g., "https://genai.example.com")
    pub proxy_url: String,
}

/// Stream function that proxies through a server instead of calling LLM providers directly.
/// The server strips the partial field from delta events to reduce bandwidth.
/// We reconstruct the partial message client-side.
pub fn stream_proxy(model: &Model, context: &Context, options: ProxyStreamOptions) -> AssistantMessageEventStream {
    let stream = create_assistant_message_event_stream();
    let model = model.clone();
    let context = context.clone();
    let task_stream = stream.clone();
    tokio::spawn(async move {
        run_proxy_stream(&task_stream, &model, &context, &options).await;
    });
    stream
}

async fn run_proxy_stream(
    stream: &AssistantMessageEventStream,
    model: &Model,
    context: &Context,
    options: &ProxyStreamOptions,
) {
    let mut partial = AssistantMessage {
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
    let mut partial_json: BTreeMap<usize, String> = BTreeMap::new();
    let mut saw_terminal_event = false;

    match read_proxy_body(stream, &mut partial, &mut partial_json, &mut saw_terminal_event, model, context, options)
        .await
    {
        Ok(()) => {
            if !saw_terminal_event {
                // A clean EOF without a done/error event means the server dropped the
                // response mid-stream. Surface it as an error instead of leaving
                // consumers waiting on a result that never arrives.
                partial.stop_reason = StopReason::Error;
                partial.error_message = Some("Connection closed by proxy server before the response completed".to_owned());
                stream.push(AssistantMessageEvent::Error { reason: ErrorReason::Error, error: partial });
            }
        }
        Err(error) => {
            let aborted = options.signal.as_ref().is_some_and(AbortSignal::aborted);
            partial.stop_reason = if aborted { StopReason::Aborted } else { StopReason::Error };
            partial.error_message = Some(error.message);
            stream.push(AssistantMessageEvent::Error {
                reason: if aborted { ErrorReason::Aborted } else { ErrorReason::Error },
                error: partial,
            });
        }
    }
    stream.end(None);
}

struct ProxyError {
    message: String,
}

async fn read_proxy_body(
    stream: &AssistantMessageEventStream,
    partial: &mut AssistantMessage,
    partial_json: &mut BTreeMap<usize, String>,
    saw_terminal_event: &mut bool,
    model: &Model,
    context: &Context,
    options: &ProxyStreamOptions,
) -> Result<(), ProxyError> {
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "model": model,
        "context": context,
        "options": options.serializable,
    });
    let response = client
        .post(format!("{}/api/stream", options.proxy_url))
        .header("Authorization", format!("Bearer {}", options.auth_token))
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .map_err(|error| ProxyError { message: error.to_string() })?;

    if !response.status().is_success() {
        let status = response.status();
        let mut error_message =
            format!("Proxy error: {} {}", status.as_u16(), status.canonical_reason().unwrap_or_default());
        if let Ok(text) = response.text().await
            && let Ok(error_data) = serde_json::from_str::<Value>(&text)
            && let Some(error) = error_data.get("error").and_then(Value::as_str)
        {
            error_message = format!("Proxy error: {error}");
        }
        return Err(ProxyError { message: error_message });
    }

    let mut chunks = response.bytes_stream();
    let mut buffer: Vec<u8> = Vec::new();
    loop {
        let aborted = async {
            match options.signal.as_ref() {
                Some(signal) => signal.cancelled().await,
                None => std::future::pending::<()>().await,
            }
        };
        let next = tokio::select! {
            biased;
            _ = aborted => None,
            next = chunks.next() => next,
        };
        let Some(chunk) = next else {
            if options.signal.as_ref().is_some_and(AbortSignal::aborted) {
                return Err(ProxyError { message: "Request aborted by user".to_owned() });
            }
            break;
        };
        let chunk = chunk.map_err(|error| ProxyError { message: error.to_string() })?;
        if options.signal.as_ref().is_some_and(AbortSignal::aborted) {
            return Err(ProxyError { message: "Request aborted by user".to_owned() });
        }
        buffer.extend_from_slice(&chunk);
        if let Some(position) = buffer.iter().rposition(|byte| *byte == b'\n') {
            let remainder = buffer.split_off(position + 1);
            let complete = std::mem::replace(&mut buffer, remainder);
            for line in complete.split(|byte| *byte == b'\n') {
                if line.is_empty() {
                    continue;
                }
                let line = String::from_utf8_lossy(line);
                if let Some(event) = process_line(&line, partial, partial_json) {
                    if matches!(event, AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. }) {
                        *saw_terminal_event = true;
                    }
                    stream.push(event);
                }
            }
        }
    }

    if options.signal.as_ref().is_some_and(AbortSignal::aborted) {
        return Err(ProxyError { message: "Request aborted by user".to_owned() });
    }

    // The final event may not be newline-terminated; flush the decoder and
    // process whatever is left in the buffer.
    if !buffer.is_empty() {
        let line = String::from_utf8_lossy(&buffer).into_owned();
        if let Some(event) = process_line(&line, partial, partial_json) {
            if matches!(event, AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. }) {
                *saw_terminal_event = true;
            }
            stream.push(event);
        }
    }

    Ok(())
}

fn process_line(
    line: &str,
    partial: &mut AssistantMessage,
    partial_json: &mut BTreeMap<usize, String>,
) -> Option<AssistantMessageEvent> {
    let data = line.strip_prefix("data: ")?;
    let data = data.trim();
    if data.is_empty() {
        return None;
    }
    let proxy_event: ProxyAssistantMessageEvent = match serde_json::from_str(data) {
        Ok(event) => event,
        Err(error) => {
            eprintln!("Unhandled proxy event: {error}");
            return None;
        }
    };
    process_proxy_event(proxy_event, partial, partial_json)
}

/// TS assigns into a possibly-sparse content array (`content[i] = block`). This port pads any gap
/// with an empty text block: a JSON hole would serialize as `null` and no proxy server emits
/// out-of-order content indices.
fn ensure_block(content: &mut Vec<ContentBlock>, index: usize) {
    while content.len() <= index {
        content.push(ContentBlock::Text(TextContent::default()));
    }
}

/// Process a proxy event and update the partial message.
fn process_proxy_event(
    proxy_event: ProxyAssistantMessageEvent,
    partial: &mut AssistantMessage,
    partial_json: &mut BTreeMap<usize, String>,
) -> Option<AssistantMessageEvent> {
    match proxy_event {
        ProxyAssistantMessageEvent::Start => Some(AssistantMessageEvent::Start { partial: partial.clone() }),

        ProxyAssistantMessageEvent::TextStart { content_index } => {
            ensure_block(&mut partial.content, content_index);
            partial.content[content_index] = ContentBlock::Text(TextContent::default());
            Some(AssistantMessageEvent::TextStart { content_index, partial: partial.clone() })
        }

        ProxyAssistantMessageEvent::TextDelta { content_index, delta } => {
            let content = partial.content.get_mut(content_index)?;
            match content {
                ContentBlock::Text(text) => {
                    text.text.push_str(&delta);
                    Some(AssistantMessageEvent::TextDelta { content_index, delta, partial: partial.clone() })
                }
                _ => None,
            }
        }

        ProxyAssistantMessageEvent::TextEnd { content_index, content_signature } => {
            let content = partial.content.get_mut(content_index)?;
            match content {
                ContentBlock::Text(text) => {
                    text.text_signature = content_signature;
                    let text = text.text.clone();
                    Some(AssistantMessageEvent::TextEnd { content_index, content: text, partial: partial.clone() })
                }
                _ => None,
            }
        }

        ProxyAssistantMessageEvent::ThinkingStart { content_index } => {
            ensure_block(&mut partial.content, content_index);
            partial.content[content_index] = ContentBlock::Thinking(ThinkingContent::default());
            Some(AssistantMessageEvent::ThinkingStart { content_index, partial: partial.clone() })
        }

        ProxyAssistantMessageEvent::ThinkingDelta { content_index, delta } => {
            let content = partial.content.get_mut(content_index)?;
            match content {
                ContentBlock::Thinking(thinking) => {
                    thinking.thinking.push_str(&delta);
                    Some(AssistantMessageEvent::ThinkingDelta { content_index, delta, partial: partial.clone() })
                }
                _ => None,
            }
        }

        ProxyAssistantMessageEvent::ThinkingEnd { content_index, content_signature } => {
            let content = partial.content.get_mut(content_index)?;
            match content {
                ContentBlock::Thinking(thinking) => {
                    thinking.thinking_signature = content_signature;
                    let text = thinking.thinking.clone();
                    Some(AssistantMessageEvent::ThinkingEnd { content_index, content: text, partial: partial.clone() })
                }
                _ => None,
            }
        }

        ProxyAssistantMessageEvent::ToolcallStart { content_index, id, tool_name } => {
            ensure_block(&mut partial.content, content_index);
            partial.content[content_index] = ContentBlock::ToolCall(ToolCall {
                id,
                name: tool_name,
                arguments: Map::new(),
                incomplete: None,
                error_message: None,
                thought_signature: None,
                namespace: None,
            });
            partial_json.insert(content_index, String::new());
            Some(AssistantMessageEvent::ToolcallStart { content_index, partial: partial.clone() })
        }

        ProxyAssistantMessageEvent::ToolcallDelta { content_index, delta } => {
            let entry = partial_json.entry(content_index).or_default();
            entry.push_str(&delta);
            let parsed = parse_streaming_json(Some(entry));
            let arguments = parsed.as_object().cloned().unwrap_or_default();
            let content = partial.content.get_mut(content_index)?;
            match content {
                ContentBlock::ToolCall(tool_call) => {
                    tool_call.arguments = arguments;
                    Some(AssistantMessageEvent::ToolcallDelta { content_index, delta, partial: partial.clone() })
                }
                _ => None,
            }
        }

        ProxyAssistantMessageEvent::ToolcallEnd { content_index, tool_call } => {
            let content = partial.content.get_mut(content_index)?;
            match content {
                ContentBlock::ToolCall(current) => {
                    let tool_call = tool_call.unwrap_or_else(|| current.clone());
                    partial.content[content_index] = ContentBlock::ToolCall(tool_call.clone());
                    partial_json.remove(&content_index);
                    Some(AssistantMessageEvent::ToolcallEnd { content_index, tool_call, partial: partial.clone() })
                }
                _ => None,
            }
        }

        ProxyAssistantMessageEvent::Done { reason, usage, provider_thinking_level } => {
            partial.stop_reason = reason;
            partial.usage = usage;
            if provider_thinking_level.is_some() {
                partial.provider_thinking_level = provider_thinking_level;
            }
            Some(AssistantMessageEvent::Done { reason: done_reason(reason), message: partial.clone() })
        }

        ProxyAssistantMessageEvent::Error { reason, error_message, usage, provider_thinking_level } => {
            partial.stop_reason = error_stop_reason(reason);
            partial.error_message = error_message;
            partial.usage = usage;
            if provider_thinking_level.is_some() {
                partial.provider_thinking_level = provider_thinking_level;
            }
            Some(AssistantMessageEvent::Error { reason, error: partial.clone() })
        }
    }
}

fn done_reason(reason: StopReason) -> maho_ai::types::DoneReason {
    match reason {
        StopReason::Length => maho_ai::types::DoneReason::Length,
        StopReason::ToolUse => maho_ai::types::DoneReason::ToolUse,
        StopReason::Deferred => maho_ai::types::DoneReason::Deferred,
        _ => maho_ai::types::DoneReason::Stop,
    }
}

fn error_stop_reason(reason: ErrorReason) -> StopReason {
    match reason {
        ErrorReason::Aborted => StopReason::Aborted,
        ErrorReason::Error => StopReason::Error,
    }
}
