use ulw_loop_core::domain_types::*;
use ulw_loop_core::goal_status::*;

fn make_criterion(id: &str, status: &str, essential: Option<bool>) -> UlwLoopSuccessCriterion {
    UlwLoopSuccessCriterion {
        id: id.into(),
        scenario: "scenario".into(),
        user_model: "happy".into(),
        expected_evidence: "evidence".into(),
        status: status.into(),
        essential,
        captured_evidence: None,
        captured_at: None,
        notes: None,
        artifacts: None,
    }
}

fn make_goal(id: &str, status: &str, criteria: Vec<UlwLoopSuccessCriterion>) -> UlwLoopItem {
    UlwLoopItem {
        id: id.into(),
        title: "Title".into(),
        objective: "Objective".into(),
        status: status.into(),
        success_criteria: criteria,
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
    }
}

fn make_plan(goals: Vec<UlwLoopItem>, active_goal_id: Option<String>) -> UlwLoopPlan {
    UlwLoopPlan {
        version: 1,
        revision: Some(1),
        codex_goal_mode: None,
        brief: Some("brief".into()),
        ledger_reset_revision: None,
        evidence_layout_version: Some(2),
        goals_path: ".omo/ulw-loop/goals.json".into(),
        ledger_path: ".omo/ulw-loop/ledger.jsonl".into(),
        brief_path: ".omo/ulw-loop/brief.md".into(),
        created_at: "2026-05-23T00:00:00Z".into(),
        updated_at: "2026-05-23T00:00:00Z".into(),
        active_goal_id,
        goals,
        codex_objective: None,
        codex_objective_aliases: None,
        acknowledged_driver_objectives: None,
        aggregate_completion: None,
        validation_batches: None,
    }
}

#[test]
fn test_has_all_criteria_pass() {
    let g1 = make_goal("G001", "in_progress", vec![make_criterion("C001", "pass", None), make_criterion("C002", "pass", None)]);
    assert!(has_all_criteria_pass(&g1));

    let g2 = make_goal("G002", "in_progress", vec![make_criterion("C001", "pass", None), make_criterion("C002", "pending", None)]);
    assert!(!has_all_criteria_pass(&g2));
    assert!(!has_all_criteria_pass(&make_goal("G003", "in_progress", vec![])));
    assert!(!has_all_criteria_pass(&make_goal("G004", "in_progress", vec![make_criterion("C001", "pass", None), make_criterion("C002", "fail", None)])));
    assert!(!has_all_criteria_pass(&make_goal("G005", "in_progress", vec![make_criterion("C001", "pass", None), make_criterion("C002", "blocked", None)])));
}

#[test]
fn test_has_essential_criteria_pass() {
    let g1 = make_goal("G001", "in_progress", vec![
        make_criterion("C001", "pass", Some(true)),
        make_criterion("C002", "pending", Some(false)),
    ]);
    assert!(has_essential_criteria_pass(&g1));

    let g2 = make_goal("G002", "in_progress", vec![
        make_criterion("C001", "fail", Some(true)),
        make_criterion("C002", "pass", Some(false)),
    ]);
    assert!(!has_essential_criteria_pass(&g2));
    let missing = make_goal("G003", "in_progress", vec![make_criterion("C001", "pass", None), make_criterion("C002", "pending", None)]);
    assert!(!has_essential_criteria_pass(&missing));
}

#[test]
fn test_is_final_run_completion_candidate() {
    let g1 = make_goal("G001", "complete", vec![]);
    let g2 = make_goal("G002", "in_progress", vec![]);
    let plan = make_plan(vec![g1.clone(), g2.clone()], Some("G002".into()));

    assert!(!is_final_run_completion_candidate(&plan, &g1));
    assert!(is_final_run_completion_candidate(&plan, &g2));
    let two_pending = make_plan(vec![make_goal("G001", "pending", vec![]), make_goal("G002", "pending", vec![])], None);
    assert!(!is_final_run_completion_candidate(&two_pending, &two_pending.goals[0]));
}

#[test]
fn test_is_ulw_loop_done() {
    let g1 = make_goal("G001", "complete", vec![]);
    let mut plan = make_plan(vec![g1], None);
    assert!(is_ulw_loop_done(&plan));

    let pending = make_plan(vec![make_goal("G002", "pending", vec![])], None);
    assert!(!is_ulw_loop_done(&pending));

    let replacement = make_goal("G003", "complete", vec![]);
    let superseded = {
        let mut g = make_goal("G002", "pending", vec![]);
        g.superseded_by = Some(vec!["G003".into()]);
        g.steering_status = Some("superseded".into());
        g
    };
    assert!(is_ulw_loop_done(&make_plan(vec![superseded, replacement], None)));

    plan.aggregate_completion = Some(UlwLoopAggregateCompletion {
        status: "complete".into(),
        completed_at: "now".into(),
        evidence: "ev".into(),
        codex_goal: None,
    });
    assert!(is_ulw_loop_done(&plan));
}

#[test]
fn test_codex_goal_mode_defaults_and_aggregate() {
    let mut plan = make_plan(vec![], None);
    plan.codex_goal_mode = None;
    assert_eq!(codex_goal_mode(&plan), "per_story");
    plan.codex_goal_mode = Some("aggregate".into());
    assert_eq!(codex_goal_mode(&plan), "aggregate");
}

#[test]
fn test_expected_and_aggregate_codex_objectives() {
    let goal = make_goal("G001", "pending", vec![]);
    let mut plan = make_plan(vec![], None);
    plan.codex_goal_mode = Some("aggregate".into());
    plan.codex_objective = Some("aggregate objective".into());
    assert_eq!(expected_codex_objective(&plan, &goal), "aggregate objective");
    assert_eq!(aggregate_codex_objective(&plan), "aggregate objective");
    plan.codex_objective = None;
    assert_eq!(aggregate_codex_objective(&plan), ULW_LOOP_AGGREGATE_CODEX_OBJECTIVE);
    assert!(ULW_LOOP_AGGREGATE_CODEX_OBJECTIVE.contains(".omo/ulw-loop"));
    assert!(!ULW_LOOP_AGGREGATE_CODEX_OBJECTIVE.contains(".omx"));
    plan.codex_goal_mode = Some("per_story".into());
    assert_eq!(expected_codex_objective(&plan, &goal), "Objective");
}

#[test]
fn test_compatible_codex_objectives_include_aliases() {
    let mut plan = make_plan(vec![], None);
    plan.codex_objective = Some("aggregate objective".into());
    plan.codex_objective_aliases = Some(vec!["legacy one".into(), "legacy two".into()]);
    assert_eq!(compatible_codex_objectives(&plan), vec!["aggregate objective", "legacy one", "legacy two"]);
}

#[test]
fn test_first_unresolved_criterion() {
    let pending = make_criterion("C002", "pending", None);
    let goal = make_goal("G001", "pending", vec![make_criterion("C001", "pass", None), pending.clone(), make_criterion("C003", "fail", None)]);
    assert_eq!(first_unresolved_criterion(&goal).map(|c| c.id.as_str()), Some("C002"));
    let all_pass = make_goal("G002", "complete", vec![make_criterion("C001", "pass", None)]);
    assert!(first_unresolved_criterion(&all_pass).is_none());
}

#[test] fn test_done_all_complete() { assert!(is_ulw_loop_done(&make_plan(vec![make_goal("G001", "complete", vec![])], None))); }
#[test] fn test_done_pending_false() { assert!(!is_ulw_loop_done(&make_plan(vec![make_goal("G001", "pending", vec![])], None))); }
#[test] fn test_done_superseded_replacement() { let mut old=make_goal("G001","pending",vec![]); old.steering_status=Some("superseded".into()); old.superseded_by=Some(vec!["G002".into()]); assert!(is_ulw_loop_done(&make_plan(vec![old,make_goal("G002","complete",vec![])],None))); }
#[test] fn test_final_candidate_single_unresolved() { let g=make_goal("G002","pending",vec![]); assert!(is_final_run_completion_candidate(&make_plan(vec![make_goal("G001","complete",vec![]),g.clone()],None),&g)); }
#[test] fn test_final_candidate_multiple_unresolved() { let g=make_goal("G001","pending",vec![]); assert!(!is_final_run_completion_candidate(&make_plan(vec![g.clone(),make_goal("G002","pending",vec![])],None),&g)); }
#[test] fn test_mode_per_story_default() { assert_eq!(codex_goal_mode(&make_plan(vec![],None)),"per_story"); }
#[test] fn test_mode_aggregate_explicit() { let mut p=make_plan(vec![],None); p.codex_goal_mode=Some("aggregate".into()); assert_eq!(codex_goal_mode(&p),"aggregate"); }
#[test] fn test_expected_aggregate_plan_objective() { let mut p=make_plan(vec![],None); p.codex_goal_mode=Some("aggregate".into()); p.codex_objective=Some("aggregate objective".into()); assert_eq!(expected_codex_objective(&p,&make_goal("G001","pending",vec![])),"aggregate objective"); }
#[test] fn test_expected_aggregate_fallback() { let mut p=make_plan(vec![],None); p.codex_goal_mode=Some("aggregate".into()); assert_eq!(expected_codex_objective(&p,&make_goal("G001","pending",vec![])),ULW_LOOP_AGGREGATE_CODEX_OBJECTIVE); }
#[test] fn test_expected_per_story_goal_objective() { let mut p=make_plan(vec![],None); p.codex_goal_mode=Some("per_story".into()); assert_eq!(expected_codex_objective(&p,&make_goal("G001","pending",vec![])),"Objective"); }
#[test] fn test_aggregate_objective_explicit() { let mut p=make_plan(vec![],None); p.codex_objective=Some("x".into()); assert_eq!(aggregate_codex_objective(&p),"x"); }
#[test] fn test_aggregate_objective_fallback() { assert_eq!(aggregate_codex_objective(&make_plan(vec![],None)),ULW_LOOP_AGGREGATE_CODEX_OBJECTIVE); }
#[test] fn test_compatible_objective_aliases() { let mut p=make_plan(vec![],None); p.codex_objective=Some("x".into()); p.codex_objective_aliases=Some(vec!["a".into(),"b".into()]); assert_eq!(compatible_codex_objectives(&p),vec!["x","a","b"]); }
#[test] fn test_all_pass_true() { assert!(has_all_criteria_pass(&make_goal("G", "pending", vec![make_criterion("C","pass",None)]))); }
#[test] fn test_all_pass_pending_false() { assert!(!has_all_criteria_pass(&make_goal("G", "pending", vec![make_criterion("C","pending",None)]))); }
#[test] fn test_all_pass_fail_false() { assert!(!has_all_criteria_pass(&make_goal("G", "pending", vec![make_criterion("C","fail",None)]))); }
