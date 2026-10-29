use ulw_loop_core::command_types::*;
use ulw_loop_core::domain_types::*;
use ulw_loop_core::plan_crud::*;
use ulw_loop_core::plan_goal_factory::{derive_goal_candidates, seed_default_success_criteria};
use ulw_loop_core::plan_io::{read_ledger, read_ulw_loop_plan, write_plan};
use ulw_loop_core::paths::{ulw_loop_brief_path, ulw_loop_goals_path, ulw_loop_ledger_path};

const NOW: &str = "2026-05-23T00:00:00.000Z";

fn goal(overrides: impl FnOnce(&mut UlwLoopItem)) -> UlwLoopItem {
    let mut value = UlwLoopItem {
        id: "G001".into(), title: "Build auth service".into(), objective: "Implement JWT auth endpoint".into(),
        status: "pending".into(), success_criteria: seed_default_success_criteria(0, "Implement JWT auth endpoint", None),
        attempt: 0, created_at: NOW.into(), updated_at: NOW.into(), started_at: None, completed_at: None,
        failed_at: None, review_blocked_at: None, evidence: None, failure_reason: None, steering_status: None,
        superseded_by: None, supersedes: None, blocked_reason: None, blocker_signature: None,
        blocker_occurrence_count: None, required_external_decision: None, non_retriable: None,
        steering_evidence: None, steering_rationale: None,
    };
    overrides(&mut value); value
}

fn plan(goals: Vec<UlwLoopItem>) -> UlwLoopPlan {
    UlwLoopPlan { version: 1, revision: None, brief: None, ledger_reset_revision: None,
        evidence_layout_version: Some(2), created_at: NOW.into(), updated_at: NOW.into(),
        brief_path: ".omo/ulw-loop/brief.md".into(), goals_path: ".omo/ulw-loop/goals.json".into(),
        ledger_path: ".omo/ulw-loop/ledger.jsonl".into(), codex_goal_mode: Some("aggregate".into()),
        codex_objective: Some("objective".into()), codex_objective_aliases: None,
        acknowledged_driver_objectives: None, aggregate_completion: None, active_goal_id: None,
        validation_batches: None, goals }
}

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

#[test]
fn test_seed_criteria_ids() { let cs = seed_default_success_criteria(0, "Implement auth endpoint", None); assert_eq!(cs.len(), 3); assert_eq!(cs.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["C001", "C002", "C003"]); }
#[test]
fn test_seed_criteria_models() { let mut models = seed_default_success_criteria(0, "x", None).into_iter().map(|c| c.user_model).collect::<Vec<_>>(); models.sort(); assert_eq!(models, ["edge", "happy", "regression"]); }
#[test]
fn test_seed_criteria_pending() { assert!(seed_default_success_criteria(0, "x", None).iter().all(|c| c.status == "pending" && c.captured_evidence.is_none())); }
#[test]
fn test_seed_criteria_essential() { assert_eq!(seed_default_success_criteria(0, "x", None).iter().map(|c| (c.id.as_str(), c.essential)).collect::<Vec<_>>(), [("C001", Some(true)), ("C002", Some(true)), ("C003", Some(false))]); }
#[test]
fn test_create_plan_files() -> Result<(), Box<dyn std::error::Error>> { let tmp=tempfile::tempdir()?; let brief="- Build JWT auth\n- Add tests"; let p=create_ulw_loop_plan(tmp.path(), &CreateGoalsOptions{brief:brief.into(),goals:None,codex_goal_mode:None,force:None,validation_batches_json:None},None,None)?; assert!(ulw_loop_brief_path(tmp.path(),None).exists()); assert!(ulw_loop_goals_path(tmp.path(),None).exists()); assert!(ulw_loop_ledger_path(tmp.path(),None).exists()); assert_eq!(p.goals.len(),2); assert_eq!(read_ledger(tmp.path(),None).iter().map(|e|e.kind.as_str()).collect::<Vec<_>>(), ["plan_created"]); Ok(()) }
#[test]
fn test_create_plan_seeds_criteria() -> Result<(), Box<dyn std::error::Error>> { let p=create_ulw_loop_plan(tempfile::tempdir()?.path(), &CreateGoalsOptions{brief:"- a\n- b\n- c".into(),goals:None,codex_goal_mode:None,force:None,validation_batches_json:None},None,None)?; assert!(p.goals.iter().all(|g|g.success_criteria.len()>=3)); Ok(()) }
#[test]
fn test_create_refuses_overwrite() -> Result<(), Box<dyn std::error::Error>> { let tmp=tempfile::tempdir()?; let o=CreateGoalsOptions{brief:"first".into(),goals:None,codex_goal_mode:None,force:None,validation_batches_json:None}; create_ulw_loop_plan(tmp.path(),&o,None,None)?; let err=create_ulw_loop_plan(tmp.path(),&CreateGoalsOptions{brief:"second".into(),..o},None,None).unwrap_err(); assert!(err.to_string().contains("Refusing to overwrite")); Ok(()) }
#[test]
fn test_create_aggregate_default() -> Result<(), Box<dyn std::error::Error>> { let p=create_ulw_loop_plan(tempfile::tempdir()?.path(), &CreateGoalsOptions{brief:"Ship feature".into(),goals:None,codex_goal_mode:None,force:None,validation_batches_json:None},None,None)?; assert_eq!(p.codex_goal_mode.as_deref(),Some("aggregate")); assert!(p.codex_objective.unwrap().contains("goals.json")); Ok(()) }
#[test]
fn test_derive_bullets() { assert_eq!(derive_goal_candidates("# Brief\n\n- Build auth\n- Add tests"), vec![("Build auth".into(),"Build auth".into()),("Add tests".into(),"Add tests".into())]); }
#[test]
fn test_derive_paragraphs() { assert_eq!(derive_goal_candidates("First objective.\n\nSecond objective.").into_iter().map(|(_,o)|o).collect::<Vec<_>>(), ["First objective.","Second objective."]); }
#[test]
fn test_derive_cjk_objective_truncates_on_utf16_units() {
    let objective = "한".repeat(90);
    let candidates = derive_goal_candidates(&objective);
    assert_eq!(candidates[0].0, format!("{}...", "한".repeat(69)));
    let criteria = seed_default_success_criteria(0, &objective, None);
    assert_eq!(criteria[0].scenario, format!("happy path for: {}...", "한".repeat(77)));
    let short = "한".repeat(72);
    assert_eq!(derive_goal_candidates(&short)[0].0, short);
}
#[test]
fn test_derive_requires_marker_whitespace_and_splits_blank_lines() {
    let brief = "Intro paragraph.\r\n \r\n---\n**Bold** line\n-flag line\n2025 roadmap. Next";
    assert_eq!(derive_goal_candidates(brief).into_iter().map(|(_, o)| o).collect::<Vec<_>>(),
        ["Intro paragraph.", "--- **Bold** line -flag line 2025 roadmap. Next"]);
    assert_eq!(derive_goal_candidates("1) First\n2. Second\n+ Third").into_iter().map(|(_, o)| o).collect::<Vec<_>>(),
        ["First", "Second", "Third"]);
}
#[test]
fn test_derive_uses_js_whitespace_bom_and_nel() {
    assert_eq!(derive_goal_candidates("\u{FEFF}- Build auth\n- Add tests").into_iter().map(|(_, o)| o).collect::<Vec<_>>(), ["Build auth", "Add tests"]);
    assert_eq!(derive_goal_candidates("one\u{85}two").into_iter().map(|(_, o)| o).collect::<Vec<_>>(), ["one\u{85}two"]);
}
#[test]
fn test_derive_empty() { assert_eq!(derive_goal_candidates(" \n\t "), vec![("Complete the requested project objective.".into(),"Complete the requested project objective.".into())]); }
#[test]
fn test_add_goal_seeds_criteria() -> Result<(),Box<dyn std::error::Error>> { let tmp=tempfile::tempdir()?; create_ulw_loop_plan(tmp.path(),&CreateGoalsOptions{brief:"Build auth".into(),goals:None,codex_goal_mode:None,force:None,validation_batches_json:None},None,None)?; let r=add_ulw_loop_goal(tmp.path(),&AddGoalOptions{title:"Add rate limit".into(),objective:"Throttle login".into(),evidence:None,success_criteria:None},None,None)?; assert_eq!(r.goal.id,"G002-add-rate-limit"); assert_eq!(r.goal.success_criteria.len(),3); Ok(()) }
#[test]
fn test_add_goal_ledger() -> Result<(),Box<dyn std::error::Error>> { let tmp=tempfile::tempdir()?; create_ulw_loop_plan(tmp.path(),&CreateGoalsOptions{brief:"Build auth".into(),goals:None,codex_goal_mode:None,force:None,validation_batches_json:None},None,None)?; add_ulw_loop_goal(tmp.path(),&AddGoalOptions{title:"Add rate limit".into(),objective:"Throttle login".into(),evidence:None,success_criteria:None},None,None)?; assert_eq!(read_ledger(tmp.path(),None).iter().map(|e|e.kind.as_str()).collect::<Vec<_>>(),["plan_created","goal_added"]); Ok(()) }
#[test]
fn test_resume_active_goal() -> Result<(),Box<dyn std::error::Error>> { let tmp=tempfile::tempdir()?; let p=create_ulw_loop_plan(tmp.path(),&CreateGoalsOptions{brief:"- First\n- Second".into(),goals:None,codex_goal_mode:None,force:None,validation_batches_json:None},None,None)?; let mut active=p.goals[1].clone(); active.status="in_progress".into(); write_plan(tmp.path(), plan(vec![goal(|g|g.id="G001-first".into()),active.clone()]),None)?; match start_next_ulw_loop(tmp.path(),None,None)? { StartNextResult::Started{goal,resumed,..}=>{assert_eq!(goal.id,active.id);assert!(resumed)}, _=>panic!("expected active")}; Ok(()) }
#[test]
fn test_retry_failed_goal() -> Result<(),Box<dyn std::error::Error>> { let tmp=tempfile::tempdir()?; let g=goal(|g|{g.status="failed".into();g.failure_reason=Some("flake".into())}); write_plan(tmp.path(),plan(vec![g]),None)?; match start_next_ulw_loop(tmp.path(),Some(&StartNextOptions{retry_failed:Some(true)}),None)? { StartNextResult::Started{goal,..}=>assert_eq!(goal.attempt,1), _=>panic!("expected retry")}; assert_eq!(read_ledger(tmp.path(),None).iter().map(|e|e.kind.as_str()).collect::<Vec<_>>(),["goal_retried","goal_started"]); Ok(()) }
#[test]
fn test_done_when_no_eligible_goals() -> Result<(),Box<dyn std::error::Error>> { let tmp=tempfile::tempdir()?; let g=goal(|g|g.status="complete".into()); write_plan(tmp.path(),plan(vec![g]),None)?; assert!(matches!(start_next_ulw_loop(tmp.path(),None,None)?,StartNextResult::Done{..})); Ok(()) }
#[test]
fn test_summary_all_statuses() { let statuses=["pending","in_progress","complete","failed","blocked","review_blocked","needs_user_decision"]; let mut gs=Vec::new(); for (i,s) in statuses.iter().enumerate(){gs.push(goal(|g|{g.id=format!("G{i:03}");g.status=(*s).into(); if *s=="blocked"{g.steering_status=Some("blocked".into())} else if *s=="needs_user_decision"{g.steering_status=Some("superseded".into())}}));} let s=summarize_ulw_loop_plan(&plan(gs)); assert_eq!((s.total,s.pending,s.in_progress,s.complete,s.failed,s.blocked,s.review_blocked,s.needs_user_decision,s.superseded),(7,1,1,1,1,1,1,1,1)); }
#[test]
fn test_summary_criteria() { let mut a=goal(|_|{}); a.success_criteria=vec![seed_default_success_criteria(0,"x",None)[0].clone(),seed_default_success_criteria(0,"x",None)[1].clone()]; a.success_criteria[0].status="pass".into(); let mut b=goal(|g|g.id="G002".into()); b.success_criteria=vec![seed_default_success_criteria(0,"x",None)[0].clone(),seed_default_success_criteria(0,"x",None)[1].clone(),seed_default_success_criteria(0,"x",None)[2].clone()]; b.success_criteria[0].status="fail".into(); b.success_criteria[1].status="blocked".into(); let c=summarize_ulw_loop_plan(&plan(vec![a,b])).criteria; assert_eq!((c.total,c.pass,c.pending,c.fail,c.blocked),(5,1,2,1,1)); }
