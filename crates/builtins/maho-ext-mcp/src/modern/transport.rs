//! Port of senpi mcp/modern/transport.ts: the stateless modern HTTP POST transport.
use super::{
    diagnostics::sanitize_modern_message,
    errors::{McpMrtrError, McpMrtrErrorCode},
    parameter_headers::{ModernParameterHeaders, clear_mcp_parameter_headers},
    request::DispatchGuard,
    wire::{ModernClientInfo, derive_modern_headers, modernize_request},
};
use crate::errors::{McpError, McpErrorKind};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ModernTransportError(pub String);

/// Upstream `modernCredentialSnapshot`: the credential pair a request was authorized with.
pub fn modern_credential_snapshot(
    tokens: Option<&crate::auth::oauth_provider::OAuthTokens>,
) -> Option<String> {
    tokens.map(|tokens| json!([tokens.access_token, tokens.refresh_token]).to_string())
}

/// Same-origin redirects only (upstream `fetchWithinOrigin`).
pub fn within_origin_client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let same = attempt
                .previous()
                .first()
                .is_some_and(|first| first.origin() == attempt.url().origin());
            if attempt.previous().len() >= 10 || !same {
                attempt.stop()
            } else {
                attempt.follow()
            }
        }))
        .build()
}

fn transport_failed() -> McpError {
    McpMrtrError::new(McpMrtrErrorCode::TransportFailed).into()
}
fn invalid_result() -> McpError {
    McpMrtrError::new(McpMrtrErrorCode::InvalidResult).into()
}
/// Upstream `JSONRPCMessageSchema.parse`: a request or notification (string `method`), a result response (`id` and
/// object `result`), or an error response (object `error`, `id` optional).
fn jsonrpc_message(value: Value) -> Result<Value, McpError> {
    let valid = value.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
        && (value.get("method").is_some_and(Value::is_string)
            || (value.get("id").is_some() && value.get("result").is_some_and(Value::is_object))
            || value.get("error").is_some_and(Value::is_object));
    if valid {
        Ok(value)
    } else {
        Err(transport_failed())
    }
}

/// Stateless POST transport; discovery and era selection belong to the connection factory.
pub struct ModernHttpTransport {
    pub server: String,
    client: reqwest::Client,
    url: url::Url,
    headers: BTreeMap<String, String>,
    client_info: ModernClientInfo,
    client_capabilities: Value,
    pub protocol_version: String,
    parameter_headers: ModernParameterHeaders,
    auth: Option<Arc<crate::auth::oauth_refresh::McpRefreshManager>>,
    warn: Arc<dyn Fn(&str) + Send + Sync>,
}
impl ModernHttpTransport {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        server: &str,
        url: url::Url,
        headers: BTreeMap<String, String>,
        client_info: ModernClientInfo,
        client_capabilities: Value,
        protocol_version: String,
        auth: Option<Arc<crate::auth::oauth_refresh::McpRefreshManager>>,
        warn: Arc<dyn Fn(&str) + Send + Sync>,
    ) -> Result<Self, McpError> {
        let client = within_origin_client()
            .map_err(|error| McpError::new(McpErrorKind::Connect, error.to_string()))?;
        Ok(Self {
            server: server.into(),
            client,
            url,
            headers,
            client_info,
            client_capabilities,
            protocol_version,
            parameter_headers: ModernParameterHeaders::default(),
            auth,
            warn,
        })
    }

    /// Sends one message. Returns the JSON-RPC reply addressed to a request; notifications that arrive on the
    /// way (SSE) go to `on_message`. Cancellation drops the attempt: no `notifications/cancelled` is POSTed.
    pub async fn send(
        &self,
        message: &Value,
        guard: Option<&DispatchGuard>,
        on_message: &(dyn Fn(Value) + Send + Sync),
    ) -> Result<Option<Value>, McpError> {
        let method = message.get("method").and_then(Value::as_str);
        if method == Some("notifications/cancelled") {
            return Ok(None);
        }
        let id = message.get("id").filter(|_| method.is_some()).cloned();
        let tool_list_first_page = (method == Some("tools/list")).then(|| {
            message
                .get("params")
                .and_then(|params| params.get("cursor"))
                .is_none()
        });
        let mut headers = self.headers.clone();
        clear_mcp_parameter_headers(&mut headers);
        // Auth headers pass through, but protocol routing is exclusively derived from the body.
        headers.retain(|name, _| {
            ![
                "mcp-session-id",
                "mcp-name",
                "mcp-method",
                "mcp-protocol-version",
                "content-type",
                "accept",
            ]
            .contains(&name.to_ascii_lowercase().as_str())
        });
        headers.insert("Content-Type".into(), "application/json".into());
        headers.insert(
            "Accept".into(),
            "application/json, text/event-stream".into(),
        );
        let body = if let Some(method) = method {
            let request = modernize_request(
                id.clone().unwrap_or(Value::from(0)),
                method,
                &self.client_info,
                &self.client_capabilities,
                &self.protocol_version,
                message.get("params"),
            );
            headers.extend(derive_modern_headers(&request));
            if method == "tools/call" {
                self.parameter_headers
                    .append(
                        &mut headers,
                        request["params"].get("name"),
                        request["params"].get("arguments"),
                    )
                    .map_err(|error| {
                        let mut failure = McpError::new(McpErrorKind::ToolExec, error.0);
                        failure.cause =
                            Some(Box::new(json!({"name": "ParameterHeaderValidationError"})));
                        failure
                    })?;
            }
            let mut request = request;
            if id.is_none()
                && let Some(object) = request.as_object_mut()
            {
                object.remove("id");
            }
            request
        } else {
            // JSON-RPC replies have no method or params; do not turn them into requests.
            headers.insert("MCP-Protocol-Version".into(), self.protocol_version.clone());
            message.clone()
        };
        let result = async {
            let mut builder = self.client.post(self.url.clone()).body(body.to_string());
            let mut snapshot = None;
            if let Some(auth) = &self.auth {
                let tokens = auth.ensure_fresh().await.map_err(|error| {
                    let mut failure = McpError::new(McpErrorKind::Auth, error.to_string());
                    failure.phase = Some("auth".into());
                    failure
                })?;
                snapshot = modern_credential_snapshot(tokens.as_ref());
                if let Some(tokens) = tokens {
                    builder = builder.bearer_auth(tokens.access_token);
                }
            }
            if let Some(guard) = guard {
                guard(snapshot)?;
            }
            for (name, value) in &headers {
                builder = builder.header(name, value);
            }
            let response = builder.send().await.map_err(|_| transport_failed())?;
            let status = response.status().as_u16();
            if status == 401 || status == 403 {
                let mut failure = McpError::new(
                    McpErrorKind::Auth,
                    "MCP server needs authentication; use /mcp auth-start.",
                );
                failure.phase = Some("auth".into());
                failure.server_name = Some(self.server.clone());
                return Err(failure);
            }
            if status == 202 || status == 204 {
                return Ok(None);
            }
            let media = response
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.split(';').next())
                .map(|value| value.trim().to_ascii_lowercase());
            match media.as_deref() {
                Some("application/json") => {
                    let ok = response.status().is_success();
                    let incoming = response
                        .json::<Value>()
                        .await
                        .map_err(|_| transport_failed())?;
                    let incoming = jsonrpc_message(incoming)?;
                    // Protocol errors (including modern negotiation errors on HTTP 400) stay on the RPC channel.
                    if !ok && incoming.get("error").is_none() {
                        return Err(transport_failed());
                    }
                    self.deliver(incoming, id.as_ref(), tool_list_first_page, on_message)
                }
                Some("text/event-stream") if response.status().is_success() => {
                    self.consume_sse(response, id.as_ref(), tool_list_first_page, on_message)
                        .await
                }
                _ => Err(transport_failed()),
            }
        }
        .await;
        result.map_err(|error| {
            let keep = error.kind == McpErrorKind::Auth
                || crate::modern::errors::as_mcp_mrtr_error(&error).is_some()
                || error
                    .cause
                    .as_deref()
                    .and_then(|cause| cause.get("name"))
                    .and_then(Value::as_str)
                    == Some("ParameterHeaderValidationError");
            if keep { error } else { transport_failed() }
        })
    }

    fn deliver(
        &self,
        mut incoming: Value,
        expected: Option<&Value>,
        tool_list_first_page: Option<bool>,
        on_message: &(dyn Fn(Value) + Send + Sync),
    ) -> Result<Option<Value>, McpError> {
        let is_reply = incoming.get("method").is_none();
        if let Some(id) = incoming.get("id").filter(|_| is_reply)
            && expected != Some(id)
        {
            return Err(invalid_result());
        }
        if is_reply
            && incoming.get("id").is_some()
            && let Some(first_page) = tool_list_first_page
            && let Some(tools) = incoming
                .get("result")
                .and_then(|result| result.get("tools"))
                .and_then(Value::as_array)
        {
            let warn = self.warn.clone();
            let filtered = self
                .parameter_headers
                .filter_tools(tools, first_page, &|message| warn(message));
            incoming["result"]["tools"] = Value::Array(filtered);
        }
        let Some(safe) = sanitize_modern_message(&incoming)? else {
            return Ok(None);
        };
        if is_reply && safe.get("id").is_some() {
            return Ok(Some(safe));
        }
        on_message(safe);
        Ok(None)
    }

    async fn consume_sse(
        &self,
        mut response: reqwest::Response,
        request_id: Option<&Value>,
        tool_list_first_page: Option<bool>,
        on_message: &(dyn Fn(Value) + Send + Sync),
    ) -> Result<Option<Value>, McpError> {
        let mut buffer = String::new();
        let mut bytes = Vec::new();
        let mut data: Vec<String> = Vec::new();
        let mut event = String::new();
        loop {
            let chunk = response.chunk().await.map_err(|_| transport_failed())?;
            let done = chunk.is_none();
            if let Some(chunk) = chunk {
                bytes.extend_from_slice(&chunk);
                // Retain only an incomplete trailing code point. Permanently malformed
                // sequences become replacement characters, as in a streaming TextDecoder.
                let mut consumed = 0;
                while consumed < bytes.len() {
                    match std::str::from_utf8(&bytes[consumed..]) {
                        Ok(text) => {
                            buffer.push_str(text);
                            consumed = bytes.len();
                        }
                        Err(error) => {
                            let valid_end = consumed + error.valid_up_to();
                            buffer.push_str(&String::from_utf8_lossy(&bytes[consumed..valid_end]));
                            consumed = valid_end;
                            let Some(invalid_len) = error.error_len() else {
                                break;
                            };
                            buffer.push('\u{fffd}');
                            consumed += invalid_len;
                        }
                    }
                }
                bytes.drain(..consumed);
            } else {
                buffer.push_str(&String::from_utf8_lossy(&bytes));
                bytes.clear();
            }
            while let Some(end) = buffer.find(['\r', '\n']) {
                // A CR at the chunk boundary might still be followed by LF.
                if !done && buffer.as_bytes()[end] == b'\r' && end + 1 == buffer.len() {
                    break;
                }
                let line = buffer[..end].to_owned();
                let skip = if buffer[end..].starts_with("\r\n") {
                    2
                } else {
                    1
                };
                buffer.drain(..end + skip);
                if line.is_empty() {
                    if !data.is_empty() && (event.is_empty() || event == "message") {
                        let incoming = serde_json::from_str::<Value>(&data.join("\n"))
                            .map_err(|_| transport_failed())
                            .and_then(jsonrpc_message)?;
                        if let Some(reply) =
                            self.deliver(incoming, request_id, tool_list_first_page, on_message)?
                            && Some(&reply["id"]) == request_id
                        {
                            return Ok(Some(reply));
                        }
                    }
                    data.clear();
                    event.clear();
                } else if !line.starts_with(':') {
                    let (field, text) = match line.find(':') {
                        Some(colon) => (
                            &line[..colon],
                            line[colon + 1..]
                                .strip_prefix(' ')
                                .unwrap_or(&line[colon + 1..]),
                        ),
                        None => (line.as_str(), ""),
                    };
                    match field {
                        "data" => data.push(text.into()),
                        "event" => event = text.into(),
                        _ => {}
                    }
                }
            }
            if done {
                if request_id.is_some() {
                    return Err(McpError::new(
                        McpErrorKind::Connect,
                        "Modern MCP SSE ended before the request response",
                    ));
                }
                return Ok(None);
            }
        }
    }
}
