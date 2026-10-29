use std::fs;

use ulw_loop_core::command_types::CreateUlwLoopOptions;
use ulw_loop_core::paths::UlwLoopScope;
use ulw_loop_core::plan_crud::create_ulw_loop_plan;
use ulw_loop_core::plan_io::{read_ledger, read_ulw_loop_plan, with_ulw_loop_mutation_lock};
use ulw_loop_core::plan_log::{log_names, plan_exists, read_newest_record, read_records};

fn scope() -> UlwLoopScope { UlwLoopScope::new("commit-log") }
fn create(root: &std::path::Path) -> ulw_loop_core::domain_types::UlwLoopPlan {
    create_ulw_loop_plan(root, &CreateUlwLoopOptions { brief: "- goal alpha".into(), goals: None, codex_goal_mode: None, force: None, validation_batches_json: None }, Some(&scope()), None).expect("create plan")
}

#[test]
fn immutable_plan_and_audit_log_reconcile_without_writes() {
    let root = tempfile::tempdir().expect("tempdir");
    let next = create(root.path());
    let dir = ulw_loop_core::paths::ulw_loop_dir(root.path(), Some(&scope()));
    let before = fs::read_to_string(dir.join("goals.json")).expect("goals");
    assert_eq!(read_ulw_loop_plan(root.path(), Some(&scope())).expect("read").revision, next.revision);
    assert_eq!(fs::read_to_string(dir.join("goals.json")).expect("goals"), before);
    with_ulw_loop_mutation_lock(root.path(), Some(&scope()), None, || read_ulw_loop_plan(root.path(), Some(&scope()))).expect("locked read");
}

#[test]
fn first_creation_reads_from_revision_log() {
    let root = tempfile::tempdir().expect("tempdir");
    create(root.path());
    let dir = ulw_loop_core::paths::ulw_loop_dir(root.path(), Some(&scope()));
    fs::remove_file(dir.join("goals.json")).expect("remove cache");
    assert!(plan_exists(root.path(), Some(&scope())));
    assert!(read_ulw_loop_plan(root.path(), Some(&scope())).is_ok());
}

#[test]
fn revision_log_preserves_legacy_views() {
    let root = tempfile::tempdir().expect("tempdir");
    create(root.path());
    let dir = ulw_loop_core::paths::ulw_loop_dir(root.path(), Some(&scope()));
    assert_eq!(log_names(&dir), vec!["00000001.json"]);
    assert!(read_newest_record(&dir).is_some());
    assert_eq!(read_records(&dir).len(), 1);
}

#[test]
fn force_recreation_creates_new_revision_and_filtered_views() {
    let root = tempfile::tempdir().expect("tempdir");
    create(root.path());
    let next = create_ulw_loop_plan(root.path(), &CreateUlwLoopOptions { brief: "- goal new".into(), goals: None, codex_goal_mode: None, force: Some(true), validation_batches_json: None }, Some(&scope()), None).expect("force create");
    assert_eq!(next.revision, Some(2));
    assert_eq!(read_ledger(root.path(), Some(&scope())).len(), 1);
    assert_eq!(log_names(ulw_loop_core::paths::ulw_loop_dir(root.path(), Some(&scope()))).len(), 2);
}

#[test]
fn read_ledger_returns_published_entries_only() {
    let root = tempfile::tempdir().expect("tempdir");
    create(root.path());
    assert_eq!(read_ledger(root.path(), Some(&scope())).len(), 1);
    assert_eq!(read_ledger(root.path(), Some(&scope()))[0].kind, "plan_created");
}

#[test]
fn log_names_ignores_non_revision_files() {
    let root = tempfile::tempdir().expect("tempdir");
    create(root.path());
    let dir = ulw_loop_core::paths::ulw_loop_dir(root.path(), Some(&scope()));
    fs::write(dir.join("revisions").join("not-a-revision.json"), "{}").expect("write junk");
    assert_eq!(log_names(&dir), vec!["00000001.json"]);
}
