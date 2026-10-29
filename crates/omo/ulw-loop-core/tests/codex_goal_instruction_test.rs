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

#[test]
fn test_aggregate_references_handoff_and_artifact() {
    let plan = make_plan();
    let instruction = build_codex_goal_instruction(BuildCodexGoalInstructionArgs { plan: &plan, goal: &plan.goals[0], is_final: Some(false), surface: Some(UlwLoopToolkitSurface::LazyCodex) });
    assert!(instruction.text.contains("aggregate"));
    assert!(instruction.text.contains(".omo/ulw-loop/goals.json"));
}

#[test]
fn test_aggregate_payload_is_objective_only() {
    let plan = make_plan();
    let instruction = build_codex_goal_instruction(BuildCodexGoalInstructionArgs { plan: &plan, goal: &plan.goals[0], is_final: Some(false), surface: Some(UlwLoopToolkitSurface::LazyCodex) });
    assert_eq!(instruction.json.objective, "Aggregate objective");
}

#[test]
fn test_non_final_omits_quality_gate_tokens() {
    let plan = make_plan();
    let instruction = build_codex_goal_instruction(BuildCodexGoalInstructionArgs { plan: &plan, goal: &plan.goals[0], is_final: Some(false), surface: Some(UlwLoopToolkitSurface::LazyCodex) });
    assert!(!instruction.text.contains("lazycodex-code-reviewer"));
    assert!(!instruction.text.contains("record-review-blockers"));
}

#[test]
fn test_final_includes_quality_gate_roles_fields_commands() {
    let plan = make_plan();
    let instruction = build_codex_goal_instruction(BuildCodexGoalInstructionArgs { plan: &plan, goal: &plan.goals[0], is_final: Some(true), surface: Some(UlwLoopToolkitSurface::LazyCodex) });
    for term in ["lazycodex-code-reviewer", "lazycodex-qa-executor", "lazycodex-gate-reviewer", "codeReview", "manualQa", "gateReview", "iteration", "criteriaCoverage", "originalIntent", "desiredOutcome", "userOutcomeReview", "update_goal", "record-review-blockers", "checkpoint"] { assert!(instruction.text.contains(term), "missing {term}"); }
}

#[test]
fn test_final_omo_senpi_teaches_single_gate_chain() {
    let plan = make_plan();
    let instruction = build_codex_goal_instruction(BuildCodexGoalInstructionArgs { plan: &plan, goal: &plan.goals[0], is_final: Some(true), surface: Some(UlwLoopToolkitSurface::OmoSenpi) });
    for term in ["main-session", "category: \"deep-high\"", "unspecified-high", "unspecified-low", "gateReview.by", "category:<name>", "--print-template", "model_unavailable"] { assert!(instruction.text.contains(term), "missing {term}"); }
    assert!(!instruction.text.contains("attempted_chain"));
}

#[test]
fn test_final_lazycodex_keeps_lazycodex_reviewers() {
    let plan = make_plan();
    let instruction = build_codex_goal_instruction(BuildCodexGoalInstructionArgs { plan: &plan, goal: &plan.goals[0], is_final: Some(true), surface: Some(UlwLoopToolkitSurface::LazyCodex) });
    assert!(instruction.text.contains("lazycodex-code-reviewer"));
    assert!(!instruction.text.contains("omo-senpi-code-reviewer"));
}

#[test]
fn test_scoped_plan_includes_session_options() {
    let mut plan = make_plan();
    plan.goals_path = ".omo/ulw-loop/session-A/goals.json".into();
    plan.ledger_path = ".omo/ulw-loop/session-A/ledger.jsonl".into();
    let instruction = build_codex_goal_instruction(BuildCodexGoalInstructionArgs { plan: &plan, goal: &plan.goals[0], is_final: Some(true), surface: Some(UlwLoopToolkitSurface::LazyCodex) });
    assert!(instruction.text.contains("record-review-blockers --session-id session-A"));
    assert!(instruction.text.contains("checkpoint --session-id session-A"));
    assert!(instruction.text.contains("complete-goals --session-id session-A --retry-failed"));
}

#[test]
fn test_per_story_uses_goal_objective() {
    let mut plan = make_plan();
    plan.codex_goal_mode = Some("per_story".into());
    plan.goals[0].objective = "per-story-objective-sentinel".into();
    let instruction = build_codex_goal_instruction(BuildCodexGoalInstructionArgs { plan: &plan, goal: &plan.goals[0], is_final: Some(false), surface: Some(UlwLoopToolkitSurface::LazyCodex) });
    assert!(instruction.text.contains("per-story-objective-sentinel"));
}

#[test]
fn test_criteria_lists_id_scenario_status() {
    let mut plan = make_plan();
    plan.goals[0].success_criteria = [
        ("C001", "happy login", "happy", "200 OK", "pending"),
        ("C002", "invalid creds", "edge", "401", "pass"),
        ("C003", "no regression /health", "regression", "/health unaffected", "fail"),
    ].into_iter().map(|(id, scenario, user_model, expected_evidence, status)| UlwLoopSuccessCriterion { id: id.into(), scenario: scenario.into(), user_model: user_model.into(), expected_evidence: expected_evidence.into(), status: status.into(), essential: None, captured_evidence: None, captured_at: None, notes: None, artifacts: None }).collect();
    let instruction = build_codex_goal_instruction(BuildCodexGoalInstructionArgs { plan: &plan, goal: &plan.goals[0], is_final: Some(false), surface: Some(UlwLoopToolkitSurface::LazyCodex) });
    for term in ["C001", "happy login", "pending", "C002", "pass", "C003", "fail"] { assert!(instruction.text.contains(term)); }
}

#[test]
fn test_pending_criteria_are_remaining_work() {
    let plan = make_plan();
    let instruction = build_codex_goal_instruction(BuildCodexGoalInstructionArgs { plan: &plan, goal: &plan.goals[0], is_final: Some(false), surface: Some(UlwLoopToolkitSurface::LazyCodex) });
    assert!(instruction.text.to_lowercase().contains("remaining") || instruction.text.to_lowercase().contains("pending"));
}

#[test]
fn test_artifact_guidance_references_loop_path() {
    let plan = make_plan();
    let instruction = build_codex_goal_instruction(BuildCodexGoalInstructionArgs { plan: &plan, goal: &plan.goals[0], is_final: Some(false), surface: Some(UlwLoopToolkitSurface::LazyCodex) });
    assert!(instruction.text.contains(".omo/ulw-loop"));
}
