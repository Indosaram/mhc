//! Legacy HTTP+SSE: one long-lived GET owns the session; POST responses are only acknowledgements.
use super::{
    ClientTransport, McpClient, McpError, McpErrorKind, McpLogger, McpTransportSpec, failure,
};
use reqwest::{Client, RequestBuilder, Response, header};
use serde_json::Value;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use url::Url;

pub(super) const MAX_PENDING_REQUESTS: usize = 1024;
const MAX_ENDPOINT_BYTES: usize = 8192;

pub(super) struct LegacySse {
    client: Client,
    url: Url,
    headers: header::HeaderMap,
    endpoint: OnceLock<Url>,
    initialized: tokio::sync::OnceCell<()>,
}

impl McpClient {
    /// Construct without opening a connection: the caller may still need to install OAuth.
    pub async fn materialize_sse(
        server: &str,
        spec: &McpTransportSpec,
        _logger: Arc<Mutex<McpLogger>>,
    ) -> Result<Arc<Self>, McpError> {
        let invalid = |msg| failure(server, McpErrorKind::Connect, msg, "create");
        let McpTransportSpec::Http { url, headers } = spec else {
            return Err(invalid("legacy SSE requires an HTTP transport"));
        };
        if !valid_url(url) {
            return Err(invalid("invalid legacy SSE URL"));
        }
        // Even non-Authorization configuration headers may contain credentials.
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .map_err(|_| invalid("could not create legacy SSE client"))?;
        let mut approved = header::HeaderMap::new();
        for (name, value) in headers {
            let name = header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| invalid("invalid legacy SSE header"))?;
            // Framing, routing and negotiated headers belong to the transport.
            if matches!(
                name.as_str(),
                "host"
                    | "connection"
                    | "content-length"
                    | "transfer-encoding"
                    | "accept"
                    | "content-type"
                    | "mcp-protocol-version"
                    | "mcp-session-id"
            ) {
                continue;
            }
            let value = header::HeaderValue::from_str(value)
                .map_err(|_| invalid("invalid legacy SSE header"))?;
            approved.insert(name, value);
        }
        let io = LegacySse {
            client,
            url: url.clone(),
            headers: approved,
            endpoint: OnceLock::new(),
            initialized: tokio::sync::OnceCell::new(),
        };
        Ok(Self::with_http_transport(
            server,
            ClientTransport::LegacySse(Box::new(io)),
        ))
    }

    pub(super) fn close_sse(&self) {
        self.closed.send_replace(true);
        if let Some(task) = self
            .http_stream
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            task.abort();
        }
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }

    fn sse_message(&self, mut message: Value) -> Result<(), &'static str> {
        if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err("invalid SSE JSON-RPC message");
        }
        if message.get("method").and_then(Value::as_str).is_some() {
            let _ = self.notifications.send(message);
        } else if let Some(id) = message.get("id").and_then(Value::as_u64) {
            let reply = if let Some(error) = message.get("error") {
                // The peer can echo the full endpoint or credentials in message/data.
                let mut err = failure(
                    &self.server,
                    McpErrorKind::Protocol,
                    "MCP protocol error",
                    "request",
                );
                if let Some(code) = error.get("code").and_then(Value::as_i64) {
                    err.cause = Some(Box::new(serde_json::json!({"code":code})));
                }
                Err(err)
            } else if let Some(result) = message.get_mut("result") {
                Ok(result.take())
            } else {
                Err(failure(
                    &self.server,
                    McpErrorKind::Protocol,
                    "missing SSE JSON-RPC result",
                    "request",
                ))
            };
            if let Some(sender) = self
                .pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&id)
            {
                let _ = sender.send(reply);
            }
        }
        Ok(())
    }
}

fn valid_url(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
        && url.has_host()
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
}

// A dropped initialization future must retire a reader already started for the handshake.
struct Connecting<'a>(Option<&'a McpClient>);
impl Drop for Connecting<'_> {
    fn drop(&mut self) {
        if let Some(owner) = self.0 {
            owner.close_sse();
        }
    }
}

impl LegacySse {
    pub(super) async fn initialize(
        &self,
        owner: &Arc<McpClient>,
        timeout: Duration,
    ) -> Result<(), McpError> {
        let connect = self.initialized.get_or_try_init(|| async {
            let mut guard = Connecting(Some(owner));
            let response = self
                .send_http(
                    owner,
                    self.client
                        .get(self.url.clone())
                        .header(header::ACCEPT, "text/event-stream"),
                    true,
                )
                .await?;
            let mime = response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.split(';').next());
            if !mime.is_some_and(|v| v.trim().eq_ignore_ascii_case("text/event-stream")) {
                return Err(failure(
                    &owner.server,
                    McpErrorKind::Protocol,
                    "expected text/event-stream",
                    "connect",
                ));
            }
            let mut stream = EventStream::new(response);
            let endpoint = loop {
                let (kind, data) = stream
                    .next()
                    .await
                    .map_err(|msg| failure(&owner.server, McpErrorKind::Connect, msg, "connect"))?;
                if kind == "endpoint" {
                    let invalid = || {
                        failure(
                            &owner.server,
                            McpErrorKind::Protocol,
                            "invalid or cross-origin SSE endpoint",
                            "connect",
                        )
                    };
                    if data.trim().is_empty()
                        || data.len() > MAX_ENDPOINT_BYTES
                        || data.chars().any(char::is_control)
                    {
                        return Err(invalid());
                    }
                    break self
                        .url
                        .join(&data)
                        .ok()
                        .filter(|url| valid_url(url) && url.origin() == self.url.origin())
                        .ok_or_else(invalid)?;
                }
            };
            self.endpoint.set(endpoint).map_err(|_| {
                failure(
                    &owner.server,
                    McpErrorKind::Protocol,
                    "duplicate SSE endpoint",
                    "connect",
                )
            })?;
            {
                let mut reader = owner
                    .http_stream
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if *owner.closed.borrow() {
                    return Err(closed(owner));
                }
                let weak = Arc::downgrade(owner);
                *reader = Some(tokio::spawn(async move {
                    while let Ok((kind, data)) = stream.next().await {
                        let Some(owner) = weak.upgrade() else { return };
                        if kind == "endpoint" {
                            break;
                        }
                        if kind.is_empty() || kind == "message" {
                            let Ok(value) = serde_json::from_str(&data) else {
                                maho_ext_api::diagnostic("MCP SSE received malformed JSON");
                                continue;
                            };
                            if let Err(error) = owner.sse_message(value) {
                                maho_ext_api::diagnostic(error);
                            }
                        }
                    }
                    if let Some(owner) = weak.upgrade() {
                        owner.close_sse();
                    }
                }));
            }
            owner.initialize_protocol(timeout).await?;
            guard.0 = None;
            Ok(())
        });
        // Includes OAuth, GET headers, a possibly silent stream, the reply and the initialized POST.
        tokio::time::timeout(
            timeout,
            until_closed(owner, async { connect.await.copied() }),
        )
        .await
        .map_err(|_| {
            failure(
                &owner.server,
                McpErrorKind::Timeout,
                "MCP SSE initialization timed out",
                "connect",
            )
        })?
    }

    pub(super) async fn post(&self, owner: &McpClient, value: &Value) -> Result<(), McpError> {
        let url = self.endpoint.get().ok_or_else(|| {
            failure(
                &owner.server,
                McpErrorKind::Connect,
                "SSE endpoint not established",
                "request",
            )
        })?;
        let req = owner.with_protocol_version(self.client.post(url.clone()).json(value));
        until_closed(owner, self.send_http(owner, req, false)).await?;
        Ok(())
    }

    async fn send_http(
        &self,
        owner: &McpClient,
        request: RequestBuilder,
        opening_stream: bool,
    ) -> Result<Response, McpError> {
        // GET and buffered JSON POST are cloneable. Retry only on an explicit auth rejection;
        // EOF, an unreadable acknowledgement, timeout or 5xx can follow an executed tool call.
        let auth = owner.auth.read().await.clone();
        let mut recovered = false;
        loop {
            let attempt = request.try_clone().ok_or_else(|| {
                failure(
                    &owner.server,
                    McpErrorKind::Connect,
                    "invalid SSE request",
                    "request",
                )
            })?;
            let response = self.send_attempt(owner, attempt, auth.as_deref()).await?;
            let status = response.status().as_u16();
            if response.status().is_success() {
                return Ok(response);
            }
            let challenge = crate::auth::challenge::step_up_required(
                status,
                response
                    .headers()
                    .get(header::WWW_AUTHENTICATE)
                    .and_then(|v| v.to_str().ok()),
            );
            drop(response);
            let error = owner.http_status_error(status, None);
            let Some(refresh) = &auth else {
                return Err(error);
            };
            if recovered || !matches!(status, 401 | 403) {
                return Err(error);
            }
            recovered = true;
            if let Some(challenge) = challenge {
                let provider = refresh.provider();
                provider.escalate_scopes(&challenge.scopes);
                let handler = owner.auth_challenge_handler.read().await.clone();
                if let Some(handler) = handler {
                    let scopes = provider.effective_scopes().unwrap_or(challenge.scopes);
                    handler
                        .reauthorize(scopes)
                        .await
                        .map_err(|_| error.clone())?;
                    if !opening_stream {
                        // New authorization may be for a different principal. Do not reuse the
                        // session URL or transfer a pending request to a replacement GET.
                        owner.close_sse();
                        return Err(failure(
                            &owner.server,
                            McpErrorKind::Auth,
                            "MCP authorization changed; reconnect SSE",
                            "request",
                        ));
                    }
                    continue;
                }
            }
            refresh.force_refresh().await.map_err(|_| error)?;
        }
    }

    async fn send_attempt(
        &self,
        owner: &McpClient,
        request: RequestBuilder,
        auth: Option<&crate::auth::oauth_refresh::McpRefreshManager>,
    ) -> Result<Response, McpError> {
        let mut headers = self.headers.clone();
        if let Some(refresh) = auth {
            let tokens = refresh
                .ensure_fresh()
                .await
                .map_err(|error| {
                    let terminal = crate::needs_auth::is_oauth_needs_auth_error(&error);
                    let mut err = failure(
                        &owner.server,
                        if terminal {
                            McpErrorKind::Auth
                        } else {
                            McpErrorKind::Connect
                        },
                        "MCP OAuth refresh failed",
                        "request",
                    );
                    err.retriable = !terminal;
                    err
                })?
                .ok_or_else(|| {
                    failure(
                        &owner.server,
                        McpErrorKind::Auth,
                        "MCP requires authorization",
                        "request",
                    )
                })?;
            let mut value =
                header::HeaderValue::from_str(&format!("Bearer {}", tokens.access_token)).map_err(
                    |_| {
                        failure(
                            &owner.server,
                            McpErrorKind::Auth,
                            "invalid MCP credentials",
                            "request",
                        )
                    },
                )?;
            value.set_sensitive(true);
            headers.insert(header::AUTHORIZATION, value);
        }
        request.headers(headers).send().await.map_err(|_| {
            failure(
                &owner.server,
                McpErrorKind::Connect,
                "SSE HTTP request failed",
                "request",
            )
        })
    }
}

fn closed(owner: &McpClient) -> McpError {
    failure(
        &owner.server,
        McpErrorKind::Connect,
        "SSE transport closed",
        "close",
    )
}

async fn until_closed<T>(
    owner: &McpClient,
    work: impl Future<Output = Result<T, McpError>>,
) -> Result<T, McpError> {
    let mut close = owner.closed.subscribe();
    tokio::select! {
        biased;
        () = async { let _ = close.wait_for(|closed| *closed).await; } => Err(closed(owner)),
        result = work => result,
    }
}

// Bound both unterminated lines and accumulated multiline events. Keep unread chunk bytes
// across the endpoint handshake: its trailing bytes may already contain the next event.
const MAX_EVENT_BYTES: usize = 4 * 1024 * 1024;
struct EventStream {
    response: Response,
    chunk: axum::body::Bytes,
    skip_lf: bool,
    first: bool,
}

impl EventStream {
    fn new(response: Response) -> Self {
        Self {
            response,
            chunk: Default::default(),
            skip_lf: false,
            first: true,
        }
    }

    async fn next(&mut self) -> Result<(String, String), &'static str> {
        let mut line = Vec::new();
        let mut kind = String::new();
        let mut data = String::new();
        loop {
            if self.chunk.is_empty() {
                self.chunk = self
                    .response
                    .chunk()
                    .await
                    .map_err(|_| "SSE stream read failed")?
                    .ok_or("SSE transport closed")?;
                continue;
            }
            if std::mem::take(&mut self.skip_lf) && self.chunk[0] == b'\n' {
                self.chunk = self.chunk.slice(1..);
                continue;
            }
            let end = self.chunk.iter().position(|b| matches!(b, b'\r' | b'\n'));
            let len = end.unwrap_or(self.chunk.len());
            if len > MAX_EVENT_BYTES.saturating_sub(data.len() + kind.len() + line.len()) {
                return Err("SSE event too large");
            }
            line.extend_from_slice(&self.chunk.split_to(len));
            if end.is_none() {
                continue;
            }
            self.skip_lf = self.chunk.split_to(1)[0] == b'\r';
            let text = std::str::from_utf8(&line).map_err(|_| "invalid SSE encoding")?;
            let text = if std::mem::take(&mut self.first) {
                text.trim_start_matches('\u{feff}')
            } else {
                text
            };
            if text.is_empty() {
                if data.pop().is_some() {
                    return Ok((kind, data));
                }
                kind.clear();
            } else {
                let (field, value) = text.split_once(':').unwrap_or((text, ""));
                let value = value.strip_prefix(' ').unwrap_or(value);
                match field {
                    "event" => {
                        kind.clear();
                        kind.push_str(value);
                    }
                    "data" => {
                        data.push_str(value);
                        data.push('\n');
                    }
                    _ => {}
                }
            }
            line.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn malformed_reply_fails_only_its_request() {
        let root = tempfile::tempdir().unwrap();
        let logger = Arc::new(Mutex::new(
            McpLogger::new("fixture", root.path(), None).unwrap(),
        ));
        let spec = McpTransportSpec::Http {
            url: "http://127.0.0.1/events".parse().unwrap(),
            headers: Default::default(),
        };
        let client = McpClient::materialize_sse("fixture", &spec, logger)
            .await
            .unwrap();
        let (bad_tx, bad_rx) = tokio::sync::oneshot::channel();
        let (good_tx, good_rx) = tokio::sync::oneshot::channel();
        client.pending.lock().unwrap().insert(1, bad_tx);
        client.pending.lock().unwrap().insert(2, good_tx);

        assert!(client.sse_message(json!({"jsonrpc":"2.0","id":1})).is_ok());
        assert!(bad_rx.await.unwrap().is_err());
        assert!(
            client
                .sse_message(json!({"jsonrpc":"2.0","id":2,"result":{"ok":true}}))
                .is_ok()
        );
        assert_eq!(good_rx.await.unwrap().unwrap()["ok"], true);
        assert!(!*client.closed.borrow());
    }
}
