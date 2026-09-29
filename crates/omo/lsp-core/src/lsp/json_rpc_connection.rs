use crate::abort::AbortSignal;
use crate::lsp::errors::LspError;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWrite;
use tokio::io::AsyncWriteExt;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

const HEADER_SEPARATOR: &[u8] = b"\r\n\r\n";
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INTERNAL_ERROR: i64 = -32603;

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;
type NotificationHandler = Arc<dyn Fn(Value) -> Result<(), LspError> + Send + Sync>;
type RequestHandler = Arc<dyn Fn(Value) -> BoxFuture<Result<Value, LspError>> + Send + Sync>;
type CloseHandler = Arc<dyn Fn() + Send + Sync>;
type ErrorHandler = Arc<dyn Fn(&LspError) + Send + Sync>;
type Reader = Box<dyn AsyncRead + Send + Unpin>;
type Writer = Box<dyn AsyncWrite + Send + Unpin>;
type PendingSender = oneshot::Sender<Result<Value, LspError>>;

#[derive(Default)]
struct State {
    pending: HashMap<String, PendingSender>,
    notification_handlers: HashMap<String, NotificationHandler>,
    request_handlers: HashMap<String, RequestHandler>,
    close_handlers: Vec<CloseHandler>,
    error_handlers: Vec<ErrorHandler>,
    next_request_id: i64,
    reader: Option<Reader>,
    reader_task: Option<JoinHandle<()>>,
    disposed: bool,
}

struct Shared {
    state: Mutex<State>,
    writer: tokio::sync::Mutex<Writer>,
}

/// TS `JsonRpcConnection`: Content-Length framed JSON-RPC 2.0 over a byte stream pair.
#[derive(Clone)]
pub struct JsonRpcConnection {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for JsonRpcConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JsonRpcConnection")
            .field("pending", &self.pending_request_count())
            .finish()
    }
}

impl JsonRpcConnection {
    pub fn new(
        reader: impl AsyncRead + Send + Unpin + 'static,
        writer: impl AsyncWrite + Send + Unpin + 'static,
    ) -> Self {
        let state = State {
            next_request_id: 1,
            reader: Some(Box::new(reader)),
            ..State::default()
        };
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(state),
                writer: tokio::sync::Mutex::new(Box::new(writer)),
            }),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// TS `listen`: starts the reader task. Must be called inside a tokio runtime.
    pub fn listen(&self) {
        let mut state = self.state();
        let Some(reader) = state.reader.take() else {
            return;
        };
        let connection = self.clone();
        state.reader_task = Some(tokio::spawn(connection.read_loop(reader)));
    }

    pub fn on_notification(
        &self,
        method: &str,
        handler: impl Fn(Value) -> Result<(), LspError> + Send + Sync + 'static,
    ) {
        self.state()
            .notification_handlers
            .insert(method.to_string(), Arc::new(handler));
    }

    pub fn on_request<F>(&self, method: &str, handler: impl Fn(Value) -> F + Send + Sync + 'static)
    where
        F: Future<Output = Result<Value, LspError>> + Send + 'static,
    {
        let handler: RequestHandler = Arc::new(move |params| Box::pin(handler(params)));
        self.state()
            .request_handlers
            .insert(method.to_string(), handler);
    }

    pub fn on_close(&self, handler: impl Fn() + Send + Sync + 'static) {
        self.state().close_handlers.push(Arc::new(handler));
    }

    pub fn on_error(&self, handler: impl Fn(&LspError) + Send + Sync + 'static) {
        self.state().error_handlers.push(Arc::new(handler));
    }

    /// TS `sendRequest`. An abort after the request was written sends
    /// `$/cancelRequest` with the same id; a late response is then ignored.
    pub async fn send_request(
        &self,
        method: &str,
        params: Option<Value>,
        signal: Option<&AbortSignal>,
    ) -> Result<Value, LspError> {
        let (id, receiver) = {
            let mut state = self.state();
            if state.disposed {
                return Err(LspError::other("JSON-RPC connection is disposed"));
            }
            let id = state.next_request_id;
            state.next_request_id += 1;
            let (sender, receiver) = oneshot::channel();
            state.pending.insert(id.to_string(), sender);
            (id, receiver)
        };
        let key = id.to_string();
        if let Some(signal) = signal.filter(|signal| signal.aborted()) {
            self.state().pending.remove(&key);
            return Err(abort_error(signal));
        }

        let mut message = json!({ "jsonrpc": "2.0", "id": id, "method": method });
        if let Some(params) = params {
            message["params"] = params;
        }
        if let Err(error) = self.write_message(&message).await {
            self.state().pending.remove(&key);
            return Err(error);
        }

        let mut receiver = receiver;
        let outcome = match signal {
            Some(signal) => {
                tokio::select! {
                    biased;
                    response = &mut receiver => Some(response),
                    () = signal.cancelled() => None,
                }
            }
            None => Some(receiver.await),
        };
        match outcome {
            Some(Ok(result)) => result,
            Some(Err(_)) => Err(LspError::other("JSON-RPC connection disposed")),
            None => {
                self.state().pending.remove(&key);
                let cancel = json!({ "jsonrpc": "2.0", "method": "$/cancelRequest", "params": { "id": id } });
                if let Err(error) = self.write_message(&cancel).await {
                    self.emit_error(&error);
                }
                Err(signal.map_or(LspError::Aborted, abort_error))
            }
        }
    }

    pub fn pending_request_count(&self) -> usize {
        self.state().pending.len()
    }

    pub async fn send_notification(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> Result<(), LspError> {
        if self.state().disposed {
            return Ok(());
        }
        let mut message = json!({ "jsonrpc": "2.0", "method": method });
        if let Some(params) = params {
            message["params"] = params;
        }
        self.write_message(&message).await
    }

    /// TS `dispose`: stops reading and rejects every pending request.
    pub fn dispose(&self) {
        let pending = {
            let mut state = self.state();
            if state.disposed {
                return;
            }
            state.disposed = true;
            if let Some(task) = state.reader_task.take() {
                task.abort();
            }
            state.reader = None;
            state.notification_handlers.clear();
            state.request_handlers.clear();
            std::mem::take(&mut state.pending)
        };
        for sender in pending.into_values() {
            let _ = sender.send(Err(LspError::other("JSON-RPC connection disposed")));
        }
    }

    async fn read_loop(self, mut reader: Reader) {
        let mut buffer: Vec<u8> = Vec::new();
        let mut chunk = vec![0_u8; 64 * 1024];
        loop {
            match reader.read(&mut chunk).await {
                Ok(0) => break,
                Ok(read) => {
                    buffer.extend_from_slice(&chunk[..read]);
                    self.drain_input_buffer(&mut buffer);
                }
                Err(error) => {
                    self.emit_error(&LspError::from(error));
                    break;
                }
            }
        }
        let handlers = self.state().close_handlers.clone();
        for handler in handlers {
            handler();
        }
    }

    fn drain_input_buffer(&self, buffer: &mut Vec<u8>) {
        loop {
            let Some(header_end) = find_subsequence(buffer, HEADER_SEPARATOR) else {
                return;
            };
            let headers = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
            let Some(content_length) = parse_content_length(&headers) else {
                buffer.clear();
                self.emit_error(&LspError::other(
                    "JSON-RPC message is missing Content-Length header",
                ));
                return;
            };
            let body_start = header_end + HEADER_SEPARATOR.len();
            let body_end = body_start + content_length;
            if buffer.len() < body_end {
                return;
            }
            let body = String::from_utf8_lossy(&buffer[body_start..body_end]).into_owned();
            buffer.drain(..body_end);
            self.dispatch_body(&body);
        }
    }

    fn dispatch_body(&self, body: &str) {
        let parsed: Value = match serde_json::from_str(body) {
            Ok(parsed) => parsed,
            Err(error) => {
                self.spawn_write_error(Value::Null, PARSE_ERROR, error.to_string());
                return;
            }
        };
        let Value::Object(message) = parsed else {
            self.spawn_write_error(
                Value::Null,
                INVALID_REQUEST,
                "Invalid JSON-RPC message".to_string(),
            );
            return;
        };
        if message.contains_key("id")
            && (message.contains_key("result") || message.contains_key("error"))
        {
            self.handle_response(&message);
            return;
        }
        let Some(method) = message
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            let id = message_id(&message).unwrap_or(Value::Null);
            self.spawn_write_error(id, INVALID_REQUEST, "Invalid JSON-RPC method".to_string());
            return;
        };
        if message.contains_key("id") {
            self.handle_request(&message, &method);
            return;
        }
        let handler = self.state().notification_handlers.get(&method).cloned();
        if let Some(handler) = handler
            && let Err(error) = handler(message.get("params").cloned().unwrap_or(Value::Null))
        {
            self.emit_error(&error);
        }
    }

    fn handle_response(&self, message: &Map<String, Value>) {
        let Some(id) = message_id(message) else {
            return;
        };
        let Some(sender) = self.state().pending.remove(&id_key(&id)) else {
            return;
        };
        let outcome = match message.get("error") {
            Some(error) => Err(json_rpc_error_to_error(error)),
            None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
        };
        let _ = sender.send(outcome);
    }

    fn handle_request(&self, message: &Map<String, Value>, method: &str) {
        let Some(id) = message_id(message) else {
            self.spawn_write_error(
                Value::Null,
                INVALID_REQUEST,
                "Invalid JSON-RPC id".to_string(),
            );
            return;
        };
        let Some(handler) = self.state().request_handlers.get(method).cloned() else {
            self.spawn_write_error(id, METHOD_NOT_FOUND, format!("Method not found: {method}"));
            return;
        };
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let connection = self.clone();
        tokio::spawn(async move {
            let written = match handler(params).await {
                Ok(result) => {
                    connection
                        .write_message(&json!({ "jsonrpc": "2.0", "id": id, "result": result }))
                        .await
                }
                Err(error) => {
                    connection
                        .write_error(id, INTERNAL_ERROR, error.to_string())
                        .await
                }
            };
            if let Err(error) = written {
                connection.emit_error(&error);
            }
        });
    }

    fn spawn_write_error(&self, id: Value, code: i64, message: String) {
        let connection = self.clone();
        tokio::spawn(async move {
            if let Err(error) = connection.write_error(id, code, message).await {
                connection.emit_error(&error);
            }
        });
    }

    async fn write_error(&self, id: Value, code: i64, message: String) -> Result<(), LspError> {
        self.write_message(
            &json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }),
        )
        .await
    }

    async fn write_message(&self, message: &Value) -> Result<(), LspError> {
        let body = message.to_string();
        let payload = format!("Content-Length: {}\r\n\r\n{body}", body.len());
        let mut writer = self.shared.writer.lock().await;
        let written = match writer.write_all(payload.as_bytes()).await {
            Ok(()) => writer.flush().await,
            Err(error) => Err(error),
        };
        written.map_err(|error| match error.kind() {
            std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset => {
                LspError::other(format!("stream was destroyed: {error}"))
            }
            _ => LspError::from(error),
        })
    }

    fn emit_error(&self, error: &LspError) {
        let handlers = self.state().error_handlers.clone();
        for handler in handlers {
            handler(error);
        }
    }
}

fn abort_error(signal: &AbortSignal) -> LspError {
    signal
        .reason()
        .unwrap_or_else(|| LspError::other("LSP request cancelled"))
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Mirrors `Number.parseInt` on the trimmed header value; negative lengths are rejected.
fn parse_content_length(headers: &str) -> Option<usize> {
    for line in headers.split("\r\n") {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if !name.trim().eq_ignore_ascii_case("content-length") {
            continue;
        }
        let value = value.trim();
        let digits: String = value.chars().take_while(char::is_ascii_digit).collect();
        return digits.parse().ok();
    }
    None
}

fn message_id(message: &Map<String, Value>) -> Option<Value> {
    match message.get("id") {
        Some(id @ (Value::Number(_) | Value::String(_) | Value::Null)) => Some(id.clone()),
        _ => None,
    }
}

fn id_key(id: &Value) -> String {
    match id {
        Value::String(value) => value.clone(),
        other => other.to_string(),
    }
}

fn json_rpc_error_to_error(value: &Value) -> LspError {
    let Some(error) = value.as_object() else {
        return LspError::other("JSON-RPC request failed");
    };
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("JSON-RPC request failed")
        .to_string();
    LspError::JsonRpc {
        code: error.get("code").and_then(Value::as_i64),
        message,
    }
}

#[cfg(test)]
#[path = "json_rpc_connection_tests.rs"]
mod tests;
