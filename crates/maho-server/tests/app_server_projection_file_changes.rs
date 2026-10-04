use maho_server::app_server::{projection_file_changes::file_change_projection, projection_wire_items::ToolExecutionStatus};
use serde_json::json;
#[test]
fn file_preview_takes_precedence_and_patch_fallback_normalizes_terminal_newline() {
    let result = json!({"details":{"preview":{"files":[{"filePath":"a","operation":"update","movePath":"b","patch":"diff"},{"filePath":"bad","operation":"unknown","patch":"ignored"}]},"patch":"fallback"}});
    let (item, diff) = file_change_projection("id", "apply_patch", &json!({}), ToolExecutionStatus::Completed, &result);
    assert_eq!(diff, "diff\n");
    assert_eq!(item["changes"], json!([{"path":"a","kind":{"type":"update","move_path":"b"},"diff":"diff\n"}]));
    let (item, diff) = file_change_projection("id", "write", &json!({"file_path":"new"}), ToolExecutionStatus::Failed, &json!({"details":{"patch":"--- /dev/null\n+++ new"}}));
    assert_eq!(item["changes"][0]["kind"], json!({"type":"add"}));
    assert!(diff.ends_with('\n'));
    assert!(file_change_projection("id", "write", &json!({}), ToolExecutionStatus::InProgress, &result).1.is_empty());
}
