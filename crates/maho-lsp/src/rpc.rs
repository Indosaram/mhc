//! Content-Length framed JSON-RPC 2.0 over an async byte stream (LSP base protocol).
use crate::types::Diagnostic;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

/// Largest accepted frame body.
pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// Reads one frame body. `Ok(None)` at a clean end of stream.
pub async fn read_frame<R: AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
) -> std::io::Result<Option<Vec<u8>>> {
    let mut length: Option<usize> = None;
    let mut saw_header = false;
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).await?;
        if read == 0 {
            return if saw_header {
                Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "stream ended inside a frame header",
                ))
            } else {
                Ok(None)
            };
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            if !saw_header {
                continue;
            }
            break;
        }
        saw_header = true;
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().ok();
        }
    }
    let length = length.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame without Content-Length",
        )
    })?;
    if length > MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).await?;
    Ok(Some(body))
}

/// Header + body for one message.
pub fn encode_frame(body: &[u8]) -> Vec<u8> {
    let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    frame.extend_from_slice(body);
    frame
}

/// Writes one frame and flushes.
pub async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    body: &[u8],
) -> std::io::Result<()> {
    writer.write_all(&encode_frame(body)).await?;
    writer.flush().await
}

/// JSON-RPC error object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RpcFailure {
    pub code: i64,
    pub message: String,
}

/// Why a request produced no result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RequestError {
    Rpc(RpcFailure),
    Closed,
    Timeout,
}

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, RpcFailure>>>>>;

/// A live JSON-RPC peer. Server-initiated requests get the minimal client answers; published
/// diagnostics are stored per URI.
pub struct Connection {
    outgoing: mpsc::UnboundedSender<Vec<u8>>,
    pending: Pending,
    next_id: AtomicI64,
    closed: Arc<AtomicBool>,
    shutdown: Arc<tokio::sync::Notify>,
    diagnostics: Arc<Mutex<HashMap<String, Vec<Diagnostic>>>>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Connection {
    /// Starts reader and writer tasks on the current tokio runtime.
    pub fn start<R, W>(reader: R, writer: W) -> Arc<Self>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        Self::start_buffered(BufReader::new(reader), writer)
    }

    /// Like [`Connection::start`] for a reader that may already hold buffered bytes.
    pub fn start_buffered<R, W>(mut reader: BufReader<R>, mut writer: W) -> Arc<Self>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (outgoing, mut queue) = mpsc::unbounded_channel::<Vec<u8>>();
        let connection = Arc::new(Self {
            outgoing: outgoing.clone(),
            pending: Arc::default(),
            next_id: AtomicI64::new(1),
            closed: Arc::new(AtomicBool::new(false)),
            shutdown: Arc::new(tokio::sync::Notify::new()),
            diagnostics: Arc::default(),
        });
        let closed = Arc::clone(&connection.closed);
        let pending = Arc::clone(&connection.pending);
        let shutdown = Arc::clone(&connection.shutdown);
        tokio::spawn(async move {
            loop {
                let body = tokio::select! {
                    body = queue.recv() => body,
                    () = shutdown.notified() => None,
                };
                let Some(body) = body else { break };
                if write_frame(&mut writer, &body).await.is_err() {
                    break;
                }
            }
            let _flushed = writer.shutdown().await;
            drop(writer);
            closed.store(true, Ordering::SeqCst);
            lock(&pending).clear();
        });
        let closed = Arc::clone(&connection.closed);
        let pending = Arc::clone(&connection.pending);
        let diagnostics = Arc::clone(&connection.diagnostics);
        tokio::spawn(async move {
            while let Ok(Some(body)) = read_frame(&mut reader).await {
                let Ok(message) = serde_json::from_slice::<Value>(&body) else {
                    continue;
                };
                dispatch(message, &pending, &diagnostics, &outgoing);
            }
            closed.store(true, Ordering::SeqCst);
            lock(&pending).clear();
        });
        connection
    }

    /// Stops writing (closing the write half) after already queued messages are flushed best-effort.
    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.shutdown.notify_one();
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst) || self.outgoing.is_closed()
    }

    /// Sends a request and waits up to `timeout` for its response.
    pub async fn request(
        &self,
        method: &str,
        params: Option<Value>,
        timeout: Duration,
    ) -> Result<Value, RequestError> {
        if self.is_closed() {
            return Err(RequestError::Closed);
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = oneshot::channel();
        lock(&self.pending).insert(id, sender);
        let mut message = json!({"jsonrpc": "2.0", "id": id, "method": method});
        if let Some(params) = params {
            message["params"] = params;
        }
        if self
            .outgoing
            .send(message.to_string().into_bytes())
            .is_err()
        {
            lock(&self.pending).remove(&id);
            return Err(RequestError::Closed);
        }
        match tokio::time::timeout(timeout, receiver).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(failure))) => Err(RequestError::Rpc(failure)),
            Ok(Err(_)) => Err(RequestError::Closed),
            Err(_) => {
                lock(&self.pending).remove(&id);
                Err(RequestError::Timeout)
            }
        }
    }

    /// Sends a notification. Returns `false` when the connection is closed.
    pub fn notify(&self, method: &str, params: Option<Value>) -> bool {
        if self.is_closed() {
            return false;
        }
        let mut message = json!({"jsonrpc": "2.0", "method": method});
        if let Some(params) = params {
            message["params"] = params;
        }
        self.outgoing.send(message.to_string().into_bytes()).is_ok()
    }

    /// Last diagnostics the server published for `uri`.
    pub fn stored_diagnostics(&self, uri: &str) -> Vec<Diagnostic> {
        lock(&self.diagnostics)
            .get(uri)
            .cloned()
            .unwrap_or_default()
    }

    pub fn clear_diagnostics(&self) {
        lock(&self.diagnostics).clear();
    }
}

fn dispatch(
    message: Value,
    pending: &Pending,
    diagnostics: &Mutex<HashMap<String, Vec<Diagnostic>>>,
    outgoing: &mpsc::UnboundedSender<Vec<u8>>,
) {
    let method = message.get("method").and_then(Value::as_str);
    let id = message.get("id").cloned().filter(|id| !id.is_null());
    match (method, id) {
        (None, Some(id)) => {
            let Some(id) = id.as_i64() else { return };
            let Some(sender) = lock(pending).remove(&id) else {
                return;
            };
            let outcome = match message.get("error") {
                Some(error) => Err(RpcFailure {
                    code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                }),
                None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
            };
            let _delivered = sender.send(outcome);
        }
        (Some(method), Some(id)) => {
            let reply = match server_request_result(method, message.get("params")) {
                Some(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                None => {
                    json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("Unhandled method {method}")}})
                }
            };
            let _queued = outgoing.send(reply.to_string().into_bytes());
        }
        (Some("textDocument/publishDiagnostics"), None) => {
            let Some(params) = message.get("params") else {
                return;
            };
            let Some(uri) = params.get("uri").and_then(Value::as_str) else {
                return;
            };
            let items = params
                .get("diagnostics")
                .and_then(|value| serde_json::from_value::<Vec<Diagnostic>>(value.clone()).ok())
                .unwrap_or_default();
            lock(diagnostics).insert(uri.to_owned(), items);
        }
        _ => {}
    }
}

/// Answers for the server→client requests a minimal client supports; `None` = method not found.
pub fn server_request_result(method: &str, params: Option<&Value>) -> Option<Value> {
    match method {
        "workspace/configuration" => {
            let items = params
                .and_then(|params| params.get("items"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            Some(Value::Array(
                items
                    .iter()
                    .map(|item| {
                        if item.get("section").and_then(Value::as_str) == Some("json") {
                            json!({"validate": {"enable": true}})
                        } else {
                            json!({})
                        }
                    })
                    .collect(),
            ))
        }
        "client/registerCapability"
        | "client/unregisterCapability"
        | "window/workDoneProgress/create" => Some(Value::Null),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frames_round_trip_and_tolerate_extra_headers() {
        let mut bytes =
            b"Content-Type: application/vscode-jsonrpc\r\nContent-Length: 2\r\n\r\n{}".to_vec();
        bytes.extend(encode_frame(b"[1]"));
        let mut reader = BufReader::new(bytes.as_slice());
        assert_eq!(
            read_frame(&mut reader).await.expect("frame 1"),
            Some(b"{}".to_vec())
        );
        assert_eq!(
            read_frame(&mut reader).await.expect("frame 2"),
            Some(b"[1]".to_vec())
        );
        assert_eq!(read_frame(&mut reader).await.expect("eof"), None);
    }

    #[tokio::test]
    async fn request_response_and_server_requests() {
        let (client_side, server_side) = tokio::io::duplex(1 << 16);
        let (client_read, client_write) = tokio::io::split(client_side);
        let connection = Connection::start(client_read, client_write);
        let (server_read, mut server_write) = tokio::io::split(server_side);
        let server = tokio::spawn(async move {
            let mut reader = BufReader::new(server_read);
            let request: Value = serde_json::from_slice(
                &read_frame(&mut reader).await.expect("read").expect("frame"),
            )
            .expect("json");
            let ask = json!({"jsonrpc":"2.0","id":"s1","method":"workspace/configuration","params":{"items":[{"section":"json"},{}]}});
            write_frame(&mut server_write, ask.to_string().as_bytes())
                .await
                .expect("ask");
            let answer: Value = serde_json::from_slice(
                &read_frame(&mut reader).await.expect("read").expect("frame"),
            )
            .expect("json");
            let publish = json!({"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":"file:///a","diagnostics":[{"range":{"start":{"line":0,"character":1},"end":{"line":0,"character":2}},"message":"m","severity":1}]}});
            write_frame(&mut server_write, publish.to_string().as_bytes())
                .await
                .expect("publish");
            let reply =
                json!({"jsonrpc":"2.0","id":request["id"],"result":{"echo":request["params"]}});
            write_frame(&mut server_write, reply.to_string().as_bytes())
                .await
                .expect("reply");
            answer
        });
        let result = connection
            .request("x/echo", Some(json!(5)), Duration::from_secs(5))
            .await
            .expect("result");
        assert_eq!(result, json!({"echo": 5}));
        let answer = server.await.expect("server");
        assert_eq!(answer["id"], "s1");
        assert_eq!(answer["result"], json!([{"validate":{"enable":true}}, {}]));
        assert_eq!(connection.stored_diagnostics("file:///a").len(), 1);
    }

    #[tokio::test]
    async fn timeout_and_close() {
        let (client_side, server_side) = tokio::io::duplex(1024);
        let (read, write) = tokio::io::split(client_side);
        let connection = Connection::start(read, write);
        assert_eq!(
            connection
                .request("slow", None, Duration::from_millis(20))
                .await,
            Err(RequestError::Timeout)
        );
        drop(server_side);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(connection.is_closed());
        assert_eq!(
            connection.request("x", None, Duration::from_secs(1)).await,
            Err(RequestError::Closed)
        );
    }
}
