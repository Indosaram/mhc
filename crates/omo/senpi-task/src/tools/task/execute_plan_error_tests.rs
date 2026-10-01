//! `tools/task/execute-plan-error.test.ts`

use std::sync::Arc;

use pretty_assertions::assert_eq;

use crate::manager::TaskManager;
use crate::manager::create_task_manager;
use crate::manager::types::{
    ChildPlanner, ManagedRunner, ManagedRunnerResult, ManagedRunners, ManagedStartSpec,
    PlanResolutionCode, PlanResolutionError, TaskManagerOptions,
};
use crate::store::TaskRecordStore;
use crate::tools::task::execute::build_task_execute;
use crate::tools::task::execute_batch::TaskToolResult;
use crate::tools::task::execute_single::TaskExecuteDeps;
use crate::tools::task::execute_spec::TaskToolDeps;
use crate::tools::task::foreground_wait::{ForegroundWaitOptions, TaskToolContext};
use crate::tools::task::validation::SpawnParamsInput;

/// `CTX`: the parent tool context used by the TS fixtures.
fn ctx() -> TaskToolContext {
    TaskToolContext {
        session_id: "parent-1".to_string(),
        cwd: "/tmp".into(),
        ..TaskToolContext::default()
    }
}

/// A runner that must never be reached: planning fails before any spawn happens.
struct UnreachableRunner;

impl ManagedRunner for UnreachableRunner {
    fn start(&self, _spec: &ManagedStartSpec) -> ManagedRunnerResult {
        panic!("runner must not be invoked when plan resolution fails")
    }
}

/// `createFakeManager({ start })` where `start` always resolves to `plan_unresolved`: the concrete
/// Rust manager is driven into that outcome by a planner that always fails with `error`.
fn plan_error_manager(error: PlanResolutionError) -> (tempfile::TempDir, TaskManager) {
    let dir = tempfile::tempdir().expect("tempdir");
    let planner: ChildPlanner = Arc::new(move |_| Err(Box::new(error.clone())));
    let store = TaskRecordStore::new(&crate::store::StateDirConfig {
        project_dir: dir.path().to_path_buf(),
        task_state_dir: None,
    });
    let runners = ManagedRunners {
        in_process: Arc::new(UnreachableRunner),
        process: Arc::new(UnreachableRunner),
    };
    let options = TaskManagerOptions::new(
        store,
        runners,
        planner,
        dir.path().to_string_lossy().into_owned(),
    );
    let manager = create_task_manager(options);
    (dir, manager)
}

fn run(manager: &TaskManager, call_id: &str, params: &SpawnParamsInput) -> TaskToolResult {
    let policy = TaskToolDeps::default();
    let tool = TaskToolDeps::default();
    let deps = TaskExecuteDeps {
        manager,
        policy: &policy,
        tool: &tool,
    };
    let execute = build_task_execute(deps, ForegroundWaitOptions::default());
    execute
        .execute(call_id, params, None, None, &ctx())
        .expect("execute should not fail")
}

fn first_text(result: &TaskToolResult) -> String {
    result
        .content
        .first()
        .filter(|content| content.kind == "text")
        .map(|content| content.text.clone())
        .unwrap_or_default()
}

#[test]
fn given_unknown_target_with_active_agents_and_categories_when_executed_then_both_roster_suffixes_are_rendered()
 {
    // given
    let mut error = PlanResolutionError::new(
        PlanResolutionCode::UnknownTarget,
        "Target \"nope\" not found.",
    );
    error.available_agents = Some(vec!["explore".to_string(), "momus".to_string()]);
    error.available_categories = Some(vec!["deep".to_string(), "quick".to_string()]);
    let (_dir, manager) = plan_error_manager(error);

    // when
    let params = SpawnParamsInput {
        prompt: Some("p".to_string()),
        subagent_type: Some("nope".to_string()),
        ..SpawnParamsInput::default()
    };
    let result = run(&manager, "call-plan-error", &params);

    // then
    assert_eq!(
        first_text(&result),
        "Target \"nope\" not found. Available agents: explore, momus. Available categories: deep, quick."
    );
}

#[test]
fn given_model_unavailable_plan_error_when_executed_then_suffix_says_valid_category_names_with_omo_json_config_hint()
 {
    // given
    let mut error = PlanResolutionError::new(
        PlanResolutionCode::ModelUnavailable,
        "No available model for category \"quick\" (attempted opengateway/glm-5.2-ultrafast).",
    );
    error.available_categories = Some(vec!["deep".to_string(), "quick".to_string()]);
    let (_dir, manager) = plan_error_manager(error);

    // when
    let params = SpawnParamsInput {
        prompt: Some("p".to_string()),
        category: Some("quick".to_string()),
        ..SpawnParamsInput::default()
    };
    let result = run(&manager, "call-model-unavailable", &params);

    // then
    let text = first_text(&result);
    assert!(text.contains("Valid category names: deep, quick"), "{text}");
    assert!(text.contains("omo.json"), "{text}");
    assert!(!text.contains("Pass model:"), "{text}");
    assert!(!text.contains("Available categories:"), "{text}");
}
