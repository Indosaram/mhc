use ulw_loop_core::domain_types::*;
use ulw_loop_core::validation_batch::*;

fn make_plan() -> UlwLoopPlan {
    UlwLoopPlan {
        version: 1,
        revision: Some(1),
        brief: Some("brief".into()),
        ledger_reset_revision: None,
        evidence_layout_version: Some(2),
        goals_path: ".omo/ulw-loop/goals.json".into(),
        ledger_path: ".omo/ulw-loop/ledger.jsonl".into(),
        brief_path: ".omo/ulw-loop/brief.md".into(),
        created_at: "2026-05-23T00:00:00Z".into(),
        updated_at: "2026-05-23T00:00:00Z".into(),
        active_goal_id: Some("G001".into()),
        goals: vec![
            UlwLoopItem {
                id: "G001".into(),
                title: "G1".into(),
                objective: "O1".into(),
                status: "complete".into(),
                success_criteria: vec![],
                attempt: 0,
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
            },
            UlwLoopItem {
                id: "G002".into(),
                title: "G2".into(),
                objective: "O2".into(),
                status: "in_progress".into(),
                success_criteria: vec![],
                attempt: 0,
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
            },
        ],
        codex_objective: None,
        codex_objective_aliases: None,
        acknowledged_driver_objectives: None,
        aggregate_completion: None,
        validation_batches: Some(vec![UlwLoopValidationBatch {
            batch_id: "B1".into(),
            member_ids: vec!["G001".into(), "G002".into()],
            final_goal_id: "G002".into(),
        }]),
        codex_goal_mode: None,
    }
}

#[test]
fn test_batch_closed_by() {
    let plan = make_plan();
    assert!(batch_closed_by(&plan, "G001").is_none());
    assert!(batch_closed_by(&plan, "G002").is_some());
    assert_eq!(batch_closed_by(&plan, "G002").unwrap().batch_id, "B1");
}

#[test]
fn test_require_batch_final_ready() {
    let plan = make_plan();
    let g2 = &plan.goals[1];
    assert!(require_batch_final_ready(&plan, g2).is_ok());
}
