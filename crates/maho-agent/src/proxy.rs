//! Port of senpi packages/agent/src/proxy.ts.
//!
//! Proxy stream function for apps that route LLM calls through a server.
//! The server manages auth and proxies requests to LLM providers.

use std::collections::BTreeMap;

use futures::StreamExt;
use maho_ai::model::Model;
use maho_ai::types::{
    AssistantMessage, AssistantMessageEvent, ContentBlock, Context, ErrorReason, StopReason,
    TextContent, ThinkingContent, ToolCall, Usage,
};
use maho_ai::utils::abort::AbortSignal;
use maho_ai::utils::event_stream::{
    AssistantMessageEventStream, create_assistant_message_event_stream,
};
use maho_ai::utils::json_parse::parse_streaming_json;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::assistant_terminal_state::now_ms;

/// Proxy event types - server sends these with partial field stripped to reduce bandwidth.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
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
        #[serde(deserialize_with = "deserialize_proxy_usage")]
        usage: Usage,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_thinking_level: Option<String>,
    },
    Error {
        reason: ErrorReason,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_cause: Option<maho_ai::types::AssistantErrorCause>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_message: Option<String>,
        #[serde(deserialize_with = "deserialize_proxy_usage")]
        usage: Usage,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_thinking_level: Option<String>,
    },
}

fn deserialize_proxy_usage<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Usage, D::Error> {
    // Internally tagged enums buffer fields through serde's Content. With arbitrary_precision,
    // decimal costs become private Number maps there; Value restores those to JSON numbers before
    // Usage's f64 fields are decoded. This also covers callers deserializing the public event type.
    let value = Value::deserialize(deserializer)?;
    serde_json::from_value(value).map_err(serde::de::Error::custom)
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
pub fn stream_proxy(
    model: &Model,
    context: &Context,
    options: ProxyStreamOptions,
) -> AssistantMessageEventStream {
    let stream = create_assistant_message_event_stream();
    let model = model.clone();
    let context = context.clone();
    let task_stream = stream.clone();
    let controller = maho_ai::utils::abort::AbortController::new();
    let signal = controller.signal();
    stream.set_cancellation_handler(Box::new(move || {
        Box::pin(async move {
            controller.abort(None);
            Ok(())
        })
    }));
    let task = tokio::spawn(async move {
        tokio::select! {
            biased;
            _ = signal.cancelled() => {},
            _ = run_proxy_stream(&task_stream, &model, &context, &options) => {},
        }
    });
    stream.set_cancellation_task(task);
    stream
}

async fn run_proxy_stream(
    stream: &AssistantMessageEventStream,
    model: &Model,
    context: &Context,
    options: &ProxyStreamOptions,
) {
    let mut partial = AssistantMessage {
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
        prompt_cache_breakpoint: None,
        prompt_cache_diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::Pending,
        stop_details: None,
        deferred: None,
        error_message: None,
        error_cause: None,
        abort_source: None,
        failure_source: None,
        raw_stop_reason: None,
        end_turn: None,
        timing: None,
        timestamp: now_ms(),
    };
    let mut partial_json: BTreeMap<usize, String> = BTreeMap::new();
    let mut saw_terminal_event = false;

    match read_proxy_body(
        stream,
        &mut partial,
        &mut partial_json,
        &mut saw_terminal_event,
        model,
        context,
        options,
    )
    .await
    {
        Ok(()) => {
            if !saw_terminal_event {
                // A clean EOF without a done/error event means the server dropped the
                // response mid-stream. Surface it as an error instead of leaving
                // consumers waiting on a result that never arrives.
                partial.stop_reason = StopReason::Error;
                partial.error_message = Some(
                    "Connection closed by proxy server before the response completed".to_owned(),
                );
                stream.push(AssistantMessageEvent::Error {
                    reason: ErrorReason::Error,
                    error: partial,
                });
            }
        }
        Err(error) => {
            let aborted = options.signal.as_ref().is_some_and(AbortSignal::aborted);
            partial.stop_reason = if aborted {
                StopReason::Aborted
            } else {
                StopReason::Error
            };
            partial.error_message = Some(error.message);
            stream.push(AssistantMessageEvent::Error {
                reason: if aborted {
                    ErrorReason::Aborted
                } else {
                    ErrorReason::Error
                },
                error: partial,
            });
        }
    }
    stream.end(None);
}

#[derive(Debug)]
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
    // PERF-DIVERGENCE(senpi proxy.ts): upstream uses fetch's global connection pool and one JSON.stringify; we reuse a static reqwest client (keep-alive, one TLS setup) and serialize a borrowed struct once instead of building a Value tree. Request bytes are identical (same key order).
    static CLIENT: std::sync::LazyLock<reqwest::Client> =
        std::sync::LazyLock::new(reqwest::Client::new);
    #[derive(Serialize)]
    struct ProxyRequestBody<'a> {
        model: &'a Model,
        context: &'a Context,
        options: &'a ProxySerializableStreamOptions,
    }
    let client = &*CLIENT;
    let body = serde_json::to_vec(&ProxyRequestBody {
        model,
        context,
        options: &options.serializable,
    })
    .map_err(|error| ProxyError {
        message: error.to_string(),
    })?;
    let cancelled = || async {
        match options.signal.as_ref() {
            Some(signal) => signal.cancelled().await,
            None => std::future::pending::<()>().await,
        }
    };
    let request = client
        .post(format!("{}/api/stream", options.proxy_url))
        .header("Authorization", format!("Bearer {}", options.auth_token))
        .header("Content-Type", "application/json")
        .body(body)
        .send();
    let response = tokio::select! {
        biased;
        _ = cancelled() => return Err(ProxyError { message: "Request aborted by user".to_owned() }),
        response = request => response.map_err(|error| ProxyError { message: error.to_string() })?,
    };

    if !response.status().is_success() {
        let status = response.status();
        let mut error_message = format!(
            "Proxy error: {} {}",
            status.as_u16(),
            status.canonical_reason().unwrap_or_default()
        );
        let text = tokio::select! {
            biased;
            _ = cancelled() => return Err(ProxyError { message: error_message }),
            text = response.text() => text,
        };
        if let Ok(text) = text
            && let Ok(error_data) = serde_json::from_str::<Value>(&text)
            && let Some(error) = error_data.get("error").and_then(Value::as_str)
        {
            error_message = format!("Proxy error: {error}");
        }
        return Err(ProxyError {
            message: error_message,
        });
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
                return Err(ProxyError {
                    message: "Request aborted by user".to_owned(),
                });
            }
            break;
        };
        let chunk = chunk.map_err(|error| ProxyError {
            message: error.to_string(),
        })?;
        if options.signal.as_ref().is_some_and(AbortSignal::aborted) {
            return Err(ProxyError {
                message: "Request aborted by user".to_owned(),
            });
        }
        // PERF-DIVERGENCE(senpi proxy.ts): upstream splits the decoded text per chunk; we search only the new chunk for the last newline and process complete lines in place, avoiding a buffer split_off/replace copy and a rescan of the old tail. The same lines are processed in the same order.
        buffer.extend_from_slice(&chunk);
        if let Some(relative) = chunk.iter().rposition(|byte| *byte == b'\n') {
            let position = buffer.len() - chunk.len() + relative;
            for line in buffer[..=position].split(|byte| *byte == b'\n') {
                if line.is_empty() {
                    continue;
                }
                let line = String::from_utf8_lossy(line);
                if let Some(event) = process_line(&line, partial, partial_json)? {
                    if matches!(
                        event,
                        AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. }
                    ) {
                        *saw_terminal_event = true;
                    }
                    stream.push(event);
                }
            }
            buffer.drain(..=position);
        }
    }

    if options.signal.as_ref().is_some_and(AbortSignal::aborted) {
        return Err(ProxyError {
            message: "Request aborted by user".to_owned(),
        });
    }

    // The final event may not be newline-terminated; flush the decoder and
    // process whatever is left in the buffer.
    if !buffer.is_empty() {
        let line = String::from_utf8_lossy(&buffer).into_owned();
        if let Some(event) = process_line(&line, partial, partial_json)? {
            if matches!(
                event,
                AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. }
            ) {
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
) -> Result<Option<AssistantMessageEvent>, ProxyError> {
    let Some(data) = line.strip_prefix("data: ") else {
        return Ok(None);
    };
    let data = data.trim();
    if data.is_empty() {
        return Ok(None);
    }
    let proxy_event: ProxyAssistantMessageEvent =
        serde_json::from_str(data).map_err(|error| ProxyError {
            message: error.to_string(),
        })?;
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
) -> Result<Option<AssistantMessageEvent>, ProxyError> {
    Ok(match proxy_event {
        ProxyAssistantMessageEvent::Start => Some(AssistantMessageEvent::Start {
            partial: partial.clone(),
        }),

        ProxyAssistantMessageEvent::TextStart { content_index } => {
            ensure_block(&mut partial.content, content_index);
            partial.content[content_index] = ContentBlock::Text(TextContent::default());
            Some(AssistantMessageEvent::TextStart {
                content_index,
                partial: partial.clone(),
            })
        }

        ProxyAssistantMessageEvent::TextDelta {
            content_index,
            delta,
        } => {
            let invalid = || ProxyError {
                message: "Received text_delta for non-text content".to_owned(),
            };
            let content = partial.content.get_mut(content_index).ok_or_else(invalid)?;
            match content {
                ContentBlock::Text(text) => {
                    text.text.push_str(&delta);
                    Some(AssistantMessageEvent::TextDelta {
                        content_index,
                        delta,
                        partial: partial.clone(),
                    })
                }
                _ => return Err(invalid()),
            }
        }

        ProxyAssistantMessageEvent::TextEnd {
            content_index,
            content_signature,
        } => {
            let invalid = || ProxyError {
                message: "Received text_end for non-text content".to_owned(),
            };
            let content = partial.content.get_mut(content_index).ok_or_else(invalid)?;
            match content {
                ContentBlock::Text(text) => {
                    text.text_signature = content_signature;
                    let text = text.text.clone();
                    Some(AssistantMessageEvent::TextEnd {
                        content_index,
                        content: text,
                        partial: partial.clone(),
                    })
                }
                _ => return Err(invalid()),
            }
        }

        ProxyAssistantMessageEvent::ThinkingStart { content_index } => {
            ensure_block(&mut partial.content, content_index);
            partial.content[content_index] = ContentBlock::Thinking(ThinkingContent::default());
            Some(AssistantMessageEvent::ThinkingStart {
                content_index,
                partial: partial.clone(),
            })
        }

        ProxyAssistantMessageEvent::ThinkingDelta {
            content_index,
            delta,
        } => {
            let invalid = || ProxyError {
                message: "Received thinking_delta for non-thinking content".to_owned(),
            };
            let content = partial.content.get_mut(content_index).ok_or_else(invalid)?;
            match content {
                ContentBlock::Thinking(thinking) => {
                    thinking.thinking.push_str(&delta);
                    Some(AssistantMessageEvent::ThinkingDelta {
                        content_index,
                        delta,
                        partial: partial.clone(),
                    })
                }
                _ => return Err(invalid()),
            }
        }

        ProxyAssistantMessageEvent::ThinkingEnd {
            content_index,
            content_signature,
        } => {
            let invalid = || ProxyError {
                message: "Received thinking_end for non-thinking content".to_owned(),
            };
            let content = partial.content.get_mut(content_index).ok_or_else(invalid)?;
            match content {
                ContentBlock::Thinking(thinking) => {
                    thinking.thinking_signature = content_signature;
                    let text = thinking.thinking.clone();
                    Some(AssistantMessageEvent::ThinkingEnd {
                        content_index,
                        content: text,
                        partial: partial.clone(),
                    })
                }
                _ => return Err(invalid()),
            }
        }

        ProxyAssistantMessageEvent::ToolcallStart {
            content_index,
            id,
            tool_name,
        } => {
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
            Some(AssistantMessageEvent::ToolcallStart {
                content_index,
                partial: partial.clone(),
            })
        }

        ProxyAssistantMessageEvent::ToolcallDelta {
            content_index,
            delta,
        } => {
            let invalid = || ProxyError {
                message: "Received toolcall_delta for non-toolCall content".to_owned(),
            };
            let content = partial.content.get_mut(content_index).ok_or_else(invalid)?;
            match content {
                ContentBlock::ToolCall(tool_call) => {
                    let entry = partial_json.entry(content_index).or_default();
                    entry.push_str(&delta);
                    let parsed = parse_streaming_json(Some(entry));
                    tool_call.arguments = parsed.as_object().cloned().unwrap_or_default();
                    Some(AssistantMessageEvent::ToolcallDelta {
                        content_index,
                        delta,
                        partial: partial.clone(),
                    })
                }
                _ => return Err(invalid()),
            }
        }

        ProxyAssistantMessageEvent::ToolcallEnd {
            content_index,
            tool_call,
        } => {
            let Some(content) = partial.content.get_mut(content_index) else {
                return Ok(None);
            };
            match content {
                ContentBlock::ToolCall(current) => {
                    let tool_call = tool_call.unwrap_or_else(|| current.clone());
                    partial.content[content_index] = ContentBlock::ToolCall(tool_call.clone());
                    partial_json.remove(&content_index);
                    Some(AssistantMessageEvent::ToolcallEnd {
                        content_index,
                        tool_call,
                        partial: partial.clone(),
                    })
                }
                _ => None,
            }
        }

        ProxyAssistantMessageEvent::Done {
            reason,
            usage,
            provider_thinking_level,
        } => {
            partial.stop_reason = reason;
            partial.usage = usage;
            if provider_thinking_level.is_some() {
                partial.provider_thinking_level = provider_thinking_level;
            }
            Some(AssistantMessageEvent::Done {
                reason: done_reason(reason),
                message: partial.clone(),
            })
        }

        ProxyAssistantMessageEvent::Error {
            reason,
            error_message,
            error_cause,
            usage,
            provider_thinking_level,
        } => {
            partial.stop_reason = error_stop_reason(reason);
            partial.error_message = error_message;
            partial.error_cause = error_cause;
            partial.usage = usage;
            if provider_thinking_level.is_some() {
                partial.provider_thinking_level = provider_thinking_level;
            }
            Some(AssistantMessageEvent::Error {
                reason,
                error: partial.clone(),
            })
        }
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ai::utils::abort::AbortController;
    use std::io::{Read, Write};
    use std::net::{Shutdown, TcpListener};
    use std::time::Duration;

    fn model() -> Model {
        serde_json::from_value(serde_json::json!({
            "id": "proxy-test", "name": "proxy-test", "api": "openai-responses",
            "provider": "openai", "baseUrl": "https://example.invalid", "reasoning": false,
            "input": ["text"], "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
            "contextWindow": 8192, "maxTokens": 2048
        }))
        .expect("model fixture")
    }

    async fn response(body: String) -> AssistantMessage {
        // Given a complete SSE response on an isolated wire-level server.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("address");
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept");
            socket
                .set_read_timeout(Some(Duration::from_secs(10)))
                .expect("read timeout");
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte).expect("request headers");
                request.push(byte[0]);
            }
            let headers = String::from_utf8(request).expect("request headers UTF-8");
            let length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().expect("content length"))
                })
                .expect("request body length");
            let mut request_body = vec![0; length];
            socket
                .read_exact(&mut request_body)
                .expect("complete request body");
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).expect("response");
            // Send FIN after the complete Content-Length body, then let the client close
            // its side before dropping the Windows socket rather than resetting it.
            socket.shutdown(Shutdown::Write).expect("response shutdown");
            let mut trailing = Vec::new();
            if let Err(error) = socket.read_to_end(&mut trailing) {
                assert_eq!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionReset,
                    "client shutdown: {error}"
                );
            }
        });
        let context = Context {
            system_prompt: None,
            messages: Vec::new(),
            tools: None,
            active_tool_names: None,
        };
        // When the real proxy consumes the response.
        let stream = stream_proxy(
            &model(),
            &context,
            ProxyStreamOptions {
                proxy_url: format!("http://{address}"),
                ..Default::default()
            },
        );
        let result = tokio::time::timeout(Duration::from_secs(10), stream.result())
            .await
            .expect("proxy settled")
            .expect("result");
        tokio::task::spawn_blocking(move || server.join().expect("server"))
            .await
            .expect("server join task");
        result
    }

    fn done() -> String {
        format!(
            "data: {}\n",
            serde_json::json!({"type": "done", "reason": "stop", "usage": Usage::default()})
        )
    }

    #[test]
    fn terminal_events_deserialize_decimal_usage_costs() {
        // Given terminal wire events with decimal and exponent costs, including noncanonical
        // number spelling that serde_json must preserve when arbitrary_precision is enabled.
        for (kind, reason) in [("done", "stop"), ("error", "error")] {
            let wire = format!(
                r#"{{"type":"{kind}","reason":"{reason}","usage":{{"input":1,"output":2,"cacheRead":3,"cacheWrite":4,"totalTokens":10,"cost":{{"input":0.0,"output":0.125,"cacheRead":1e-3,"cacheWrite":0.2500,"total":3.760e-1}}}}}}"#
            );
            // When deserializing the same public event type used by the SSE parser.
            let event: ProxyAssistantMessageEvent =
                serde_json::from_str(&wire).expect("terminal event with numeric costs");
            // Then both terminal variants retain all numeric usage fields.
            let usage = match event {
                ProxyAssistantMessageEvent::Done { usage, .. }
                | ProxyAssistantMessageEvent::Error { usage, .. } => usage,
                other => panic!("unexpected terminal event: {other:?}"),
            };
            assert_eq!(usage.input, 1);
            assert_eq!(usage.output, 2);
            assert_eq!(usage.cache_read, 3);
            assert_eq!(usage.cache_write, 4);
            assert_eq!(usage.total_tokens, 10);
            for (actual, expected) in [
                (usage.cost.input, 0.0),
                (usage.cost.output, 0.125),
                (usage.cost.cache_read, 0.001),
                (usage.cost.cache_write, 0.25),
                (usage.cost.total, 0.376),
            ] {
                assert!((actual - expected).abs() < 1e-12, "cost: {actual}");
            }
        }
    }

    #[tokio::test]
    async fn malformed_json_terminates_before_a_later_done() {
        let result = response(format!("data: {{broken\n{}", done())).await;
        // Then corrupt JSON cannot be discarded in favor of success.
        assert_eq!(result.stop_reason, StopReason::Error);
        assert!(result.error_message.is_some());
    }

    #[tokio::test]
    async fn malformed_json_at_eof_is_a_parse_failure() {
        let result = response("data: {broken".to_owned()).await;
        // Then the EOF flush uses the same fallible parser, not the clean-EOF error.
        assert_eq!(result.stop_reason, StopReason::Error);
        let parse_error = serde_json::from_str::<ProxyAssistantMessageEvent>("{broken")
            .expect_err("invalid JSON");
        assert_eq!(result.error_message, Some(parse_error.to_string()));
    }

    #[tokio::test]
    async fn invalid_content_transitions_terminate_the_stream() {
        for event in [
            "text_delta",
            "text_end",
            "thinking_delta",
            "thinking_end",
            "toolcall_delta",
        ] {
            for start in [
                "",
                "data: {\"type\":\"toolcall_start\",\"contentIndex\":0,\"id\":\"call\",\"toolName\":\"lookup\"}\n",
            ] {
                // Given a missing block or a block of the wrong kind.
                let start = if event == "toolcall_delta" && !start.is_empty() {
                    "data: {\"type\":\"text_start\",\"contentIndex\":0}\n"
                } else {
                    start
                };
                let invalid = serde_json::json!({"type": event, "contentIndex": 0, "delta": "x"});
                let result = response(format!("{start}data: {invalid}\n{}", done())).await;
                // Then no invalid delta/end can be skipped to reach done.
                assert_eq!(result.stop_reason, StopReason::Error, "{event}: {start}");
                assert!(result.error_message.is_some());
            }
        }
    }

    #[tokio::test]
    async fn invalid_toolcall_end_remains_ignored_like_upstream() {
        let result = response(format!(
            "data: {{\"type\":\"toolcall_end\",\"contentIndex\":0}}\n{}",
            done()
        ))
        .await;
        // Then this deliberately tolerated upstream event is not widened into an error.
        assert_eq!(
            result.stop_reason,
            StopReason::Stop,
            "proxy error_message: {:?}",
            result.error_message
        );
    }

    async fn abort_stalled_response(headers: &'static [u8]) {
        // Given a server holding the response open at the requested phase.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("address");
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept");
            socket
                .set_read_timeout(Some(Duration::from_secs(10)))
                .expect("read timeout");
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte).expect("request headers");
                request.push(byte[0]);
            }
            let request = String::from_utf8(request).expect("request headers UTF-8");
            let length = request
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().expect("content length"))
                })
                .expect("request body length");
            let mut body = vec![0; length];
            socket.read_exact(&mut body).expect("complete request body");
            socket.write_all(headers).expect("headers");
            ready_tx.send(()).expect("ready");
            release_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("release server");
        });
        let controller = AbortController::new();
        let context = Context {
            system_prompt: None,
            messages: Vec::new(),
            tools: None,
            active_tool_names: None,
        };
        let stream = stream_proxy(
            &model(),
            &context,
            ProxyStreamOptions {
                proxy_url: format!("http://{address}"),
                signal: Some(controller.signal()),
                ..Default::default()
            },
        );
        tokio::time::timeout(Duration::from_secs(10), ready_rx)
            .await
            .expect("server ready")
            .expect("ready");
        // When cancellation happens while the server cannot finish its response.
        controller.abort(None);
        let result = tokio::time::timeout(Duration::from_secs(5), stream.result()).await;
        release_tx.send(()).expect("release");
        server.join().expect("server");
        // Then cancellation settles without waiting for headers or error-body EOF.
        assert_eq!(
            result
                .expect("cancellation settled")
                .expect("result")
                .stop_reason,
            StopReason::Aborted
        );
    }

    #[tokio::test]
    async fn cancellation_settles_while_waiting_for_headers() {
        abort_stalled_response(b"").await;
    }

    #[tokio::test]
    async fn cancellation_settles_with_an_unfinished_error_body() {
        abort_stalled_response(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{").await;
    }
}
