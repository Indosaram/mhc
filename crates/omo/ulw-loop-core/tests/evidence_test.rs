use ulw_loop_core::domain_types::*;
use ulw_loop_core::evidence::*;
use ulw_loop_core::plan_io::{read_ledger, read_ulw_loop_plan, write_plan};

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

fn make_multi_criterion_plan() -> UlwLoopPlan {
    let mut plan = make_plan();
    plan.goals[0].success_criteria = vec![
        UlwLoopSuccessCriterion { id: "C001".into(), status: "pass".into(), captured_evidence: Some("old".into()), captured_at: Some("old".into()), ..plan.goals[0].success_criteria[0].clone() },
        UlwLoopSuccessCriterion { id: "C002".into(), status: "fail".into(), captured_evidence: Some("older".into()), captured_at: Some("older".into()), ..plan.goals[0].success_criteria[0].clone() },
        UlwLoopSuccessCriterion { id: "C003".into(), status: "blocked".into(), captured_evidence: Some("oldest".into()), captured_at: Some("oldest".into()), ..plan.goals[0].success_criteria[0].clone() },
    ];
    plan
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

#[test]
fn test_record_evidence_captures_trimmed_timestamp_and_ledger_fields() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    std::fs::create_dir_all(tmp.path().join(".omo/ulw-loop"))?;
    write_plan(tmp.path(), make_plan(), None)?;
    let res = record_evidence(tmp.path(), &RecordEvidenceArgs { goal_id: "G001".into(), criterion_id: "C001".into(), status: "pass".into(), evidence: "  curl /login returns 200 + token verified  ".into(), notes: None, artifacts: None }, None)?;
    assert_eq!(res.criterion.captured_evidence.as_deref(), Some("curl /login returns 200 + token verified"));
    assert!(res.criterion.captured_at.as_deref().is_some_and(|v| v.contains('T')));
    assert_eq!(res.ledger_entry.goal_id.as_deref(), Some("G001"));
    assert_eq!(res.ledger_entry.criterion_id.as_deref(), Some("C001"));
    Ok(())
}

#[test]
fn test_record_evidence_blocked() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    std::fs::create_dir_all(tmp.path().join(".omo/ulw-loop"))?;
    write_plan(tmp.path(), make_plan(), None)?;
    let res = record_evidence(tmp.path(), &RecordEvidenceArgs { goal_id: "G001".into(), criterion_id: "C001".into(), status: "blocked".into(), evidence: "auth not in CI yet".into(), notes: None, artifacts: None }, None)?;
    assert_eq!(res.criterion.status, "blocked");
    assert_eq!(res.ledger_entry.kind, "criterion_blocked");
    Ok(())
}

#[test]
fn test_record_evidence_unknown_goal_errors() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".omo/ulw-loop")).unwrap();
    write_plan(tmp.path(), make_plan(), None).unwrap();
    let err = record_evidence(tmp.path(), &RecordEvidenceArgs { goal_id: "GUNKNOWN".into(), criterion_id: "C001".into(), status: "pass".into(), evidence: "x".into(), notes: None, artifacts: None }, None).unwrap_err();
    assert_eq!(err.code, "ULW_LOOP_GOAL_NOT_FOUND");
}

#[test]
fn test_record_evidence_unknown_criterion_errors() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".omo/ulw-loop")).unwrap();
    write_plan(tmp.path(), make_plan(), None).unwrap();
    let err = record_evidence(tmp.path(), &RecordEvidenceArgs { goal_id: "G001".into(), criterion_id: "CUNKNOWN".into(), status: "pass".into(), evidence: "x".into(), notes: None, artifacts: None }, None).unwrap_err();
    assert_eq!(err.code, "ULW_LOOP_CRITERION_NOT_FOUND");
}

#[test]
fn test_record_evidence_empty_evidence_errors() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".omo/ulw-loop")).unwrap();
    write_plan(tmp.path(), make_plan(), None).unwrap();
    let err = record_evidence(tmp.path(), &RecordEvidenceArgs { goal_id: "G001".into(), criterion_id: "C001".into(), status: "pass".into(), evidence: "   ".into(), notes: None, artifacts: None }, None).unwrap_err();
    assert_eq!(err.code, "ULW_LOOP_EVIDENCE_REQUIRED");
}

#[test]
fn test_mark_criteria_pending_reset_resets_all_fields() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    std::fs::create_dir_all(tmp.path().join(".omo/ulw-loop"))?;
    write_plan(tmp.path(), make_multi_criterion_plan(), None)?;
    let (plan, count) = mark_criteria_pending_reset_for_goal(tmp.path(), "G001", None)?;
    assert_eq!(count, 3);
    for criterion in &plan.goals[0].success_criteria {
        assert_eq!(criterion.status, "pending");
        assert!(criterion.captured_evidence.is_none());
        assert!(criterion.captured_at.is_none());
    }
    assert_eq!(read_ledger(tmp.path(), None).last().unwrap().kind, "criteria_revised");
    Ok(())
}

#[test]
fn test_criteria_summary_counts_all_statuses() {
    let mut plan = make_plan();
    plan.goals[0].success_criteria = vec![
        UlwLoopSuccessCriterion { status: "pass".into(), ..plan.goals[0].success_criteria[0].clone() },
        UlwLoopSuccessCriterion { id: "C002".into(), status: "pending".into(), ..plan.goals[0].success_criteria[0].clone() },
        UlwLoopSuccessCriterion { id: "C003".into(), status: "fail".into(), ..plan.goals[0].success_criteria[0].clone() },
        UlwLoopSuccessCriterion { id: "C004".into(), status: "blocked".into(), ..plan.goals[0].success_criteria[0].clone() },
    ];
    let summary = criteria_summary(&plan);
    assert_eq!(summary.total_criteria, 4);
    assert_eq!(summary.pass_count, 1);
    assert_eq!(summary.pending_count, 1);
    assert_eq!(summary.fail_count, 1);
    assert_eq!(summary.blocked_count, 1);
    assert_eq!(summary.goals_with_unresolved_criteria, vec!["G001"]);
}

#[test]
fn test_criteria_summary_empty_and_unresolved_filter() {
    let mut plan = make_plan();
    plan.goals[0].success_criteria.clear();
    let summary = criteria_summary(&plan);
    assert_eq!(summary.total_criteria, 0);
    assert!(summary.goals_with_unresolved_criteria.is_empty());

    plan.goals[0].success_criteria = vec![
        UlwLoopSuccessCriterion { status: "pass".into(), ..make_plan().goals[0].success_criteria[0].clone() },
        UlwLoopSuccessCriterion { id: "C002".into(), status: "pending".into(), ..make_plan().goals[0].success_criteria[0].clone() },
        UlwLoopSuccessCriterion { id: "C003".into(), status: "fail".into(), ..make_plan().goals[0].success_criteria[0].clone() },
    ];
    assert_eq!(unresolved_criteria_of(&plan.goals[0]).iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), vec!["C002", "C003"]);
}

#[test]
fn test_record_evidence_persists_fresh_read() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    std::fs::create_dir_all(tmp.path().join(".omo/ulw-loop"))?;
    write_plan(tmp.path(), make_plan(), None)?;
    record_evidence(tmp.path(), &RecordEvidenceArgs { goal_id: "G001".into(), criterion_id: "C001".into(), status: "pass".into(), evidence: "observable proof".into(), notes: None, artifacts: None }, None)?;
    assert_eq!(read_ulw_loop_plan(tmp.path(), None)?.goals[0].success_criteria[0].status, "pass");
    Ok(())
}

#[test]
fn test_mark_criteria_pending_reset_emits_single_revision_event() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    std::fs::create_dir_all(tmp.path().join(".omo/ulw-loop"))?;
    write_plan(tmp.path(), make_plan(), None)?;
    mark_criteria_pending_reset_for_goal(tmp.path(), "G001", None)?;
    let entries = read_ledger(tmp.path(), None);
    assert_eq!(entries.last().unwrap().kind, "criteria_revised");
    Ok(())
}

#[test]
fn test_record_evidence_invalid_status_errors() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".omo/ulw-loop")).unwrap();
    write_plan(tmp.path(), make_plan(), None).unwrap();
    let err = record_evidence(tmp.path(), &RecordEvidenceArgs { goal_id: "G001".into(), criterion_id: "C001".into(), status: "unknown".into(), evidence: "x".into(), notes: None, artifacts: None }, None).unwrap_err();
    assert_eq!(err.code, "ULW_LOOP_CRITERION_STATUS_INVALID");
}
