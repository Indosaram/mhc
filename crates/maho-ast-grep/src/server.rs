//! Newline-delimited JSON-RPC server on stdio (spec §7).

use crate::exec::{AbortSignal, abort_channel};
use crate::schema::tool_definitions;
use crate::tools::{Context, call};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

pub const SERVER_INFO_NAME: &str = "ast_grep";
pub const DEFAULT_PROTOCOL_VERSION: &str = "2024-11-05";
/// Environment variable carrying the registering session's working directory.
pub const PROJECT_CWD_ENV: &str = "MHC_AST_GREP_PROJECT_CWD";
pub const LEGACY_PROJECT_CWD_ENV: &str = "OMO_AST_GREP_PROJECT_CWD";
pub const IDLE_TIMEOUT_ENV: &str = "MHC_AST_GREP_IDLE_TIMEOUT_MS";
pub const LEGACY_IDLE_TIMEOUT_ENV: &str = "OMO_AST_GREP_IDLE_TIMEOUT_MS";
pub const DEFAULT_IDLE_TIMEOUT_MS: u64 = 600_000;
/// One-line usage message for a bad command line.
pub const USAGE: &str = "usage: mhc-ast-grep mcp  (serves the structural search tools on stdin/stdout)";

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;

/// Server settings.
#[derive(Clone, Debug)]
pub struct ServerOptions {
    pub context: Context,
    /// `None` disables the idle timeout.
    pub idle_timeout: Option<Duration>,
}

impl ServerOptions {
    /// Settings from the process environment.
    pub fn from_env() -> Self {
        let project_cwd = std::env::var(PROJECT_CWD_ENV)
            .ok()
            .or_else(|| std::env::var(LEGACY_PROJECT_CWD_ENV).ok())
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from);
        Self {
            context: Context {
                toolchain: crate::resolve::Toolchain::from_env(),
                project_cwd,
            },
            idle_timeout: idle_timeout(|name| std::env::var(name).ok()),
        }
    }
}

/// The idle timeout from the environment: unset, empty, non-numeric or non-finite keeps the
/// default (10 minutes); zero or negative disables it.
pub fn idle_timeout(env: impl Fn(&str) -> Option<String>) -> Option<Duration> {
    let raw = env(IDLE_TIMEOUT_ENV)
        .filter(|value| !value.trim().is_empty())
        .or_else(|| env(LEGACY_IDLE_TIMEOUT_ENV).filter(|value| !value.trim().is_empty()));
    let ms = match raw.as_deref().map(str::trim).and_then(|text| text.parse::<f64>().ok()) {
        Some(number) if number.is_finite() => number,
        _ => DEFAULT_IDLE_TIMEOUT_MS as f64,
    };
    if ms <= 0.0 {
        None
    } else {
        Some(Duration::from_millis(ms as u64))
    }
}

/// One `tools/call` reply.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolReply {
    pub text: String,
    pub is_error: bool,
}

impl ToolReply {
    pub fn to_json(&self) -> Value {
        json!({ "content": [{ "type": "text", "text": self.text }], "isError": self.is_error })
    }
}

/// The tool handler.
pub struct Server {
    options: ServerOptions,
}

impl Server {
    pub fn new(options: ServerOptions) -> Self {
        Self { options }
    }

    pub fn options(&self) -> &ServerOptions {
        &self.options
    }

    /// Handle one tool call; the reply text is the result object as JSON.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: Option<&Value>,
        abort: &mut AbortSignal,
    ) -> ToolReply {
        let outcome = call(&self.options.context, name, arguments, abort).await;
        ToolReply {
            text: serde_json::to_string(&outcome.value).unwrap_or_else(|_| outcome.value.to_string()),
            is_error: outcome.is_error,
        }
    }
}

fn error_reply(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// Handle one input line with a fresh cancellation signal; `None` when nothing is written back.
pub async fn handle_message(server: &Server, line: &str, abort: &mut AbortSignal) -> Option<Value> {
    let text = line.trim();
    if text.is_empty() {
        return None;
    }
    let Ok(message) = serde_json::from_str::<Value>(text) else {
        return Some(error_reply(Value::Null, PARSE_ERROR, "Parse error: the line is not valid JSON"));
    };
    let Some(object) = message.as_object() else {
        return Some(error_reply(
            Value::Null,
            INVALID_REQUEST,
            "Invalid request: expected a JSON object",
        ));
    };
    let id = object.get("id").cloned();
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        return id
            .filter(|_| !object.contains_key("result") && !object.contains_key("error"))
            .map(|id| error_reply(id, INVALID_REQUEST, "Invalid request: missing method"));
    };
    // Notifications (no id) never get a reply.
    let id = id?;
    let params = object.get("params");
    let reply = match method {
        "initialize" => {
            let version = params
                .and_then(|p| p.get("protocolVersion"))
                .and_then(Value::as_str)
                .unwrap_or(DEFAULT_PROTOCOL_VERSION);
            json!({ "jsonrpc": "2.0", "id": id, "result": {
                "protocolVersion": version,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": SERVER_INFO_NAME, "version": env!("CARGO_PKG_VERSION") },
            }})
        }
        "ping" => json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
        "tools/list" => json!({ "jsonrpc": "2.0", "id": id, "result": { "tools": tool_definitions() } }),
        "tools/call" => match params.and_then(|p| p.get("name")).and_then(Value::as_str) {
            Some(name) if !name.is_empty() => {
                let arguments = params.and_then(|p| p.get("arguments"));
                let reply = server.call_tool(name, arguments, abort).await;
                json!({ "jsonrpc": "2.0", "id": id, "result": reply.to_json() })
            }
            _ => error_reply(id, INVALID_PARAMS, "Invalid params: `name` is required"),
        },
        other => error_reply(id, METHOD_NOT_FOUND, &format!("Method not found: {other}")),
    };
    Some(reply)
}

async fn write_reply<W: AsyncWrite + Unpin>(output: &mut W, reply: &Value) -> std::io::Result<()> {
    let mut bytes = serde_json::to_vec(reply).map_err(std::io::Error::other)?;
    bytes.push(b'\n');
    output.write_all(&bytes).await?;
    output.flush().await
}

/// Serve until end of input, the idle timeout, or `parent_exit`. A call in progress when the
/// parent exits is aborted and its reply written before returning. Every call has its own signal.
pub async fn serve<R, W, P>(
    server: &Server,
    mut input: R,
    mut output: W,
    parent_exit: P,
) -> std::io::Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
    P: std::future::Future<Output = ()>,
{
    tokio::pin!(parent_exit);
    let mut line = String::new();
    loop {
        line.clear();
        let idle = server.options.idle_timeout;
        let read = tokio::select! {
            read = input.read_line(&mut line) => read?,
            () = async {
                match idle {
                    Some(duration) => tokio::time::sleep(duration).await,
                    None => std::future::pending::<()>().await,
                }
            } => return Ok(()),
            () = &mut parent_exit => return Ok(()),
        };
        if read == 0 {
            return Ok(());
        }
        let (handle, mut signal) = abort_channel();
        let mut parent_gone = false;
        let reply = {
            let work = handle_message(server, &line, &mut signal);
            tokio::pin!(work);
            loop {
                tokio::select! {
                    reply = &mut work => break reply,
                    () = &mut parent_exit, if !parent_gone => {
                        parent_gone = true;
                        handle.abort();
                    }
                }
            }
        };
        if let Some(reply) = reply {
            write_reply(&mut output, &reply).await?;
        }
        if parent_gone {
            return Ok(());
        }
    }
}

/// Run the program: returns the process exit code. Any first argument other than `mcp` prints
/// usage to `stderr` and gives 2.
pub async fn run_cli<R, W, E, P>(
    args: &[String],
    server: &Server,
    stdin: R,
    stdout: W,
    stderr: &mut E,
    parent_exit: P,
) -> i32
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
    E: AsyncWrite + Unpin,
    P: std::future::Future<Output = ()>,
{
    if args.first().map(String::as_str) != Some("mcp") {
        let _ = stderr.write_all(format!("{USAGE}\n").as_bytes()).await;
        let _ = stderr.flush().await;
        return 2;
    }
    match serve(server, stdin, stdout, parent_exit).await {
        Ok(()) => 0,
        Err(error) => {
            let _ = stderr
                .write_all(format!("mhc-ast-grep: transport error: {error}\n").as_bytes())
                .await;
            let _ = stderr.flush().await;
            1
        }
    }
}
