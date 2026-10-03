use maho_codemode::tool::{detached_cell_contract::*, detached_cell_notification::*, interrupt_note::*, types::EvalLanguage};
use maho_ext_api::AgentToolResult;

fn snapshot(language: EvalLanguage, retained: Option<bool>, note: Option<&str>) -> EvalDetachedCellSnapshot {
    EvalDetachedCellSnapshot { cell_id: "cancelled".into(), language, started_at_ms: 0.0, state: EvalDetachedCellState::Cancelled, queued_behind: None, output_tail: String::new(), result: AgentToolResult::text("buffered tail"), state_retained: retained, interrupt_note: note.map(str::to_owned), hard_limit_seconds: None, run_budget_seconds: None }
}
#[tokio::test] async fn js_retained_state() { let cell = snapshot(EvalLanguage::Js, Some(true), None); assert!(build_detached_cell_notification(&cell, None).await.content.contains(&interruption_state_note(cell.language, cell.state_retained).unwrap())); }
#[tokio::test] async fn js_lost_state() { let cell = snapshot(EvalLanguage::Js, Some(false), None); assert!(build_detached_cell_notification(&cell, None).await.content.contains(&interruption_state_note(cell.language, cell.state_retained).unwrap())); }
#[tokio::test] async fn py_lost_state() { let cell = snapshot(EvalLanguage::Py, Some(false), None); assert!(build_detached_cell_notification(&cell, None).await.content.contains(&interruption_state_note(cell.language, cell.state_retained).unwrap())); }
#[tokio::test] async fn supplied_note_follows_state() { let cell = snapshot(EvalLanguage::Js, Some(false), Some("A synchronous call is blocking the old worker.\n")); assert!(build_detached_cell_notification(&cell, None).await.content.contains(&format!("{} A synchronous call is blocking the old worker.", interruption_state_note(cell.language, cell.state_retained).unwrap()))); }
#[tokio::test] async fn unknown_state_note() { let cell = snapshot(EvalLanguage::Js, None, None); assert!(build_detached_cell_notification(&cell, None).await.content.contains(&unknown_interruption_state_note(cell.language))); }
#[tokio::test] async fn overflow_spills_full_utf8_body_and_caps_tail() {
    let root = tempfile::tempdir().unwrap();
    let mut cell = snapshot(EvalLanguage::Js, Some(true), None);
    cell.result = AgentToolResult::text("한글".repeat(1000));
    let path = detached_notification_spill_path(Some(root.path()), "../cell").unwrap();
    assert_eq!(path.file_name().unwrap(), "detached-eval-___cell.log");
    let notification = build_detached_cell_notification(&cell, Some(&path)).await;
    assert!(notification.content.contains(&path.display().to_string()));
    assert!(tokio::fs::read_to_string(path).await.unwrap().contains(&"한글".repeat(1000)));
    let tail = notification.content.split("Buffered output tail:\n").nth(1).unwrap().split('\n').next().unwrap();
    assert!(tail.len() <= 512);
}
