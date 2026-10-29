use ulw_loop_core::checkpoint::*;
use ulw_loop_core::domain_types::*;
use ulw_loop_core::plan_io::write_plan;

fn make_plan(goals: Vec<UlwLoopItem>) -> UlwLoopPlan {
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
        goals,
        codex_objective: Some("aggregate objective".into()),
        codex_objective_aliases: None,
        acknowledged_driver_objectives: None,
        aggregate_completion: None,
        validation_batches: None,
    }
}

fn make_criterion(id: &str, status: &str, essential: bool) -> UlwLoopSuccessCriterion {
    UlwLoopSuccessCriterion {
        id: id.into(),
        scenario: "scenario".into(),
        user_model: "happy".into(),
        expected_evidence: "evidence".into(),
        status: status.into(),
        essential: Some(essential),
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

#[test]
fn test_checkpoint_non_final_complete() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let g1 = make_goal("G001", "in_progress", vec![
        make_criterion("C001", "pass", true),
        make_criterion("C002", "pending", false),
    ]);
    let g2 = make_goal("G002", "pending", vec![]);
    let plan = make_plan(vec![g1, g2]);
    std::fs::create_dir_all(tmp.path().join(".omo/ulw-loop"))?;
    write_plan(tmp.path(), plan.clone(), None)?;

    let snapshot = serde_json::json!({
        "goal": {
            "objective": "aggregate objective",
            "status": "in_progress",
        }
    }).to_string();

    let args = CheckpointUlwLoopArgs {
        goal_id: "G001".into(),
        status: "complete".into(),
        evidence: "implementation done and validation passed".into(),
        codex_goal_json: Some(snapshot),
        quality_gate_json: None,
    };

    let result = checkpoint_ulw_loop(tmp.path(), &args, None, None)?;
    assert_eq!(result.goal.status, "complete");
    assert_eq!(result.ledger_entry.kind, "goal_completed");
    Ok(())
}

#[test]
fn test_checkpoint_failed() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let g1 = make_goal("G001", "in_progress", vec![make_criterion("C001", "fail", true)]);
    let plan = make_plan(vec![g1]);
    std::fs::create_dir_all(tmp.path().join(".omo/ulw-loop"))?;
    write_plan(tmp.path(), plan.clone(), None)?;

    let args = CheckpointUlwLoopArgs {
        goal_id: "G001".into(),
        status: "failed".into(),
        evidence: "compilation error".into(),
        codex_goal_json: None,
        quality_gate_json: None,
    };

    let result = checkpoint_ulw_loop(tmp.path(), &args, None, None)?;
    assert_eq!(result.goal.status, "failed");
    assert_eq!(result.ledger_entry.kind, "goal_failed");
    Ok(())
}
