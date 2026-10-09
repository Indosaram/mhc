use std::fs;
use ulw_loop_core::command_types::CreateUlwLoopOptions;
use ulw_loop_core::domain_types::{UlwLoopItem, UlwLoopPlan};
use ulw_loop_core::paths::{ulw_loop_dir, UlwLoopScope};
use ulw_loop_core::plan_commit::{commit, CommitMutation};
use ulw_loop_core::plan_crud::create_ulw_loop_plan;
use ulw_loop_core::plan_log::{assert_projection_complete, read_newest_record, reconcile_plan};

fn scope() -> UlwLoopScope { UlwLoopScope::new("legacy-projection") }
fn item(id: &str, status: &str) -> UlwLoopItem { UlwLoopItem { id:id.into(), title:id.into(), objective:id.into(), status:status.into(), success_criteria:vec![], attempt:0, created_at:"2026-09-16T01:00:00.000Z".into(), updated_at:"2026-09-16T01:00:00.000Z".into(), started_at:None, completed_at:None, failed_at:None, review_blocked_at:None, evidence:None, failure_reason:None, steering_status:None, superseded_by:None, supersedes:None, blocked_reason:None, blocker_signature:None, blocker_occurrence_count:None, required_external_decision:None, non_retriable:None, steering_evidence:None, steering_rationale:None } }
fn setup(root:&std::path::Path)->(UlwLoopPlan,std::path::PathBuf){let p=create_ulw_loop_plan(root,&CreateUlwLoopOptions{brief:"- alpha goal".into(),goals:None,codex_goal_mode:None,force:None,validation_batches_json:None},Some(&scope()),None).expect("plan");(p,ulw_loop_dir(root,Some(&scope())))}

#[test]
fn reconciles_legacy_goals_and_ledger_events() { let root=tempfile::tempdir().expect("tempdir"); let (mut p,dir)=setup(root.path()); p.goals.extend([item("G002-legacy-done","complete"),item("G003-legacy-active","in_progress"),item("G004-legacy-pending","pending")]); p.active_goal_id=Some("G003-legacy-active".into()); fs::write(dir.join("goals.json"),serde_json::to_vec(&p).expect("json")).expect("write"); let got=reconcile_plan(&dir).expect("reconciled"); assert_eq!(got.goals.len(),4); assert_eq!(got.active_goal_id.as_deref(),Some("G003-legacy-active")); }
#[test]
fn commit_preserves_all_four_goals_after_legacy_projection() { let root=tempfile::tempdir().expect("tempdir"); let (mut p,dir)=setup(root.path()); p.goals.extend([item("G002-legacy-done","complete"),item("G003-legacy-active","in_progress"),item("G004-legacy-pending","pending")]); let mut entries=vec![]; commit(root.path(),Some(&scope()),CommitMutation{plan:&mut p,entries:&mut entries}).expect("commit"); assert_eq!(read_newest_record(&dir).expect("record").plan.goals.len(),4); }
#[test]
fn checkpoint_like_commit_keeps_pending_legacy_goal() { let root=tempfile::tempdir().expect("tempdir"); let (mut p,_)=setup(root.path()); p.goals.extend([item("G003-legacy-active","complete"),item("G004-legacy-pending","pending")]); assert_eq!(p.goals.iter().find(|g|g.id=="G004-legacy-pending").map(|g|g.status.as_str()),Some("pending")); }
#[test]
fn truncated_projection_is_rejected_before_rewrite() { let root=tempfile::tempdir().expect("tempdir"); let (p,_)=setup(root.path()); let ledger=vec![ulw_loop_core::domain_types::UlwLoopLedgerEntry{kind:"goal_added".into(),goal_id:Some("G002-legacy-done".into()),..Default::default()}]; let err=assert_projection_complete(&p,&ledger).expect_err("truncated"); assert_eq!(err.code,"ULW_LOOP_PROJECTION_TRUNCATED"); assert!(err.message.contains("G002-legacy-done")); }
