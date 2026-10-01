//! `tools/task/integration.test.ts`

use std::sync::Arc;

use pretty_assertions::assert_eq;

use crate::manager::TaskManager;
use crate::manager::manager_tests::fakes::{HarnessOptions, default_manager, lock, make_manager};
use crate::manager::types::{
    ChildPlanner, ManagerStartSpec, PlanResolutionCode, PlanResolutionError, ResolvedChildPlan,
};
use crate::state::TaskStatus;
use crate::tools::task::execute::build_task_execute;
use crate::tools::task::execute_batch::TaskToolResult;
use crate::tools::task::execute_single::TaskExecuteDeps;
use crate::tools::task::execute_spec::TaskToolDeps;
use crate::tools::task::foreground_wait::{ForegroundWaitOptions, TaskToolContext};
use crate::tools::task::spawn_policy::SpawnPolicyDeps;
use crate::tools::task::types::SkillResolution;
use crate::tools::task::validation::SpawnParamsInput;

/// No extra spawn policy: the TS deps pass none, so every default policy hook applies.
struct NoPolicy;

impl SpawnPolicyDeps for NoPolicy {}

fn ctx() -> TaskToolContext {
    TaskToolContext {
        cwd: "/work/project".to_string(),
        session_id: "parent-session-1".to_string(),
        ..TaskToolContext::default()
    }
}

fn tool_deps() -> TaskToolDeps {
    TaskToolDeps {
        omo_config: Default::default(),
        agents: Default::default(),
        load_skills: Some(Arc::new(|_names: &[String], _cwd: &str| {
            SkillResolution::default()
        })),
        ..TaskToolDeps::default()
    }
}

fn run(manager: &TaskManager, call_id: &str, params: &SpawnParamsInput) -> TaskToolResult {
    let tool = tool_deps();
    let policy = NoPolicy;
    let deps = TaskExecuteDeps {
        manager,
        tool: &tool,
        policy: &policy,
    };
    let execute = build_task_execute(deps, ForegroundWaitOptions::default());
    execute
        .execute(call_id, params, None, None, &ctx())
        .expect("execute succeeds")
}

#[test]
fn given_a_background_spawn_when_driven_end_to_end_then_the_engine_start_api_persists_a_running_record_and_returns_its_st_id()
 {
    // given the real manager + real store (no raw store writes from the tool layer)
    let harness = default_manager();

    // when
    let result = run(
        &harness.manager,
        "call-1",
        &SpawnParamsInput {
            prompt: Some("explore".to_string()),
            category: Some("quick".to_string()),
            run_in_background: Some(true),
            ..SpawnParamsInput::default()
        },
    );

    // then the tool drove manager.start with the caller session, and the record landed in the store
    let task_id = result.details.task_id.clone();
    assert!(task_id.starts_with("st_"), "unexpected task id {task_id}");
    let specs = harness.in_process.specs();
    let first = specs.first().expect("a started spec");
    assert_eq!(first.parent_session_id, "parent-session-1");
    assert_eq!(first.prompt, "explore");
    let record = harness
        .store
        .load(&task_id)
        .expect("store readable")
        .expect("expected a persisted record");
    assert_eq!(record.status, TaskStatus::Running);
    assert_eq!(record.category.as_deref(), Some("quick"));
}

#[test]
fn given_both_targets_when_driven_then_the_engine_start_api_is_never_reached() {
    // given
    let harness = default_manager();

    // when
    let result = run(
        &harness.manager,
        "call-2",
        &SpawnParamsInput {
            prompt: Some("p".to_string()),
            category: Some("quick".to_string()),
            subagent_type: Some("momus".to_string()),
            ..SpawnParamsInput::default()
        },
    );

    // then
    assert_eq!(result.details.status, "invalid_arguments");
    assert_eq!(lock(&harness.in_process.started_specs).len(), 0);
}

#[test]
fn given_an_unknown_category_when_the_planner_rejects_it_then_the_tool_reports_the_available_categories()
 {
    // given a planner that rejects the target with the available list
    let planner: ChildPlanner = Arc::new(
        |_spec: &ManagerStartSpec| -> Result<ResolvedChildPlan, Box<PlanResolutionError>> {
            let mut error = PlanResolutionError::new(
                PlanResolutionCode::UnknownTarget,
                "Category \"ghost\" not found",
            );
            error.available_categories = Some(vec!["quick".to_string(), "deep".to_string()]);
            Err(Box::new(error))
        },
    );
    let harness = make_manager(HarnessOptions {
        planner: Some(planner),
        ..HarnessOptions::default()
    });

    // when
    let result = run(
        &harness.manager,
        "call-3",
        &SpawnParamsInput {
            prompt: Some("p".to_string()),
            category: Some("ghost".to_string()),
            ..SpawnParamsInput::default()
        },
    );

    // then
    assert_eq!(result.details.status, "plan_error");
    let text = result
        .content
        .first()
        .filter(|content| content.kind == "text")
        .map(|content| content.text.clone())
        .unwrap_or_default();
    assert!(text.contains("quick"), "missing quick in {text}");
    assert!(text.contains("deep"), "missing deep in {text}");
}
