use super::projection_wire_items::ToolExecutionStatus;
use serde_json::{Value, json};

fn nonempty(value: &Value) -> Option<&str> { value.as_str().filter(|value| !value.is_empty()) }
fn patch(value: &Value) -> Option<String> {
    nonempty(value).map(|value| if value.ends_with('\n') { value.into() } else { format!("{value}\n") })
}
fn kind(operation: &str, move_path: Option<&str>) -> Option<Value> {
    match operation {
        "add" | "delete" => Some(json!({"type":operation})),
        "update" => Some(json!({"type":"update","move_path":move_path})),
        _ => None,
    }
}
pub fn file_change_projection(id: &str, name: &str, args: &Value, status: ToolExecutionStatus, result: &Value) -> (Value, String) {
    let mut changes = Vec::new();
    if status != ToolExecutionStatus::InProgress && result["details"].is_object() {
        for file in result["details"]["preview"]["files"].as_array().into_iter().flatten() {
            if let (Some(path), Some(operation), Some(diff)) = (nonempty(&file["filePath"]), nonempty(&file["operation"]), patch(&file["patch"]))
                && let Some(kind) = kind(operation, nonempty(&file["movePath"])) {
                changes.push(json!({"path":path,"kind":kind,"diff":diff}));
            }
        }
        if changes.is_empty() && matches!(name, "edit" | "write")
            && let Some(path) = nonempty(&args["path"]).or_else(|| nonempty(&args["file_path"]))
            && let Some(diff) = patch(&result["details"]["patch"]) {
            let operation = result["details"]["operation"].as_str().filter(|operation| matches!(*operation, "add" | "delete" | "update")).unwrap_or(if diff.starts_with("--- /dev/null\n") { "add" } else { "update" });
            if let Some(kind) = kind(operation, None) { changes.push(json!({"path":path,"kind":kind,"diff":diff})); }
        }
    }
    let diff = changes.iter().filter_map(|change| change["diff"].as_str()).collect();
    (json!({"type":"fileChange","id":id,"changes":changes,"status":status}), diff)
}
