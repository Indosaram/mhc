//! JSON-RPC stdio server: request dispatch, idle timeout, and shutdown outcomes.
//!
//! This mirrors the reference implementation's `runJsonRpcStdioServer`: requests are read from a
//! byte stream using [`crate::transport`] framing (line or `Content-Length`, auto-detected), handed
//! to a request handler, and the returned response is written back with the framing the request
//! arrived in. Two optional stop conditions settle the server: an idle timeout and an opt-in
//! parent-liveness watchdog (see [`crate::watchdog`]).

use std::io::Read;
use std::io::Write;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::RecvTimeoutError;
use std::sync::mpsc::Sender;
use std::time::Duration;
use std::time::Instant;

use serde_json::Value;

use crate::record::is_plain_record;
use crate::responses::error_response;
use crate::responses::json_rpc_id;
use crate::responses::message_from_error;
use crate::transport::StdioJsonRpcDecoder;
use crate::transport::StdioJsonRpcMessage;
use crate::transport::StdioJsonRpcResponseMode;
use crate::transport::write_stdio_json_rpc_response;
use crate::types::JsonRpcId;
use crate::types::JsonRpcResponse;
use crate::types::McpLifecycleLog;
use crate::types::McpLogFields;
use crate::types::NoopLog;
use crate::watchdog::ParentWatchdogConfig;
use crate::watchdog::create_parent_watchdog;

/// Default idle timeout: ten minutes, matching the reference implementation.
pub const DEFAULT_IDLE_TIMEOUT_MS: u64 = 10 * 60_000;

/// How long the dispatch loop waits for input before re-checking the stop conditions.
///
/// The reference implementation parks on its event loop and therefore reacts to the parent
/// watchdog instantly; a blocking reader cannot, so the port polls in short slices and settles as
/// soon as a stop condition is observed.
const STOP_CHECK_INTERVAL: Duration = Duration::from_millis(25);

/// Reader buffer size for a single `read` call.
const READ_CHUNK_BYTES: usize = 8 * 1024;

/// Observes handler failures when `on_handler_error` is configured instead of aborting the server.
pub type HandlerErrorObserver<E> = Arc<dyn Fn(&E) + Send + Sync>;

/// Replaces the default `-32700` response for a malformed payload; `None` skips the response.
pub type ParseErrorResponseOverride = Arc<dyn Fn(&str) -> Option<JsonRpcResponse> + Send + Sync>;

/// Lifecycle notification hook (`on_idle_timeout`, `on_parent_exit`).
pub type LifecycleHook = Arc<dyn Fn() + Send + Sync>;

/// Why [`run_json_rpc_stdio_server`] returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerOutcome {
    /// The input stream reached end of file (or the reader stopped without error).
    InputClosed,
    /// No message arrived within the configured idle timeout.
    IdleTimeout,
    /// The watched parent process disappeared.
    ParentExit,
    /// The output closed while a response was written; the server stopped quietly.
    TerminalOutputError,
}

/// Failure that ends the server before any stop condition fires.
#[derive(Debug)]
pub enum ServerError<E> {
    /// Reading the input or writing to a non-terminal output failed.
    Io(std::io::Error),
    /// The handler failed and no `on_handler_error` observer was configured.
    Handler(E),
}

impl<E: std::fmt::Display> std::fmt::Display for ServerError<E> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServerError::Io(error) => write!(formatter, "{error}"),
            ServerError::Handler(error) => write!(formatter, "{error}"),
        }
    }
}

impl<E: std::fmt::Debug + std::fmt::Display> std::error::Error for ServerError<E> {}

/// Server configuration, mirroring the reference implementation's `JsonRpcStdioServerConfig`.
///
/// `handlerOptions` has no counterpart: a Rust handler closes over whatever state it needs.
pub struct JsonRpcStdioServerConfig<H, E> {
    /// Handles one decoded request payload. Returning `Ok(None)` skips the request silently.
    pub handler: H,
    /// Idle timeout in milliseconds; `Some(0)` disables it. Defaults to
    /// [`DEFAULT_IDLE_TIMEOUT_MS`].
    pub idle_timeout_ms: Option<u64>,
    /// Called once when the idle timeout expires.
    pub on_idle_timeout: Option<LifecycleHook>,
    /// Opt-in parent-liveness watchdog; `None` creates no timer at all.
    pub parent_watchdog: Option<ParentWatchdogConfig>,
    /// Called once when the watched parent is reported dead.
    pub on_parent_exit: Option<LifecycleHook>,
    /// Receives lifecycle events; defaults to a no-op log.
    pub log: Option<Arc<dyn McpLifecycleLog + Send + Sync>>,
    /// Replaces the default JSON parse error response.
    pub parse_error_response: Option<ParseErrorResponseOverride>,
    /// Observes handler failures; when absent a handler failure aborts the server.
    pub on_handler_error: Option<HandlerErrorObserver<E>>,
}

impl<H, E> JsonRpcStdioServerConfig<H, E> {
    /// Builds a configuration with defaults and the given handler.
    pub fn new(handler: H) -> Self {
        Self {
            handler,
            idle_timeout_ms: None,
            on_idle_timeout: None,
            parent_watchdog: None,
            on_parent_exit: None,
            log: None,
            parse_error_response: None,
            on_handler_error: None,
        }
    }
}

/// Serves JSON-RPC requests from `input` to `output` until a stop condition is observed.
///
/// `input` is read on a detached worker thread, so it must be `Send + 'static` (for standard
/// input pass `std::io::stdin()`); `output` is written synchronously on the calling thread.
pub fn run_json_rpc_stdio_server<R, W, H, E>(
    input: R,
    output: &mut W,
    config: JsonRpcStdioServerConfig<H, E>,
) -> Result<ServerOutcome, ServerError<E>>
where
    R: Read + Send + 'static,
    W: Write,
    H: FnMut(&Value) -> Result<Option<JsonRpcResponse>, E>,
    E: std::fmt::Display,
{
    let JsonRpcStdioServerConfig {
        handler,
        idle_timeout_ms,
        on_idle_timeout,
        parent_watchdog,
        on_parent_exit,
        log,
        parse_error_response,
        on_handler_error,
    } = config;
    let idle_timeout_ms = idle_timeout_ms.unwrap_or(DEFAULT_IDLE_TIMEOUT_MS);
    let log: Arc<dyn McpLifecycleLog + Send + Sync> = log.unwrap_or_else(|| Arc::new(NoopLog));
    let stop = Stop::new();
    let (receiver, reader) = spawn_reader(input);

    log.log("stdio_started", Some(&started_fields(idle_timeout_ms)));

    let mut watchdog = create_parent_watchdog(parent_watchdog, {
        let stop = stop.clone();
        let log = Arc::clone(&log);
        let on_parent_exit = on_parent_exit.clone();
        move |parent_pid, poll_interval_ms| {
            log.log(
                "parent_exit",
                Some(&log_fields(&[
                    ("parent_pid", Value::from(parent_pid)),
                    ("poll_interval_ms", Value::from(poll_interval_ms)),
                ])),
            );
            stop.close(ServerOutcome::ParentExit);
            if let Some(hook) = on_parent_exit.as_ref() {
                hook();
            }
        }
    });

    let shared = SharedState {
        stop,
        idle_timeout_ms,
        on_idle_timeout,
    };
    let mut server = Server {
        receiver,
        reader: Some(reader),
        output,
        handler,
        on_handler_error,
        parse_error_response,
        log: Arc::clone(&log),
    };
    let outcome = server.serve(&shared);

    watchdog.clear();
    log.log("stdio_stopped", None);
    outcome
}

/// Stop conditions shared with the watchdog thread.
struct SharedState {
    stop: Stop,
    idle_timeout_ms: u64,
    on_idle_timeout: Option<LifecycleHook>,
}

/// First stop condition wins; later ones are ignored.
#[derive(Clone)]
struct Stop {
    closed: Arc<std::sync::atomic::AtomicBool>,
    outcome: Arc<std::sync::Mutex<Option<ServerOutcome>>>,
}

impl Stop {
    fn new() -> Self {
        Self {
            closed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            outcome: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    fn close(&self, outcome: ServerOutcome) {
        let mut recorded = self
            .outcome
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if recorded.is_none() {
            *recorded = Some(outcome);
        }
        drop(recorded);
        self.closed.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn outcome(&self) -> Option<ServerOutcome> {
        *self
            .outcome
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// An input event produced by the reader thread.
enum InputEvent {
    Messages(Vec<StdioJsonRpcMessage>),
    End,
    ReadError(std::io::Error),
}

/// Reads `input` on a worker thread so the dispatch loop can observe stop conditions while the
/// underlying stream is blocked.
///
/// The thread is not joined when it is still blocked in `read` (a stream that never yields again
/// would deadlock the caller); it is joined when it already finished.
fn spawn_reader<R: Read + Send + 'static>(
    mut input: R,
) -> (Receiver<InputEvent>, std::thread::JoinHandle<()>) {
    let (sender, receiver) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let mut decoder = StdioJsonRpcDecoder::new();
        let mut buffer = vec![0u8; READ_CHUNK_BYTES];
        loop {
            match input.read(&mut buffer) {
                Ok(0) => {
                    if let Some(trailing) = decoder.finish()
                        && !send(&sender, InputEvent::Messages(vec![trailing]))
                    {
                        return;
                    }
                    let _ = send(&sender, InputEvent::End);
                    return;
                }
                Ok(read) => {
                    let messages = decoder.push(&buffer[..read]);
                    if !messages.is_empty() && !send(&sender, InputEvent::Messages(messages)) {
                        return;
                    }
                }
                Err(error) => {
                    if error.kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    let _ = send(&sender, InputEvent::ReadError(error));
                    return;
                }
            }
        }
    });
    (receiver, handle)
}

/// Sends one event; `false` means the dispatch loop has already returned.
fn send(sender: &Sender<InputEvent>, event: InputEvent) -> bool {
    sender.send(event).is_ok()
}

/// Dispatch loop and response writer for one server run.
struct Server<'a, W, H, E> {
    receiver: Receiver<InputEvent>,
    reader: Option<std::thread::JoinHandle<()>>,
    output: &'a mut W,
    handler: H,
    on_handler_error: Option<HandlerErrorObserver<E>>,
    parse_error_response: Option<ParseErrorResponseOverride>,
    log: Arc<dyn McpLifecycleLog + Send + Sync>,
}

impl<W, H, E> Server<'_, W, H, E>
where
    W: Write,
    H: FnMut(&Value) -> Result<Option<JsonRpcResponse>, E>,
    E: std::fmt::Display,
{
    /// Runs until a stop condition fires, then reaps a reader thread that already finished.
    fn serve(&mut self, shared: &SharedState) -> Result<ServerOutcome, ServerError<E>> {
        let mut last_activity = Instant::now();
        let outcome = loop {
            if let Some(outcome) = shared.stop.outcome() {
                break Ok(outcome);
            }
            let wait = match idle_remaining(shared.idle_timeout_ms, last_activity) {
                IdleWait::Expired => {
                    self.log.log(
                        "idle_timeout",
                        Some(&log_fields(&[(
                            "idle_timeout_ms",
                            Value::from(shared.idle_timeout_ms),
                        )])),
                    );
                    shared.stop.close(ServerOutcome::IdleTimeout);
                    if let Some(hook) = shared.on_idle_timeout.as_ref() {
                        hook();
                    }
                    break Ok(ServerOutcome::IdleTimeout);
                }
                IdleWait::Disabled => STOP_CHECK_INTERVAL,
                IdleWait::Pending(remaining) => remaining.min(STOP_CHECK_INTERVAL),
            };
            match self.receiver.recv_timeout(wait) {
                Ok(InputEvent::Messages(messages)) => {
                    last_activity = Instant::now();
                    let mut keep_serving = true;
                    for message in messages {
                        if let Some(outcome) = shared.stop.outcome() {
                            return Ok(outcome);
                        }
                        if !self.handle_message(message)? {
                            keep_serving = false;
                            break;
                        }
                    }
                    if !keep_serving {
                        break Ok(ServerOutcome::TerminalOutputError);
                    }
                }
                Ok(InputEvent::End) => {
                    break Ok(shared.stop.outcome().unwrap_or(ServerOutcome::InputClosed));
                }
                Ok(InputEvent::ReadError(error)) => match shared.stop.outcome() {
                    Some(outcome) if is_swallowable_read_error(&error) => break Ok(outcome),
                    _ => break Err(ServerError::Io(error)),
                },
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    break Ok(shared.stop.outcome().unwrap_or(ServerOutcome::InputClosed));
                }
            }
        };
        if let Some(reader) = self.reader.take()
            && reader.is_finished()
        {
            let _ = reader.join();
        }
        outcome
    }

    /// Handles one decoded message; `Ok(false)` means the loop must stop quietly.
    fn handle_message(&mut self, message: StdioJsonRpcMessage) -> Result<bool, ServerError<E>> {
        match message {
            StdioJsonRpcMessage::ParseError {
                message,
                response_mode,
            } => self.handle_parse_error(&message, response_mode),
            StdioJsonRpcMessage::Request {
                payload,
                response_mode,
            } => self.handle_request(&payload, response_mode),
        }
    }

    fn handle_parse_error(
        &mut self,
        message: &str,
        response_mode: StdioJsonRpcResponseMode,
    ) -> Result<bool, ServerError<E>> {
        self.log.log(
            "parse_error",
            Some(&log_fields(&[("message", Value::from(message))])),
        );
        let response = match self.parse_error_response.as_ref() {
            Some(override_response) => override_response(message),
            None => Some(error_response(
                JsonRpcId::Null,
                -32700,
                "Parse error",
                Some(Value::from(message)),
            )),
        };
        let Some(response) = response else {
            return Ok(true);
        };
        self.write_response(&response, response_mode)
    }

    fn handle_request(
        &mut self,
        payload: &Value,
        response_mode: StdioJsonRpcResponseMode,
    ) -> Result<bool, ServerError<E>> {
        let (id, method) = request_descriptor(payload);
        self.log.log(
            "request",
            Some(&log_fields(&[
                ("id", id.as_text().map(Value::from).unwrap_or(Value::Null)),
                (
                    "method",
                    method.clone().map(Value::from).unwrap_or(Value::Null),
                ),
            ])),
        );
        let response = match (self.handler)(payload) {
            Ok(response) => response,
            Err(error) => match self.on_handler_error.as_ref() {
                Some(observer) => {
                    observer(&error);
                    return Ok(true);
                }
                None => return Err(ServerError::Handler(error)),
            },
        };
        let Some(response) = response else {
            return Ok(true);
        };
        let id_text = response.id.to_string();
        let is_error = response.is_error();
        if !self.write_response(&response, response_mode)? {
            return Ok(false);
        }
        self.log.log(
            "response",
            Some(&log_fields(&[
                ("id", Value::from(id_text)),
                ("method", method.map(Value::from).unwrap_or(Value::Null)),
                ("is_error", Value::from(is_error)),
            ])),
        );
        Ok(true)
    }

    /// Writes one response; `Ok(false)` means the output closed (a terminal write failure).
    fn write_response(
        &mut self,
        response: &JsonRpcResponse,
        response_mode: StdioJsonRpcResponseMode,
    ) -> Result<bool, ServerError<E>> {
        match write_stdio_json_rpc_response(self.output, response, response_mode) {
            Ok(()) => Ok(true),
            Err(error) if is_terminal_output_error(&error) => {
                self.log.log(
                    "output_error",
                    Some(&log_fields(&[(
                        "message",
                        Value::from(message_from_error(&error)),
                    )])),
                );
                Ok(false)
            }
            Err(error) => Err(ServerError::Io(error)),
        }
    }
}

/// How much longer the server may stay idle.
enum IdleWait {
    Disabled,
    Expired,
    Pending(Duration),
}

fn idle_remaining(idle_timeout_ms: u64, last_activity: Instant) -> IdleWait {
    if idle_timeout_ms == 0 {
        return IdleWait::Disabled;
    }
    let idle_timeout = Duration::from_millis(idle_timeout_ms);
    match idle_timeout.checked_sub(last_activity.elapsed()) {
        Some(remaining) if !remaining.is_zero() => IdleWait::Pending(remaining),
        _ => IdleWait::Expired,
    }
}

/// Extracts the JSON-RPC id and method for logging; both fall back the way the reference does.
fn request_descriptor(payload: &Value) -> (JsonRpcId, Option<String>) {
    if !is_plain_record(payload) {
        return (JsonRpcId::Null, None);
    }
    let id = json_rpc_id(payload.get("id").unwrap_or(&Value::Null));
    let method = payload
        .get("method")
        .and_then(Value::as_str)
        .map(str::to_string);
    (id, method)
}

/// A terminal write failure means the peer closed the pipe; the server stops quietly.
fn is_terminal_output_error(error: &std::io::Error) -> bool {
    matches!(error.kind(), std::io::ErrorKind::BrokenPipe)
}

/// A read failure the port swallows when a stop condition already closed the server.
fn is_swallowable_read_error(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::UnexpectedEof
    )
}

fn started_fields(idle_timeout_ms: u64) -> McpLogFields {
    let cwd = std::env::current_dir()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_default();
    log_fields(&[
        ("cwd", Value::from(cwd)),
        ("idle_timeout_ms", Value::from(idle_timeout_ms)),
    ])
}

fn log_fields(entries: &[(&str, Value)]) -> McpLogFields {
    entries
        .iter()
        .map(|(name, value)| ((*name).to_string(), value.clone()))
        .collect()
}
