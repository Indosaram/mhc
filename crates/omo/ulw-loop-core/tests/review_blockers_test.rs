use ulw_loop_core::command_types::*;
use ulw_loop_core::plan_crud::create_ulw_loop_plan;
use ulw_loop_core::review_blockers::*;

#[test]
fn test_record_review_blockers() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let opts = CreateGoalsOptions {
        brief: "- lone goal".into(),
        goals: None,
        codex_goal_mode: Some("aggregate".into()),
        force: None,
        validation_batches_json: None,
    };
    let mut plan = create_ulw_loop_plan(tmp.path(), &opts, None, None)?;
    let target_id = plan.goals[0].id.clone();
    plan.goals[0].status = "in_progress".into();
    plan.active_goal_id = Some(target_id.clone());
    ulw_loop_core::plan_io::write_plan(tmp.path(), plan, None)?;

    let snapshot = serde_json::json!({
        "goal": {
            "objective": "aggregate objective",
            "status": "in_progress",
        }
    }).to_string();

    let blocker_opts = RecordFinalReviewBlockersOptions {
        goal_id: target_id,
        title: "Blocker resolution".into(),
        objective: "Fix findings".into(),
        evidence: "Review findings evidence".into(),
        codex_goal: None,
        codex_goal_json: Some(snapshot),
    };

    let result = record_final_review_blockers(tmp.path(), &blocker_opts, None)?;
    assert_eq!(result.blocked_goal.status, "review_blocked");
    assert!(result.new_goal.id.starts_with("G002"));
    assert_eq!(result.plan.goals.len(), 2);
    Ok(())
}
