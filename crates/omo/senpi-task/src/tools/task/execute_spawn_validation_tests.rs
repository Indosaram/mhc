//! `tools/task/execute-spawn-validation.test.ts`

use std::sync::Arc;

use pretty_assertions::assert_eq;

use crate::manager::manager_tests::fakes::{HarnessOptions, default_manager, make_manager};
use crate::manager::types::{ListScope, PlanResolutionCode, PlanResolutionError};
use crate::tools::task::execute::build_task_execute;
use crate::tools::task::execute_batch::TaskToolResult;
use crate::tools::task::execute_single::TaskExecuteDeps;
use crate::tools::task::execute_spec::{TaskAncestry, TaskToolDeps};
use crate::tools::task::foreground_wait::{ForegroundWaitOptions, TaskToolContext};
use crate::tools::task::validation::SpawnParamsInput;

/// `CTX`: the tool context used by every case (parent session `parent-1`).
fn ctx() -> TaskToolContext {
    TaskToolContext {
        session_id: "parent-1".to_string(),
        cwd: "/tmp".into(),
        ..TaskToolContext::default()
    }
}

fn first_text(result: &TaskToolResult) -> String {
    result
        .content
        .first()
        .filter(|content| content.kind == "text")
        .map(|content| content.text.clone())
        .unwrap_or_default()
}

fn params(prompt: &str) -> SpawnParamsInput {
    SpawnParamsInput {
        prompt: Some(prompt.to_string()),
        ..SpawnParamsInput::default()
    }
}

/// `makeDeps(manager)`: deps with default tool/policy seams.
fn make_deps<'a>(manager: &'a crate::manager::TaskManager, tool: &'a TaskToolDeps) -> TaskExecuteDeps<'a> {
    TaskExecuteDeps {
        manager,
        tool,
        policy: tool,
    }
}

#[test]
fn given_both_category_and_subagent_type_when_executed_then_it_returns_the_xor_error_result_without_spawning()
 {
    let harness = default_manager();
    let tool = TaskToolDeps::default();
    let deps = make_deps(&harness.manager, &tool);
    let execute = build_task_execute(deps, ForegroundWaitOptions::default());

    let input = SpawnParamsInput {
        category: Some("quick".to_string()),
        subagent_type: Some("momus".to_string()),
        ..params("p")
    };
    let result = execute
        .execute("c", &input, None, None, &ctx())
        .expect("execute");

    assert!(harness.manager.list(&ListScope::All).is_empty());
    assert_eq!(result.details.status, "invalid_arguments");
    assert!(first_text(&result).contains("EITHER category OR subagent_type"));
}

#[test]
fn given_an_unknown_category_when_executed_then_it_returns_the_category_listing_plan_error() {
    // The default test planner accepts every category; the real production planner rejects an
    // unknown one before ever starting a child. Mirror that rejection here so the spawn never
    // reaches a runner (matching the TS fake manager, which resolves `start` to `plan_unresolved`
    // without spawning).
    let planner: crate::manager::types::ChildPlanner = Arc::new(|spec| {
        if spec.category.as_deref() == Some("nope") {
            let mut error = PlanResolutionError::new(
                PlanResolutionCode::UnknownTarget,
                "Category \"nope\" not found",
            );
            error.available_categories = Some(vec!["quick".to_string(), "deep".to_string()]);
            return Err(Box::new(error));
        }
        Ok(crate::manager::types::ResolvedChildPlan {
            model: "anthropic/claude".to_string(),
            category: spec.category.clone(),
            agent_type: spec.subagent_type.clone(),
            ..crate::manager::types::ResolvedChildPlan::default()
        })
    });
    let harness = make_manager(HarnessOptions {
        planner: Some(planner),
        ..HarnessOptions::default()
    });
    let tool = TaskToolDeps::default();
    let deps = make_deps(&harness.manager, &tool);
    let execute = build_task_execute(deps, ForegroundWaitOptions::default());

    let input = SpawnParamsInput {
        category: Some("nope".to_string()),
        ..params("p")
    };
    let result = execute
        .execute("c", &input, None, None, &ctx())
        .expect("execute");

    assert_eq!(result.details.status, "plan_error");
    let text = first_text(&result);
    assert!(text.contains("quick"));
    assert!(text.contains("deep"));
}

#[test]
fn given_an_injected_ancestry_when_spawning_then_child_depth_and_root_derive_from_it() {
    // The injected ancestry reports depth 2, so the child depth is 3; the real manager enforces
    // `max_depth`, unlike the TS fake, so this harness needs enough headroom to admit it.
    let harness = make_manager(HarnessOptions {
        config: Some(crate::manager::manager_tests::fakes::config(5, 3)),
        ..HarnessOptions::default()
    });
    let tool = TaskToolDeps {
        resolve_ancestry: Some(Arc::new(|_session_id: &str| {
            Some(TaskAncestry {
                depth: 2,
                root_session_id: "root-session".to_string(),
            })
        })),
        ..TaskToolDeps::default()
    };
    let deps = make_deps(&harness.manager, &tool);
    let execute = build_task_execute(deps, ForegroundWaitOptions::default());

    let input = SpawnParamsInput {
        category: Some("quick".to_string()),
        run_in_background: Some(true),
        ..params("p")
    };
    let result = execute
        .execute("c", &input, None, None, &ctx())
        .expect("execute");

    let record = harness
        .manager
        .get(&result.details.task_id)
        .expect("spawned record");
    assert_eq!(record.depth, 3);
    assert_eq!(record.root_session_id, "root-session");
}

#[test]
fn given_category_with_model_when_executed_then_it_returns_the_exclusivity_error_without_spawning()
{
    let harness = default_manager();
    let tool = TaskToolDeps::default();
    let deps = make_deps(&harness.manager, &tool);
    let execute = build_task_execute(deps, ForegroundWaitOptions::default());

    let input = SpawnParamsInput {
        category: Some("architect".to_string()),
        model: Some("quotio-openai/gpt-5.6-luna-fast".to_string()),
        ..params("p")
    };
    let result = execute
        .execute("c", &input, None, None, &ctx())
        .expect("execute");

    assert!(harness.manager.list(&ListScope::All).is_empty());
    assert_eq!(result.details.status, "invalid_arguments");
    assert!(first_text(&result).contains("omo.json"));
}
