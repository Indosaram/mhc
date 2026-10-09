use ulw_loop_core::command_types::*;
use ulw_loop_core::plan_crud::*;
use ulw_loop_core::plan_io::read_ulw_loop_plan;

#[test]
fn test_create_and_summarize_plan() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let brief = "- goal alpha\n- goal beta";
    let opts = CreateGoalsOptions {
        brief: brief.into(),
        goals: None,
        codex_goal_mode: Some("aggregate".into()),
        force: None,
        validation_batches_json: None,
    };

    let plan = create_ulw_loop_plan(tmp.path(), &opts, None, None)?;
    assert_eq!(plan.goals.len(), 2);
    assert!(plan.goals[0].id.starts_with("G001"));
    assert!(plan.goals[1].id.starts_with("G002"));

    let summary = summarize_ulw_loop_plan(&plan);
    assert_eq!(summary.total, 2);
    assert_eq!(summary.pending, 2);
    assert_eq!(summary.complete, 0);

    let read_back = read_ulw_loop_plan(tmp.path(), None)?;
    assert_eq!(read_back.goals.len(), 2);
    Ok(())
}

#[test]
fn test_add_goal() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let opts = CreateGoalsOptions {
        brief: "- goal alpha".into(),
        goals: None,
        codex_goal_mode: None,
        force: None,
        validation_batches_json: None,
    };
    create_ulw_loop_plan(tmp.path(), &opts, None, None)?;

    let add_opts = AddGoalOptions {
        title: "goal beta".into(),
        objective: "do beta".into(),
        evidence: None,
        success_criteria: None,
    };
    let res = add_ulw_loop_goal(tmp.path(), &add_opts, None, None)?;
    assert!(res.goal.id.starts_with("G002"));
    assert_eq!(res.plan.goals.len(), 2);
    Ok(())
}

#[test]
fn test_start_next_goal() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let opts = CreateGoalsOptions {
        brief: "- goal alpha\n- goal beta".into(),
        goals: None,
        codex_goal_mode: None,
        force: None,
        validation_batches_json: None,
    };
    create_ulw_loop_plan(tmp.path(), &opts, None, None)?;

    let res = start_next_ulw_loop(tmp.path(), None, None)?;
    match res {
        StartNextResult::Started { goal, resumed, .. } => {
            assert!(goal.id.starts_with("G001"));
            assert!(!resumed);
        }
        StartNextResult::Done { .. } => panic!("expected started goal"),
    }
    Ok(())
}
