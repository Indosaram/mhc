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
    let read_options = Arc::new(options);
    let options = read_options.clone();
    CursorExecHandlers {
        read: Some(Arc::new(move |args: ReadArgs| { let options = options.clone(); Box::pin(async move { handler_result(execute_tool(&options, "read", args.tool_call_id, object([("path", args.path.into()), ("offset", args.offset.map(Value::from).unwrap_or(Value::Null)), ("limit", args.limit.map(Value::from).unwrap_or(Value::Null))])).await) }) })),
        ls: None,
        grep: None,
        write: None,
        delete: None,
        shell: None,
        shell_stream: None,
        diagnostics: None,
        mcp: None,
        mcp_approval_preflight: None,
        pi_read: Some(Arc::new(move |call: CursorPiCall<PiReadExecArgs>| { let options = read_options.clone(); Box::pin(async move { handler_result(execute_tool(&options, "read", call.tool_call_id, object([("path", call.args.path.into()), ("offset", call.args.offset.map(Value::from).unwrap_or(Value::Null)), ("limit", call.args.limit.map(Value::from).unwrap_or(Value::Null))])).await) }) })),
        pi_bash: None,
        pi_edit: None,
        pi_write: None,
        pi_grep: None,
        pi_find: None,
        pi_ls: None,
        on_tool_result: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn error_result_is_error() { assert!(error_result("id".into(), "read".into(), "bad").is_error); }

    #[test]
    fn error_result_preserves_call_identity_and_message() {
        let result = error_result("call-7".into(), "grep".into(), "not available");
        assert_eq!(result.tool_call_id, "call-7");
        assert_eq!(result.tool_name, "grep");
        assert!(matches!(&result.content[0], ContentBlock::Text(text) if text.text == "not available"));
    }
}
