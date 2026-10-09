use std::fs;
use ulw_loop_core::domain_types::*;
use ulw_loop_core::evidence::{record_evidence, RecordEvidenceArgs};
use ulw_loop_core::paths::UlwLoopScope;
use ulw_loop_core::plan_io::write_plan;

fn plan() -> UlwLoopPlan {
    let now = "2026-09-07T00:00:00.000Z".to_string();
    UlwLoopPlan { version: 1, revision: None, brief: None, ledger_reset_revision: None,
        evidence_layout_version: Some(2), created_at: now.clone(), updated_at: now,
        brief_path: ".omo/ulw-loop/s1/brief.md".into(), goals_path: ".omo/ulw-loop/s1/goals.json".into(),
        ledger_path: ".omo/ulw-loop/s1/ledger.jsonl".into(), codex_goal_mode: Some("aggregate".into()),
        codex_objective: None, codex_objective_aliases: None, acknowledged_driver_objectives: None,
        aggregate_completion: None, active_goal_id: Some("G001".into()), validation_batches: None,
        goals: vec![UlwLoopItem { id:"G001".into(), title:"Goal one".into(), objective:"Goal one".into(), status:"in_progress".into(), attempt:1,
            created_at:"2026-09-07T00:00:00.000Z".into(), updated_at:"2026-09-07T00:00:00.000Z".into(), started_at:None, completed_at:None, failed_at:None, review_blocked_at:None, evidence:None, failure_reason:None, steering_status:None, superseded_by:None, supersedes:None, blocked_reason:None, blocker_signature:None, blocker_occurrence_count:None, required_external_decision:None, non_retriable:None, steering_evidence:None, steering_rationale:None,
            success_criteria: (1..=3).map(|i| UlwLoopSuccessCriterion { id:format!("C{i:03}"), scenario:"scenario".into(), user_model:"happy".into(), expected_evidence:"evidence".into(), essential:Some(true), captured_evidence:None, status:"pending".into(), captured_at:None, notes:None, artifacts:None }).collect() }] }
}

#[test]
fn test_independent_writers_preserve_all_criteria() -> Result<(), Box<dyn std::error::Error>> {
    let tmp=tempfile::tempdir()?; let scope=UlwLoopScope::new("s1"); fs::create_dir_all(tmp.path().join(".omo/ulw-loop/s1"))?; write_plan(tmp.path(),plan(),Some(&scope))?;
    for i in 1..=3 { record_evidence(tmp.path(), &RecordEvidenceArgs{goal_id:"G001".into(),criterion_id:format!("C{i:03}"),status:"pass".into(),evidence:format!("proof C{i:03}"),notes:None,artifacts:None},Some(&scope))?; }
    let persisted=ulw_loop_core::plan_io::read_ulw_loop_plan(tmp.path(),Some(&scope))?; assert_eq!(persisted.goals[0].success_criteria.iter().map(|c|c.status.as_str()).collect::<Vec<_>>(),["pass","pass","pass"]); assert_eq!(ulw_loop_core::plan_io::read_ledger(tmp.path(),Some(&scope)).len(),3); Ok(())
}
