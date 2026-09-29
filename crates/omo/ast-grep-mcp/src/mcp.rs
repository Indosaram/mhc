use std::io::Read;
use std::io::Write;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;

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

use crate::abort::AbortSignal;
use crate::tools::LANGUAGES;
use crate::tools::STRICTNESS;
use crate::tools::rewrite::REWRITE_TOOL_DESCRIPTION;
use crate::tools::rewrite::REWRITE_TOOL_NAME;
use crate::tools::rewrite::RewriteHooks;
use crate::tools::rewrite::execute_rewrite;
use crate::tools::scan::SCAN_TOOL_DESCRIPTION;
use crate::tools::scan::SCAN_TOOL_NAME;
use crate::tools::scan::execute_scan;
use crate::tools::search::SEARCH_TOOL_DESCRIPTION;
use crate::tools::search::SEARCH_TOOL_NAME;
use crate::tools::search::execute_search;
use crate::tools::search::parse_search_input;

pub const AST_GREP_MCP_NAME: &str = "ast_grep";
pub const AST_GREP_SERVER_NAME: &str = "ast_grep";
pub const AST_GREP_SERVER_VERSION: &str = "0.1.0";
const DEFAULT_PROTOCOL_VERSION: &str = "2024-11-05";

pub const AST_GREP_ERROR_CODES: [&str; 19] = [
    "INVALID_ARGUMENT",
    "BINARY_NOT_FOUND",
    "BINARY_INVALID",
    "UNSUPPORTED_LANGUAGE",
    "PATTERN_HINT_REJECTED",
    "PATTERN_PARSE_FAILED",
    "RULE_PARSE_FAILED",
    "REWRITE_UNBOUND_METAVARIABLE",
    "REWRITE_METAVARIABLE_KIND_MISMATCH",
    "PATH_NOT_FOUND",
    "PATH_UNREADABLE",
    "TIMEOUT",
    "ABORTED",
    "OUTPUT_TOO_LARGE",
    "OUTPUT_PARSE_FAILED",
    "PREVIEW_TRUNCATED",
    "REWRITE_STALE_PREVIEW",
    "SG_FAILED",
    "ENCODING_ERROR",
];

const PATTERN_BYTES_NOTE: &str =
    "Max 16 KiB (16384 BYTES, UTF-8) — the limit counts bytes, not characters.";
const REWRITE_BYTES_NOTE: &str =
    "Max 64 KiB (65536 BYTES, UTF-8) — the limit counts bytes, not characters.";
const INLINE_RULES_BYTES_NOTE: &str =
    "Max 64 KiB (65536 BYTES, UTF-8) — the limit counts bytes, not characters.";

fn common_properties() -> [(&'static str, Value); 7] {
    [
        (
            "paths",
            json!({
                "type": "array",
                "minItems": 1,
                "maxItems": 64,
                "items": { "type": "string", "minLength": 1, "maxLength": 4096 },
                "description": "Files or directories to search. Required — there is no implicit '.' default.",
            }),
        ),
        (
            "workdir",
            json!({
                "type": "string",
                "minLength": 1,
                "maxLength": 4096,
                "description": "Working directory for the sg process. Defaults to the server's cwd.",
            }),
        ),
        (
            "globs",
            json!({
                "type": "array",
                "maxItems": 32,
                "items": { "type": "string", "minLength": 1, "maxLength": 1024 },
                "description": "Optional include/exclude globs passed through to ast-grep.",
            }),
        ),
        (
            "maxMatches",
            json!({ "type": "integer", "minimum": 1, "maximum": 500, "description": "Maximum matches to return (default 50)." }),
        ),
        (
            "timeoutMs",
            json!({ "type": "integer", "minimum": 1000, "maximum": 300000, "description": "Whole-call timeout budget in milliseconds (default 300000)." }),
        ),
        (
            "includeHidden",
            json!({ "type": "boolean", "description": "Include hidden files (--no-ignore hidden)." }),
        ),
        (
            "followSymlinks",
            json!({ "type": "boolean", "description": "Follow symlinks (--follow)." }),
        ),
    ]
}

fn common(key: &str) -> Value {
    common_properties()
        .into_iter()
        .find(|(name, _)| *name == key)
        .map(|(_, value)| value)
        .expect("known common property")
}

fn properties(entries: Vec<(&str, Value)>) -> Value {
    Value::Object(
        entries
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v))
            .collect(),
    )
}

fn pattern_property() -> Value {
    json!({ "type": "string", "minLength": 1, "description": format!("ast-grep pattern — code, not regex. {PATTERN_BYTES_NOTE}") })
}

fn language_property() -> Value {
    json!({ "type": "string", "enum": LANGUAGES, "description": "Language the pattern must parse in." })
}

fn selector_property() -> Value {
    json!({ "type": "string", "minLength": 1, "maxLength": 128, "description": "Optional sub-node selector." })
}

fn strictness_property() -> Value {
    json!({ "type": "string", "enum": STRICTNESS, "description": "Match strictness (default smart)." })
}

fn force_property() -> Value {
    json!({ "type": "boolean", "description": "Bypass non-fatal pattern hint rejections." })
}

pub fn ast_grep_mcp_tools() -> Vec<McpToolDescriptor> {
    let search = properties(vec![
        ("pattern", pattern_property()),
        ("language", language_property()),
        ("paths", common("paths")),
        ("workdir", common("workdir")),
        ("globs", common("globs")),
        ("selector", selector_property()),
        ("strictness", strictness_property()),
        ("maxMatches", common("maxMatches")),
        ("timeoutMs", common("timeoutMs")),
        ("includeHidden", common("includeHidden")),
        ("followSymlinks", common("followSymlinks")),
        ("force", force_property()),
    ]);
    let rewrite = properties(vec![
        ("pattern", pattern_property()),
        (
            "rewrite",
            json!({ "type": "string", "description": format!("Replacement code; empty deletes the match. {REWRITE_BYTES_NOTE}") }),
        ),
        ("language", language_property()),
        ("paths", common("paths")),
        ("workdir", common("workdir")),
        ("globs", common("globs")),
        ("selector", selector_property()),
        ("strictness", strictness_property()),
        (
            "apply",
            json!({ "type": "boolean", "description": "Write the rewrite to disk. Default false (dry run)." }),
        ),
        ("maxMatches", common("maxMatches")),
        ("timeoutMs", common("timeoutMs")),
        ("includeHidden", common("includeHidden")),
        ("followSymlinks", common("followSymlinks")),
        ("force", force_property()),
    ]);
    let scan = properties(vec![
        (
            "ruleFile",
            json!({ "type": "string", "minLength": 1, "maxLength": 4096, "description": "Path to a YAML rule file. Mutually exclusive with inlineRules." }),
        ),
        (
            "inlineRules",
            json!({ "type": "string", "minLength": 1, "description": format!("Inline YAML rule text. Mutually exclusive with ruleFile. {INLINE_RULES_BYTES_NOTE}") }),
        ),
        ("paths", common("paths")),
        ("workdir", common("workdir")),
        ("globs", common("globs")),
        ("maxMatches", common("maxMatches")),
        ("timeoutMs", common("timeoutMs")),
        ("includeHidden", common("includeHidden")),
        ("followSymlinks", common("followSymlinks")),
        (
            "includeMetadata",
            json!({ "type": "boolean", "description": "Include rule metadata in each match." }),
        ),
        (
            "apply",
            json!({ "type": "boolean", "description": "Write rule fixes to disk. Default false (dry run)." }),
        ),
    ]);
    vec![
        McpToolDescriptor {
            name: SEARCH_TOOL_NAME.into(),
            title: None,
            description: SEARCH_TOOL_DESCRIPTION.into(),
            input_schema: json!({
                "type": "object",
                "properties": search,
                "required": ["pattern", "language", "paths"],
                "additionalProperties": false,
            }),
        },
        McpToolDescriptor {
            name: REWRITE_TOOL_NAME.into(),
            title: None,
            description: REWRITE_TOOL_DESCRIPTION.into(),
            input_schema: json!({
                "type": "object",
                "properties": rewrite,
                "required": ["pattern", "rewrite", "language", "paths"],
                "additionalProperties": false,
            }),
        },
        McpToolDescriptor {
            name: SCAN_TOOL_NAME.into(),
            title: None,
            description: SCAN_TOOL_DESCRIPTION.into(),
            input_schema: json!({
                "type": "object",
                "properties": scan,
                "required": ["paths"],
                "oneOf": [
                    { "type": "object", "required": ["ruleFile"], "not": { "required": ["inlineRules"] } },
                    { "type": "object", "required": ["inlineRules"], "not": { "required": ["ruleFile"] } },
                ],
                "additionalProperties": false,
            }),
        },
    ]
}

/// A tool implementation: `Err` stands in for a thrown exception and becomes `SG_FAILED`.
pub type ToolExecutor =
    Arc<dyn Fn(&Value, &str, Option<&AbortSignal>) -> Result<Value, String> + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SgResolveError {
    pub message: String,
    pub hints: Vec<String>,
}

pub type SgPathResolver = Arc<dyn Fn() -> Result<String, SgResolveError> + Send + Sync>;

#[derive(Clone, Default)]
pub struct AstGrepToolExecutors {
    pub search: Option<ToolExecutor>,
    pub rewrite: Option<ToolExecutor>,
    pub scan: Option<ToolExecutor>,
}

#[derive(Clone, Default)]
pub struct AstGrepMcpOptions {
    pub resolve_sg_path: Option<SgPathResolver>,
    pub executors: AstGrepToolExecutors,
    pub signal: Option<AbortSignal>,
    pub lifecycle_log: Option<Arc<dyn McpLifecycleLog + Send + Sync>>,
    pub parent_watchdog: Option<ParentWatchdogConfig>,
}

pub fn handle_ast_grep_mcp_request(
    input: &Value,
    options: &AstGrepMcpOptions,
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
    let method = request.get("method");
    match method.and_then(Value::as_str) {
        Some("notifications/initialized") => None,
        Some("ping") => Some(success_response(id, Map::new())),
        Some("initialize") => Some(success_response(
            id,
            record(json!({
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": AST_GREP_SERVER_NAME, "version": AST_GREP_SERVER_VERSION },
                "protocolVersion": requested_protocol_version(request.get("params")),
            })),
        )),
        Some("tools/list") => Some(success_response(
            id,
            record(json!({ "tools": ast_grep_mcp_tools() })),
        )),
        Some("tools/call") => Some(handle_tool_call(id, request.get("params"), options)),
        _ => Some(error_response(
            id,
            -32601,
            format!("Method not found: {}", method_text(method)),
            None,
        )),
    }
}

fn method_text(method: Option<&Value>) -> String {
    match method {
        None => "undefined".into(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    }
}

fn record(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

fn requested_protocol_version(params: Option<&Value>) -> String {
    params
        .and_then(|params| params.get("protocolVersion"))
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_PROTOCOL_VERSION)
        .to_owned()
}

pub fn coerce_tool_arguments(value: Option<&Value>) -> Value {
    match value {
        Some(Value::Object(map)) => Value::Object(map.clone()),
        _ => Value::Object(Map::new()),
    }
}

fn handle_tool_call(
    id: JsonRpcId,
    params: Option<&Value>,
    options: &AstGrepMcpOptions,
) -> JsonRpcResponse {
    let Some(name) = params
        .and_then(Value::as_object)
        .and_then(|params| params.get("name"))
        .and_then(Value::as_str)
    else {
        return error_response(id, -32602, "tools/call requires params.name", None);
    };
    let args = coerce_tool_arguments(params.and_then(|params| params.get("arguments")));
    if ![SEARCH_TOOL_NAME, REWRITE_TOOL_NAME, SCAN_TOOL_NAME].contains(&name) {
        let available = ast_grep_mcp_tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>()
            .join(", ");
        return tool_failure(
            id,
            "INVALID_ARGUMENT",
            &format!("Unknown ast_grep tool: {name}. Available tools: {available}."),
            &[],
            None,
        );
    }
    let sg_path = match resolve_sg_path(options) {
        Ok(path) => path,
        Err(error) => {
            return tool_failure(id, "BINARY_NOT_FOUND", &error.message, &error.hints, None);
        }
    };
    match dispatch(name, &args, &sg_path, options) {
        Ok(payload) => {
            let is_error = payload.get("ok") != Some(&Value::Bool(true));
            tool_response(id, &payload, is_error)
        }
        Err(DispatchError::Argument { message, language }) => {
            tool_failure(id, "INVALID_ARGUMENT", &message, &[], Some(&language))
        }
        Err(DispatchError::Failed(message)) => tool_failure(id, "SG_FAILED", &message, &[], None),
    }
}

enum DispatchError {
    Argument { message: String, language: String },
    Failed(String),
}

fn dispatch(
    name: &str,
    args: &Value,
    sg_path: &str,
    options: &AstGrepMcpOptions,
) -> Result<Value, DispatchError> {
    let signal = options.signal.as_ref();
    let executors = &options.executors;
    if name == SEARCH_TOOL_NAME {
        let parsed = parse_search_input(args).map_err(|message| DispatchError::Argument {
            message,
            language: args
                .get("language")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned(),
        })?;
        return match &executors.search {
            Some(execute) => {
                execute(&parsed.to_value(), sg_path, signal).map_err(DispatchError::Failed)
            }
            None => Ok(execute_search(&parsed, sg_path, signal)),
        };
    }
    if name == REWRITE_TOOL_NAME {
        return match &executors.rewrite {
            Some(execute) => execute(args, sg_path, signal).map_err(DispatchError::Failed),
            None => Ok(execute_rewrite(
                args,
                sg_path,
                signal,
                &RewriteHooks::default(),
            )),
        };
    }
    match &executors.scan {
        Some(execute) => execute(args, sg_path, signal).map_err(DispatchError::Failed),
        None => Ok(execute_scan(args, sg_path, signal)),
    }
}

fn resolve_sg_path(options: &AstGrepMcpOptions) -> Result<String, SgResolveError> {
    if let Some(resolve) = &options.resolve_sg_path {
        return resolve();
    }
    match utils::resolve_sg_binary_sync(&utils::SgResolverOptions::default()) {
        utils::SgResolution::Found { path, .. } => Ok(path),
        utils::SgResolution::NotFound { error } => Err(SgResolveError {
            message: error.message,
            hints: error.hints,
        }),
    }
}

fn tool_response(id: JsonRpcId, payload: &Value, is_error: bool) -> JsonRpcResponse {
    success_response(
        id,
        record(json!({
            "content": [{ "type": "text", "text": payload.to_string() }],
            "isError": is_error,
        })),
    )
}

fn tool_failure(
    id: JsonRpcId,
    code: &str,
    message: &str,
    hints: &[String],
    language: Option<&str>,
) -> JsonRpcResponse {
    let mut error = Map::new();
    error.insert("code".into(), json!(code));
    error.insert("message".into(), json!(message));
    error.insert("retryable".into(), json!(false));
    error.insert("phase".into(), json!("preflight"));
    if let Some(language) = language {
        error.insert("language".into(), json!(language));
    }
    let details = if hints.is_empty() {
        json!({})
    } else {
        json!({ "hints": hints })
    };
    error.insert("details".into(), details);
    tool_response(
        id,
        &json!({ "schemaVersion": 1, "ok": false, "error": error }),
        true,
    )
}

/// Serves MCP over newline-delimited JSON-RPC until `input` closes or the parent process exits.
/// Each request gets a fresh [`AbortSignal`]; a parent exit aborts the one in flight.
pub fn run_mcp_stdio_server<R, W>(
    input: R,
    output: &mut W,
    options: AstGrepMcpOptions,
) -> Result<ServerOutcome, ServerError<std::convert::Infallible>>
where
    R: Read + Send + 'static,
    W: Write,
{
    let active: Arc<Mutex<Option<AbortSignal>>> = Arc::new(Mutex::new(None));
    let handler_active = Arc::clone(&active);
    let parent_watchdog = options.parent_watchdog.clone().unwrap_or_default();
    let lifecycle_log = options.lifecycle_log.clone();
    let handler =
        move |request: &Value| -> Result<Option<JsonRpcResponse>, std::convert::Infallible> {
            let signal = AbortSignal::new();
            *handler_active
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(signal.clone());
            let request_options = AstGrepMcpOptions {
                signal: Some(signal),
                ..options.clone()
            };
            let response = handle_ast_grep_mcp_request(request, &request_options);
            *handler_active
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = None;
            Ok(response)
        };
    let mut config = JsonRpcStdioServerConfig::new(handler);
    config.idle_timeout_ms = Some(0);
    config.parent_watchdog = Some(parent_watchdog);
    config.log = lifecycle_log;
    config.on_parent_exit = Some(Arc::new(move || {
        if let Some(signal) = active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            signal.abort("parent process exited");
        }
    }));
    run_json_rpc_stdio_server(input, output, config)
}
