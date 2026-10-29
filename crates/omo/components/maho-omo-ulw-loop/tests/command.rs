use maho_omo_ulw_loop::omo_command::*;
use maho_omo_ulw_loop::index::status_has_active_incomplete_run;

#[test]
fn active_pending_goal() {
    assert!(status_has_active_incomplete_run(&serde_json::json!({"ok":true,"plan":{"goals":[{"status":"pending"}]}})));
}

#[test]
fn blocked_goal_not_active() {
    assert!(!status_has_active_incomplete_run(&serde_json::json!({"ok":true,"plan":{"goals":[{"status":"pending","steeringStatus":"blocked"}]}})));
}

#[test]
fn aggregate_complete_not_active() {
    assert!(!status_has_active_incomplete_run(&serde_json::json!({"ok":true,"plan":{"aggregateCompletion":{"status":"complete"},"goals":[{"status":"pending"}]}})));
}

#[test]
fn completed_criteria_and_superseded_goals_are_inactive() {
    for goal in [
        serde_json::json!({"status":"pending","successCriteria":[{"status":"pass"}]}),
        serde_json::json!({"status":"in_progress","steeringStatus":"superseded"}),
        serde_json::json!({"status":"complete"}),
        serde_json::Value::Null,
    ] {
        assert!(!status_has_active_incomplete_run(&serde_json::json!({"ok":true,"plan":{"goals":[goal]}})));
    }
}

#[test]
fn empty_and_malformed_status_are_inactive() {
    for value in [
        serde_json::Value::Null,
        serde_json::json!({"ok":false}),
        serde_json::json!({"ok":true,"plan":{"goals":[]}}),
        serde_json::json!({"ok":true,"plan":{"goals":{}}}),
    ] {
        assert!(!status_has_active_incomplete_run(&value));
    }
}

#[test]
fn incomplete_criteria_keep_in_progress_goal_active() {
    for criteria in [
        serde_json::json!([]),
        serde_json::json!([{"status":"fail"}]),
        serde_json::json!([null]),
        serde_json::json!([{"status":"pass"},{"status":"pending"}]),
    ] {
        assert!(status_has_active_incomplete_run(
            &serde_json::json!({"ok":true,"plan":{"goals":[{"status":"in_progress","successCriteria":criteria}]}})
        ));
    }
}

#[tokio::test]
async fn in_process_status_returns_error_when_no_plan() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let result = run_omo_status(root.path(), Some("session")).await;
    assert_eq!(result.code, 1);
    let val: serde_json::Value = serde_json::from_str(&result.stdout)?;
    assert_eq!(val["ok"], false);
    Ok(())
}

#[tokio::test]
async fn in_process_status_returns_plan_when_present() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let plan = ulw_loop_core::domain_types::UlwLoopPlan {
        version: 1,
        revision: Some(1),
        codex_goal_mode: Some("aggregate".into()),
        brief: Some("test".into()),
        ledger_reset_revision: None,
        evidence_layout_version: Some(2),
        goals_path: ".omo/ulw-loop/goals.json".into(),
        ledger_path: ".omo/ulw-loop/ledger.jsonl".into(),
        brief_path: ".omo/ulw-loop/brief.md".into(),
        created_at: "2026-01-01T00:00:00Z".into(),
        updated_at: "2026-01-01T00:00:00Z".into(),
        active_goal_id: Some("G001".into()),
        goals: vec![ulw_loop_core::domain_types::UlwLoopItem {
            id: "G001".into(),
            title: "Task 1".into(),
            objective: "Do task 1".into(),
            status: "pending".into(),
            success_criteria: vec![],
            attempt: 0,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
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
    };
    let dir = root.path().join(".omo/ulw-loop/session");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("goals.json"), serde_json::to_string(&plan)?)?;

    let result = run_omo_status(root.path(), Some("session")).await;
    assert_eq!(result.code, 0);
    let val: serde_json::Value = serde_json::from_str(&result.stdout)?;
    assert_eq!(val["ok"], true);
    assert_eq!(val["plan"]["goals"][0]["id"], "G001");
    Ok(())
}

#[tokio::test]
async fn in_process_status_reflects_active_run() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let plan = ulw_loop_core::domain_types::UlwLoopPlan {
        version: 1,
        revision: Some(1),
        codex_goal_mode: Some("aggregate".into()),
        brief: Some("test".into()),
        ledger_reset_revision: None,
        evidence_layout_version: Some(2),
        goals_path: ".omo/ulw-loop/goals.json".into(),
        ledger_path: ".omo/ulw-loop/ledger.jsonl".into(),
        brief_path: ".omo/ulw-loop/brief.md".into(),
        created_at: "2026-01-01T00:00:00Z".into(),
        updated_at: "2026-01-01T00:00:00Z".into(),
        active_goal_id: Some("G001".into()),
        goals: vec![ulw_loop_core::domain_types::UlwLoopItem {
            id: "G001".into(),
            title: "Task 1".into(),
            objective: "Do task 1".into(),
            status: "pending".into(),
            success_criteria: vec![],
            attempt: 0,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
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
    };
    let dir = root.path().join(".omo/ulw-loop/session");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("goals.json"), serde_json::to_string(&plan)?)?;

    let result = run_omo_status(root.path(), Some("session")).await;
    let val: serde_json::Value = serde_json::from_str(&result.stdout)?;
    assert!(status_has_active_incomplete_run(&val));
    Ok(())
}
