use ulw_loop_core::command_types::CreateGoalsOptions;
use ulw_loop_core::plan_commit::{commit, CommitMutation};
use ulw_loop_core::plan_io::{read_ulw_loop_plan, with_ulw_loop_mutation_lock};
use ulw_loop_core::sdk::{AgentToolkit, ToolkitContext, ToolkitResponse};
use ulw_loop_core::surface::UlwLoopToolkitSurface;

#[test]
fn completed_plan_rejects_overwrite_unless_forced() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let ctx = ToolkitContext { cwd: tmp.path().to_string_lossy().into(), session_id: "completed-plan".into(), surface: UlwLoopToolkitSurface::OmoSenpi };
    let toolkit = AgentToolkit::new(ctx.clone(), None)?;
    let created = toolkit.create_goals(&CreateGoalsOptions { brief: "- original".into(), goals: None, codex_goal_mode: None, force: None, validation_batches_json: None });
    assert!(matches!(created, ToolkitResponse::Success { .. }));
    with_ulw_loop_mutation_lock(tmp.path(), Some(&ulw_loop_core::paths::UlwLoopScope { session_id: Some(ctx.session_id.clone()), ..Default::default() }), None, || {
        let scope=ulw_loop_core::paths::UlwLoopScope { session_id: Some(ctx.session_id.clone()), ..Default::default() };
        let mut plan=read_ulw_loop_plan(tmp.path(),Some(&scope))?; for goal in &mut plan.goals { goal.status="complete".into(); }
        commit(tmp.path(),Some(&scope),CommitMutation { plan: &mut plan, entries: &mut Vec::new() })
    })?;
    let response=toolkit.create_goals(&CreateGoalsOptions { brief:"- replacement".into(), goals:None, codex_goal_mode:None, force:None, validation_batches_json:None });
    match response { ToolkitResponse::Failure { error, .. } => assert_eq!(error.code,"ULW_LOOP_PLAN_EXISTS_COMPLETE"), _ => panic!("expected completed-plan rejection") }
    assert!(matches!(toolkit.create_goals(&CreateGoalsOptions { brief:"- replacement".into(), goals:None, codex_goal_mode:None, force:Some(true), validation_batches_json:None }), ToolkitResponse::Success { .. }));
    Ok(())
}
