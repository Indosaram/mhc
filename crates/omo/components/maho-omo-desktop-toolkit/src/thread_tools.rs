//! Port of `omo-senpi/src/components/desktop-toolkit/thread-tools.ts` at omo v5.1.52: a minimal MCP
//! client (JSON or SSE replies) for the loopback desktop thread toolkit. The bearer lives only in the
//! grant's token file; every error that crosses this boundary is a fixed string.

use maho_ai::utils::abort::AbortSignal;
use maho_ext_api::{AgentToolResult, ContentBlock};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::{Map, Value, json};
use std::time::Duration;

use crate::ToolkitGrant;

const CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);

/// One tool the desktop app offers, already validated.
#[derive(Clone, Debug, PartialEq)]
pub struct DesktopTool {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Value,
}

/// Tools that passed validation plus the reason for every descriptor that did not.
#[derive(Debug, Default)]
pub struct ListedTools {
    pub tools: Vec<DesktopTool>,
    pub invalid: Vec<&'static str>,
}

fn object(value: &Value) -> Result<&Map<String, Value>, String> {
    value
        .as_object()
        .ok_or_else(|| "invalid desktop toolkit response".to_owned())
}

async fn response_message(mut response: reqwest::Response, id: u64) -> Result<Value, String> {
    let media_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(|value| value.trim().to_owned());
    if media_type.as_deref() == Some("application/json") {
        let bytes = response
            .bytes()
            .await
            .map_err(|_| "desktop toolkit HTTP request failed".to_owned())?;
        let message: Value = serde_json::from_slice(&bytes)
            .map_err(|_| "invalid desktop toolkit response".to_owned())?;
        object(&message)?;
        if message.get("jsonrpc") != Some(&json!("2.0")) || message.get("id") != Some(&json!(id)) {
            return Err("invalid desktop toolkit response id".into());
        }
        return Ok(message);
    }
    if media_type.as_deref() != Some("text/event-stream") {
        return Err("invalid desktop toolkit response type".into());
    }
    let mut buffer: Vec<u8> = Vec::new();
    let mut data: Vec<String> = Vec::new();
    loop {
        let chunk = response
            .chunk()
            .await
            .map_err(|_| "desktop toolkit HTTP request failed".to_owned())?
            .ok_or_else(|| "desktop toolkit stream ended without a response".to_owned())?;
        buffer.extend_from_slice(&chunk);
        while let Some(end) = buffer
                .iter()
                .position(|byte| *byte == b'\r' || *byte == b'\n')
        {
            if buffer[end] == b'\r' && end == buffer.len() - 1 {
                break;
            }
            let line = String::from_utf8_lossy(&buffer[..end]).into_owned();
            let crlf = buffer.get(end..end + 2) == Some(b"\r\n".as_slice());
            buffer.drain(..end + if crlf { 2 } else { 1 });
            if line.is_empty() {
                if data.is_empty() {
                    continue;
                }
                let message: Value = serde_json::from_str(&data.join("\n"))
                    .map_err(|_| "invalid desktop toolkit response".to_owned())?;
                object(&message)?;
                data.clear();
                if message.get("jsonrpc") == Some(&json!("2.0"))
                    && message.get("id") == Some(&json!(id))
                {
                    return Ok(message);
                }
            } else if line == "data" || line.starts_with("data:") {
                let value = &line[4..];
                let value = value.strip_prefix(':').unwrap_or(value);
                data.push(value.strip_prefix(' ').unwrap_or(value).to_owned());
            }
        }
    }
}

struct Client {
    http: reqwest::Client,
    endpoint: String,
    token_file: String,
    token: String,
    headers: HeaderMap,
    next_id: u64,
}

impl Client {
    fn new(grant: &ToolkitGrant) -> Result<Self, String> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .map_err(|_| "desktop toolkit HTTP request failed".to_owned())?;
        Ok(Self {
            http,
            endpoint: grant.endpoint.clone(),
            token_file: grant.token_file.clone(),
            token: String::new(),
            headers: HeaderMap::new(),
            next_id: 0,
        })
    }

    async fn open(&mut self) -> Result<(), String> {
        let raw = tokio::fs::read_to_string(&self.token_file)
            .await
            .map_err(|_| "desktop toolkit credential unavailable".to_owned())?;
        self.token = raw.trim().to_owned();
        if self.token.is_empty() {
            return Err("desktop toolkit credential unavailable".into());
        }
        let bearer = HeaderValue::from_str(&format!("Bearer {}", self.token))
            .map_err(|_| "desktop toolkit credential unavailable".to_owned())?;
        self.headers.insert(reqwest::header::AUTHORIZATION, bearer);
        self.headers.insert(
            reqwest::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        self.headers.insert(
            reqwest::header::ACCEPT,
            HeaderValue::from_static("application/json, text/event-stream"),
        );
        let initialized = self
            .request(
                "initialize",
                Some(json!({
                    "protocolVersion": "2025-03-26",
                    "capabilities": {},
                    "clientInfo": {"name": "omo-desktop-thread", "version": "1"},
                })),
                false,
            )
            .await?;
        let version = initialized
            .get("protocolVersion")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing desktop toolkit protocol version".to_owned())?;
        let version = HeaderValue::from_str(version)
            .map_err(|_| "missing desktop toolkit protocol version".to_owned())?;
        self.headers
            .insert(HeaderName::from_static("mcp-protocol-version"), version);
        self.request("notifications/initialized", None, true)
            .await?;
        Ok(())
    }

    async fn request(
        &mut self,
        method: &str,
        params: Option<Value>,
        notification: bool,
    ) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let mut body = json!({"jsonrpc": "2.0", "method": method});
        if !notification {
            body["id"] = json!(id);
        }
        if let Some(params) = params {
            body["params"] = params;
        }
        let response = self
            .http
            .post(&self.endpoint)
            .headers(self.headers.clone())
            .body(body.to_string())
            .send()
            .await
            .map_err(|_| "desktop toolkit HTTP request failed".to_owned())?;
        if !response.status().is_success() {
            return Err("desktop toolkit HTTP request failed".into());
        }
        if method == "initialize"
            && let Some(session) = response.headers().get("mcp-session-id")
        {
            self.headers
                .insert(HeaderName::from_static("mcp-session-id"), session.clone());
        }
        if notification {
            return Ok(Value::Object(Map::new()));
        }
        let message = response_message(response, id).await?;
        if message.get("error").is_some() {
            return Err("desktop toolkit RPC request failed".into());
        }
        let result = message
            .get("result")
            .filter(|value| value.is_object())
            .ok_or_else(|| "invalid desktop toolkit response".to_owned())?
            .clone();
        // This guard catches only the exact bearer substring in JSON, not encoded or transformed secrets.
        if result.to_string().contains(&self.token) {
            return Err("desktop toolkit response contains a credential".into());
        }
        Ok(result)
    }

    /// Best-effort session cleanup with its own deadline: it must not replace the operation's outcome.
    async fn close(&self) {
        if self.headers.contains_key("mcp-session-id") {
            let _ = self
                .http
                .delete(&self.endpoint)
                .headers(self.headers.clone())
                .timeout(CLEANUP_TIMEOUT)
                .send()
                .await;
        }
    }
}

/// Runs `operation` against a freshly initialized MCP session, bounded by the deadline and the
/// caller's signal, and always tries to terminate the session afterwards.
async fn with_client<T>(
    grant: &ToolkitGrant,
    timeout_ms: u64,
    signal: Option<&AbortSignal>,
    operation: impl AsyncFnOnce(&mut Client) -> Result<T, String>,
) -> Result<T, String> {
    let mut client = Client::new(grant)?;
    let outcome = {
        let work = async {
            client.open().await?;
            operation(&mut client).await
        };
        let aborted = async {
            match signal {
                Some(signal) => signal.cancelled().await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            result = work => result,
            () = aborted => Err("desktop toolkit request aborted".to_owned()),
            () = tokio::time::sleep(Duration::from_millis(timeout_ms)) => {
                Err("desktop toolkit request timed out".to_owned())
            }
        }
    };
    client.close().await;
    outcome
}

fn is_valid_tool_name(name: &str) -> bool {
    // `desktop_` + name must be `^[a-zA-Z0-9_-]{1,64}$`.
    !name.is_empty()
        && name.len() + "desktop_".len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

pub async fn list_thread_tools(
    grant: &ToolkitGrant,
    timeout_ms: u64,
    signal: Option<&AbortSignal>,
) -> Result<ListedTools, String> {
    with_client(grant, timeout_ms, signal, async |client| {
        let mut listed = ListedTools::default();
        let mut cursor: Option<String> = None;
        loop {
            let params = cursor.as_ref().map(|cursor| json!({"cursor": cursor}));
            let page = client.request("tools/list", params, false).await?;
            let tools = page
                .get("tools")
                .and_then(Value::as_array)
                .ok_or_else(|| "invalid desktop toolkit tools".to_owned())?;
            for value in tools {
                let Some(tool) = value.as_object() else {
                    listed.invalid.push("invalid_descriptor");
                    continue;
                };
                let name = match tool.get("name").and_then(Value::as_str) {
                    Some(name) if is_valid_tool_name(name) => name,
                    _ => {
                        listed.invalid.push("invalid_name");
                        continue;
                    }
                };
                let schema = tool.get("inputSchema").filter(|schema| {
                    schema
                        .as_object()
                        .is_some_and(|object| object.get("type") == Some(&json!("object")))
                });
                let Some(schema) = schema else {
                    listed.invalid.push("invalid_schema");
                    continue;
                };
                let description = match tool.get("description") {
                    None => None,
                    Some(Value::String(text)) => Some(text.clone()),
                    Some(_) => {
                        listed.invalid.push("invalid_descriptor");
                        continue;
                    }
                };
                listed.tools.push(DesktopTool {
                    name: name.to_owned(),
                    description,
                    input_schema: schema.clone(),
                });
            }
            match page.get("nextCursor") {
                None => break,
                Some(Value::String(next)) => cursor = Some(next.clone()),
                Some(_) => return Err("invalid desktop toolkit cursor".into()),
            }
        }
        Ok(listed)
    })
    .await
}

fn failure(text: &str, reason: &str) -> AgentToolResult {
    AgentToolResult {
        content: vec![ContentBlock::text(text)],
        details: json!({"reason": reason}),
        structured_content: None,
        usage: None,
        nested_calls: None,
        added_tool_names: None,
        terminate: None,
        is_error: Some(true),
    }
}

pub async fn call_thread_tool(
    grant: Option<&ToolkitGrant>,
    name: &str,
    args: Value,
    timeout_ms: u64,
    signal: Option<&AbortSignal>,
) -> AgentToolResult {
    let Some(grant) = grant else {
        return failure(
            "Desktop thread toolkit is not granted to the calling session.",
            "no_session_grant",
        );
    };
    let outcome = with_client(grant, timeout_ms, signal, async |client| {
        let result = client
            .request(
                "tools/call",
                Some(json!({"name": name, "arguments": args})),
                false,
            )
            .await?;
        let content = result.get("content").and_then(Value::as_array);
        let is_error = match result.get("isError") {
            None => None,
            Some(Value::Bool(flag)) => Some(*flag),
            Some(_) => return Err("invalid desktop toolkit content".to_owned()),
        };
        let Some(content) = content else {
            return Err("invalid desktop toolkit content".to_owned());
        };
        let blocks = content
            .iter()
            .map(|part| {
                serde_json::from_value::<ContentBlock>(part.clone())
                    .unwrap_or_else(|_| ContentBlock::text(part.to_string()))
            })
            .collect();
        Ok(AgentToolResult {
            content: blocks,
            details: json!({}),
            structured_content: result.get("structuredContent").cloned(),
            usage: None,
            nested_calls: None,
            added_tool_names: None,
            terminate: None,
            is_error,
        })
    })
    .await;
    // Neither transport errors nor server replies may disclose the file's bearer.
    outcome.unwrap_or_else(|_| failure("Desktop thread toolkit call failed.", "call_failed"))
}
