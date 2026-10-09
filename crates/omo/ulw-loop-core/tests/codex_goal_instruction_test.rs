use ulw_loop_core::codex_goal_instruction::*;
use ulw_loop_core::domain_types::*;
use ulw_loop_core::surface::UlwLoopToolkitSurface;

fn make_plan() -> UlwLoopPlan {
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
        active_goal_id: Some("G001".into()),
        goals: vec![UlwLoopItem {
            id: "G001".into(),
            title: "Goal 1".into(),
            objective: "Do goal 1".into(),
            status: "in_progress".into(),
            success_criteria: vec![UlwLoopSuccessCriterion {
                id: "C001".into(),
                scenario: "happy path".into(),
                user_model: "happy".into(),
                expected_evidence: "observable proof".into(),
                status: "pending".into(),
                essential: Some(true),
                captured_evidence: None,
                captured_at: None,
                notes: None,
                artifacts: None,
            }],
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
        }],
        codex_objective: Some("Aggregate objective".into()),
        codex_objective_aliases: None,
        acknowledged_driver_objectives: None,
        aggregate_completion: None,
        validation_batches: None,
    }
}

#[test]
fn test_build_codex_goal_instruction() {
    let plan = make_plan();
    let goal = &plan.goals[0];
    let instruction = build_codex_goal_instruction(BuildCodexGoalInstructionArgs {
        plan: &plan,
        goal,
        is_final: Some(true),
        surface: Some(UlwLoopToolkitSurface::LazyCodex),
    });

    assert!(instruction.text.contains("Aggregate objective"));
    assert!(instruction.text.contains("G001"));
    assert_eq!(instruction.json.objective, "Aggregate objective");
}
