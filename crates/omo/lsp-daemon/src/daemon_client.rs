//! Port of `daemon-client.ts`: the public client that forwards tool calls to the daemon.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use lsp_core::abort::AbortSignal;
use lsp_core::request_context::{LspRequestContext, parse_lsp_request_context};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::Instant;

pub use crate::daemon_failure_result::ToolExecutionResult;
use crate::daemon_failure_result::daemon_failure_result;
use crate::daemon_request_error::{DaemonCallError, DaemonRequestError, DaemonRequestErrorKind};
use crate::ensure_daemon::{
    BoxFuture, EnsureDaemonDeps, EnsureDaemonOptions, ensure_daemon_running,
};
use crate::ipc_protocol::{auth_envelope, is_auth_error_response, read_auth_token};
use crate::paths::{DaemonPaths, daemon_paths};
use crate::request_routing::CONTEXT_KEY;
use crate::runtime_contract::Env;
use crate::socket_jsonrpc::{LineBuffer, encode_json_line};
use crate::transport;

const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 30_000;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
static NEXT_PROXY_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

pub type DaemonToolContext = LspRequestContext;

/// Startup hook (TS `CallToolOptions.ensure`); errors are opaque causes.
pub type EnsureFn =
    Arc<dyn Fn(DaemonPaths, Option<AbortSignal>) -> BoxFuture<Result<(), String>> + Send + Sync>;

/// TS `CallToolOptions`. `context` is mandatory; `None` rejects before dispatch.
#[derive(Clone, Default)]
pub struct CallToolOptions {
    pub context: Option<DaemonToolContext>,
    pub paths: Option<DaemonPaths>,
    pub request_timeout_ms: Option<u64>,
    pub signal: Option<AbortSignal>,
    pub ensure: Option<EnsureFn>,
}

fn default_ensure() -> EnsureFn {
    Arc::new(|paths, signal| {
        Box::pin(async move {
            let options = EnsureDaemonOptions {
                signal,
                ..EnsureDaemonOptions::default()
            };
            ensure_daemon_running(&paths, &EnsureDaemonDeps::default(), options)
                .await
                .map_err(|error| error.to_string())
        })
    })
}

/// TS `callToolViaDaemon`. `Err` only for caller errors (missing context, bad paths);
/// every transport failure becomes a structured `isError` result.
pub async fn call_tool_via_daemon(
    name: &str,
    args: Map<String, Value>,
    options: CallToolOptions,
) -> Result<ToolExecutionResult, DaemonCallError> {
    let context = options
        .context
        .ok_or_else(|| DaemonRequestError::new("daemon tool context is required", false))?;
    let context = require_context(&context)?;
    let paths = match options.paths {
        Some(paths) => paths,
        None => daemon_paths().map_err(|error| DaemonCallError::Other(error.to_string()))?,
    };
    let ensure = options.ensure.unwrap_or_else(default_ensure);
    let timeout_ms = options
        .request_timeout_ms
        .unwrap_or(DEFAULT_REQUEST_TIMEOUT_MS);
    let mut request_args = args;
    request_args.insert(
        CONTEXT_KEY.to_string(),
        serde_json::to_value(&context)
            .map_err(|error| DaemonCallError::Other(error.to_string()))?,
    );

    let mut last_error = DaemonCallError::Other("daemon call did not run".to_string());
    let mut auth_refresh_used = false;
    for _attempt in 0..3 {
        let outcome = attempt(
            &paths,
            &ensure,
            options.signal.as_ref(),
            name,
            &request_args,
            timeout_ms,
        )
        .await;
        let error = match outcome {
            Ok(result) => return Ok(result),
            Err(error) => error,
        };
        let stop = match &error {
            DaemonCallError::Request(request) => match request.kind {
                DaemonRequestErrorKind::AuthenticationRejected if !auth_refresh_used => {
                    auth_refresh_used = true;
                    false
                }
                DaemonRequestErrorKind::Cancelled => true,
                DaemonRequestErrorKind::Request
                | DaemonRequestErrorKind::AuthenticationRejected
                | DaemonRequestErrorKind::TimedOut { .. } => {
                    request.request_written || !is_retryable_tool(name)
                }
            },
            DaemonCallError::Other(_) => false,
        };
        last_error = error;
        if stop {
            break;
        }
    }
    Ok(daemon_failure_result(&paths, &last_error))
}

async fn attempt(
    paths: &DaemonPaths,
    ensure: &EnsureFn,
    signal: Option<&AbortSignal>,
    name: &str,
    args: &Map<String, Value>,
    timeout_ms: u64,
) -> Result<ToolExecutionResult, DaemonCallError> {
    ensure_daemon_available(paths, ensure, signal).await?;
    let token = read_auth_token(paths)
        .ok_or_else(|| DaemonRequestError::new("daemon auth token missing", false))?;
    Ok(send_tool_call(paths, &token, name, args, timeout_ms, signal).await?)
}

/// TS `callDiagnosticsViaDaemon`.
pub async fn call_diagnostics_via_daemon(
    file_path: &str,
    options: CallToolOptions,
) -> Result<ToolExecutionResult, DaemonCallError> {
    let mut args = Map::new();
    args.insert("filePath".to_string(), json!(file_path));
    args.insert("severity".to_string(), json!("error"));
    call_tool_via_daemon("diagnostics", args, options).await
}

/// TS `currentRequestContext`: Codex defaults rooted at the process cwd and `HOME`.
pub fn current_request_context(env: &Env) -> Result<DaemonToolContext, DaemonCallError> {
    let cwd = std::env::current_dir()
        .map_err(|error| DaemonCallError::Other(error.to_string()))?
        .to_string_lossy()
        .into_owned();
    let home = env
        .get("HOME")
        .cloned()
        .or_else(|| crate::platform::home_dir().map(|home| home.to_string_lossy().into_owned()))
        .unwrap_or_default();
    let join = |base: &str, parts: &[&str]| {
        let mut path = Path::new(base).to_path_buf();
        path.extend(parts);
        path.to_string_lossy().into_owned()
    };
    let value = json!({
        "cwd": cwd,
        "projectConfigPaths": [join(&cwd, &[".codex", "lsp-client.json"])],
        "userConfigPath": join(&home, &[".codex", "lsp-client.json"]),
        "installDecisionsPath": join(&home, &[".codex", "lsp-install-decisions.json"]),
        "capabilities": {"installDecisionTool": true},
    });
    parse_lsp_request_context(&value).map_err(|error| DaemonCallError::Other(error.message))
}

fn require_context(context: &DaemonToolContext) -> Result<DaemonToolContext, DaemonCallError> {
    let value =
        serde_json::to_value(context).map_err(|error| DaemonCallError::Other(error.to_string()))?;
    parse_lsp_request_context(&value).map_err(|error| DaemonCallError::Other(error.message))
}

async fn ensure_daemon_available(
    paths: &DaemonPaths,
    ensure: &EnsureFn,
    signal: Option<&AbortSignal>,
) -> Result<(), DaemonCallError> {
    let Some(signal) = signal else {
        return ensure(paths.clone(), None)
            .await
            .map_err(DaemonCallError::Other);
    };
    if signal.aborted() {
        return Err(DaemonRequestError::cancelled(false).into());
    }
    let result = tokio::select! {
        biased;
        () = signal.cancelled() => return Err(DaemonRequestError::cancelled(false).into()),
        result = ensure(paths.clone(), Some(signal.clone())) => result,
    };
    result.map_err(|error| {
        if signal.aborted() {
            DaemonRequestError::cancelled(false).into()
        } else {
            DaemonCallError::Other(error)
        }
    })
}

enum Interrupt {
    Timeout,
    Abort,
}

async fn interrupted(deadline: Instant, signal: Option<&AbortSignal>) -> Interrupt {
    match signal {
        Some(signal) => tokio::select! {
            biased;
            () = signal.cancelled() => Interrupt::Abort,
            () = tokio::time::sleep_until(deadline) => Interrupt::Timeout,
        },
        None => {
            tokio::time::sleep_until(deadline).await;
            Interrupt::Timeout
        }
    }
}

fn interrupt_error(
    interrupt: &Interrupt,
    request_written: bool,
    timeout_ms: u64,
) -> DaemonRequestError {
    match interrupt {
        Interrupt::Timeout => DaemonRequestError::timed_out(request_written, timeout_ms),
        Interrupt::Abort => DaemonRequestError::cancelled(request_written),
    }
}

async fn send_tool_call(
    paths: &DaemonPaths,
    token: &str,
    name: &str,
    args: &Map<String, Value>,
    timeout_ms: u64,
    signal: Option<&AbortSignal>,
) -> Result<ToolExecutionResult, DaemonRequestError> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let request_id = allocate_proxy_request_id();
    if signal.is_some_and(AbortSignal::aborted) {
        return Err(DaemonRequestError::cancelled(false));
    }
    let payload = encode_json_line(&json!({
        "jsonrpc": "2.0",
        "id": request_id,
        "method": "tools/call",
        "params": {"_omo": auth_envelope(token), "name": name, "arguments": args},
    }));
    let connect_and_write = async {
        let mut stream = transport::connect(&paths.socket).await?;
        stream.write_all(payload.as_bytes()).await?;
        Ok::<_, std::io::Error>(stream)
    };
    let mut stream = tokio::select! {
        biased;
        interrupt = interrupted(deadline, signal) => return Err(interrupt_error(&interrupt, false, timeout_ms)),
        result = connect_and_write => result.map_err(|error| DaemonRequestError::new(error.to_string(), false))?,
    };
    let outcome = tokio::select! {
        biased;
        interrupt = interrupted(deadline, signal) => Err(interrupt),
        result = read_response(&mut stream, request_id) => Ok(result),
    };
    match outcome {
        Ok(result) => result,
        Err(interrupt) => {
            let cancel = encode_json_line(&json!({
                "jsonrpc": "2.0",
                "method": "$/cancelRequest",
                "params": {"_omo": auth_envelope(token), "id": request_id},
            }));
            let _ignored = stream.write_all(cancel.as_bytes()).await;
            let _ignored = stream.flush().await;
            Err(interrupt_error(&interrupt, true, timeout_ms))
        }
    }
}

async fn read_response(
    stream: &mut transport::Stream,
    request_id: u64,
) -> Result<ToolExecutionResult, DaemonRequestError> {
    let mut decoder = LineBuffer::new();
    let mut chunk = [0_u8; 8192];
    loop {
        let read = stream
            .read(&mut chunk)
            .await
            .map_err(|error| DaemonRequestError::new(error.to_string(), true))?;
        if read == 0 {
            return Err(DaemonRequestError::new("daemon connection closed", true));
        }
        // Only the first decodable line matters; the TS decoder settles on it.
        let Some(line) = decoder
            .push(&chunk[..read])
            .into_iter()
            .find_map(Result::ok)
        else {
            continue;
        };
        if is_auth_error_response(&line) {
            return Err(DaemonRequestError::authentication_rejected());
        }
        return to_tool_result(&line, request_id)
            .ok_or_else(|| DaemonRequestError::new("invalid daemon response", true));
    }
}

fn to_tool_result(message: &Value, request_id: u64) -> Option<ToolExecutionResult> {
    let record = message.as_object()?;
    if record.get("id")?.as_u64()? != request_id {
        return None;
    }
    let result = record.get("result")?.as_object()?;
    Some(ToolExecutionResult {
        content: result.get("content")?.as_array()?.clone(),
        is_error: result.get("isError") == Some(&Value::Bool(true)),
        details: result.get("details").cloned(),
    })
}

fn allocate_proxy_request_id() -> u64 {
    let mut current = NEXT_PROXY_REQUEST_ID.load(Ordering::Relaxed);
    loop {
        let next = if current >= MAX_SAFE_INTEGER {
            1
        } else {
            current + 1
        };
        match NEXT_PROXY_REQUEST_ID.compare_exchange_weak(
            current,
            next,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return current,
            Err(observed) => current = observed,
        }
    }
}

/// Rename mutates the workspace, so it is never retried.
pub fn is_retryable_tool(name: &str) -> bool {
    name != "rename" && name != "lsp_rename"
}
