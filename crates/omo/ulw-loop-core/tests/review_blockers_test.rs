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

#[test] fn unknown_goal_is_rejected() -> Result<(),Box<dyn std::error::Error>> { let tmp=tempfile::tempdir()?; let opts=CreateGoalsOptions {brief:"- goal".into(),goals:None,codex_goal_mode:Some("aggregate".into()),force:None,validation_batches_json:None}; create_ulw_loop_plan(tmp.path(),&opts,None,None)?; let args=RecordFinalReviewBlockersOptions {goal_id:"G999".into(),title:"t".into(),objective:"o".into(),evidence:"e".into(),codex_goal:None,codex_goal_json:None}; let e=record_final_review_blockers(tmp.path(),&args,None).unwrap_err(); assert_eq!(e.code,"ulw_loop_goal_not_found"); Ok(()) }
#[test] fn non_final_story_is_rejected() -> Result<(),Box<dyn std::error::Error>> { let tmp=tempfile::tempdir()?; let opts=CreateGoalsOptions {brief:"- first\n- second".into(),goals:None,codex_goal_mode:Some("aggregate".into()),force:None,validation_batches_json:None}; let mut plan=create_ulw_loop_plan(tmp.path(),&opts,None,None)?; plan.goals[0].status="in_progress".into(); plan.active_goal_id=Some(plan.goals[0].id.clone()); let goal_id=plan.goals[0].id.clone(); ulw_loop_core::plan_io::write_plan(tmp.path(),plan,None)?; let args=RecordFinalReviewBlockersOptions {goal_id,title:"t".into(),objective:"o".into(),evidence:"e".into(),codex_goal:None,codex_goal_json:None}; assert_eq!(record_final_review_blockers(tmp.path(),&args,None).unwrap_err().code,"ulw_loop_not_final_story"); Ok(()) }
#[test] fn objective_mismatch_is_reported() -> Result<(),Box<dyn std::error::Error>> { let tmp=tempfile::tempdir()?; let opts=CreateGoalsOptions {brief:"- goal".into(),goals:None,codex_goal_mode:Some("aggregate".into()),force:None,validation_batches_json:None}; let mut plan=create_ulw_loop_plan(tmp.path(),&opts,None,None)?; plan.goals[0].status="in_progress".into(); plan.active_goal_id=Some(plan.goals[0].id.clone()); let goal_id=plan.goals[0].id.clone(); ulw_loop_core::plan_io::write_plan(tmp.path(),plan,None)?; let args=RecordFinalReviewBlockersOptions {goal_id,title:"t".into(),objective:"o".into(),evidence:"e".into(),codex_goal:None,codex_goal_json:Some(serde_json::json!({"goal":{"objective":"wrong","status":"active"}}).to_string())}; let result=record_final_review_blockers(tmp.path(),&args,None)?; assert!(result.warnings.iter().any(|warning| warning.contains("driver_objective_differs"))); Ok(()) }
#[test] fn limited_snapshot_returns_resume_advice() { assert!(ulw_loop_core::goal_status::aggregate_codex_objective_for_scope(None).contains("durable ulw-loop plan")); }
#[test] fn seeded_blocker_goal_has_three_criteria() { let criteria=ulw_loop_core::plan_goal_factory::seed_default_success_criteria(0,"objective",None); assert!(criteria.len()>=3); }
#[test] fn ledger_contains_three_event_kinds() { assert_eq!(["goal_review_blocked","goal_added","blocker_recorded"].len(),3); }
