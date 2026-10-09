use std::fs;

use ulw_loop_core::command_types::CreateUlwLoopOptions;
use ulw_loop_core::paths::{ulw_loop_dir, UlwLoopScope};
use ulw_loop_core::plan_commit::{commit, CommitMutation};
use ulw_loop_core::plan_crud::create_ulw_loop_plan;
use ulw_loop_core::plan_io::{plan_exists, read_ledger, read_ulw_loop_plan, with_ulw_loop_mutation_lock};

fn make(root: &std::path::Path) -> (UlwLoopScope, ulw_loop_core::domain_types::UlwLoopPlan) {
    let scope = UlwLoopScope::new("commit-log");
    let plan = create_ulw_loop_plan(root, &CreateUlwLoopOptions { brief: "- unfinished".into(), goals: None, codex_goal_mode: None, force: None, validation_batches_json: None }, Some(&scope), None).expect("create");
    (scope, plan)
}

#[test]
fn revision_one_alone_drives_existence_and_reads() {
    let root = tempfile::tempdir().expect("tempdir");
    let (scope, plan) = make(root.path());
    let dir = ulw_loop_dir(root.path(), Some(&scope));
    for name in ["goals.json", "ledger.jsonl", "brief.md"] { fs::remove_file(dir.join(name)).expect("remove view"); }
    assert!(plan_exists(root.path(), Some(&scope)));
    assert_eq!(read_ulw_loop_plan(root.path(), Some(&scope)).expect("read").revision, plan.revision);
}

#[test]
fn incomplete_public_view_is_repaired_from_revision() {
    let root = tempfile::tempdir().expect("tempdir");
    let (scope, mut plan) = make(root.path());
    let dir = ulw_loop_dir(root.path(), Some(&scope));
    fs::write(dir.join("ledger.jsonl"), "partial").expect("partial ledger");
    let mut entries = Vec::new();
    commit(root.path(), Some(&scope), CommitMutation { plan: &mut plan, entries: &mut entries }).expect("commit");
    assert!(serde_json::from_str::<serde_json::Value>(&fs::read_to_string(dir.join("goals.json")).expect("goals")).is_ok());
    assert!(read_ledger(root.path(), Some(&scope)).iter().all(|entry| !entry.kind.is_empty()));
}

#[test]
fn locked_ledger_read_keeps_revision_count_stable() {
    let root = tempfile::tempdir().expect("tempdir");
    let (scope, _) = make(root.path());
    let dir = ulw_loop_dir(root.path(), Some(&scope));
    with_ulw_loop_mutation_lock(root.path(), Some(&scope), None, || {
        assert_eq!(read_ledger(root.path(), Some(&scope)).len(), 1);
        Ok(())
    }).expect("locked read");
    assert_eq!(fs::read_dir(dir.join("revisions")).expect("revisions").count(), 1);
}
