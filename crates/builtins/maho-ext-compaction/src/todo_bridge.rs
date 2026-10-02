use maho_ext_api::{CustomMessage, DeliverAs, ExtensionApi, ExtensionContext, ExtensionFailure, SendMessageOptions, ToolContent};
use serde_json::{Value, json};

pub const TODO_SNAPSHOT_CUSTOM_TYPE: &str = "compaction.todo-snapshot";
pub const TODO_SNAPSHOT_SCHEMA: &str = "senpi.compaction.todo-snapshot.v1";

fn is_todo_entry(value: &Value) -> bool {
    (value.get("id").is_some_and(Value::is_string) || value.get("content").is_some_and(Value::is_string) || value.get("text").is_some_and(Value::is_string))
        && value.get("status").is_none_or(Value::is_string)
}
fn is_todo_phase(value: &Value) -> bool {
    value.get("name").is_some_and(Value::is_string) && value.get("tasks").and_then(Value::as_array).is_some_and(|tasks| tasks.iter().all(is_todo_entry))
}
fn read_todos(entry: &Value) -> Vec<Value> {
    let data = &entry["data"];
    if let Some(todos) = data.get("todos").and_then(Value::as_array) { return todos.iter().filter(|todo| is_todo_entry(todo)).cloned().collect(); }
    data.get("phases").and_then(Value::as_array).into_iter().flatten().filter(|phase| is_todo_phase(phase))
        .flat_map(|phase| phase["tasks"].as_array().into_iter().flatten().cloned()).collect()
}
fn is_todo_custom(entry: &Value) -> bool {
    entry.get("type").and_then(Value::as_str) == Some("custom")
        && entry.get("customType").and_then(Value::as_str).is_some_and(|kind| kind.starts_with("todowrite") || matches!(kind, "senpi.todo-state" | "todo-list"))
}

pub fn find_todo_entries(entries: &[Value], branch_id: Option<&str>) -> Vec<Value> {
    entries.iter().filter(|entry| is_todo_custom(entry))
        .filter(|entry| branch_id.is_none_or(|branch| entry.get("parentId").and_then(Value::as_str) == Some(branch)))
        .flat_map(read_todos).collect()
}

pub fn normalize_snapshot_items(value: &Value) -> Option<Vec<Value>> {
    let values = value.as_array()?;
    if values.iter().all(is_todo_phase) { return Some(values.clone()); }
    if !values.iter().all(|entry| entry.get("type").and_then(Value::as_str) == Some("custom")
        && entry.get("id").is_some_and(Value::is_string)
        && entry.get("parentId").is_some_and(|parent| parent.is_null() || parent.is_string())
        && entry.get("timestamp").is_some_and(Value::is_string)
        && entry.get("customType").is_some_and(Value::is_string)) {
        return values.iter().all(is_todo_entry).then(|| values.clone());
    }
    let latest = values.iter().rposition(is_todo_custom);
    Some(latest.map_or_else(Vec::new, |index| {
        if values[index]["customType"] == "senpi.todo-state" { latest_phases(&values[..=index]) } else { read_todos(&values[index]) }
    }))
}

fn parse_payload(data: &Value) -> Option<Vec<Value>> {
    if data.get("schema").and_then(Value::as_str) == Some("v2") || data.get("phases").is_some_and(Value::is_array) {
        let phases = data.get("phases")?.as_array()?;
        let mut parsed = Vec::new();
        for phase in phases {
            let name = phase.get("name")?.as_str()?;
            let mut tasks = Vec::new();
            for task in phase.get("tasks")?.as_array()? {
                let content = task.get("content")?.as_str()?;
                let status = match task.get("status")?.as_str()? {
                    "cancelled" => "abandoned", status @ ("pending" | "in_progress" | "completed" | "abandoned") => status, _ => return None,
                };
                tasks.push(json!({"content":content,"status":status}));
            }
            parsed.push(json!({"name":name,"tasks":tasks}));
        }
        return Some(parsed);
    }
    let mut tasks = Vec::new();
    for task in data.get("todos")?.as_array()? {
        let content = task.get("content")?.as_str()?;
        let status = match task.get("status").and_then(Value::as_str) {
            Some("cancelled") => "abandoned", Some(status @ ("pending" | "in_progress" | "completed" | "abandoned")) => status, _ => "pending",
        };
        tasks.push(json!({"content":content,"status":status}));
    }
    Some(vec![json!({"name":"Tasks","tasks":tasks})])
}

pub fn latest_phases(entries: &[Value]) -> Vec<Value> {
    let mut phases = Vec::new();
    for entry in entries {
        let data = if entry.get("type").and_then(Value::as_str) == Some("custom") && entry.get("customType").and_then(Value::as_str) == Some("senpi.todo-state") {
            Some(&entry["data"])
        } else if entry.get("type").and_then(Value::as_str) == Some("message") && entry["message"]["role"] == "toolResult"
            && matches!(entry["message"]["toolName"].as_str(), Some("todo" | "todowrite")) { Some(&entry["message"]["details"]) } else { None };
        if let Some(parsed) = data.and_then(parse_payload) { phases = parsed; }
    }
    phases
}

pub fn create_todo_snapshot(context: &ExtensionContext) -> Value {
    json!({"schema":TODO_SNAPSHOT_SCHEMA,"todos":latest_phases(&crate::speculative::branch_values(context)),"capturedAt":chrono::Utc::now().timestamp_millis()})
}
pub fn capture_todo_snapshot(api: &ExtensionApi, context: &ExtensionContext) -> Result<(), ExtensionFailure> {
    api.append_entry(TODO_SNAPSHOT_CUSTOM_TYPE, Some(create_todo_snapshot(context)))
}
pub fn restore_todos_if_missing(api: &ExtensionApi, context: &ExtensionContext) -> Result<(), ExtensionFailure> {
    let entries = crate::speculative::branch_values(context);
    if !latest_phases(&entries).is_empty() { return Ok(()); }
    for entry in entries.iter().rev() {
        if entry.get("type").and_then(Value::as_str) != Some("custom") || entry.get("customType").and_then(Value::as_str) != Some(TODO_SNAPSHOT_CUSTOM_TYPE) { continue; }
        let data = &entry["data"];
        if data["schema"] != TODO_SNAPSHOT_SCHEMA { continue; }
        let Some(todos) = normalize_snapshot_items(&data["todos"]) else { continue; };
        if todos.is_empty() { return Ok(()); }
        let details = json!({"schema":TODO_SNAPSHOT_SCHEMA,"todos":todos,"capturedAt":data.get("capturedAt").filter(|value| value.is_number()).cloned().unwrap_or(json!(0))});
        return api.send_message(CustomMessage { custom_type: "compaction.todo-restore-request".into(), content: vec![ToolContent::text(format!("Restore missing todo tasks from snapshot: {}", details["todos"]))], display: false, details: Some(details) }, SendMessageOptions { trigger_turn: true, deliver_as: Some(DeliverAs::NextTurn) });
    }
    Ok(())
}
