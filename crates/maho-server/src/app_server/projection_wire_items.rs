use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const MAX_TOOL_OUTPUT_BYTES: usize = 256 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolItemType { CommandExecution, FileChange, McpToolCall, DynamicToolCall }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolExecutionStatus { InProgress, Completed, Failed }
pub struct ActiveToolItem {
    pub id: String,
    pub name: String,
    pub args: Value,
    pub output: String,
    pub completed: bool,
}
pub fn classify_tool(name: &str) -> ToolItemType {
    match name {
        "bash" => ToolItemType::CommandExecution,
        "edit" | "write" | "apply_patch" => ToolItemType::FileChange,
        _ if name.split_once("__").is_some_and(|(server, _)| !server.is_empty() && !server.contains('_')) => ToolItemType::McpToolCall,
        _ => ToolItemType::DynamicToolCall,
    }
}
pub fn remaining_command_output_bytes(output: &str) -> i128 {
    i128::try_from(MAX_TOOL_OUTPUT_BYTES).unwrap_or_default() - i128::try_from(output.len()).unwrap_or_default()
}
pub fn cap_command_output(value: &str, max_bytes: usize) -> &str {
    let mut end = value.len().min(max_bytes);
    while !value.is_char_boundary(end) { end -= 1; }
    &value[..end]
}
pub fn extract_tool_text(result: &Value) -> String {
    match result.get("content").and_then(Value::as_array) {
        Some(content) => content.iter().filter(|item| item["type"] == "text").filter_map(|item| item["text"].as_str()).collect(),
        None => result["text"].as_str().unwrap_or_default().into(),
    }
}
pub fn command_execution_item(tool: &ActiveToolItem, status: ToolExecutionStatus, cwd: &str, result: &Value) -> Value {
    let command = if tool.args.is_object() { tool.args["command"].as_str().or_else(|| tool.args["cmd"].as_str()).map(str::to_owned).unwrap_or_else(|| tool.args.to_string()) } else { String::new() };
    let exit_code = result["details"].get("exitCode").filter(|value| !value.is_null()).or_else(|| result["details"].get("code")).filter(|value| value.is_number());
    json!({"type":"commandExecution","id":tool.id,"command":command,"cwd":cwd,"processId":null,"source":"agent","status":status,"commandActions":[],"aggregatedOutput":if tool.output.is_empty() { Value::Null } else { json!(tool.output) },"exitCode":if status == ToolExecutionStatus::InProgress { None } else { exit_code },"durationMs":null})
}
pub fn mcp_tool_call_item(tool: &ActiveToolItem, status: ToolExecutionStatus, result: &Value) -> Value {
    let (server, name) = tool.name.split_once("__").unwrap_or(("", &tool.name));
    let content = result["content"].as_array().cloned().unwrap_or_default();
    let text = extract_tool_text(result);
    json!({"type":"mcpToolCall","id":tool.id,"server":server,"tool":name,"status":status,"arguments":tool.args,"appContext":null,"pluginId":null,"result":if status == ToolExecutionStatus::Completed { json!({"content":content,"structuredContent":null,"_meta":null}) } else { Value::Null },"error":if status == ToolExecutionStatus::Failed { json!({"message":if text.is_empty() { "Tool execution failed" } else { &text }}) } else { Value::Null },"durationMs":null})
}
pub fn dynamic_tool_call_item(tool: &ActiveToolItem, status: ToolExecutionStatus, result: &Value, is_error: bool) -> Value {
    json!({"type":"dynamicToolCall","id":tool.id,"namespace":null,"tool":tool.name,"arguments":tool.args,"status":status,"contentItems":if status == ToolExecutionStatus::InProgress { Value::Null } else { json!([{"type":"inputText","text":extract_tool_text(result)}]) },"success":if status == ToolExecutionStatus::InProgress { None } else { Some(!is_error) },"durationMs":null})
}

pub fn tool_wire_projection(tool: &ActiveToolItem, status: ToolExecutionStatus, cwd: &str, result: &Value, is_error: bool) -> (Value, String) {
    match classify_tool(&tool.name) {
        ToolItemType::CommandExecution => (command_execution_item(tool, status, cwd, result), String::new()),
        ToolItemType::FileChange => super::projection_file_changes::file_change_projection(&tool.id, &tool.name, &tool.args, status, result),
        ToolItemType::McpToolCall => (mcp_tool_call_item(tool, status, result), String::new()),
        ToolItemType::DynamicToolCall => (dynamic_tool_call_item(tool, status, result, is_error), String::new()),
    }
}
pub fn provider_native_item(id: &str, message: &Value, content: &Value) -> Value {
    if content["kind"] == "web_search_call" { return super::projection_web_search::web_search_item(id, content); }
    json!({"type":"providerNative","id":id,"provider":message["provider"],"api":message["api"],"nativeType":content["kind"],"payload":content["raw"]})
}
pub fn build_wire_item(mut item: Value) -> Value {
    if item["type"] == "commandExecution" && let Some(output) = item["aggregatedOutput"].as_str() {
        item["aggregatedOutput"] = json!(cap_command_output(output, MAX_TOOL_OUTPUT_BYTES));
    }
    item
}
