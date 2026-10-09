use ulw_loop_core::domain_types::*;
use ulw_loop_core::plan_io::write_plan;
use ulw_loop_core::steering::*;
use ulw_loop_core::steering_types::*;

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
            title: "Task 1".into(),
            objective: "Do task 1".into(),
            status: "pending".into(),
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
        }],
        codex_objective: None,
        codex_objective_aliases: None,
        acknowledged_driver_objectives: None,
        aggregate_completion: None,
        validation_batches: None,
    }
}

#[test]
fn test_annotate_ledger() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let plan = make_plan();
    std::fs::create_dir_all(tmp.path().join(".omo/ulw-loop"))?;
    write_plan(tmp.path(), plan.clone(), None)?;

    let proposal = UlwLoopSteeringProposal {
        kind: "annotate_ledger".into(),
        source: "cli".into(),
        evidence: "observed behavior".into(),
        rationale: "rationale for note".into(),
        idempotency_key: None,
        target_goal_id: None,
        target_goal_ids: None,
        criterion_id: None,
        title: None,
        objective: None,
        child_goals: None,
        revised_title: None,
        revised_objective: None,
        pending_order: None,
        blocked_reason: None,
        scenario: None,
        expected_evidence: None,
        user_model: None,
        directive_text: None,
        prompt_signature: None,
        raw_proposal: None,
        goal_id: None,
        after: None,
    };

    let result = steer_ulw_loop(tmp.path(), &proposal, None, None)?;
    assert!(result.accepted);
    assert_eq!(result.audit.kind, "annotate_ledger");
    Ok(())
}

#[test]
fn test_add_subgoal() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let plan = make_plan();
    std::fs::create_dir_all(tmp.path().join(".omo/ulw-loop"))?;
    write_plan(tmp.path(), plan.clone(), None)?;

    let proposal = UlwLoopSteeringProposal {
        kind: "add_subgoal".into(),
        source: "cli".into(),
        evidence: "need another task".into(),
        rationale: "discovered requirement".into(),
        title: Some("Task 2".into()),
        objective: Some("Do task 2".into()),
        idempotency_key: None,
        target_goal_id: None,
        target_goal_ids: None,
        criterion_id: None,
        child_goals: None,
        revised_title: None,
        revised_objective: None,
        pending_order: None,
        blocked_reason: None,
        scenario: None,
        expected_evidence: None,
        user_model: None,
        directive_text: None,
        prompt_signature: None,
        raw_proposal: None,
        goal_id: None,
        after: None,
    };

    let result = steer_ulw_loop(tmp.path(), &proposal, None, None)?;
    assert!(result.accepted);
    assert_eq!(result.plan.goals.len(), 2);
    assert_eq!(result.plan.goals[1].title, "Task 2");
    Ok(())
}

#[test]
fn test_parse_ulw_loop_steering_directive() {
    let directive = r#"OMO_ULW_LOOP_STEER: {"kind":"annotate_ledger","source":"cli","evidence":"proof","rationale":"why"}"#;
    let parsed = parse_ulw_loop_steering_directive(directive);
    assert!(parsed.is_some());
    let p = parsed.unwrap();
    assert_eq!(p.kind, "annotate_ledger");
    assert_eq!(p.evidence, "proof");
}
