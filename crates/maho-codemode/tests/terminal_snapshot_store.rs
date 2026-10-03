use maho_codemode::tool::{detached_cell_contract::*, terminal_snapshot_store::*, types::EvalLanguage};
use maho_ext_api::AgentToolResult;

fn snapshot(id: &str) -> EvalDetachedCellSnapshot {
    EvalDetachedCellSnapshot { cell_id: id.into(), language: EvalLanguage::Js, started_at_ms: 0.0, state: EvalDetachedCellState::Completed, queued_behind: None, output_tail: String::new(), result: AgentToolResult::text(id), state_retained: None, interrupt_note: None, hard_limit_seconds: None, run_budget_seconds: None }
}

#[test]
fn cap_evicts_oldest_and_get_does_not_refresh() {
    let mut store = TerminalSnapshotStore::new(2);
    store.remember(snapshot("a"));
    store.remember(snapshot("b"));
    assert!(store.get("a").is_some());
    store.remember(snapshot("c"));
    assert!(store.get("a").is_none());
    assert_eq!(store.list().iter().map(|snapshot| snapshot.cell_id.as_str()).collect::<Vec<_>>(), ["b", "c"]);
}

#[test]
fn replacement_refreshes_and_deletion_clear_release() {
    let mut store = TerminalSnapshotStore::new(2);
    store.remember(snapshot("a"));
    store.remember(snapshot("b"));
    store.remember(snapshot("a"));
    assert_eq!(store.list().iter().map(|snapshot| snapshot.cell_id.as_str()).collect::<Vec<_>>(), ["b", "a"]);
    store.delete("b");
    assert_eq!(store.list().len(), 1);
    store.clear();
    assert!(store.list().is_empty());
}

#[test]
fn zero_capacity_retains_nothing() {
    let mut store = TerminalSnapshotStore::new(0);
    store.remember(snapshot("a"));
    assert!(store.get("a").is_none());
}
