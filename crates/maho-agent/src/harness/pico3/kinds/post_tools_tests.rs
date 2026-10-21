use super::*;

fn call()->ToolCall {serde_json::from_value(serde_json::json!({"id":"c","name":"x","arguments":{}})).expect("call")}
fn task(outcome:serde_json::Value)->Task {serde_json::from_value(serde_json::json!({"id":5,"conversationId":1,"kind":"pi.tool","input":null,"status":"terminal","after":[],"owns":[],"outcome":outcome})).expect("task")}
fn controlled(control:ToolControl)->Row {Row {call:call(),entry:Some(6),control:Some(control),missing:None}}

#[test]
fn orphaned_task_requires_missing_result_synthesis() {
    let task=task(serde_json::json!({"status":"orphaned"}));
    let row=row(call(),&task).expect("row");
    assert!(matches!(row.missing,Some(Missing::Orphaned)));assert!(row.entry.is_none());
}
#[test]
fn aborted_task_with_durable_result_does_not_synthesize_duplicate() {
    let task=task(serde_json::json!({"status":"aborted","result":{"entry":8}}));
    let row=row(call(),&task).expect("row");
    assert_eq!(row.entry,Some(8));assert!(row.missing.is_none());assert!(row.control.is_none());
}
#[test]
fn aborted_task_without_result_synthesizes_aborted_not_orphaned() {
    let task=task(serde_json::json!({"status":"aborted","result":{}}));
    let row=row(call(),&task).expect("row");
    assert!(matches!(row.missing,Some(Missing::Aborted)));
}
#[test]
fn live_dependency_is_rejected_before_post_tools_hooks() {
    let mut task=task(serde_json::json!({"status":"orphaned"}));task.status=TaskStatus::Running;
    let result=row(call(),&task);
    assert!(result.is_err());
}
#[test]
fn controls_are_folded_in_assistant_source_order() {
    let rows=vec![controlled(ToolControl {terminate:Some(True),handoff:Some("first".into()),add_tools:Some(vec!["b".into(),"a".into()])}),controlled(ToolControl {terminate:None,handoff:Some("last".into()),add_tools:Some(vec!["c".into(),"b".into()])})];
    let mut selected=vec!["a".into()];
    let (terminate,handoff)=controls(&rows,&mut selected);
    assert!(terminate);assert_eq!(handoff.as_deref(),Some("last"));assert_eq!(selected,vec!["a","b","c"]);
}
#[test]
fn completed_control_is_preserved_with_result_entry() {
    let task=task(serde_json::json!({"status":"completed","result":{"entry":6,"control":{"addTools":["new"],"terminate":true}}}));
    let row=row(call(),&task).expect("row");
    assert_eq!(row.entry,Some(6));assert!(row.missing.is_none());assert!(row.control.expect("control").terminate.is_some());
}
