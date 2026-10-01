// Copyright (c) 2025 Mario Zechner; Copyright (c) 2025-2026 Can Bölük.
// Adapted from oh-my-pi's MIT-licensed todo tool via senpi.
use serde_json::Value;
use crate::todo_types::{DEFAULT_INIT_PHASE,TODO_STATE_ENTRY_TYPE,TodoItem,TodoPhase,TodoStatus};
fn parse_todo_item(value:&Value,lenient:bool)->Option<TodoItem> {
    let content=value.get("content")?.as_str()?.to_owned();
    let status=match value.get("status").and_then(Value::as_str) {
        Some("pending")=>TodoStatus::Pending, Some("in_progress")=>TodoStatus::InProgress,
        Some("completed")=>TodoStatus::Completed, Some("abandoned"|"cancelled")=>TodoStatus::Abandoned,
        _ if lenient=>TodoStatus::Pending, _=>return None,
    };
    Some(TodoItem{content,status})
}
fn parse_todo_phase(value:&Value)->Option<TodoPhase> {
    Some(TodoPhase{name:value.get("name")?.as_str()?.into(),tasks:value.get("tasks")?.as_array()?.iter().map(|t|parse_todo_item(t,false)).collect::<Option<Vec<_>>>()?})
}
pub fn read_todo_payload(value:&Value)->Option<Vec<TodoPhase>> {
    if !value.is_object() { return None; }
    if value.get("schema").and_then(Value::as_str)==Some("v2") || value.get("phases").is_some_and(Value::is_array) {
        return value.get("phases")?.as_array()?.iter().map(parse_todo_phase).collect();
    }
    Some(vec![TodoPhase{name:DEFAULT_INIT_PHASE.into(),tasks:value.get("todos")?.as_array()?.iter().map(|t|parse_todo_item(t,true)).collect::<Option<Vec<_>>>()?}])
}
pub fn is_todo_item(value:&Value)->bool { parse_todo_item(value,false).is_some() }
pub fn is_todo_item_array(value:&Value)->bool { value.as_array().is_some_and(|a|a.iter().all(is_todo_item)) }
pub fn is_todo_phase(value:&Value)->bool { parse_todo_phase(value).is_some() }
pub fn is_todo_phase_array(value:&Value)->bool { value.as_array().is_some_and(|a|a.iter().all(is_todo_phase)) }
pub fn get_latest_phases_from_branch_entries(entries:&[Value])->Vec<TodoPhase> {
    let mut phases=Vec::new();
    for entry in entries {
        if entry.get("type").and_then(Value::as_str)==Some("custom") && entry.get("customType").and_then(Value::as_str)==Some(TODO_STATE_ENTRY_TYPE) {
            if let Some(parsed)=entry.get("data").and_then(read_todo_payload) { phases=parsed; }
            continue;
        }
        if entry.get("type").and_then(Value::as_str)!=Some("message") { continue; }
        let Some(message)=entry.get("message") else { continue; };
        if message.get("role").and_then(Value::as_str)!=Some("toolResult") || !matches!(message.get("toolName").and_then(Value::as_str),Some("todo"|"todowrite")) { continue; }
        if let Some(parsed)=message.get("details").and_then(read_todo_payload) { phases=parsed; }
    }
    phases
}
pub fn get_latest_todos_from_branch_entries(entries:&[Value])->Vec<TodoItem> { get_latest_phases_from_branch_entries(entries).into_iter().flat_map(|p|p.tasks).collect() }
#[cfg(test)]
mod tests {
    use super::*; use serde_json::json;
    #[test] fn cancelled_is_abandoned() { assert_eq!(read_todo_payload(&json!({"schema":"v2","phases":[{"name":"Setup","tasks":[{"content":"x","status":"cancelled"}]}]})).unwrap()[0].tasks[0].status,TodoStatus::Abandoned); }
    #[test] fn legacy_unknown_status_survives() { assert_eq!(read_todo_payload(&json!({"todos":[{"content":"x","status":"blocked"}]})).unwrap()[0].tasks[0].status,TodoStatus::Pending); }
    #[test] fn malformed_v2_does_not_fallback() { assert!(read_todo_payload(&json!({"schema":"v2","todos":[{"content":"x"}]})).is_none()); }
    #[test] fn strict_unknown_status_rejected() { assert!(!is_todo_item(&json!({"content":"x","status":"blocked"}))); }
    #[test] fn malformed_last_entry_preserves_previous() { let entries=[json!({"type":"custom","customType":TODO_STATE_ENTRY_TYPE,"data":{"todos":[{"content":"x","status":"pending"}]}}),json!({"type":"custom","customType":TODO_STATE_ENTRY_TYPE,"data":{"schema":"v2","phases":[{"name":"Bad","tasks":[{}]}]}})]; assert_eq!(get_latest_todos_from_branch_entries(&entries)[0].content,"x"); }
    #[test] fn tool_result_can_clear_state() { let entries=[json!({"type":"custom","customType":TODO_STATE_ENTRY_TYPE,"data":{"todos":[{"content":"x"}]}}),json!({"type":"message","message":{"role":"toolResult","toolName":"todo","details":{"phases":[]}}})]; assert!(get_latest_phases_from_branch_entries(&entries).is_empty()); }
}
