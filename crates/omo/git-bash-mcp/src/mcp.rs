//! Port of `packages/git-bash-mcp/src/mcp.ts`.
//!
//! Serves the `git_bash` MCP tool family over JSON-RPC stdio. The `run` tool is registered only on
//! native Windows with a resolvable Git Bash; off Windows the stdio server returns without serving.

use std::collections::HashMap;
use std::convert::Infallible;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use mcp_stdio_core::JsonRpcId;
use mcp_stdio_core::JsonRpcResponse;
use mcp_stdio_core::JsonRpcStdioServerConfig;
use mcp_stdio_core::McpLifecycleLog;
use mcp_stdio_core::McpToolDescriptor;
use mcp_stdio_core::ParentWatchdogConfig;
use mcp_stdio_core::ServerError;
use mcp_stdio_core::ServerOutcome;
use mcp_stdio_core::error_response;
use mcp_stdio_core::json_rpc_id;
use mcp_stdio_core::run_json_rpc_stdio_server;
use mcp_stdio_core::success_response;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use utils::GitBashResolution;
use utils::GitBashResolverInput;
use utils::GitBashSource;
use utils::resolve_git_bash;
use utils::resolve_git_bash_for_current_process;

use crate::runner::GitBashRunInput;
use crate::runner::GitBashRunResult;
use crate::runner::RunGitBashCommand;
use crate::runner::run_git_bash_command;

pub const GIT_BASH_SERVER_NAME: &str = "git_bash";
pub const GIT_BASH_SERVER_VERSION: &str = "0.1.0";
pub const DEFAULT_PROTOCOL_VERSION: &str = "2024-11-05";
pub const DEFAULT_TIMEOUT_MS: u64 = 120_000;
pub const MAX_TIMEOUT_MS: u64 = 30 * 60_000;

/// The inherited exec_command timeout chain, in priority order; upstream strings are preserved.
pub const EXEC_COMMAND_TIMEOUT_ENV_KEYS: [&str; 4] = [
    "OMO_CODEX_GIT_BASH_TIMEOUT_MS",
    "OMO_CODEX_EXEC_COMMAND_TIMEOUT_MS",
    "CODEX_EXEC_COMMAND_TIMEOUT_MS",
    "EXEC_COMMAND_TIMEOUT_MS",
];

/// Server options, mirroring the reference `GitBashMcpOptions`.
///
/// The seams (`exists`, `where_bash`, `run_git_bash`, `platform`, `env`) default to the real host
/// when unset, exactly as the reference falls back to `process.platform`, `process.env` and the
/// real filesystem.
#[derive(Clone, Default)]
pub struct GitBashMcpOptions {
    pub lifecycle_log: Option<Arc<dyn McpLifecycleLog + Send + Sync>>,
    pub platform: Option<String>,
    pub env: Option<HashMap<String, String>>,
    pub exists: Option<Arc<dyn Fn(&str) -> bool + Send + Sync>>,
    pub where_bash: Option<Arc<dyn Fn() -> Vec<String> + Send + Sync>>,
    pub run_git_bash: Option<Arc<RunGitBashCommand>>,
    pub default_timeout_ms: Option<f64>,
    pub parent_watchdog: Option<ParentWatchdogConfig>,
}

/// Why [`run_mcp_stdio_server`] returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitBashServerOutcome {
    /// `canRunGitBash` was false: the reference `runMcpStdioServer` returns without serving.
    Disabled,
    /// The JSON-RPC server ran and settled for this reason.
    Served(ServerOutcome),
}

/// Handles one MCP request; `None` means no response (a notification).
pub fn handle_git_bash_mcp_request(
    input: &Value,
    options: &GitBashMcpOptions,
) -> Option<JsonRpcResponse> {
    let Value::Object(request) = input else {
        return Some(error_response(
            JsonRpcId::Null,
            -32600,
            "Invalid Request",
            None,
        ));
    };
    let id = json_rpc_id(request.get("id").unwrap_or(&Value::Null));
    match request.get("method").and_then(Value::as_str) {
        Some("initialize") => Some(success_response(
            id,
            record(json!({
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": GIT_BASH_SERVER_NAME, "version": GIT_BASH_SERVER_VERSION },
                "protocolVersion": protocol_version_from_input(request),
            })),
        )),
        Some("tools/list") => Some(success_response(
            id,
            record(json!({ "tools": tools_for_options(options) })),
        )),
        Some("tools/call") => {
            let params = request.get("params").filter(|value| value.is_object());
            let name = params
                .and_then(|params| params.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let args = params.and_then(|params| params.get("arguments"));
            Some(call_tool(id, name, args, options))
        }
        Some("notifications/initialized") => None,
        _ => Some(error_response(id, -32601, "Method not found", None)),
    }
}

/// Serves MCP over newline-delimited JSON-RPC until `input` closes or the parent process exits.
///
/// Off Windows — and on Windows without a resolvable Git Bash — the reference returns without
/// serving, so this returns [`GitBashServerOutcome::Disabled`] and writes nothing.
///
/// # Errors
/// Returns [`ServerError::Io`] when the transport fails.
pub fn run_mcp_stdio_server<R, W>(
    input: R,
    output: &mut W,
    options: GitBashMcpOptions,
) -> Result<GitBashServerOutcome, ServerError<Infallible>>
where
    R: Read + Send + 'static,
    W: Write,
{
    if !can_run_git_bash(&options) {
        return Ok(GitBashServerOutcome::Disabled);
    }
    let parent_watchdog = options.parent_watchdog.clone().unwrap_or_default();
    let lifecycle_log = options.lifecycle_log.clone();
    let handler = move |request: &Value| -> Result<Option<JsonRpcResponse>, Infallible> {
        Ok(handle_git_bash_mcp_request(request, &options))
    };
    let mut config = JsonRpcStdioServerConfig::new(handler);
    config.idle_timeout_ms = Some(0);
    config.parent_watchdog = Some(parent_watchdog);
    config.log = lifecycle_log;
    config.parse_error_response = Some(Arc::new(|_message: &str| {
        Some(error_response(
            JsonRpcId::Null,
            -32601,
            "Method not found",
            None,
        ))
    }));
    run_json_rpc_stdio_server(input, output, config).map(GitBashServerOutcome::Served)
}

fn call_tool(
    id: JsonRpcId,
    name: &str,
    args: Option<&Value>,
    options: &GitBashMcpOptions,
) -> JsonRpcResponse {
    match name {
        "which_bash" => tool_response(id, which_bash_payload(&resolve(options)), false),
        "diagnose" => tool_response(
            id,
            diagnose_payload(&resolve(options), &platform_from_options(options)),
            false,
        ),
        "run" => {
            let empty = Map::new();
            let args = args.and_then(Value::as_object).unwrap_or(&empty);
            run_tool_response(id, args, options)
        }
        _ => tool_response(id, format!("Unknown git_bash tool: {name}"), true),
    }
}

fn run_tool_response(
    id: JsonRpcId,
    args: &Map<String, Value>,
    options: &GitBashMcpOptions,
) -> JsonRpcResponse {
    if platform_from_options(options) != "win32" {
        return tool_response(
            id,
            "git_bash run is only available on native Windows.".to_owned(),
            true,
        );
    }
    let command = args
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if command.is_empty() {
        return tool_response(
            id,
            "run.command must be a non-empty string.".to_owned(),
            true,
        );
    }
    let cwd = match parse_workdir(args) {
        ParsedWorkdir::Absent => None,
        ParsedWorkdir::Value(text) => Some(text),
        ParsedWorkdir::Invalid => {
            return tool_response(
                id,
                "run.workdir must be a non-empty string when provided.".to_owned(),
                true,
            );
        }
    };
    let timeout_value = args
        .get("timeout")
        .filter(|value| !value.is_null())
        .or_else(|| args.get("timeout_ms").filter(|value| !value.is_null()));
    let Some(timeout_ms) = parse_timeout_ms(timeout_value, options) else {
        return tool_response(
            id,
            format!("run.timeout must be an integer between 1 and {MAX_TIMEOUT_MS}."),
            true,
        );
    };
    let resolution = resolve(options);
    let Some(bash_path) = resolution_path(&resolution) else {
        return tool_response(id, which_bash_payload(&resolution), true);
    };
    let input = GitBashRunInput {
        bash_path: bash_path.to_owned(),
        command: command.to_owned(),
        cwd,
        timeout_ms,
        env: Some(resolved_env(options)),
    };
    let result = match &options.run_git_bash {
        Some(run) => run(&input),
        None => run_git_bash_command(&input).map_err(|error| error.to_string()),
    };
    match result {
        Ok(result) => tool_response(id, run_payload(&result), false),
        Err(message) => tool_response(id, message, true),
    }
}

fn tools_for_options(options: &GitBashMcpOptions) -> Vec<McpToolDescriptor> {
    let shared = vec![
        McpToolDescriptor {
            name: "which_bash".to_owned(),
            title: None,
            description: "Resolve the Git Bash bash.exe path used by the git_bash MCP.".to_owned(),
            input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        },
        McpToolDescriptor {
            name: "diagnose".to_owned(),
            title: None,
            description: "Report whether Git Bash command execution is available on this host."
                .to_owned(),
            input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        },
    ];
    if !can_run_git_bash(options) {
        return shared;
    }
    let mut tools = vec![run_tool(options)];
    tools.extend(shared);
    tools
}

fn run_tool(options: &GitBashMcpOptions) -> McpToolDescriptor {
    McpToolDescriptor {
        name: "run".to_owned(),
        title: None,
        description: "Run a shell command through Git Bash on native Windows. Prefer this git_bash run tool for bash/shell commands on Windows before built-in exec_command or Bash; use exec_command only when git_bash is unavailable or for non-shell operations.".to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "The command to execute." },
                "timeout": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_TIMEOUT_MS,
                    "description": format!(
                        "Optional timeout in milliseconds. If omitted, uses the inherited exec_command timeout when configured; otherwise {}ms.",
                        default_timeout_ms(options),
                    ),
                },
                "workdir": {
                    "type": "string",
                    "description": "The working directory to run the command in. Defaults to the current directory. Use this instead of 'cd' commands.",
                },
                "description": {
                    "type": "string",
                    "description": "Clear, concise description of what this command does in 5-10 words.",
                },
            },
            "required": ["command"],
            "additionalProperties": false,
        }),
    }
}

fn can_run_git_bash(options: &GitBashMcpOptions) -> bool {
    if platform_from_options(options) != "win32" {
        return false;
    }
    resolution_path(&resolve(options)).is_some()
}

fn resolve(options: &GitBashMcpOptions) -> GitBashResolution {
    if options.exists.is_none() && options.where_bash.is_none() {
        return resolve_for_current_process(options);
    }
    let platform = platform_from_options(options);
    let env = resolved_env(options);
    let exists: Arc<dyn Fn(&str) -> bool + Send + Sync> = options
        .exists
        .clone()
        .unwrap_or_else(|| Arc::new(always_false));
    let where_bash: Arc<dyn Fn() -> Vec<String> + Send + Sync> = options
        .where_bash
        .clone()
        .unwrap_or_else(|| Arc::new(no_paths));
    resolve_git_bash(&GitBashResolverInput {
        platform: &platform,
        env: &env,
        exists: &*exists,
        where_bash: &*where_bash,
    })
}

fn resolve_for_current_process(options: &GitBashMcpOptions) -> GitBashResolution {
    if options.platform.is_none() && options.env.is_none() {
        return resolve_git_bash_for_current_process();
    }
    let platform = platform_from_options(options);
    let env = resolved_env(options);
    resolve_git_bash(&GitBashResolverInput {
        platform: &platform,
        env: &env,
        exists: &|path| Path::new(path).exists(),
        where_bash: &where_bash,
    })
}

/// The `where bash` probe; the resolver itself is reused from `maho-utils`, which keeps this probe
/// private, so the MCP layer carries its own copy of that one-line process call.
fn where_bash() -> Vec<String> {
    Command::new("where")
        .arg("bash")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn always_false(_path: &str) -> bool {
    false
}

fn no_paths() -> Vec<String> {
    Vec::new()
}

fn platform_from_options(options: &GitBashMcpOptions) -> String {
    options
        .platform
        .clone()
        .unwrap_or_else(|| utils::node_platform().to_owned())
}

fn resolved_env(options: &GitBashMcpOptions) -> HashMap<String, String> {
    options
        .env
        .clone()
        .unwrap_or_else(|| std::env::vars().collect())
}

fn resolution_path(resolution: &GitBashResolution) -> Option<&str> {
    match resolution {
        GitBashResolution::Found {
            path: Some(path), ..
        } => Some(path.as_str()),
        _ => None,
    }
}

fn resolution_value(resolution: &GitBashResolution) -> Value {
    match resolution {
        GitBashResolution::Found {
            path,
            source,
            checked_paths,
        } => json!({
            "found": true,
            "path": path,
            "source": source_name(*source),
            "checkedPaths": checked_paths,
        }),
        GitBashResolution::Missing {
            checked_paths,
            install_hint,
        } => json!({
            "found": false,
            "checkedPaths": checked_paths,
            "installHint": install_hint,
        }),
    }
}

fn source_name(source: GitBashSource) -> &'static str {
    match source {
        GitBashSource::NotRequired => "not-required",
        GitBashSource::Env => "env",
        GitBashSource::ProgramFiles => "program-files",
        GitBashSource::ProgramFilesX86 => "program-files-x86",
        GitBashSource::Path => "path",
    }
}

fn which_bash_payload(resolution: &GitBashResolution) -> String {
    to_pretty_json(&resolution_value(resolution))
}

fn diagnose_payload(resolution: &GitBashResolution, platform: &str) -> String {
    let enabled = platform == "win32" && resolution_path(resolution).is_some();
    let status = match (platform == "win32", enabled) {
        (true, true) => "ready",
        (true, false) => "missing-git-bash",
        (false, _) => "disabled: git_bash command execution is only exposed on native Windows",
    };
    to_pretty_json(&json!({
        "platform": platform,
        "enabled": enabled,
        "status": status,
        "resolution": resolution_value(resolution),
    }))
}

fn run_payload(result: &GitBashRunResult) -> String {
    to_pretty_json(&json!({
        "exitCode": result.exit_code,
        "stdout": result.stdout,
        "stderr": result.stderr,
        "timedOut": result.timed_out,
    }))
}

/// `JSON.stringify(value, null, 2)`: two-space indent and no trailing newline.
fn to_pretty_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).expect("a serde_json Value always serializes")
}

fn tool_response(id: JsonRpcId, text: String, is_error: bool) -> JsonRpcResponse {
    success_response(
        id,
        record(json!({
            "content": [{ "type": "text", "text": text }],
            "isError": is_error,
        })),
    )
}

enum ParsedWorkdir {
    Absent,
    Value(String),
    Invalid,
}

fn parse_workdir(args: &Map<String, Value>) -> ParsedWorkdir {
    let value = args
        .get("workdir")
        .filter(|value| !value.is_null())
        .or_else(|| args.get("cwd").filter(|value| !value.is_null()));
    match value {
        None => ParsedWorkdir::Absent,
        Some(Value::String(text)) if !text.trim().is_empty() => ParsedWorkdir::Value(text.clone()),
        Some(_) => ParsedWorkdir::Invalid,
    }
}

fn parse_timeout_ms(value: Option<&Value>, options: &GitBashMcpOptions) -> Option<u64> {
    match value {
        None => Some(default_timeout_ms(options)),
        Some(value) => normalize_timeout_ms(value),
    }
}

fn default_timeout_ms(options: &GitBashMcpOptions) -> u64 {
    if let Some(configured) = options.default_timeout_ms
        && let Some(timeout_ms) = normalize_timeout_ms(&Value::from(configured))
    {
        return timeout_ms;
    }
    let env = resolved_env(options);
    for key in EXEC_COMMAND_TIMEOUT_ENV_KEYS {
        if let Some(value) = env.get(key)
            && let Some(timeout_ms) = normalize_timeout_ms(&Value::from(value.as_str()))
        {
            return timeout_ms;
        }
    }
    DEFAULT_TIMEOUT_MS
}

fn normalize_timeout_ms(value: &Value) -> Option<u64> {
    let parsed = js_number(value)?;
    if !is_js_integer(parsed) || parsed < 1.0 || parsed > MAX_TIMEOUT_MS as f64 {
        return None;
    }
    // Range-checked integral float: the cast cannot truncate or lose the sign.
    Some(parsed as u64)
}

/// JS `Number(value)` for the shapes the reference accepts; `None` where JS yields `NaN`.
fn js_number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) if !text.trim().is_empty() => text.trim().parse::<f64>().ok(),
        _ => None,
    }
}

/// JS `Number.isInteger`.
fn is_js_integer(value: f64) -> bool {
    value.is_finite() && value.fract() == 0.0
}

fn protocol_version_from_input(request: &Map<String, Value>) -> String {
    request
        .get("params")
        .filter(|value| value.is_object())
        .and_then(|params| params.get("protocolVersion"))
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_PROTOCOL_VERSION)
        .to_owned()
}

fn record(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}
