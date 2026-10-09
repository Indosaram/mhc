use ulw_loop_core::domain_types::*;
use ulw_loop_core::evidence::*;
use ulw_loop_core::plan_io::{read_ulw_loop_plan, write_plan};

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
            title: "Title".into(),
            objective: "Objective".into(),
            status: "in_progress".into(),
            success_criteria: vec![UlwLoopSuccessCriterion {
                id: "C001".into(),
                scenario: "scenario 1".into(),
                user_model: "happy".into(),
                expected_evidence: "evidence 1".into(),
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
        codex_objective: None,
        codex_objective_aliases: None,
        acknowledged_driver_objectives: None,
        aggregate_completion: None,
        validation_batches: None,
    }
}

#[test]
fn test_record_evidence_pass() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let plan = make_plan();
    std::fs::create_dir_all(tmp.path().join(".omo/ulw-loop"))?;
    write_plan(tmp.path(), plan.clone(), None)?;

    let args = RecordEvidenceArgs {
        goal_id: "G001".into(),
        criterion_id: "C001".into(),
        status: "pass".into(),
        evidence: "observable proof".into(),
        notes: Some("all good".into()),
        artifacts: None,
    };

    let res = record_evidence(tmp.path(), &args, None)?;
    assert_eq!(res.criterion.status, "pass");
    assert_eq!(res.criterion.captured_evidence.as_deref(), Some("observable proof"));
    assert_eq!(res.ledger_entry.kind, "evidence_captured");

    let updated_plan = read_ulw_loop_plan(tmp.path(), None)?;
    assert_eq!(updated_plan.goals[0].success_criteria[0].status, "pass");
    Ok(())
}

#[test]
fn test_record_evidence_fail() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let plan = make_plan();
    std::fs::create_dir_all(tmp.path().join(".omo/ulw-loop"))?;
    write_plan(tmp.path(), plan.clone(), None)?;

    let args = RecordEvidenceArgs {
        goal_id: "G001".into(),
        criterion_id: "C001".into(),
        status: "fail".into(),
        evidence: "test failed".into(),
        notes: None,
        artifacts: None,
    };

    let res = record_evidence(tmp.path(), &args, None)?;
    assert_eq!(res.criterion.status, "fail");
    assert_eq!(res.ledger_entry.kind, "criterion_failed");
    Ok(())
}
