use maho_codemode::tool::{
    detached_cell_contract::*, detached_cell_notification::*, interrupt_note::*,
    types::EvalLanguage,
};
use maho_ext_api::AgentToolResult;

fn snapshot(
    language: EvalLanguage,
    retained: Option<bool>,
    note: Option<&str>,
) -> EvalDetachedCellSnapshot {
    EvalDetachedCellSnapshot {
        cell_id: "cancelled".into(),
        language,
        started_at_ms: 0.0,
        state: EvalDetachedCellState::Cancelled,
        queued_behind: None,
        output_tail: String::new(),
        result: AgentToolResult::text("buffered tail"),
        state_retained: retained,
        interrupt_note: note.map(str::to_owned),
        hard_limit_seconds: None,
        run_budget_seconds: None,
    }
}
#[tokio::test]
async fn js_retained_state() {
    let cell = snapshot(EvalLanguage::Js, Some(true), None);
    assert!(
        build_detached_cell_notification(&cell, None)
            .await
            .content
            .contains(&interruption_state_note(cell.language, cell.state_retained).unwrap())
    );
}
#[tokio::test]
async fn js_lost_state() {
    let cell = snapshot(EvalLanguage::Js, Some(false), None);
    assert!(
        build_detached_cell_notification(&cell, None)
            .await
            .content
            .contains(&interruption_state_note(cell.language, cell.state_retained).unwrap())
    );
}
#[tokio::test]
async fn supplied_note_follows_state() {
    let cell = snapshot(
        EvalLanguage::Js,
        Some(false),
        Some("A synchronous call is blocking the old worker.\n"),
    );
    assert!(
        build_detached_cell_notification(&cell, None)
            .await
            .content
            .contains(&format!(
                "{} A synchronous call is blocking the old worker.",
                interruption_state_note(cell.language, cell.state_retained).unwrap()
            ))
    );
}
#[tokio::test]
async fn unknown_state_note() {
    let cell = snapshot(EvalLanguage::Js, None, None);
    assert!(
        build_detached_cell_notification(&cell, None)
            .await
            .content
            .contains(&unknown_interruption_state_note(cell.language))
    );
}
#[tokio::test]
async fn overflow_spills_full_utf8_body_and_caps_tail() {
    let root = tempfile::tempdir().unwrap();
    let mut cell = snapshot(EvalLanguage::Js, Some(true), None);
    cell.result = AgentToolResult::text("한글".repeat(1000));
    let path = detached_notification_spill_path(Some(root.path()), "../cell").unwrap();
    assert_eq!(path.file_name().unwrap(), "detached-eval-___cell.log");
    let notification = build_detached_cell_notification(&cell, Some(&path)).await;
    assert!(notification.content.contains(&path.display().to_string()));
    assert!(
        tokio::fs::read_to_string(path)
            .await
            .unwrap()
            .contains(&"한글".repeat(1000))
    );
    let tail = notification
        .content
        .split("Buffered output tail:\n")
        .nth(1)
        .unwrap()
        .split('\n')
        .next()
        .unwrap();
    assert!(tail.len() <= 512);
}

/// senpi detached-cell-notification.test.ts: "Given a failed py cell whose kernel died ($kernelState)" — exercised on a js
/// cell (the only kernel maho-codemode ships); the note depends only on `details.kernelState`.
#[tokio::test]
async fn failed_cell_whose_kernel_died_does_not_claim_variables_survived() {
    let survived = "Kernel state updated - variables are available to the next eval cell.";
    for (kernel_state, says) in [
        ("lost", "every global is lost"),
        ("restarted", "globals from earlier cells are gone"),
        ("not-run", "never ran and changed no kernel state"),
    ] {
        let mut cell = snapshot(EvalLanguage::Js, None, None);
        cell.cell_id = format!("death-{kernel_state}");
        cell.state = EvalDetachedCellState::Failed;
        cell.result = AgentToolResult::text("JavaScript kernel died");
        cell.result.details = serde_json::json!({"language":"js","durationMs":0,"toolCalls":[],"truncated":false,"kernelState":kernel_state});
        let content = build_detached_cell_notification(&cell, None).await.content;
        assert!(content.contains(says), "{kernel_state}: {content}");
        assert!(!content.contains(survived), "{kernel_state}: {content}");
    }
}

/// senpi detached-cell-notification.test.ts: "Given a completed cell whose kernel is $name" (over its ceiling / just recycled).
#[tokio::test]
async fn completed_cell_over_ceiling_or_recycled_does_not_claim_old_globals_survive() {
    let survived = "Kernel state updated - variables are available to the next eval cell.";
    for (name, memory, says) in [
        (
            "over its ceiling",
            serde_json::json!({"liveBytes":1,"measure":"heap","overCeiling":true}),
            "Kernel memory is over its ceiling - the kernel restarts before the next eval cell and every global is lost.",
        ),
        (
            "just recycled",
            serde_json::json!({"liveBytes":1,"measure":"heap","recycled":true}),
            "The kernel was restarted before this cell ran - globals from earlier cells are gone; this cell's variables are available to the next eval cell.",
        ),
    ] {
        let mut cell = snapshot(EvalLanguage::Js, Some(true), None);
        cell.cell_id = "memory-js".into();
        cell.state = EvalDetachedCellState::Completed;
        cell.result = AgentToolResult::text("done");
        cell.result.details = serde_json::json!({"language":"js","durationMs":0,"toolCalls":[],"truncated":false,"memory":memory});
        let content = build_detached_cell_notification(&cell, None).await.content;
        assert!(content.contains(says), "{name}: {content}");
        assert!(!content.contains(survived), "{name}: {content}");
    }
}
