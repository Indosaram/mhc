use crate::abort::AbortSignal;
use crate::request_context::LspRequestContext;
use crate::request_context::LspRequestContextParseError;
use crate::request_context::StandaloneMcpRequestContextInput;
use crate::request_context::create_standalone_mcp_request_context;
use crate::request_context::scope_request_context;
use crate::tools::coerce_tool_arguments;
use crate::tools::execute_lsp_tool;
use crate::tools::lsp_mcp_tools;
use mcp_stdio_core::JsonRpcId;
use mcp_stdio_core::JsonRpcResponse;
use mcp_stdio_core::JsonRpcStdioServerConfig;
use mcp_stdio_core::ServerError;
use mcp_stdio_core::ServerOutcome;
use mcp_stdio_core::error_response;
use mcp_stdio_core::json_rpc_id;
use mcp_stdio_core::run_json_rpc_stdio_server;
use mcp_stdio_core::success_response;
use mcp_stdio_core::watchdog::ParentWatchdogConfig;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use std::collections::VecDeque;
use std::convert::Infallible;
use std::io::Read;
use std::io::Write;
use std::sync::Arc;
use std::sync::Mutex;

const SERVER_NAME: &str = "lsp";
const SERVER_VERSION: &str = "0.1.0";
const DEFAULT_PROTOCOL_VERSION: &str = "2024-11-05";

/// TS `HandleLspMcpRequestOptions`.
#[derive(Debug, Clone, Default)]
pub struct HandleLspMcpRequestOptions {
    pub signal: Option<AbortSignal>,
}

/// TS `LspMcpStdioServerOptions`; `parent_watchdog: None` uses the watchdog defaults.
#[derive(Clone, Default)]
pub struct LspMcpStdioServerOptions {
    pub parent_watchdog: Option<ParentWatchdogConfig>,
}

#[derive(Debug, thiserror::Error)]
pub enum LspMcpServerError {
    #[error(transparent)]
    Context(#[from] LspRequestContextParseError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => {
            Map::new()
        }
    }
}

/// TS `handleLspMcpRequest`; `None` means no response (notifications).
pub async fn handle_lsp_mcp_request(
    input: &Value,
    options: HandleLspMcpRequestOptions,
) -> Option<JsonRpcResponse> {
    let Value::Object(request) = input else {
        return Some(error_response(
            JsonRpcId::Null,
            -32600,
            "Invalid Request",
            None,
        ));
    };
    let id = json_rpc_id(request.get("id").unwrap_or(&Value::Null));
    let method = request.get("method");
    match method.and_then(Value::as_str) {
        Some("notifications/initialized") => None,
        Some("ping") => Some(success_response(id, Map::new())),
        Some("initialize") => {
            let protocol_version = request
                .get("params")
                .and_then(|params| params.get("protocolVersion"))
                .and_then(Value::as_str)
                .unwrap_or(DEFAULT_PROTOCOL_VERSION);
            Some(success_response(
                id,
                object(json!({
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION },
                    "protocolVersion": protocol_version,
                })),
            ))
        }
        Some("tools/list") => {
            let tools: Vec<Value> = lsp_mcp_tools()
                .into_iter()
                .map(|tool| {
                    json!({
                        "name": tool.name, "title": tool.title,
                        "description": tool.description, "inputSchema": tool.input_schema,
                    })
                })
                .collect();
            Some(success_response(id, object(json!({ "tools": tools }))))
        }
        Some("tools/call") => {
            Some(handle_tool_call(id, request.get("params"), options.signal.as_ref()).await)
        }
        _ => {
            let shown = match method {
                None => "undefined".to_string(),
                Some(Value::String(text)) => text.clone(),
                Some(other) => other.to_string(),
            };
            Some(error_response(
                id,
                -32601,
                format!("Method not found: {shown}"),
                None,
            ))
        }
    }
}

async fn handle_tool_call(
    id: JsonRpcId,
    params: Option<&Value>,
    signal: Option<&AbortSignal>,
) -> JsonRpcResponse {
    let Some(name) = params
        .and_then(|params| params.get("name"))
        .and_then(Value::as_str)
    else {
        return error_response(id, -32602, "tools/call requires params.name", None);
    };
    let arguments = coerce_tool_arguments(
        params
            .and_then(|params| params.get("arguments"))
            .unwrap_or(&Value::Null),
    );
    match execute_lsp_tool(name, &arguments, signal).await {
        Ok(result) => success_response(
            id,
            object(json!({
                "content": result.content_json(),
                "isError": result.is_error,
                "details": result.details,
            })),
        ),
        Err(error) => success_response(
            id,
            object(json!({
                "content": [{ "type": "text", "text": error.to_string() }],
                "isError": true,
            })),
        ),
    }
}

/// TS `runMcpStdioServer`: standalone request context, no idle timeout, parent watchdog on.
pub fn run_mcp_stdio_server<R, W>(
    input: R,
    output: &mut W,
    options: LspMcpStdioServerOptions,
) -> Result<ServerOutcome, LspMcpServerError>
where
    R: Read + Send + 'static,
    W: Write,
{
    let context: LspRequestContext =
        create_standalone_mcp_request_context(StandaloneMcpRequestContextInput::default())?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let handler = |request: &Value| -> Result<Option<JsonRpcResponse>, Infallible> {
        Ok(runtime.block_on(scope_request_context(
            context.clone(),
            handle_lsp_mcp_request(request, HandleLspMcpRequestOptions::default()),
        )))
    };
    let (input, payloads) = ParseErrorPayloadReader::wrap(input);
    let mut config = JsonRpcStdioServerConfig::new(handler);
    config.parse_error_response = Some(Arc::new(move |message: &str| {
        let data = match payloads.lock().ok().and_then(|mut queue| queue.pop_front()) {
            Some(payload) => format!("{message}: {payload}"),
            None => message.to_string(),
        };
        Some(error_response(
            JsonRpcId::Null,
            -32700,
            "Parse error",
            Some(Value::from(data)),
        ))
    }));
    config.idle_timeout_ms = Some(0);
    config.parent_watchdog = Some(options.parent_watchdog.unwrap_or_default());
    match run_json_rpc_stdio_server(input, output, config) {
        Ok(outcome) => Ok(outcome),
        Err(ServerError::Io(error)) => Err(LspMcpServerError::Io(error)),
        Err(ServerError::Handler(never)) => match never {},
    }
}

const MAX_PAYLOAD_ECHO_CHARS: usize = 200;

/// Sibling gap adapter: `mcp-stdio-core` parse errors carry only serde's message, while the TS
/// `JSON.parse` message quotes the offending text. This reader mirrors line-mode framing and
/// queues each unparseable line (in stream order, before the server sees the bytes).
struct ParseErrorPayloadReader<R> {
    inner: R,
    line: Vec<u8>,
    framed: bool,
    payloads: Arc<Mutex<VecDeque<String>>>,
}

impl<R: Read> ParseErrorPayloadReader<R> {
    fn wrap(inner: R) -> (Self, Arc<Mutex<VecDeque<String>>>) {
        let payloads = Arc::new(Mutex::new(VecDeque::new()));
        let reader = Self {
            inner,
            line: Vec::new(),
            framed: false,
            payloads: Arc::clone(&payloads),
        };
        (reader, payloads)
    }

    fn record_line(&mut self) {
        let raw = std::mem::take(&mut self.line);
        let text = String::from_utf8_lossy(&raw);
        let text = text.trim();
        if text.is_empty() || serde_json::from_str::<Value>(text).is_ok() {
            return;
        }
        let echo: String = text.chars().take(MAX_PAYLOAD_ECHO_CHARS).collect();
        if let Ok(mut queue) = self.payloads.lock() {
            queue.push_back(echo);
        }
    }
}

impl<R: Read> Read for ParseErrorPayloadReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buf)?;
        if self.framed {
            return Ok(read);
        }
        if read == 0 {
            self.record_line();
            return Ok(0);
        }
        for &byte in &buf[..read] {
            if self.line.is_empty() && byte == b'C' {
                self.framed = true;
                return Ok(read);
            }
            if byte == b'\n' {
                self.record_line();
            } else {
                self.line.push(byte);
            }
        }
        Ok(read)
    }
}

#[cfg(test)]
#[path = "mcp_tests.rs"]
mod tests;
