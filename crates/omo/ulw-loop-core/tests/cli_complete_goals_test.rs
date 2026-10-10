#![allow(clippy::unwrap_used)]
use tempfile::tempdir;
use ulw_loop_core::command_types::CreateGoalsOptions;
use ulw_loop_core::sdk::factory::AgentToolkit;
use ulw_loop_core::sdk::types::{CompleteGoalsArgs, ToolkitContext, ToolkitResponse};
use ulw_loop_core::surface::resolve_toolkit_surface;

fn toolkit(dir: &tempfile::TempDir) -> AgentToolkit { AgentToolkit::new(ToolkitContext { cwd: dir.path().to_string_lossy().into_owned(), session_id: "cli-test".into(), surface: resolve_toolkit_surface() }, None).unwrap() }
fn create(dir: &tempfile::TempDir) { let r = toolkit(dir).create_goals(&CreateGoalsOptions { brief: "- Goal A\n- Goal B".into(), goals: None, codex_goal_mode: Some("aggregate".into()), force: Some(false), validation_batches_json: None }); assert!(matches!(r, ToolkitResponse::Success { .. })); }

#[test]
fn complete_goals_starts_next_goal_with_instruction() {
    let dir = tempdir().unwrap(); create(&dir);
    match toolkit(&dir).complete_goals(Some(&CompleteGoalsArgs { retry_failed: false })) { ToolkitResponse::Success { result, .. } => match result { ulw_loop_core::plan_crud::StartNextResult::Started { goal, .. } => assert_eq!(goal.status, "in_progress"), _ => panic!("goal did not start") }, _ => panic!("complete-goals failed") }
}

#[test]
fn complete_goals_resumes_without_second_ledger_entry() {
    let dir = tempdir().unwrap(); create(&dir); let args = CompleteGoalsArgs { retry_failed: false };
    let _ = toolkit(&dir).complete_goals(Some(&args)); let before = std::fs::read_to_string(dir.path().join(".omo/ulw-loop/cli-test/ledger.jsonl")).unwrap();
    match toolkit(&dir).complete_goals(Some(&args)) { ToolkitResponse::Success { result: ulw_loop_core::plan_crud::StartNextResult::Started { resumed, .. }, .. } => assert!(resumed), _ => panic!("resume failed") }
    let after = std::fs::read_to_string(dir.path().join(".omo/ulw-loop/cli-test/ledger.jsonl")).unwrap(); assert_eq!(before, after);
}


