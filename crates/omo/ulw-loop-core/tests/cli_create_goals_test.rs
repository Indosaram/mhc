#![allow(clippy::unwrap_used)]
use std::fs;
use tempfile::tempdir;
use ulw_loop_core::command_types::CreateGoalsOptions;
use ulw_loop_core::goal_status::aggregate_codex_objective_for_scope;
use ulw_loop_core::paths::UlwLoopScope;
use ulw_loop_core::sdk::factory::AgentToolkit;
use ulw_loop_core::sdk::types::{CompleteGoalsArgs, ToolkitContext, ToolkitResponse};
use ulw_loop_core::surface::resolve_toolkit_surface;

fn tk(dir: &tempfile::TempDir, id: &str) -> AgentToolkit { AgentToolkit::new(ToolkitContext { cwd: dir.path().to_string_lossy().into_owned(), session_id: id.into(), surface: resolve_toolkit_surface() }, None).unwrap() }
fn opts(brief: &str) -> CreateGoalsOptions { CreateGoalsOptions { brief: brief.into(), goals: None, codex_goal_mode: Some("aggregate".into()), force: Some(false), validation_batches_json: None } }

#[test]
fn creates_plan_artifacts_and_seeded_criteria() { let d=tempdir().unwrap(); match tk(&d,"cli-test").create_goals(&opts("- Goal A\n- Goal B")) { ToolkitResponse::Success { result, .. } => { assert_eq!(result.goals[0].success_criteria[0].id,"C001"); assert!(fs::read_to_string(d.path().join(".omo/ulw-loop/cli-test/brief.md")).unwrap().contains("Goal A")); assert!(fs::read_to_string(d.path().join(".omo/ulw-loop/cli-test/goals.json")).unwrap().contains("successCriteria")); assert!(fs::read_to_string(d.path().join(".omo/ulw-loop/cli-test/ledger.jsonl")).unwrap().contains("plan_created")); }, _=>panic!("create failed") } }

#[test]
fn completed_default_aggregate_requires_fresh_session() { let d=tempdir().unwrap(); let t=tk(&d,"cli-test"); let _=t.create_goals(&opts("- Finished")); let r=t.complete_goals(Some(&CompleteGoalsArgs{retry_failed:false})); assert!(matches!(r,ToolkitResponse::Success{..})); assert!(aggregate_codex_objective_for_scope(Some(&UlwLoopScope::new("cli-test"))).contains("cli-test")); }

#[test]
fn active_v2_plan_exposes_attempt_directory() { let d=tempdir().unwrap(); let t=tk(&d,"cli-test"); let _=t.create_goals(&opts("- Goal A\n- Goal B")); let _=t.complete_goals(Some(&CompleteGoalsArgs{retry_failed:false})); match t.status(){ToolkitResponse::Success{result,..}=>assert_eq!(result.current_attempt_dir.as_deref(),Some(".omo/evidence/ulw/cli-test/G001-goal-a/a1")), _=>panic!("status failed")} }

#[test]
fn session_ids_write_isolated_plans() { let d=tempdir().unwrap(); let _=tk(&d,"session-A").create_goals(&opts("- Alpha")); let _=tk(&d,"session-B").create_goals(&opts("- Beta")); assert!(fs::read_to_string(d.path().join(".omo/ulw-loop/session-A/goals.json")).unwrap().contains("Alpha")); assert!(fs::read_to_string(d.path().join(".omo/ulw-loop/session-B/goals.json")).unwrap().contains("Beta")); }

#[test]
fn thread_scope_normalizes_to_thread_id() { let d=tempdir().unwrap(); let _=tk(&d,"thread-123").create_goals(&opts("- Thread scoped")); assert!(fs::read_to_string(d.path().join(".omo/ulw-loop/thread-123/goals.json")).unwrap().contains("Thread scoped")); }

#[test]
fn explicit_session_wins_over_thread_scope() { let d=tempdir().unwrap(); let _=tk(&d,"manual-456").create_goals(&opts("- Manual scoped")); assert!(d.path().join(".omo/ulw-loop/manual-456/goals.json").exists()); }

#[test]
fn sessions_do_not_read_each_others_state() { let d=tempdir().unwrap(); let _=tk(&d,"session-A").create_goals(&opts("- Session A run")); match tk(&d,"session-B").status(){ToolkitResponse::Failure{error,..}=>assert_eq!(error.code,"ULW_LOOP_PLAN_MISSING"), _=>panic!("foreign plan visible")} }

#[test]
fn empty_session_id_is_rejected_without_plan() { let d=tempdir().unwrap(); assert!(AgentToolkit::new(ToolkitContext{cwd:d.path().to_string_lossy().into_owned(),session_id:"".into(),surface:resolve_toolkit_surface()},None).is_err()); assert!(fs::read_dir(d.path()).unwrap().next().is_none()); }

#[test]
fn missing_session_id_is_rejected_without_plan() { let d=tempdir().unwrap(); assert!(AgentToolkit::new(ToolkitContext{cwd:d.path().to_string_lossy().into_owned(),session_id:"   ".into(),surface:resolve_toolkit_surface()},None).is_err()); assert!(fs::read_dir(d.path()).unwrap().next().is_none()); }

