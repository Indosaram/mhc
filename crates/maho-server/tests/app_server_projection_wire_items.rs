use maho_server::app_server::projection_wire_items::*;
use serde_json::json;
#[test]
fn output_cap_preserves_utf8_and_tool_classification_obeys_mcp_prefix() {
    assert_eq!(cap_command_output("한글ab", 4), "한");
    assert_eq!(cap_command_output("한글ab", 0), "");
    assert_eq!(classify_tool("server__tool"), ToolItemType::McpToolCall);
    assert_eq!(classify_tool("bad_name__tool"), ToolItemType::DynamicToolCall);
    assert_eq!(classify_tool("__tool"), ToolItemType::DynamicToolCall);
    assert_eq!(classify_tool("apply_patch"), ToolItemType::FileChange);
    assert_eq!(classify_tool("bash"), ToolItemType::CommandExecution);
}
#[test]
fn tool_items_preserve_wire_nulls_and_terminal_status() {
    let tool = ActiveToolItem { id:"id".into(), name:"server__tool".into(), args:json!({"cmd":"ls"}), output:String::new(), completed:false };
    let result = json!({"content":[{"type":"text","text":"one"},{"type":"image","data":"ignored"},{"type":"text","text":"two"}],"details":{"exitCode":null,"code":7}});
    assert_eq!(extract_tool_text(&result), "onetwo");
    assert!(command_execution_item(&tool, ToolExecutionStatus::InProgress, "/tmp", &result)["exitCode"].is_null());
    assert_eq!(command_execution_item(&tool, ToolExecutionStatus::Failed, "/tmp", &result)["exitCode"], 7);
    assert_eq!(mcp_tool_call_item(&tool, ToolExecutionStatus::Completed, &result)["tool"], "tool");
    assert_eq!(dynamic_tool_call_item(&tool, ToolExecutionStatus::Failed, &result, true)["success"], false);
}
