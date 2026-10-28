//! Command line and newline-delimited JSON-RPC serving (spec §2, §3, §6).

use crate::host::Host;
use crate::server::Server;
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

/// Server name reported by `initialize`.
pub const SERVER_INFO_NAME: &str = "git_bash";
/// Protocol version answered when the client sends none.
pub const DEFAULT_PROTOCOL_VERSION: &str = "2024-11-05";
/// One-line usage message for a bad command line.
pub const USAGE: &str = "usage: mhc-git-bash [mcp]  (serves the Git Bash MCP tools on stdin/stdout)";

pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_REQUEST: i64 = -32600;

/// What the command line asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CliCommand {
    Serve,
    Usage,
}

/// Arguments after the program name: none or `mcp` serve; anything else is a usage error.
pub fn parse_args<S: AsRef<str>>(args: &[S]) -> CliCommand {
    match args {
        [] => CliCommand::Serve,
        [only] if only.as_ref() == "mcp" => CliCommand::Serve,
        _ => CliCommand::Usage,
    }
}

/// Run the program: returns the process exit code. stdin is not read and stdout is not written
/// unless the server is available (spec §3). `parent_exit` resolves when the parent process is
/// gone; it ends serving like end of file.
pub async fn run_cli<H, R, W, E, P>(
    args: &[String],
    server: &Server<H>,
    stdin: R,
    stdout: W,
    stderr: &mut E,
    parent_exit: P,
) -> i32
where
    H: Host,
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
    E: AsyncWrite + Unpin,
    P: std::future::Future<Output = ()>,
{
    if parse_args(args) == CliCommand::Usage {
        let _ = stderr.write_all(format!("{USAGE}\n").as_bytes()).await;
        let _ = stderr.flush().await;
        return 2;
    }
    if !server.is_available() {
        return 0;
    }
    match serve(server, stdin, stdout, parent_exit).await {
        Ok(()) => 0,
        Err(error) => {
            let _ = stderr
                .write_all(format!("mhc-git-bash: transport error: {error}\n").as_bytes())
                .await;
            let _ = stderr.flush().await;
            1
        }
    }
}

/// Serve JSON-RPC lines from `input` until end of file or `parent_exit` resolves. There is no
/// idle timeout.
pub async fn serve<H, R, W, P>(
    server: &Server<H>,
    mut input: R,
    mut output: W,
    parent_exit: P,
) -> std::io::Result<()>
where
    H: Host,
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
    P: std::future::Future<Output = ()>,
{
    tokio::pin!(parent_exit);
    let mut line = String::new();
    loop {
        line.clear();
        let read = tokio::select! {
            read = input.read_line(&mut line) => read?,
            () = &mut parent_exit => return Ok(()),
        };
        if read == 0 {
            return Ok(());
        }
        if let Some(reply) = handle_line(server, &line).await {
            let mut bytes = serde_json::to_vec(&reply).map_err(std::io::Error::other)?;
            bytes.push(b'\n');
            output.write_all(&bytes).await?;
            output.flush().await?;
        }
    }
}

fn error_reply(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn result_reply(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// Handle one input line; `None` when nothing is written back.
pub async fn handle_line<H: Host>(server: &Server<H>, line: &str) -> Option<Value> {
    let text = line.trim();
    if text.is_empty() {
        return None;
    }
    let message: Value = match serde_json::from_str(text) {
        Ok(message) => message,
        Err(_) => {
            return Some(error_reply(
                Value::Null,
                METHOD_NOT_FOUND,
                "Parse error: the line is not valid JSON",
            ));
        }
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
        // A response from the client or a malformed request: answer only when it has an id.
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
                .and_then(|params| params.get("protocolVersion"))
                .and_then(Value::as_str)
                .unwrap_or(DEFAULT_PROTOCOL_VERSION);
            result_reply(
                id,
                json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": {
                        "name": SERVER_INFO_NAME,
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                }),
            )
        }
        "tools/list" => result_reply(id, json!({ "tools": server.tool_definitions() })),
        "tools/call" => {
            let name = params
                .and_then(|params| params.get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let arguments = params.and_then(|params| params.get("arguments"));
            let reply = server.call_tool(name, arguments).await;
            result_reply(id, reply.to_json())
        }
        other => error_reply(id, METHOD_NOT_FOUND, &format!("Method not found: {other}")),
    };
    Some(reply)
}
