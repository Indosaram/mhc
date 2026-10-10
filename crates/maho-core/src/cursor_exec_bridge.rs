//! Cursor exec-channel to local tool bridge.
//!
//! This is the Rust counterpart of senpi's `cursor-exec-bridge.ts`.  The
//! Cursor API owns conversion from `ToolResultMessage` to protobuf results;
//! bridge handlers therefore deliberately return `ToolResultMessage`s.

use std::sync::Arc;

use maho_agent::types::{AgentEvent, AgentTool};
use maho_ai::api::cursor_agent::types::{CursorExecHandlers, CursorExecHandlerResult, CursorMcpCall, CursorPiCall};
use maho_ai::api::cursor_agent::r#gen::agent_pb::*;
use maho_ai::types::{ContentBlock, ToolResultMessage};
use maho_ai::utils::abort::AbortSignal;
use serde_json::{Map, Value};

pub type ToolResolver = Arc<dyn Fn(&str) -> Option<AgentTool> + Send + Sync>;
pub type EventEmitter = Arc<dyn Fn(AgentEvent, AbortSignal) -> maho_ai::types::BoxFuture<'static, ()> + Send + Sync>;
pub type ResultEmitter = Arc<dyn Fn(String, String, Value, ToolResultMessage, bool) -> maho_ai::types::BoxFuture<'static, ()> + Send + Sync>;
pub type Preflight = Arc<dyn Fn(String, String, Value) -> maho_ai::types::BoxFuture<'static, Option<String>> + Send + Sync>;

pub struct CursorExecBridgeOptions {
    pub get_tool: ToolResolver,
    pub emit_event: EventEmitter,
    pub emit_tool_result: Option<ResultEmitter>,
    pub preflight_tool_call: Option<Preflight>,
    pub get_abort_signal: Arc<dyn Fn() -> Option<AbortSignal> + Send + Sync>,
}

fn error_result(id: String, name: String, message: impl Into<String>) -> ToolResultMessage {
    ToolResultMessage {
        tool_call_id: id,
        tool_name: name,
        content: vec![ContentBlock::text(message)],
        details: None,
        usage: None,
        added_tool_names: None,
        is_error: true,
        timestamp: now_ms(),
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as i64
}

fn is_record(value: &Value) -> bool { value.is_object() }

fn empty_result(id: String, name: String) -> ToolResultMessage {
    ToolResultMessage { tool_call_id: id, tool_name: name, content: vec![ContentBlock::text("")], details: None,
        usage: None, added_tool_names: None, is_error: false, timestamp: now_ms() }
}

fn map_cursor_pi_grep_args(args: &PiGrepExecArgs) -> Value {
    let mut map = Map::new();
    map.insert("pattern".into(), args.pattern.clone().into());
    if let Some(value) = &args.path { map.insert("path".into(), value.clone().into()); }
    if let Some(value) = &args.glob { map.insert("glob".into(), value.clone().into()); }
    if let Some(value) = args.ignore_case { map.insert("ignoreCase".into(), value.into()); }
    if let Some(value) = args.literal { map.insert("literal".into(), value.into()); }
    if let Some(value) = args.context { map.insert("context".into(), value.into()); }
    if let Some(value) = args.limit { map.insert("limit".into(), value.into()); }
    Value::Object(map)
}

#[allow(dead_code)]
fn empty_result_shape_test_helper() -> ToolResultMessage { empty_result("id".into(), "read".into()) }

fn text_result(id: String, name: String, result: maho_agent::types::AgentToolResult) -> ToolResultMessage {
    ToolResultMessage {
        tool_call_id: id,
        tool_name: name,
        content: result.content,
        details: Some(result.details),
        usage: result.usage,
        added_tool_names: result.added_tool_names,
        is_error: result.is_error.unwrap_or(false),
        timestamp: now_ms(),
    }
}

async fn execute_tool(options: &CursorExecBridgeOptions, name: &str, id: String, args: Value) -> ToolResultMessage {
    let Some(signal) = (options.get_abort_signal)() else {
        return error_result(id, name.to_owned(), "Tool execution has no active run");
    };
    if signal.aborted() {
        return error_result(id, name.to_owned(), "Tool execution has no active run");
    }
    let Some(tool) = (options.get_tool)(name) else {
        return error_result(id, name.to_owned(), format!("Tool \"{name}\" is not available in this session"));
    };
    let args = if args.is_null() { Value::Object(Map::new()) } else { args };
    let Some(args_object) = args.as_object() else {
        return error_result(id, name.to_owned(), "Tool arguments must be an object");
    };
    let args = Value::Object(args_object.iter().filter(|(_, v)| !v.is_null()).map(|(k, v)| (k.clone(), v.clone())).collect());
    (options.emit_event)(AgentEvent::ToolExecutionStart { tool_call_id: id.clone(), tool_name: name.to_owned(), args: args.clone() }, signal.clone()).await;
    let prepared = tool.prepare_arguments.as_ref().map(|prepare| prepare(args.clone())).unwrap_or(args.clone());
    if !is_record(&prepared) { return error_result(id, name.to_owned(), format!("Tool \"{name}\" prepared non-object arguments")); }
    let result = if let Some(preflight) = &options.preflight_tool_call {
        match preflight(id.clone(), name.to_owned(), prepared.clone()).await {
            Some(reason) => error_result(id.clone(), name.to_owned(), reason),
            None => text_result(id.clone(), name.to_owned(), (tool.execute)(id.clone(), prepared, Some(signal.clone()), None).await),
        }
    } else {
        text_result(id.clone(), name.to_owned(), (tool.execute)(id.clone(), prepared, Some(signal.clone()), None).await)
    };
    (options.emit_event)(AgentEvent::ToolExecutionEnd { tool_call_id: id.clone(), tool_name: name.to_owned(), result: serde_json::to_value(&result).unwrap_or(Value::Null), is_error: result.is_error }, signal).await;
    if let Some(emit) = &options.emit_tool_result {
        emit(id, name.to_owned(), args, result.clone(), result.is_error).await;
    }
    result
}

fn handler_result<T>(result: ToolResultMessage) -> CursorExecHandlerResult<T> {
    CursorExecHandlerResult::ToolResult(result)
}

fn object(entries: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    Value::Object(entries.into_iter().map(|(key, value)| (key.to_owned(), value)).collect())
}

pub fn create_cursor_exec_bridge(options: CursorExecBridgeOptions) -> CursorExecHandlers {
    let shared = Arc::new(options);
    macro_rules! handler { ($shared:expr, $ty:ty, $name:expr, $id:expr, $args:expr) => {{
        let shared = $shared.clone();
        let name: String = ($name).into();
        Box::pin(async move { handler_result::<$ty>(execute_tool(&shared, &name, $id, $args).await) })
    }} }
    CursorExecHandlers {
        read: Some(Arc::new({ let shared = shared.clone(); move |a: ReadArgs| handler!(&shared, ReadResult, "read", a.tool_call_id, object([("path", a.path.into()), ("offset", a.offset.map(Value::from).unwrap_or(Value::Null)), ("limit", a.limit.map(Value::from).unwrap_or(Value::Null))])) })),
        ls: Some(Arc::new({ let shared = shared.clone(); move |a: LsArgs| handler!(&shared, LsResult, "ls", a.tool_call_id, object([("path", maho_ai::api::cursor_agent::pi_args::pi_ls_path(Some(&a.path)).into())])) })),
        grep: Some(Arc::new({ let shared = shared.clone(); move |a: GrepArgs| handler!(&shared, GrepResult, "grep", a.tool_call_id, object([("pattern", a.pattern.into()), ("path", a.path.map(Value::from).unwrap_or(Value::Null)), ("glob", a.glob.map(Value::from).unwrap_or(Value::Null)), ("ignoreCase", a.case_insensitive.map(Value::from).unwrap_or(Value::Null))])) })),
        write: Some(Arc::new({ let shared = shared.clone(); move |a: WriteArgs| handler!(&shared, WriteResult, "write", a.tool_call_id, object([("path", a.path.into()), ("content", a.file_text.into())])) })),
        delete: None, shell: Some(Arc::new({ let shared = shared.clone(); move |a: ShellArgs| handler!(&shared, ShellResult, "bash", a.tool_call_id, object([("command", maho_ai::api::cursor_agent::pi_args::compose_shell_command(&a.command, Some(&a.working_directory)).into()), ("timeout", (a.timeout > 0).then_some(a.timeout).map(Value::from).unwrap_or(Value::Null))])) })),
        shell_stream: None, diagnostics: None,
        mcp: Some(Arc::new({ let shared = shared.clone(); move |a: CursorMcpCall| { let name = if a.tool_name.is_empty() { a.name } else { a.tool_name }; handler!(&shared, McpResult, name, a.tool_call_id, Value::Object(a.args)) } })),
        mcp_approval_preflight: None,
        pi_read: Some(Arc::new({ let shared = shared.clone(); move |a: CursorPiCall<PiReadExecArgs>| handler!(&shared, PiReadExecResult, "read", a.tool_call_id, object([("path", a.args.path.into()), ("offset", a.args.offset.map(Value::from).unwrap_or(Value::Null)), ("limit", a.args.limit.map(Value::from).unwrap_or(Value::Null))])) })),
        pi_bash: Some(Arc::new({ let shared = shared.clone(); move |a: CursorPiCall<PiBashExecArgs>| handler!(&shared, PiBashExecResult, "bash", a.tool_call_id, object([("command", a.args.command.into()), ("timeout", a.args.timeout.filter(|v| *v >= 0.0).map(Value::from).unwrap_or(Value::Null))])) })),
        pi_edit: Some(Arc::new({ let shared = shared.clone(); move |a: CursorPiCall<PiEditExecArgs>| handler!(&shared, PiEditExecResult, "edit", a.tool_call_id, object([("path", a.args.path.into()), ("edits", serde_json::Value::Array(a.args.edits.into_iter().map(|e| serde_json::json!({"oldText":e.old_text,"newText":e.new_text})).collect()))])) })),
        pi_write: Some(Arc::new({ let shared = shared.clone(); move |a: CursorPiCall<PiWriteExecArgs>| handler!(&shared, PiWriteExecResult, "write", a.tool_call_id, object([("path", a.args.path.into()), ("content", a.args.content.into())])) })),
        pi_grep: Some(Arc::new({ let shared = shared.clone(); move |a: CursorPiCall<PiGrepExecArgs>| handler!(&shared, PiGrepExecResult, "grep", a.tool_call_id, map_cursor_pi_grep_args(&a.args)) })),
        pi_find: Some(Arc::new({ let shared = shared.clone(); move |a: CursorPiCall<PiFindExecArgs>| handler!(&shared, PiFindExecResult, "find", a.tool_call_id, object([("pattern", a.args.pattern.into()), ("path", a.args.path.map(Value::from).unwrap_or(Value::Null)), ("limit", a.args.limit.map(Value::from).unwrap_or(Value::Null))])) })),
        pi_ls: Some(Arc::new({ let shared = shared.clone(); move |a: CursorPiCall<PiLsExecArgs>| handler!(&shared, PiLsExecResult, "ls", a.tool_call_id, object([("path", a.args.path.unwrap_or_else(|| ".".into()).into()), ("limit", a.args.limit.map(Value::from).unwrap_or(Value::Null))])) })),
        on_tool_result: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn error_result_is_error() { assert!(error_result("id".into(), "read".into(), "bad").is_error); }

    #[test]
    fn error_result_preserves_call_identity_and_message() {
        let result = error_result("call-7".into(), "grep".into(), "not available");
        assert_eq!(result.tool_call_id, "call-7");
        assert_eq!(result.tool_name, "grep");
        assert!(matches!(&result.content[0], ContentBlock::Text(text) if text.text == "not available"));
    }

    #[test]
    fn map_grep_args_uses_camel_case_and_omits_absent_values() {
        let args = PiGrepExecArgs { pattern: "needle".into(), path: Some("src".into()), glob: None,
            ignore_case: Some(true), literal: Some(false), context: Some(2), limit: Some(10) };
        assert_eq!(map_cursor_pi_grep_args(&args), json!({"pattern":"needle","path":"src","ignoreCase":true,"literal":false,"context":2,"limit":10}));
    }

    #[test]
    fn empty_result_has_success_shape() {
        let result = empty_result("call".into(), "read".into());
        assert_eq!(result.tool_call_id, "call");
        assert_eq!(result.tool_name, "read");
        assert!(!result.is_error);
        assert!(matches!(&result.content[0], ContentBlock::Text(text) if text.text.is_empty()));
    }

    #[test]
    fn bridge_registers_each_supported_handler_kind() {
        let controller = maho_ai::utils::abort::AbortController::new();
        let options = CursorExecBridgeOptions {
            get_tool: Arc::new(|_| None),
            emit_event: Arc::new(|_, _| Box::pin(async {})),
            emit_tool_result: None, preflight_tool_call: None,
            get_abort_signal: Arc::new(move || Some(controller.signal())),
        };
        let handlers = create_cursor_exec_bridge(options);
        assert!(handlers.read.is_some() && handlers.ls.is_some() && handlers.grep.is_some() && handlers.write.is_some());
        assert!(handlers.shell.is_some() && handlers.mcp.is_some());
        assert!(handlers.pi_read.is_some() && handlers.pi_bash.is_some() && handlers.pi_edit.is_some());
        assert!(handlers.pi_write.is_some() && handlers.pi_grep.is_some() && handlers.pi_find.is_some() && handlers.pi_ls.is_some());
        assert!(handlers.delete.is_none() && handlers.diagnostics.is_none() && handlers.mcp_approval_preflight.is_none());
    }
}
