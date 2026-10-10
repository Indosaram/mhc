#![allow(clippy::unwrap_used)]
use ulw_loop_core::checkpoint::{checkpoint_ulw_loop, CheckpointUlwLoopArgs};
use ulw_loop_core::domain_types::*;
use ulw_loop_core::plan_io::write_plan;

fn criterion(status: &str) -> UlwLoopSuccessCriterion { UlwLoopSuccessCriterion { id: "C001".into(), scenario: "scenario".into(), user_model: "happy".into(), expected_evidence: "evidence".into(), status: status.into(), essential: Some(true), captured_evidence: None, captured_at: None, notes: None, artifacts: None } }
fn goal(id: &str, status: &str, signature: Option<&str>, criteria: Vec<UlwLoopSuccessCriterion>) -> UlwLoopItem { UlwLoopItem { id: id.into(), title: id.into(), objective: "Objective".into(), status: status.into(), success_criteria: criteria, attempt: 1, created_at: "2026-05-23T00:00:00Z".into(), updated_at: "2026-05-23T00:00:00Z".into(), started_at: None, completed_at: None, failed_at: None, review_blocked_at: None, evidence: None, failure_reason: None, steering_status: None, superseded_by: None, supersedes: None, blocked_reason: None, blocker_signature: signature.map(str::to_string), blocker_occurrence_count: None, required_external_decision: None, non_retriable: None, steering_evidence: None, steering_rationale: None } }
fn plan(goals: Vec<UlwLoopItem>) -> UlwLoopPlan { UlwLoopPlan { version: 1, revision: Some(1), codex_goal_mode: Some("aggregate".into()), brief: Some("brief".into()), ledger_reset_revision: None, evidence_layout_version: Some(2), goals_path: ".omo/ulw-loop/goals.json".into(), ledger_path: ".omo/ulw-loop/ledger.jsonl".into(), brief_path: ".omo/ulw-loop/brief.md".into(), created_at: "2026-05-23T00:00:00Z".into(), updated_at: "2026-05-23T00:00:00Z".into(), active_goal_id: Some(goals.last().map(|g| g.id.clone()).unwrap_or_default()), goals, codex_objective: Some("aggregate objective".into()), codex_objective_aliases: None, acknowledged_driver_objectives: None, aggregate_completion: None, validation_batches: None } }
fn args(id: &str, status: &str, evidence: &str) -> CheckpointUlwLoopArgs { CheckpointUlwLoopArgs { goal_id: id.into(), status: status.into(), evidence: evidence.into(), codex_goal_json: None, quality_gate_json: None } }
fn setup(p: &UlwLoopPlan) -> tempfile::TempDir { let tmp = tempfile::tempdir().unwrap(); std::fs::create_dir_all(tmp.path().join(".omo/ulw-loop")).unwrap(); write_plan(tmp.path(), p.clone(), None).unwrap(); tmp }

#[test]
fn failed_sets_status_timestamp_and_ledger() -> Result<(), Box<dyn std::error::Error>> { let tmp=setup(&plan(vec![goal("G001","in_progress",None,vec![criterion("pending")])])); let r=checkpoint_ulw_loop(tmp.path(),&args("G001","failed","tests failed"),None,None)?; assert_eq!(r.goal.status,"failed"); assert!(r.goal.failed_at.is_some()); assert_eq!(r.ledger_entry.kind,"goal_failed"); Ok(()) }

#[test]
fn classifies_external_authorization_blocker_signature() -> Result<(), Box<dyn std::error::Error>> { let tmp=setup(&plan(vec![goal("G001","in_progress",None,vec![])])); let r=checkpoint_ulw_loop(tmp.path(),&args("G001","failed","ghcr.io returned 401 authentication required because token missing"),None,None)?; assert_eq!(r.goal.blocker_signature.as_deref(),Some("GHCR_PULL_ACCESS:HTTP_401_ANONYMOUS:GHCR_VISIBILITY_OR_CREDENTIAL_REQUIRED")); Ok(()) }

#[test]
fn third_same_signature_marks_user_decision_and_non_retriable() -> Result<(), Box<dyn std::error::Error>> { let sig="EXTERNAL_AUTHORIZATION_REQUIRED"; let tmp=setup(&plan(vec![goal("G001","failed",Some(sig),vec![]),goal("G002","blocked",Some(sig),vec![]),goal("G003","in_progress",None,vec![])])); let r=checkpoint_ulw_loop(tmp.path(),&args("G003","failed","Registry returned 401 because credentials are missing"),None,None)?; assert_eq!(r.goal.status,"needs_user_decision"); assert_eq!(r.goal.non_retriable,Some(true)); Ok(()) }

#[test]
fn failed_skips_criteria_gate() { let tmp=setup(&plan(vec![goal("G001","in_progress",None,vec![criterion("pending")])])); let r=checkpoint_ulw_loop(tmp.path(),&args("G001","failed","not done"),None,None); assert_eq!(r.unwrap().goal.status,"failed"); }

#[test]
fn blocked_preserves_blocker_fields_and_ledger() -> Result<(), Box<dyn std::error::Error>> { let tmp=setup(&plan(vec![goal("G001","in_progress",None,vec![])])); let r=checkpoint_ulw_loop(tmp.path(),&args("G001","blocked","ghcr.io requires token and credentials are missing"),None,None)?; assert_eq!(r.goal.status,"blocked"); assert!(r.goal.blocked_reason.as_deref().unwrap().contains("ghcr.io")); assert!(r.goal.blocker_signature.as_deref().unwrap().contains("GHCR_PULL_ACCESS")); assert_eq!(r.ledger_entry.kind,"goal_blocked"); Ok(()) }

#[test]
fn blocked_skips_criteria_gate() { let tmp=setup(&plan(vec![goal("G001","in_progress",None,vec![criterion("pending")])])); let r=checkpoint_ulw_loop(tmp.path(),&args("G001","blocked","waiting for approval"),None,None); assert_eq!(r.unwrap().goal.status,"blocked"); }


