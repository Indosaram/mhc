use ulw_loop_core::domain_types::*;
use ulw_loop_core::goal_status::*;

fn make_criterion(id: &str, status: &str, essential: Option<bool>) -> UlwLoopSuccessCriterion {
    UlwLoopSuccessCriterion {
        id: id.into(),
        scenario: "scenario".into(),
        user_model: "happy".into(),
        expected_evidence: "evidence".into(),
        status: status.into(),
        essential,
        captured_evidence: None,
        captured_at: None,
        notes: None,
        artifacts: None,
    }
}

fn make_goal(id: &str, status: &str, criteria: Vec<UlwLoopSuccessCriterion>) -> UlwLoopItem {
    UlwLoopItem {
        id: id.into(),
        title: "Title".into(),
        objective: "Objective".into(),
        status: status.into(),
        success_criteria: criteria,
        attempt: 1,
        created_at: "2026-05-23T00:00:00Z".into(),
        updated_at: "2026-05-23T00:00:00Z".into(),
        started_at: None,
        completed_at: None,
        failed_at: None,
        review_blocked_at: None,
        evidence: None,
        failure_reason: None,
        steering_status: None,
        superseded_by: None,
        supersedes: None,
        blocked_reason: None,
        blocker_signature: None,
        blocker_occurrence_count: None,
        required_external_decision: None,
        non_retriable: None,
        steering_evidence: None,
        steering_rationale: None,
    }
}

fn make_plan(goals: Vec<UlwLoopItem>, active_goal_id: Option<String>) -> UlwLoopPlan {
    UlwLoopPlan {
        version: 1,
        revision: Some(1),
        codex_goal_mode: Some("aggregate".into()),
        brief: Some("brief".into()),
        ledger_reset_revision: None,
        evidence_layout_version: Some(2),
        goals_path: ".omo/ulw-loop/goals.json".into(),
        ledger_path: ".omo/ulw-loop/ledger.jsonl".into(),
        brief_path: ".omo/ulw-loop/brief.md".into(),
        created_at: "2026-05-23T00:00:00Z".into(),
        updated_at: "2026-05-23T00:00:00Z".into(),
        active_goal_id,
        goals,
        codex_objective: None,
        codex_objective_aliases: None,
        acknowledged_driver_objectives: None,
        aggregate_completion: None,
        validation_batches: None,
    }
}

#[test]
fn test_has_all_criteria_pass() {
    let g1 = make_goal("G001", "in_progress", vec![make_criterion("C001", "pass", None), make_criterion("C002", "pass", None)]);
    assert!(has_all_criteria_pass(&g1));

    let g2 = make_goal("G002", "in_progress", vec![make_criterion("C001", "pass", None), make_criterion("C002", "pending", None)]);
    assert!(!has_all_criteria_pass(&g2));
}

#[test]
fn test_has_essential_criteria_pass() {
    let g1 = make_goal("G001", "in_progress", vec![
        make_criterion("C001", "pass", Some(true)),
        make_criterion("C002", "pending", Some(false)),
    ]);
    assert!(has_essential_criteria_pass(&g1));

    let g2 = make_goal("G002", "in_progress", vec![
        make_criterion("C001", "fail", Some(true)),
        make_criterion("C002", "pass", Some(false)),
    ]);
    assert!(!has_essential_criteria_pass(&g2));
}

#[test]
fn test_is_final_run_completion_candidate() {
    let g1 = make_goal("G001", "complete", vec![]);
    let g2 = make_goal("G002", "in_progress", vec![]);
    let plan = make_plan(vec![g1.clone(), g2.clone()], Some("G002".into()));

    assert!(!is_final_run_completion_candidate(&plan, &g1));
    assert!(is_final_run_completion_candidate(&plan, &g2));
}

#[test]
fn test_is_ulw_loop_done() {
    let g1 = make_goal("G001", "complete", vec![]);
    let mut plan = make_plan(vec![g1], None);
    assert!(is_ulw_loop_done(&plan));

    plan.aggregate_completion = Some(UlwLoopAggregateCompletion {
        status: "complete".into(),
        completed_at: "now".into(),
        evidence: "ev".into(),
        codex_goal: None,
    });
    assert!(is_ulw_loop_done(&plan));
}
