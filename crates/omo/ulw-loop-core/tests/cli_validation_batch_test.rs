use ulw_loop_core::domain_types::UlwLoopItem;
use ulw_loop_core::validation_batch::parse_validation_batches;

fn goal(id: &str) -> UlwLoopItem { UlwLoopItem { id: id.into(), title: id.into(), objective: id.into(), status: "pending".into(), success_criteria: vec![], attempt: 0, created_at: "2026-05-23T00:00:00Z".into(), updated_at: "2026-05-23T00:00:00Z".into(), started_at: None, completed_at: None, failed_at: None, review_blocked_at: None, evidence: None, failure_reason: None, steering_status: None, superseded_by: None, supersedes: None, blocked_reason: None, blocker_signature: None, blocker_occurrence_count: None, required_external_decision: None, non_retriable: None, steering_evidence: None, steering_rationale: None } }

#[test]
fn create_goals_validation_batch_json_includes_batch() {
    let goals = vec![goal("G001-goal-alpha"), goal("G002-goal-beta")];
    let batches = parse_validation_batches(Some(r#"[{"batchId":"VB001","memberIds":["G001-goal-alpha","G002-goal-beta"],"finalGoalId":"G002-goal-beta"}]"#), &goals).unwrap().unwrap();
    assert_eq!(batches[0].batch_id, "VB001");
}

#[test]
fn create_goals_rejects_final_goal_outside_members_with_distinct_code() {
    let goals = vec![goal("G001-goal-alpha"), goal("G002-goal-beta"), goal("G003-goal-gamma")];
    let error = parse_validation_batches(Some(r#"[{"batchId":"VB001","memberIds":["G001-goal-alpha","G002-goal-beta"],"finalGoalId":"G003-goal-gamma"}]"#), &goals).unwrap_err();
    assert_eq!(error.code, "ULW_LOOP_VALIDATION_BATCH_FINAL_NOT_MEMBER");
}
