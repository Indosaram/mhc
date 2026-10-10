use std::fs;

use ulw_loop_core::command_types::CreateUlwLoopOptions;
use ulw_loop_core::domain_types::UlwLoopPlan;
use ulw_loop_core::paths::{ulw_loop_dir, UlwLoopScope};
use ulw_loop_core::plan_crud::create_ulw_loop_plan;
use ulw_loop_core::plan_log::{read_newest_record, read_records, reconcile_plan};

fn scope() -> UlwLoopScope { UlwLoopScope::new("newest-record") }
fn plan(root: &std::path::Path) -> UlwLoopPlan {
    create_ulw_loop_plan(root, &CreateUlwLoopOptions { brief: "initial".into(), goals: None, codex_goal_mode: None, force: None, validation_batches_json: None }, Some(&scope()), None).expect("plan")
}
fn seed(dir: &std::path::Path, name: &str, revision: u64, brief: &str, base: &UlwLoopPlan) -> UlwLoopPlan {
    let mut p = base.clone(); p.revision = Some(revision); p.brief = Some(brief.into());
    fs::create_dir_all(dir.join("revisions")).expect("revisions");
    let record = serde_json::json!({"version": 1, "revision": revision, "plan": p, "ledger": [{"at":"now","kind":"goal_added","revision":revision,"id":format!("{revision}-0")}]});
    fs::write(dir.join("revisions").join(name), serde_json::to_vec(&record).expect("record")).expect("write");
    serde_json::from_value(record["plan"].clone()).expect("plan")
}

#[test]
fn newest_valid_numeric_revision_wins() {
    let root = tempfile::tempdir().expect("tempdir"); let base = plan(root.path()); let dir = ulw_loop_dir(root.path(), Some(&scope()));
    seed(&dir, "00000009.json", 9, "nine", &base); let newest = seed(&dir, "000000010.json", 10, "ten", &base);
    assert_eq!(base.revision, Some(1)); assert_eq!(read_newest_record(&dir).expect("newest").revision, 10); assert_eq!(reconcile_plan(&dir).expect("plan").brief, newest.brief);
}

#[test]
fn corrupt_newest_falls_back_to_older_valid_record() {
    let root = tempfile::tempdir().expect("tempdir"); let base = plan(root.path()); let dir = ulw_loop_dir(root.path(), Some(&scope()));
    let older = seed(&dir, "00000009.json", 9, "nine", &base); fs::write(dir.join("revisions/000000010.json"), "{not json").expect("corrupt");
    assert_eq!(read_newest_record(&dir).expect("older").revision, 9); assert_eq!(reconcile_plan(&dir).expect("plan").brief, older.brief);
}

#[test]
fn older_unreadable_directory_does_not_hide_newest_record() {
    let root = tempfile::tempdir().expect("tempdir"); let base = plan(root.path()); let dir = ulw_loop_dir(root.path(), Some(&scope()));
    let newest = seed(&dir, "000000010.json", 10, "ten", &base); fs::remove_file(dir.join("revisions/00000001.json")).expect("remove"); fs::create_dir(dir.join("revisions/00000001.json")).expect("directory");
    let record = read_newest_record(&dir).expect("newest"); assert_eq!(record.revision, 10); assert_eq!(reconcile_plan(&dir).expect("plan").brief, newest.brief); assert!(!read_records(&dir).is_empty());
}

#[test]
fn newest_unreadable_directory_propagates_as_no_valid_record() {
    let root = tempfile::tempdir().expect("tempdir"); let base = plan(root.path()); let dir = ulw_loop_dir(root.path(), Some(&scope()));
    seed(&dir, "00000009.json", 9, "nine", &base); fs::create_dir(dir.join("revisions/000000010.json")).expect("directory");
    assert!(read_newest_record(&dir).is_none()); assert!(reconcile_plan(&dir).is_some());
}
