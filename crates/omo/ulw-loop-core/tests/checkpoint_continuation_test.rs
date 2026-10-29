use ulw_loop_core::checkpoint::CheckpointUlwLoopArgs;
use ulw_loop_core::checkpoint_continuation::{checkpoint_and_continue, ContinuationNext};
use ulw_loop_core::domain_types::*;
use ulw_loop_core::plan_io::{read_ulw_loop_plan, write_plan};

fn criterion(id: &str, status: &str) -> UlwLoopSuccessCriterion {
    UlwLoopSuccessCriterion {
        id: id.into(), scenario: "scenario".into(), user_model: "happy".into(),
        expected_evidence: "evidence".into(), status: status.into(), essential: Some(true),
        captured_evidence: None, captured_at: None, notes: None, artifacts: None,
    }
}

fn goal(id: &str, status: &str, attempt: u32) -> UlwLoopItem {
    UlwLoopItem {
        id: id.into(), title: id.into(), objective: format!("Objective {id}"), status: status.into(),
        success_criteria: vec![criterion("C001", "pass")], attempt, created_at: "2026-05-23T00:00:00Z".into(),
        updated_at: "2026-05-23T00:00:00Z".into(), started_at: None, completed_at: None, failed_at: None,
        review_blocked_at: None, evidence: None, failure_reason: None, steering_status: None,
        superseded_by: None, supersedes: None, blocked_reason: None, blocker_signature: None,
        blocker_occurrence_count: None, required_external_decision: None, non_retriable: None,
        steering_evidence: None, steering_rationale: None,
    }
}

fn plan(goals: Vec<UlwLoopItem>) -> UlwLoopPlan {
    UlwLoopPlan {
        version: 1, revision: Some(1), codex_goal_mode: Some("aggregate".into()), brief: Some("brief".into()),
        ledger_reset_revision: None, evidence_layout_version: Some(2), goals_path: ".omo/ulw-loop/goals.json".into(),
        ledger_path: ".omo/ulw-loop/ledger.jsonl".into(), brief_path: ".omo/ulw-loop/brief.md".into(),
        created_at: "2026-05-23T00:00:00Z".into(), updated_at: "2026-05-23T00:00:00Z".into(),
        active_goal_id: Some("G001".into()), goals, codex_objective: Some("aggregate objective".into()),
        codex_objective_aliases: None, acknowledged_driver_objectives: None, aggregate_completion: None,
        validation_batches: None,
    }
}

fn snapshot(status: &str) -> String {
    serde_json::json!({"goal":{"objective":"aggregate objective","status":status}}).to_string()
}

fn args(status: &str) -> CheckpointUlwLoopArgs {
    CheckpointUlwLoopArgs { goal_id: "G001".into(), status: status.into(),
        evidence: "implementation done and validation passed".into(), codex_goal_json: Some(snapshot("active")), quality_gate_json: None }
}

#[test]
fn completed_non_final_goal_with_advance_starts_next_pending_goal() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    std::fs::create_dir_all(tmp.path().join(".omo/ulw-loop"))?;
    write_plan(tmp.path(), plan(vec![goal("G001", "in_progress", 1), goal("G002", "pending", 0)]), None)?;
    let result = checkpoint_and_continue(tmp.path(), &args("complete"), true, None)?;
    match result.next { Some(ContinuationNext::Resumed { resumed, goal, .. }) => { assert!(!resumed); assert_eq!(goal.id, "G002"); assert_eq!(goal.status, "in_progress"); }, _ => panic!("expected resumed next goal") }
    assert_eq!(read_ulw_loop_plan(tmp.path(), None)?.active_goal_id.as_deref(), Some("G002"));
    Ok(())
}

#[test]
fn advance_disabled_preserves_old_two_call_state() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    std::fs::create_dir_all(tmp.path().join(".omo/ulw-loop"))?;
    write_plan(tmp.path(), plan(vec![goal("G001", "in_progress", 1), goal("G002", "pending", 0)]), None)?;
    let result = checkpoint_and_continue(tmp.path(), &args("complete"), false, None)?;
    assert!(result.next.is_none());
    assert!(read_ulw_loop_plan(tmp.path(), None)?.active_goal_id.is_none());
    Ok(())
}

#[test]
fn failed_checkpoint_does_not_start_next_goal() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    std::fs::create_dir_all(tmp.path().join(".omo/ulw-loop"))?;
    write_plan(tmp.path(), plan(vec![goal("G001", "in_progress", 1), goal("G002", "pending", 0)]), None)?;
    let result = checkpoint_and_continue(tmp.path(), &args("failed"), true, None)?;
    assert!(result.next.is_none());
    assert!(read_ulw_loop_plan(tmp.path(), None)?.active_goal_id.is_none());
    Ok(())
}
