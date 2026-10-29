use ulw_loop_core::domain_types::{UlwLoopItem, UlwLoopPlan, UlwLoopSuccessCriterion};
use ulw_loop_core::status_next_actions::status_next_actions;
use ulw_loop_core::surface::UlwLoopToolkitSurface;

fn criterion(id: &str, status: &str) -> UlwLoopSuccessCriterion {
    UlwLoopSuccessCriterion { id: id.into(), scenario: "scenario".into(), user_model: "happy".into(), expected_evidence: "proof".into(), essential: None, captured_evidence: (status == "pass").then(|| "proof".into()), status: status.into(), captured_at: None, notes: None, artifacts: None }
}
fn goal(status: &str, criteria: Vec<UlwLoopSuccessCriterion>) -> UlwLoopItem {
    UlwLoopItem { id: "G001".into(), title: "Build auth".into(), objective: "Implement auth".into(), status: status.into(), success_criteria: criteria, attempt: 1, created_at: "2026-05-23T00:00:00.000Z".into(), updated_at: "2026-05-23T00:00:00.000Z".into(), started_at: None, completed_at: None, failed_at: None, review_blocked_at: None, evidence: None, failure_reason: None, steering_status: None, superseded_by: None, supersedes: None, blocked_reason: None, blocker_signature: None, blocker_occurrence_count: None, required_external_decision: None, non_retriable: None, steering_evidence: None, steering_rationale: None }
}
fn plan(goals: Vec<UlwLoopItem>, active: Option<&str>, layout: Option<u32>) -> UlwLoopPlan {
    UlwLoopPlan { version: 1, revision: None, brief: None, ledger_reset_revision: None, evidence_layout_version: layout, created_at: "2026-05-23T00:00:00.000Z".into(), updated_at: "2026-05-23T00:00:00.000Z".into(), brief_path: "brief".into(), goals_path: "goals".into(), ledger_path: "ledger".into(), codex_goal_mode: None, codex_objective: None, codex_objective_aliases: None, acknowledged_driver_objectives: None, aggregate_completion: None, active_goal_id: active.map(str::to_string), validation_batches: None, goals }
}

#[test]
fn empty_plan_points_at_create_goals() {
    assert!(status_next_actions(&plan(vec![], None, None), None).join("\n").contains("create-goals"));
}
#[test]
fn unresolved_criteria_name_remaining_ids() {
    let actions = status_next_actions(&plan(vec![goal("in_progress", vec![criterion("C001", "pass"), criterion("C002", "pending"), criterion("C003", "fail")])], Some("G001"), Some(2)), None).join("\n");
    assert!(actions.contains("record-evidence") && actions.contains("C002") && actions.contains("C003") && !actions.contains("C001"));
}
#[test]
fn final_passing_story_points_at_completion_template() {
    let actions = status_next_actions(&plan(vec![goal("in_progress", vec![criterion("C001", "pass")])], Some("G001"), Some(2)), None).join("\n");
    assert!(actions.contains("update_goal complete, then checkpoint --print-template"));
}
#[test]
fn review_blocked_goal_points_at_review_blockers() {
    let actions = status_next_actions(&plan(vec![goal("review_blocked", vec![criterion("C001", "pass")])], None, Some(2)), None).join("\n");
    assert!(actions.contains("record-review-blockers"));
}
#[test]
fn legacy_layout_omits_current_attempt_guidance() {
    let actions = status_next_actions(&plan(vec![goal("in_progress", vec![criterion("C001", "pass")])], Some("G001"), None), None).join("\n");
    assert!(actions.contains("evidence-layout v1") && actions.contains(".omo/evidence/"));
}
#[test]
fn codex_surface_uses_cli_session_guidance() {
    let actions = status_next_actions(&plan(vec![], None, None), Some(UlwLoopToolkitSurface::LazyCodex));
    assert!(actions[0].contains("ulw-loop create-goals"));
}
