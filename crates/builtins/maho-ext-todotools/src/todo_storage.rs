// Copyright (c) 2025 Mario Zechner; Copyright (c) 2025-2026 Can Bölük.
// Adapted from oh-my-pi's MIT-licensed todo tool via senpi.
use serde_json::Value;
use std::time::{SystemTime,UNIX_EPOCH};
use crate::todo_types::{DEFAULT_INIT_PHASE,TODO_STATE_ENTRY_TYPE,TodoAsk,TodoItem,TodoPhase,TodoState,TodoStatus};
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
pub fn read_todo_state_payload(value:&Value)->Option<TodoState> {
    if !value.is_object() { return None; }
    if value.get("schema").and_then(Value::as_str)==Some("v2") || value.get("phases").is_some_and(Value::is_array) {
        let phases=value.get("phases")?.as_array()?.iter().map(parse_todo_phase).collect::<Option<Vec<_>>>()?;
        let ask=value.get("ask").filter(|ask|ask.get("entryId").and_then(Value::as_str).is_some()&&ask.get("text").and_then(Value::as_str).is_some()&&ask.get("capturedAt").and_then(Value::as_u64).is_some()).and_then(|ask|serde_json::from_value::<TodoAsk>(ask.clone()).ok());
        return Some(TodoState{phases,ask});
    }
    Some(TodoState{phases:vec![TodoPhase{name:DEFAULT_INIT_PHASE.into(),tasks:value.get("todos")?.as_array()?.iter().map(|t|parse_todo_item(t,true)).collect::<Option<Vec<_>>>()?}],ask:None})
}
pub fn read_todo_payload(value:&Value)->Option<Vec<TodoPhase>> { read_todo_state_payload(value).map(|state|state.phases) }
pub fn is_todo_item(value:&Value)->bool { parse_todo_item(value,false).is_some() }
pub fn is_todo_item_array(value:&Value)->bool { value.as_array().is_some_and(|a|a.iter().all(is_todo_item)) }
pub fn is_todo_phase(value:&Value)->bool { parse_todo_phase(value).is_some() }
pub fn is_todo_phase_array(value:&Value)->bool { value.as_array().is_some_and(|a|a.iter().all(is_todo_phase)) }
pub fn get_latest_phases_from_branch_entries(entries:&[Value])->Vec<TodoPhase> {
    get_latest_state_from_branch_entries(entries).phases
}
pub fn get_latest_state_from_branch_entries(entries:&[Value])->TodoState {
    let mut state=TodoState{phases:Vec::new(),ask:None};
    for entry in entries {
        if entry.get("type").and_then(Value::as_str)==Some("custom") && entry.get("customType").and_then(Value::as_str)==Some(TODO_STATE_ENTRY_TYPE) {
            if let Some(parsed)=entry.get("data").and_then(read_todo_state_payload) { state=parsed; }
            continue;
        }
        if entry.get("type").and_then(Value::as_str)!=Some("message") { continue; }
        let Some(message)=entry.get("message") else { continue; };
        if message.get("role").and_then(Value::as_str)!=Some("toolResult") || !matches!(message.get("toolName").and_then(Value::as_str),Some("todo"|"todowrite")) { continue; }
        if let Some(parsed)=message.get("details").and_then(read_todo_state_payload) { state=parsed; }
    }
    state
}
#[cfg(test)]
mod ask_state_tests {
    use super::*; use serde_json::json;
    #[test] fn latest_state_restores_and_replaces_ask_with_phases() { let entries=[json!({"type":"custom","customType":TODO_STATE_ENTRY_TYPE,"data":{"schema":"v2","phases":[],"ask":{"entryId":"m1","text":"Ship release","capturedAt":42}}}),json!({"type":"message","message":{"role":"toolResult","toolName":"todo","details":{"schema":"v2","phases":[{"name":"Build","tasks":[]}]}}})]; let state=get_latest_state_from_branch_entries(&entries); assert_eq!(state.ask,None); assert_eq!(state.phases[0].name,"Build"); }
    #[test] fn invalid_ask_is_ignored_without_dropping_phases() { let state=read_todo_state_payload(&json!({"schema":"v2","phases":[],"ask":{"text":"missing identity"}})).unwrap(); assert!(state.ask.is_none()); }
    #[test] fn captures_first_ask_until_a_state_exists_then_uses_latest(){let entries=[json!({"type":"message","id":"m1","message":{"role":"user","content":"First request"}}),json!({"type":"custom","customType":TODO_STATE_ENTRY_TYPE,"data":{"schema":"v2","phases":[]}}),json!({"type":"message","id":"m2","message":{"role":"user","content":"Redirect"}})];assert_eq!(capture_list_ask(&entries,None).unwrap().text,"Redirect");assert_eq!(capture_list_ask(&entries[..1],None).unwrap().text,"First request");}
    #[test] fn skips_answer_frames_and_restores_snapshot_ask(){let ask=json!({"entryId":"old","text":"Original","capturedAt":1});let entries=[json!({"type":"message","id":"m1","message":{"role":"user","content":"<answer>\nnot a request\n</answer>"}}),json!({"type":"custom_message","customType":"compaction.todo-restore-request","details":{"ask":ask}})];assert_eq!(capture_list_ask(&entries,None).unwrap().text,"Original");}
}
pub fn get_latest_todos_from_branch_entries(entries:&[Value])->Vec<TodoItem> { get_latest_phases_from_branch_entries(entries).into_iter().flat_map(|p|p.tasks).collect() }
pub fn capture_list_ask(entries:&[Value],_previous:Option<&TodoAsk>)->Option<TodoAsk>{
    let last_state=entries.iter().rposition(|entry|entry.get("type").and_then(Value::as_str)==Some("custom")&&entry.get("customType").and_then(Value::as_str)==Some(TODO_STATE_ENTRY_TYPE));
    let restore=entries.iter().enumerate().rev().find(|(_,entry)|entry.get("type").and_then(Value::as_str)==Some("custom_message")&&entry.get("customType").and_then(Value::as_str)==Some("compaction.todo-restore-request")).and_then(|(index,entry)|entry.pointer("/details/ask").and_then(|ask|serde_json::from_value::<TodoAsk>(ask.clone()).ok()).map(|ask|(index,ask)));
    let requests=entries.iter().enumerate().filter(|(_,entry)|entry.get("type").and_then(Value::as_str)==Some("message")&&entry.pointer("/message/role").and_then(Value::as_str)==Some("user")).filter_map(|(index,entry)|{
        let message=entry.get("message")?;let content=message.get("content")?;let raw=if let Some(text)=content.as_str(){text}else{content.as_array()?.iter().find(|block|block.get("type").and_then(Value::as_str)==Some("text"))?.get("text")?.as_str()?};
        let answer_frame=raw.lines().any(|line|line.trim()=="<answer>")&&raw.lines().any(|line|line.trim()=="</answer>");if answer_frame{return None;}
        let text=crate::todo_format::sanitize_todo_text(raw);(!text.is_empty()).then(||(index,entry.get("id").and_then(Value::as_str).unwrap_or_default().to_owned(),text))
    }).collect::<Vec<_>>();
    if let Some((_,ask))=restore.filter(|(index,_)|Some(*index)>last_state&&requests.last().is_none_or(|request|request.0<*index)){return Some(ask);}
    let selected=if last_state.is_none(){requests.first()}else{requests.last()}?;
    if last_state.is_some()&&selected.0<=last_state.unwrap_or_default(){return None;}
    let chars=selected.2.chars().collect::<Vec<_>>();let text=if chars.len()>200{format!("{}… (+{} chars)",chars[..200].iter().collect::<String>(),chars.len()-200)}else{selected.2.clone()};
    let captured_at=SystemTime::now().duration_since(UNIX_EPOCH).map(|elapsed|elapsed.as_millis().min(u128::from(u64::MAX)) as u64).unwrap_or_default();
    Some(TodoAsk{entry_id:selected.1.clone(),text,captured_at})
}
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
