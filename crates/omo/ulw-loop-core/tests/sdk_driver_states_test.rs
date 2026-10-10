use serde_json::json;
use ulw_loop_core::command_types::{CreateGoalsOptions, RecordEvidenceOptions};
use ulw_loop_core::plan_io::read_ulw_loop_plan;
use ulw_loop_core::paths::UlwLoopScope;
use ulw_loop_core::sdk::{AgentToolkit, CheckpointArgs, ToolkitContext, ToolkitResponse};
use ulw_loop_core::surface::UlwLoopToolkitSurface;

fn toolkit(dir: &tempfile::TempDir, session: &str) -> AgentToolkit {
    AgentToolkit::new(ToolkitContext { cwd: dir.path().display().to_string(), session_id: session.into(), surface: UlwLoopToolkitSurface::OmoSenpi }, None).expect("valid context")
}

fn seeded(session: &str, brief: &str) -> (tempfile::TempDir, String, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let tk = toolkit(&dir, session);
    let created = tk.create_goals(&CreateGoalsOptions { brief: brief.into(), goals: None, codex_goal_mode: None, force: None, validation_batches_json: None });
    assert!(matches!(created, ToolkitResponse::Success { .. }));
    let started = tk.complete_goals(None);
    let (goal_id, criteria) = match started { ToolkitResponse::Success { result: ulw_loop_core::plan_crud::StartNextResult::Started { goal, .. }, .. } => (goal.id, goal.success_criteria), _ => panic!("goal did not start") };
    for criterion in criteria {
        let result = tk.record_evidence(&RecordEvidenceOptions { goal_id: goal_id.clone(), criterion_id: criterion.id, status: "pass".into(), evidence: "driver-state fixture proof".into(), notes: None, artifacts: None });
        assert!(matches!(result, ToolkitResponse::Success { .. }));
    }
    let plan = read_ulw_loop_plan(dir.path(), Some(&UlwLoopScope { session_id: Some(session.into()), ..Default::default() })).expect("plan");
    (dir, goal_id, plan.codex_objective.unwrap_or_default())
}

fn close(dir: &tempfile::TempDir, session: &str, goal_id: &str, snapshot: Option<serde_json::Value>) -> ToolkitResponse<serde_json::Value> {
    toolkit(dir, session).checkpoint(&CheckpointArgs { goal_id: Some(goal_id.into()), status: Some("complete".into()), evidence: Some("driver-state checkpoint".into()), codex_goal_json: snapshot.map(|v| v.to_string()), quality_gate_json: None, print_template: None })
}

#[test]
fn no_driver_snapshot_completes_and_advises_create_goal() { let (dir, goal, _) = seeded("driver-absent", "- alpha goal\n- beta goal"); let result = close(&dir, "driver-absent", &goal, None); match result { ToolkitResponse::Success { next_actions, .. } => assert!(next_actions.join(" ").contains("create_goal")), _ => panic!("checkpoint failed") } }

#[test]
fn active_driver_matching_objective_has_no_difference_warning() { let (dir, goal, objective) = seeded("driver-active", "- alpha goal\n- beta goal"); let result = close(&dir, "driver-active", &goal, Some(json!({"goal":{"objective":objective,"status":"active"}}))); match result { ToolkitResponse::Success { next_actions, warnings, .. } => { let all = [warnings, next_actions].concat().join(" "); assert!(!all.contains("driver_objective_differs")); }, _ => panic!("checkpoint failed") } }

#[test]
fn completed_driver_advises_recreating_driver() { let (dir, goal, objective) = seeded("driver-early", "- alpha goal\n- beta goal"); let result = close(&dir, "driver-early", &goal, Some(json!({"goal":{"objective":objective,"status":"complete"}}))); match result { ToolkitResponse::Success { next_actions, .. } => assert!(next_actions.join(" ").contains("create_goal")), _ => panic!("checkpoint failed") } }

#[test]
fn limited_driver_statuses_advise_resume() { for status in ["paused", "usage_limited", "budget_limited"] { let (dir, goal, objective) = seeded(&format!("driver-{status}"), "- alpha goal\n- beta goal"); let result = close(&dir, &format!("driver-{status}"), &goal, Some(json!({"goal":{"objective":objective,"status":status}}))); match result { ToolkitResponse::Success { next_actions, .. } => assert!(next_actions.join(" ").contains("/goal resume")), _ => panic!("checkpoint failed for {status}") } } }

#[test]
fn differing_driver_objective_warns_without_rejection_or_action() { let (dir, goal, _) = seeded("driver-differs", "- alpha goal\n- beta goal"); let result = close(&dir, "driver-differs", &goal, Some(json!({"goal":{"objective":"a different objective","status":"active"}}))); match result { ToolkitResponse::Success { next_actions, warnings, .. } => { assert!(warnings.join(" ").contains("driver_objective_differs")); assert!(!next_actions.join(" ").contains("driver_objective_differs")); assert!(!next_actions.join(" ").contains("create_goal")); }, _ => panic!("checkpoint rejected") } }

#[test]
fn differing_driver_objective_is_acknowledged_once() { let (dir, first, _) = seeded("driver-differs-once", "- alpha goal\n- beta goal\n- gamma goal"); let driver = json!({"goal":{"objective":"  a different\n objective  ","status":"active"}}); let first_result = close(&dir, "driver-differs-once", &first, Some(driver.clone())); assert!(matches!(first_result, ToolkitResponse::Success { .. })); let second = match toolkit(&dir, "driver-differs-once").complete_goals(None) { ToolkitResponse::Success { result: ulw_loop_core::plan_crud::StartNextResult::Started { goal, .. }, .. } => goal, _ => panic!("second goal did not start") }; for criterion in &second.success_criteria { assert!(matches!(toolkit(&dir, "driver-differs-once").record_evidence(&RecordEvidenceOptions { goal_id: second.id.clone(), criterion_id: criterion.id.clone(), status: "pass".into(), evidence: "proof".into(), notes: None, artifacts: None }), ToolkitResponse::Success { .. })); } let second_result = close(&dir, "driver-differs-once", &second.id, Some(driver)); match second_result { ToolkitResponse::Success { warnings, next_actions, .. } => { assert!(warnings.is_empty()); assert!(next_actions.is_empty()); }, _ => panic!("second checkpoint failed") } let plan = read_ulw_loop_plan(&dir, Some(&UlwLoopScope { session_id: Some("driver-differs-once".into()), ..Default::default() })).expect("plan"); assert_eq!(plan.acknowledged_driver_objectives, Some(vec!["a different objective".into()])); }

#[test]
fn malformed_snapshot_returns_typed_error_and_leaves_goal_open() { let (dir, goal, _) = seeded("driver-malformed", "- alpha goal\n- beta goal"); assert!(matches!(close(&dir, "driver-malformed", &goal, None), ToolkitResponse::Success { .. })); let (dir2, goal2, _) = seeded("driver-malformed-2", "- alpha goal\n- beta goal"); match toolkit(&dir2, "driver-malformed-2").checkpoint(&ulw_loop_core::sdk::CheckpointArgs { goal_id: Some(goal2), status: Some("complete".into()), evidence: Some("driver-state checkpoint".into()), codex_goal_json: Some("not json and not a path".into()), quality_gate_json: None, print_template: None }) { ToolkitResponse::Failure { error, .. } => assert_eq!(error.code, "ULW_LOOP_CODEX_GOAL_JSON_INVALID"), _ => panic!("malformed snapshot accepted") } let plan = read_ulw_loop_plan(&dir2, Some(&UlwLoopScope { session_id: Some("driver-malformed-2".into()), ..Default::default() })).expect("plan"); assert_ne!(plan.goals[0].status, "complete"); }
