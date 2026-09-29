//! Port of `proxy.ts`: the thin stdio MCP proxy that forwards tool calls to the daemon.

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use lsp_core::abort::AbortController;
use lsp_core::mcp::{HandleLspMcpRequestOptions, handle_lsp_mcp_request};
use lsp_core::request_context::{
    LspRequestContext, StandaloneMcpRequestContextInput, create_standalone_mcp_request_context,
    scope_request_context,
};
use mcp_stdio_core::{
    JsonRpcId, JsonRpcResponse, JsonRpcStdioServerConfig, ParentWatchdogConfig, ServerError,
    ServerOutcome, error_response, json_rpc_id, run_json_rpc_stdio_server, success_response,
};
use serde_json::{Map, Value, json};

use crate::daemon_client::{CallToolOptions, EnsureFn, call_tool_via_daemon};
use crate::paths::{DaemonPaths, daemon_paths};

const DEFAULT_STARTUP_TIMEOUT_MS: u64 = 10_000;

/// Diagnostic sink (TS `stderr: Writable`); returning `Err` models a throwing stream.
pub type StderrSink = Arc<dyn Fn(&str) -> std::io::Result<()> + Send + Sync>;

/// TS `ProxyOptions`.
#[derive(Clone, Default)]
pub struct ProxyOptions {
    pub stderr: Option<StderrSink>,
    pub paths: Option<DaemonPaths>,
    pub context: Option<LspRequestContext>,
    pub cwd: Option<String>,
    /// `None` reads the process environment.
    pub env: Option<HashMap<String, String>>,
    pub home_dir: Option<String>,
    pub ensure: Option<EnsureFn>,
    pub startup_timeout_ms: Option<u64>,
    pub parent_watchdog: Option<ParentWatchdogConfig>,
    pub request_timeout_ms: Option<u64>,
}

#[derive(Debug)]
pub enum ProxyError {
    Context(String),
    Paths(String),
    Io(std::io::Error),
}

impl std::fmt::Display for ProxyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Context(message) | Self::Paths(message) => formatter.write_str(message),
            Self::Io(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ProxyError {}

/// Input side of the proxy: a pump thread forwards the caller's reader over a channel
/// so the stdio server sees end-of-input as soon as the startup watchdog fires or the
/// proxy stops, even while the underlying read is still blocked (TS `input.destroy()`).
struct LifecycleInput {
    chunks: std::sync::mpsc::Receiver<std::io::Result<Vec<u8>>>,
    pending: Vec<u8>,
    ended: Arc<AtomicBool>,
    lifecycle: AbortController,
}

const INPUT_STOP_CHECK: Duration = Duration::from_millis(10);

impl LifecycleInput {
    fn spawn<R: Read + Send + 'static>(
        mut inner: R,
        ended: Arc<AtomicBool>,
        lifecycle: AbortController,
    ) -> Self {
        let (sender, chunks) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut buffer = vec![0_u8; 8192];
            loop {
                match inner.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => {
                        if sender.send(Ok(buffer[..read].to_vec())).is_err() {
                            break;
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(error) => {
                        let _ignored = sender.send(Err(error));
                        break;
                    }
                }
            }
        });
        Self {
            chunks,
            pending: Vec::new(),
            ended,
            lifecycle,
        }
    }

    fn end(&self) -> std::io::Result<usize> {
        self.lifecycle.abort();
        Ok(0)
    }
}

impl Read for LifecycleInput {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        while self.pending.is_empty() {
            if self.ended.load(Ordering::SeqCst) {
                return self.end();
            }
            match self.chunks.recv_timeout(INPUT_STOP_CHECK) {
                Ok(Ok(chunk)) => self.pending = chunk,
                Ok(Err(error)) => {
                    self.lifecycle.abort();
                    return Err(error);
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return self.end(),
            }
        }
        let count = self.pending.len().min(buffer.len());
        buffer[..count].copy_from_slice(&self.pending[..count]);
        self.pending.drain(..count);
        Ok(count)
    }
}

/// TS `runMcpStdioProxy`.
///
/// Input end (or a startup watchdog expiry) aborts every in-flight daemon call.
pub fn run_mcp_stdio_proxy<R, W>(
    input: R,
    output: &mut W,
    options: ProxyOptions,
) -> Result<ServerOutcome, ProxyError>
where
    R: Read + Send + 'static,
    W: Write,
{
    let stderr: StderrSink = options.stderr.clone().unwrap_or_else(|| {
        Arc::new(|text: &str| {
            let mut stderr = std::io::stderr();
            stderr.write_all(text.as_bytes())
        })
    });
    let startup_timeout_ms = options
        .startup_timeout_ms
        .unwrap_or(DEFAULT_STARTUP_TIMEOUT_MS);
    let lifecycle = AbortController::new();
    let ended = Arc::new(AtomicBool::new(false));
    let first_request = Arc::new(StartupGate::default());
    let watchdog = (startup_timeout_ms > 0).then(|| {
        spawn_startup_watchdog(
            startup_timeout_ms,
            Arc::clone(&first_request),
            Arc::clone(&ended),
            lifecycle.clone(),
            Arc::clone(&stderr),
        )
    });

    let (input, parse_payloads) = ParseErrorPayloadReader::wrap(input);
    let result = serve(
        LifecycleInput::spawn(input, Arc::clone(&ended), lifecycle.clone()),
        parse_payloads,
        output,
        &options,
        &lifecycle,
        &first_request,
        &stderr,
    );
    first_request.disarm();
    ended.store(true, Ordering::SeqCst);
    if let Some(watchdog) = watchdog {
        let _joined = watchdog.join();
    }
    lifecycle.abort();
    result
}

fn serve<W>(
    input: LifecycleInput,
    parse_payloads: Arc<Mutex<VecDeque<String>>>,
    output: &mut W,
    options: &ProxyOptions,
    lifecycle: &AbortController,
    first_request: &Arc<StartupGate>,
    stderr: &StderrSink,
) -> Result<ServerOutcome, ProxyError>
where
    W: Write,
{
    let paths = match &options.paths {
        Some(paths) => paths.clone(),
        None => daemon_paths().map_err(|error| ProxyError::Paths(error.to_string()))?,
    };
    let context = match &options.context {
        Some(context) => context.clone(),
        None => standalone_context(options).map_err(ProxyError::Context)?,
    };
    let call_options = CallToolOptions {
        context: Some(context.clone()),
        paths: Some(paths),
        request_timeout_ms: options.request_timeout_ms,
        signal: Some(lifecycle.signal()),
        ensure: options.ensure.clone(),
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(ProxyError::Io)?;
    let gate = Arc::clone(first_request);
    let handler = move |request: &Value| -> Result<Option<JsonRpcResponse>, ProxyError> {
        gate.disarm();
        Ok(runtime.block_on(scope_request_context(
            context.clone(),
            handle_proxy_request(request, call_options.clone()),
        )))
    };
    let mut config = JsonRpcStdioServerConfig::new(handler);
    config.parse_error_response = Some(Arc::new(move |message: &str| {
        let data = match parse_payloads
            .lock()
            .ok()
            .and_then(|mut queue| queue.pop_front())
        {
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
    config.parent_watchdog = Some(options.parent_watchdog.clone().unwrap_or_default());
    let error_sink = Arc::clone(stderr);
    config.on_handler_error = Some(Arc::new(move |error: &ProxyError| {
        let _ignored = error_sink(&format!("[lsp-daemon] proxy error: {error}\n"));
    }));
    match run_json_rpc_stdio_server(input, output, config) {
        Ok(outcome) => Ok(outcome),
        Err(ServerError::Io(error)) => Err(ProxyError::Io(error)),
        Err(ServerError::Handler(error)) => Err(error),
    }
}

const MAX_PAYLOAD_ECHO_CHARS: usize = 200;

/// Sibling-gap adapter (private copy of lsp-core's): mcp-stdio-core parse errors carry only
/// serde's message, while TS `JSON.parse` quotes the offending text. This mirrors line-mode
/// framing and queues each unparseable line before the server sees the bytes.
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
        self.payloads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_back(echo);
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

/// One-shot latch: disarming before expiry cancels the startup watchdog.
#[derive(Default)]
struct StartupGate {
    disarmed: Mutex<bool>,
    changed: std::sync::Condvar,
}

impl StartupGate {
    fn disarm(&self) {
        *self.disarmed.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.changed.notify_all();
    }

    /// Waits up to `timeout`; `true` when the gate was disarmed first.
    fn wait_disarmed(&self, timeout: Duration) -> bool {
        let guard = self.disarmed.lock().unwrap_or_else(PoisonError::into_inner);
        let (guard, _result) = self
            .changed
            .wait_timeout_while(guard, timeout, |disarmed| !*disarmed)
            .unwrap_or_else(PoisonError::into_inner);
        *guard
    }
}

fn spawn_startup_watchdog(
    timeout_ms: u64,
    gate: Arc<StartupGate>,
    ended: Arc<AtomicBool>,
    lifecycle: AbortController,
    stderr: StderrSink,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        if gate.wait_disarmed(Duration::from_millis(timeout_ms)) {
            return;
        }
        ended.store(true, Ordering::SeqCst);
        lifecycle.abort();
        let message =
            format!("[lsp-daemon] no MCP request received within {timeout_ms}ms; exiting\n");
        if let Err(error) = stderr(&message) {
            eprintln!("[lsp-daemon] startup diagnostic failed: {error}");
        }
    })
}

/// `mcp` subcommand entry: proxy the real stdin/stdout.
pub fn run_process_proxy() -> Result<ServerOutcome, ProxyError> {
    let mut stdout = std::io::stdout();
    run_mcp_stdio_proxy(std::io::stdin(), &mut stdout, ProxyOptions::default())
}

fn standalone_context(options: &ProxyOptions) -> Result<LspRequestContext, String> {
    let env: HashMap<String, String> = options
        .env
        .clone()
        .unwrap_or_else(|| std::env::vars().collect());
    let cwd = options.cwd.clone().or_else(|| {
        infer_open_code_project_cwd(env.get("LSP_TOOLS_MCP_PROJECT_CONFIG").map(String::as_str))
    });
    let env = if cwd.is_some() {
        canonicalize_context_env(env)
    } else {
        env
    };
    create_standalone_mcp_request_context(StandaloneMcpRequestContextInput {
        cwd,
        env: Some(env),
        home_dir: options.home_dir.clone(),
    })
    .map_err(|error| error.message)
}

async fn handle_proxy_request(
    parsed: &Value,
    call_options: CallToolOptions,
) -> Option<JsonRpcResponse> {
    let Some((id, name, args)) = as_tool_call(parsed) else {
        return handle_lsp_mcp_request(parsed, HandleLspMcpRequestOptions::default()).await;
    };
    let result = match call_tool_via_daemon(&name, args, call_options).await {
        Ok(result) => result,
        Err(error) => crate::daemon_failure_result::ToolExecutionResult {
            content: vec![json!({"type": "text", "text": error.to_string()})],
            is_error: true,
            details: None,
        },
    };
    let mut body = Map::new();
    body.insert("content".to_string(), Value::Array(result.content));
    body.insert("isError".to_string(), Value::Bool(result.is_error));
    if let Some(details) = result.details {
        body.insert("details".to_string(), details);
    }
    Some(success_response(id, body))
}

fn as_tool_call(parsed: &Value) -> Option<(mcp_stdio_core::JsonRpcId, String, Map<String, Value>)> {
    let record = parsed.as_object()?;
    if record.get("method").and_then(Value::as_str) != Some("tools/call") {
        return None;
    }
    let params = record.get("params")?.as_object()?;
    let name = params.get("name")?.as_str()?.to_string();
    let args = params
        .get("arguments")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    Some((
        json_rpc_id(record.get("id").unwrap_or(&Value::Null)),
        name,
        args,
    ))
}

const PATH_DELIMITER: char = if cfg!(windows) { ';' } else { ':' };

/// TS `inferOpenCodeProjectCwd`.
pub fn infer_open_code_project_cwd(project_config_env: Option<&str>) -> Option<String> {
    project_config_env?
        .split(PATH_DELIMITER)
        .find_map(project_root_from_open_code_config_path)
}

fn project_root_from_open_code_config_path(path: &str) -> Option<String> {
    let path = Path::new(path);
    let file_name = path.file_name()?.to_str()?;
    if file_name != "lsp.json" && file_name != "lsp-client.json" {
        return None;
    }
    let config_dir = path.parent()?;
    let dir_name = config_dir.file_name()?.to_str()?;
    if dir_name != ".opencode" && dir_name != ".omo" {
        return None;
    }
    Some(config_dir.parent()?.to_string_lossy().into_owned())
}

fn canonicalize_context_env(mut env: HashMap<String, String>) -> HashMap<String, String> {
    if let Some(value) = env.get("LSP_TOOLS_MCP_PROJECT_CONFIG").cloned() {
        let list: Vec<String> = value
            .split(PATH_DELIMITER)
            .map(|entry| canonicalize_path(entry).unwrap_or_else(|| entry.to_string()))
            .collect();
        env.insert(
            "LSP_TOOLS_MCP_PROJECT_CONFIG".to_string(),
            list.join(&PATH_DELIMITER.to_string()),
        );
    }
    for key in [
        "LSP_TOOLS_MCP_USER_CONFIG",
        "LSP_TOOLS_MCP_INSTALL_DECISIONS",
    ] {
        if let Some(canonical) = env.get(key).and_then(|value| canonicalize_path(value)) {
            env.insert(key.to_string(), canonical);
        }
    }
    env
}

fn canonicalize_path(value: &str) -> Option<String> {
    let path = PathBuf::from(value);
    if !path.is_absolute() || !path.exists() {
        return Some(value.to_string());
    }
    std::fs::canonicalize(path)
        .ok()
        .map(|path| path.to_string_lossy().into_owned())
}
