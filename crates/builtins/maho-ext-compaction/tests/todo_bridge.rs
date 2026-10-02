use maho_ext_compaction::todo_bridge::*;
use serde_json::json;

#[test]
fn legacy_restore_preserves_current_tasks_or_returns_original_snapshot() {
    let snapshot = vec![json!({"text":"old","status":"blocked"})];
    let current = vec![json!({"text":"new","status":"completed"})];
    assert_eq!(restore_legacy_todos_if_missing(&snapshot, &current), (false, current));
    assert_eq!(restore_legacy_todos_if_missing(&snapshot, &[]), (true, snapshot));
    assert_eq!(restore_legacy_todos_if_missing(&[], &[]), (false, Vec::new()));
}

#[test]
fn snapshot_preserves_arbitrary_legacy_status_strings() {
    let todos = json!([{"id":"one","content":"task","status":"blocked"}]);
    assert_eq!(normalize_snapshot_items(&todos), Some(todos.as_array().unwrap().clone()));
    assert!(normalize_snapshot_items(&json!([{"content":"task","status":3}])).is_none());
}

#[test]
fn find_entries_filters_exact_branch_and_flattens_phases() {
    let entries = [json!({"type":"custom","customType":"todowrite.state","parentId":"a","data":{"todos":[{"content":"one","status":"blocked"}]}}), json!({"type":"custom","customType":"senpi.todo-state","parentId":"b","data":{"phases":[{"name":"Setup","tasks":[{"content":"two"}]}]}})];
    assert_eq!(find_todo_entries(&entries, Some("a")), vec![json!({"content":"one","status":"blocked"})]);
    assert_eq!(find_todo_entries(&entries, None).len(), 2);
}

#[test]
fn latest_state_migrates_unknown_status_and_maps_cancelled() {
    let entries = [json!({"type":"custom","customType":"senpi.todo-state","data":{"todos":[{"content":"task","status":"blocked"},{"content":"old","status":"cancelled"}]}})];
    assert_eq!(latest_phases(&entries), vec![json!({"name":"Tasks","tasks":[{"content":"task","status":"pending"},{"content":"old","status":"abandoned"}]})]);
}

#[test]
fn malformed_latest_payload_does_not_erase_valid_state() {
    let entries = [json!({"type":"custom","customType":"senpi.todo-state","data":{"schema":"v2","phases":[{"name":"Setup","tasks":[{"content":"task","status":"pending"}]}]}}), json!({"type":"message","message":{"role":"toolResult","toolName":"todo","details":{"phases":[{"name":"Invalid","tasks":[{"content":"bad","status":"blocked"}]}]}}})];
    assert_eq!(latest_phases(&entries)[0]["name"], "Setup");
}

#[test]
fn legacy_custom_envelopes_normalize_to_latest_list() {
    let entries = json!([{"type":"custom","id":"one","parentId":null,"timestamp":"date","customType":"todo-list","data":{"todos":[{"text":"legacy","status":"blocked"}]}}]);
    assert_eq!(normalize_snapshot_items(&entries), Some(vec![json!({"text":"legacy","status":"blocked"})]));
}
