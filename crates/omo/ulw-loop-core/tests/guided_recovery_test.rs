use std::fs;
use ulw_loop_core::plan_io::read_ulw_loop_plan;
use ulw_loop_core::paths::ulw_loop_dir;
use ulw_loop_core::checkpoint::{checkpoint_ulw_loop, CheckpointUlwLoopArgs};
use ulw_loop_core::domain_types::*;
use ulw_loop_core::plan_io::write_plan;

fn goal(id: &str, status: &str) -> UlwLoopItem {
    UlwLoopItem { id: id.into(), title: "Title".into(), objective: "Objective".into(), status: status.into(), success_criteria: vec![UlwLoopSuccessCriterion { id: "C001".into(), scenario: "scenario".into(), user_model: "happy".into(), expected_evidence: "evidence".into(), status: "pass".into(), essential: Some(true), captured_evidence: None, captured_at: None, notes: None, artifacts: None }], attempt: 1, created_at: "2026-05-23T00:00:00Z".into(), updated_at: "2026-05-23T00:00:00Z".into(), started_at: None, completed_at: None, failed_at: None, review_blocked_at: None, evidence: None, failure_reason: None, steering_status: None, superseded_by: None, supersedes: None, blocked_reason: None, blocker_signature: None, blocker_occurrence_count: None, required_external_decision: None, non_retriable: None, steering_evidence: None, steering_rationale: None }
}

fn plan() -> UlwLoopPlan {
    UlwLoopPlan { version: 1, revision: Some(1), codex_goal_mode: Some("aggregate".into()), brief: Some("brief".into()), ledger_reset_revision: None, evidence_layout_version: Some(2), goals_path: ".omo/ulw-loop/goals.json".into(), ledger_path: ".omo/ulw-loop/ledger.jsonl".into(), brief_path: ".omo/ulw-loop/brief.md".into(), created_at: "2026-05-23T00:00:00Z".into(), updated_at: "2026-05-23T00:00:00Z".into(), active_goal_id: Some("G001".into()), goals: vec![goal("G001", "in_progress"), goal("G002", "pending")], codex_objective: Some("aggregate objective".into()), codex_objective_aliases: None, acknowledged_driver_objectives: None, aggregate_completion: None, validation_batches: None }
}

#[test]
fn test_missing_plan_names_create_goals_command() {
    let tmp = tempfile::tempdir().unwrap();
    let err = read_ulw_loop_plan(tmp.path(), None).unwrap_err();
    assert_eq!(err.code, "ULW_LOOP_PLAN_MISSING");
    assert!(err.message.contains("ulw-loop create-goals --brief \"<brief>\" --json"));
}

#[test]
fn test_missing_plan_read_creates_no_directories() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(read_ulw_loop_plan(tmp.path(), None).is_err());
    assert!(!tmp.path().join(".omo").exists());
}

#[test]
fn test_missing_plan_lists_existing_session_ids() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir_all(ulw_loop_dir(tmp.path(), None).join("session-alpha")).unwrap();
    fs::create_dir_all(ulw_loop_dir(tmp.path(), None).join("session-beta")).unwrap();
    let err = read_ulw_loop_plan(tmp.path(), None).unwrap_err();
    assert!(err.message.contains("session-alpha"));
    assert!(err.message.contains("session-beta"));
    assert_eq!(err.details.unwrap()["existingSessionIds"], serde_json::json!(["session-alpha", "session-beta"]));
}

#[test]
fn test_missing_plan_without_sessions_has_no_session_details() {
    let tmp = tempfile::tempdir().unwrap();
    let err = read_ulw_loop_plan(tmp.path(), None).unwrap_err();
    assert!(!err.details.unwrap().contains_key("existingSessionIds"));
}

#[test]
fn test_checkpoint_reconciles_different_driver_objective_with_advice() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir_all(tmp.path().join(".omo/ulw-loop")).unwrap();
    write_plan(tmp.path(), plan(), None).unwrap();
    let result = checkpoint_ulw_loop(tmp.path(), &CheckpointUlwLoopArgs { goal_id: "G001".into(), status: "complete".into(), evidence: "work complete".into(), codex_goal_json: Some(r#"{"goal":{"objective":"wrong objective","status":"budget_limited"}}"#.into()), quality_gate_json: None }, None, None).unwrap();
    assert!(result.next_actions.join(" ").contains("/goal resume"));
    assert!(result.warnings.join(" ").contains("driver_objective_differs"));
}

