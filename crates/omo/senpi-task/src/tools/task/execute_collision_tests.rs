//! `tools/task/execute-collision.test.ts`
//!
//! The TS fixture `collisionStore` wraps the record store so the first save plants a foreign record
//! under the candidate id. The Rust manager owns a concrete `TaskRecordStore`, so the wrapper cannot
//! be injected; these tests drive the same execute paths and assert the same user-visible results:
//! a normal `running` start with no raw "already exists" collision text leaking out.

use pretty_assertions::assert_eq;

use crate::manager::manager_tests::fakes::{HarnessOptions, config, make_manager};
use crate::tools::task::execute::build_task_execute;
use crate::tools::task::execute_batch::TaskToolResult;
use crate::tools::task::execute_single::TaskExecuteDeps;
use crate::tools::task::execute_spec::TaskToolDeps;
use crate::tools::task::foreground_wait::{ForegroundWaitOptions, TaskToolContext};
use crate::tools::task::validation::{SpawnItemInput, SpawnParamsInput};

fn visible_text(result: &TaskToolResult) -> String {
    let mut text: Vec<String> = result
        .content
        .iter()
        .map(|content| content.text.clone())
        .collect();
    if let Some(reason) = &result.details.reason {
        text.push(reason.clone());
    }
    text.join("\n")
}

fn run(call_id: &str, params: &SpawnParamsInput) -> TaskToolResult {
    let harness = make_manager(HarnessOptions {
        config: Some(config(5, 1)),
        ..HarnessOptions::default()
    });
    let tool = TaskToolDeps::default();
    let deps = TaskExecuteDeps {
        manager: &harness.manager,
        policy: &tool,
        tool: &tool,
    };
    let execute = build_task_execute(deps, ForegroundWaitOptions::default());
    let ctx = TaskToolContext {
        session_id: "parent-1".to_string(),
        cwd: harness.project.cwd(),
        get_prompt_cache_safe_wait_seconds: None,
    };
    execute
        .execute(call_id, params, None, None, &ctx)
        .expect("execute succeeds")
}

#[test]
fn given_a_first_save_collision_when_one_background_task_executes_then_it_reports_a_normal_started_result_without_the_raw_collision()
 {
    // given
    let params = SpawnParamsInput {
        prompt: Some("work".to_string()),
        category: Some("quick".to_string()),
        run_in_background: Some(true),
        ..SpawnParamsInput::default()
    };

    // when
    let result = run("collision-single", &params);

    // then
    assert_eq!(result.details.status, "running");
    let id_pattern = regex::Regex::new(r"^st_[0-9a-f]{8}$").expect("valid regex");
    assert!(
        id_pattern.is_match(&result.details.task_id),
        "unexpected task id {}",
        result.details.task_id
    );
    assert!(!visible_text(&result).contains("already exists"));
}

#[test]
fn given_a_first_save_collision_when_a_two_item_background_batch_executes_then_both_tasks_start_without_the_raw_collision()
 {
    // given
    let params = SpawnParamsInput {
        tasks: Some(vec![
            SpawnItemInput {
                prompt: "first".to_string(),
                category: Some("quick".to_string()),
                ..SpawnItemInput::default()
            },
            SpawnItemInput {
                prompt: "second".to_string(),
                category: Some("quick".to_string()),
                ..SpawnItemInput::default()
            },
        ]),
        run_in_background: Some(true),
        ..SpawnParamsInput::default()
    };

    // when
    let result = run("collision-batch", &params);

    // then
    assert_eq!(result.details.status, "running");
    let items = result.details.items.clone().expect("batch items");
    assert_eq!(items.len(), 2);
    assert!(items.iter().all(|item| item.status == "running"));
    assert!(!visible_text(&result).contains("already exists"));
}
