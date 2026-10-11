use std::fs;

use tempfile::tempdir;
use ulw_loop_core::cli_arg_parser::read_value;
use ulw_loop_core::cli_output::ULW_LOOP_HELP;
use ulw_loop_core::domain_types::UlwLoopPlan;
use ulw_loop_core::paths::UlwLoopScope;
use ulw_loop_core::sdk::factory::AgentToolkit;
use ulw_loop_core::sdk::types::ToolkitContext;
use ulw_loop_core::surface::resolve_toolkit_surface;
use ulw_loop_core::cli_commands::is_ulw_loop_subcommand;

fn toolkit(dir: &tempfile::TempDir) -> AgentToolkit {
    AgentToolkit::new(ToolkitContext {
        cwd: dir.path().to_string_lossy().into_owned(),
        session_id: "cli-test".into(),
        surface: resolve_toolkit_surface(),
    }, None).expect("valid test toolkit")
}

fn create_plan(dir: &tempfile::TempDir) -> UlwLoopPlan {
    let response = toolkit(dir).create_goals(&ulw_loop_core::command_types::CreateGoalsOptions {
        brief: "- Goal A\n- Goal B".into(), goals: None, codex_goal_mode: Some("aggregate".into()),
        force: Some(false), validation_batches_json: None,
    });
    match response { ulw_loop_core::sdk::types::ToolkitResponse::Success { result, .. } => result,
        ulw_loop_core::sdk::types::ToolkitResponse::Failure { error, .. } => panic!("{}", error.message) }
}

#[test]
fn prints_usage_when_no_subcommand() { assert!(ULW_LOOP_HELP.contains("mhc ulw-loop")); }

#[test]
fn status_summary_includes_criteria_counts() {
    let dir = tempdir().unwrap(); let plan = create_plan(&dir);
    let summary = ulw_loop_core::plan_crud::summarize_ulw_loop_plan(&plan);
    assert_eq!(summary.criteria.total, 6); assert_eq!(summary.criteria.pass, 0);
}

#[test]
fn records_evidence_and_returns_updated_criterion() {
    let dir = tempdir().unwrap(); let plan = create_plan(&dir);
    let response = toolkit(&dir).record_evidence(&ulw_loop_core::command_types::RecordEvidenceOptions {
        goal_id: plan.goals[0].id.clone(), criterion_id: "C001".into(), status: "pass".into(),
        evidence: "curl passed".into(), notes: None, artifacts: None,
    });
    match response { ulw_loop_core::sdk::types::ToolkitResponse::Success { result, .. } => {
        assert_eq!(result.criterion.id, "C001"); assert_eq!(result.criterion.status, "pass");
        assert_eq!(result.criterion.captured_evidence.as_deref(), Some("curl passed"));
    }, _ => panic!("record-evidence failed") }
}

#[test]
fn unknown_goal_returns_error() {
    let dir = tempdir().unwrap(); let _ = create_plan(&dir);
    let response = toolkit(&dir).record_evidence(&ulw_loop_core::command_types::RecordEvidenceOptions {
        goal_id: "G404".into(), criterion_id: "C001".into(), status: "pass".into(), evidence: "x".into(), notes: None, artifacts: None,
    });
    assert!(matches!(response, ulw_loop_core::sdk::types::ToolkitResponse::Failure { .. }));
}

#[test]
fn missing_flags_are_rejected() { assert!(read_value(&["--criterion-id".into()], "--goal-id").is_none()); }

#[test]
fn criteria_lists_goal_criteria() {
    let dir = tempdir().unwrap(); let plan = create_plan(&dir);
    match toolkit(&dir).criteria(&plan.goals[0].id) { ulw_loop_core::sdk::types::ToolkitResponse::Success { result, .. } => {
        assert_eq!(result.goal_id, "G001-goal-a"); assert_eq!(result.criteria[0].id, "C001"); assert_eq!(result.criteria[0].user_model, "happy");
    }, _ => panic!("criteria failed") }
}

#[test]
fn criteria_json_shape_has_goal_id_and_first_criterion() {
    let dir = tempdir().unwrap(); let plan = create_plan(&dir);
    match toolkit(&dir).criteria(&plan.goals[0].id) { ulw_loop_core::sdk::types::ToolkitResponse::Success { result, .. } => {
        let json = serde_json::to_value(result).unwrap(); assert_eq!(json["goalId"], "G001-goal-a"); assert_eq!(json["criteria"][0]["id"], "C001");
    }, _ => panic!("criteria failed") }
}

#[test]
fn steer_dispatches_to_steering_engine() {
    let dir = tempdir().unwrap(); let _ = create_plan(&dir);
    let proposal = ulw_loop_core::steering_types::UlwLoopSteeringProposal { kind: "add_subgoal".into(), source: "cli".into(), evidence: "user requested it".into(), rationale: "keeps plan accurate".into(), title: Some("Extra".into()), objective: Some("Do extra".into()), ..Default::default() };
    let result = ulw_loop_core::steering::steer_ulw_loop(dir.path(), &proposal, Some(&UlwLoopScope::new("cli-test")), None).unwrap();
    assert!(result.accepted); assert_eq!(result.plan.goals.len(), 3); assert_eq!(result.plan.goals[2].title, "Extra");
}

#[test]
fn add_goal_appends_pending_goal() {
    let dir = tempdir().unwrap(); let _ = create_plan(&dir);
    let result = toolkit(&dir).add_goal(&ulw_loop_core::command_types::AddGoalOptions { title: "Later".into(), objective: "Do later".into(), evidence: None, success_criteria: None });
    match result { ulw_loop_core::sdk::types::ToolkitResponse::Success { result, .. } => { assert_eq!(result.goal.title, "Later"); assert_eq!(result.goal.status, "pending"); }, _ => panic!("add-goal failed") }
}

#[test]
fn unknown_subcommand_is_not_vocabulary() { assert!(!is_ulw_loop_subcommand("wat")); }

#[test]
fn status_without_plan_returns_error() {
    let dir = tempdir().unwrap(); let response = toolkit(&dir).status();
    assert!(matches!(response, ulw_loop_core::sdk::types::ToolkitResponse::Failure { .. }));
}

#[test]
fn status_error_does_not_create_stdout_artifacts() {
    let dir = tempdir().unwrap(); let _ = toolkit(&dir).status();
    assert!(!dir.path().join(".omo/ulw-loop/cli-test/goals.json").exists());
    assert!(fs::read_dir(dir.path()).unwrap().next().is_none());
}
