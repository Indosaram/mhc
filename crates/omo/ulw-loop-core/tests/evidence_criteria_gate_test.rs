use ulw_loop_core::domain_types::*;
use ulw_loop_core::evidence::{require_all_criteria_pass, require_essential_criteria_pass};

const NOW: &str = "2026-05-23T00:00:00.000Z";

fn criterion(id: &str, user_model: &str, status: &str, essential: Option<bool>) -> UlwLoopSuccessCriterion {
    UlwLoopSuccessCriterion { id: id.into(), scenario: "happy path login returns 200".into(), user_model: user_model.into(), expected_evidence: "curl /login -d {valid} returns 200 + token".into(), captured_evidence: None, status: status.into(), essential, captured_at: None, notes: None, artifacts: None }
}

fn goal(criteria: Vec<UlwLoopSuccessCriterion>) -> UlwLoopItem {
    UlwLoopItem { id: "G001".into(), title: "Auth endpoint".into(), objective: "Build JWT auth".into(), status: "in_progress".into(), success_criteria: criteria, attempt: 1, created_at: NOW.into(), updated_at: NOW.into(), started_at: None, completed_at: None, failed_at: None, review_blocked_at: None, evidence: None, failure_reason: None, steering_status: None, superseded_by: None, supersedes: None, blocked_reason: None, blocker_signature: None, blocker_occurrence_count: None, required_external_decision: None, non_retriable: None, steering_evidence: None, steering_rationale: None }
}

#[test]
fn all_criteria_pass_does_not_throw() {
    assert!(require_all_criteria_pass(&goal(vec![criterion("C001", "happy", "pass", None), criterion("C002", "edge", "pass", None)])).is_ok());
}

#[test]
fn all_criteria_pending_throws() {
    assert!(require_all_criteria_pass(&goal(vec![criterion("C001", "happy", "pass", None), criterion("C002", "edge", "pending", None), criterion("C003", "regression", "pass", None)])).is_err());
}

#[test]
fn all_criteria_fail_or_blocked_throws() {
    assert!(require_all_criteria_pass(&goal(vec![criterion("C001", "happy", "fail", None)])).is_err());
    assert!(require_all_criteria_pass(&goal(vec![criterion("C001", "happy", "blocked", None)])).is_err());
}

#[test]
fn all_criteria_error_includes_goal_and_unresolved_details() {
    let error = require_all_criteria_pass(&goal(vec![criterion("C001", "happy", "pass", None), criterion("C002", "edge", "pending", None), criterion("C003", "regression", "pass", None)])).expect_err("unresolved");
    assert_eq!(error.code, "ulw_loop_criteria_not_all_pass");
    assert_eq!(error.details.as_ref().unwrap()["goalId"], "G001");
    assert!(error.details.as_ref().unwrap()["unresolved"].is_array());
}

#[test]
fn essential_pass_allows_nonessential_pending() {
    assert!(require_essential_criteria_pass(&goal(vec![criterion("C001", "happy", "pass", Some(true)), criterion("C002", "edge", "pending", Some(false))])).is_ok());
}

#[test]
fn essential_pending_throws() {
    assert!(require_essential_criteria_pass(&goal(vec![criterion("C001", "happy", "pending", Some(true)), criterion("C002", "edge", "pass", Some(false))])).is_err());
}
