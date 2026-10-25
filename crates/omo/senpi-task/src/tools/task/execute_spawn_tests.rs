//! `tools/task/execute-spawn.test.ts`
//!
//! The Rust `TaskManager` is concrete, so the TS scripted fake manager is replaced by the shared
//! manager test harness (`default_manager`). Assertions that captured the `ManagerStartSpec` in TS
//! read the persisted task record instead, and foreground runs complete the child from a scoped
//! thread once it becomes resident.

use pretty_assertions::assert_eq;

use crate::manager::TaskManager;
use crate::manager::manager_tests::fakes::default_manager;
use crate::tools::task::execute::build_task_execute;
use crate::tools::task::execute_batch::TaskToolResult;
use crate::tools::task::execute_single::TaskExecuteDeps;
use crate::tools::task::execute_spec::TaskToolDeps;
use crate::tools::task::foreground_wait::{ForegroundWaitOptions, TaskToolContext};
use crate::tools::task::validation::SpawnParamsInput;

const PARENT_SESSION: &str = "parent-session-1";

#[test]
fn requested_kernel_tools_reach_the_resolved_start_hook_before_spawn() {
    let harness = default_manager();
    let manager = harness.manager.clone();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = seen.clone();
    let tool = TaskToolDeps {
        start: Some(std::sync::Arc::new(move |spec, names| {
            manager.start_prepared(spec, |_, plan, mode| {
                captured.lock().expect("capture lock").push((
                    names.to_vec(),
                    plan.model.clone(),
                    mode,
                ));
                Err("grant refused".into())
            })
        })),
        ..Default::default()
    };
    let mut input = params("lookup", "quick", Some(true));
    input.tools = Some(vec!["lookup".into()]);
    let result = build_task_execute(
        TaskExecuteDeps {
            manager: &harness.manager,
            tool: &tool,
            policy: &tool,
        },
        ForegroundWaitOptions::default(),
    )
    .execute("kernel-grant", &input, None, None, &ctx())
    .expect("execute");
    let seen = seen.lock().expect("capture lock");
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0, vec!["lookup".to_owned()]);
    assert!(!seen[0].1.is_empty());
    assert_eq!(result.details.status, "plan_error");
    assert!(harness.manager.resident_task_ids().is_empty());
}

/// `CTX` from the TS fixtures.
fn ctx() -> TaskToolContext {
    TaskToolContext {
        session_id: PARENT_SESSION.to_string(),
        cwd: std::env::temp_dir().to_string_lossy().into_owned(),
        ..TaskToolContext::default()
    }
}

fn params(prompt: &str, category: &str, run_in_background: Option<bool>) -> SpawnParamsInput {
    SpawnParamsInput {
        prompt: Some(prompt.to_string()),
        category: Some(category.to_string()),
        run_in_background,
        ..SpawnParamsInput::default()
    }
}

/// `makeDeps(manager)` + `buildTaskExecute(..)` + a single `execute(..)` call.
fn run_execute(manager: &TaskManager, call_id: &str, input: &SpawnParamsInput) -> TaskToolResult {
    let tool = TaskToolDeps::default();
    let deps = TaskExecuteDeps {
        manager,
        tool: &tool,
        policy: &tool,
    };
    let execute = build_task_execute(deps, ForegroundWaitOptions::default());
    execute
        .execute(call_id, input, None, None, &ctx())
        .expect("execute should succeed")
}

/// Runs a foreground execute while a helper thread completes the first resident child.
fn run_foreground(
    manager: &TaskManager,
    call_id: &str,
    input: &SpawnParamsInput,
    complete: impl Fn(&str) + Send + Sync,
) -> TaskToolResult {
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let task_id = loop {
                if let Some(id) = manager.resident_task_ids().into_iter().next() {
                    break id;
                }
                std::thread::yield_now();
            };
            complete(&task_id);
        });
        run_execute(manager, call_id, input)
    })
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
fn given_run_in_background_true_when_executed_then_it_returns_immediately_without_awaiting_child_completion()
 {
    let harness = default_manager();

    let result = run_execute(
        &harness.manager,
        "call-1",
        &params("explore", "quick", Some(true)),
    );

    assert_eq!(harness.manager.waiter_count(&result.details.task_id), 0);
    assert!(!result.details.task_id.is_empty());
    assert_eq!(result.details.status, "running");
    assert_eq!(result.details.run_in_background, Some(true));
    assert_eq!(
        result.content.first().map(|content| content.kind.as_str()),
        Some("text")
    );
}

#[test]
fn given_a_background_task_when_the_start_result_is_rendered_then_it_directs_the_parent_to_yield_instead_of_polling()
 {
    // given
    let harness = default_manager();

    // when
    let result = run_execute(
        &harness.manager,
        "call-background-guidance",
        &params("research", "deep", Some(true)),
    );

    // then
    let normalized = first_text(&result).to_lowercase();
    assert!(
        normalized.contains("automatically delivered"),
        "{normalized}"
    );
    assert!(normalized.contains("end your turn"), "{normalized}");
    assert!(normalized.contains("independent work"), "{normalized}");
    assert!(!normalized.contains("read progress"), "{normalized}");
}

#[test]
fn given_the_caller_session_when_spawning_then_caller_session_id_is_injected_as_parent_session_id() {
    let harness = default_manager();

    let result = run_execute(&harness.manager, "c", &params("p", "quick", Some(true)));

    let record = harness
        .manager
        .get(&result.details.task_id)
        .expect("started task should be recorded");
    assert_eq!(record.parent_session_id, PARENT_SESSION);
    assert_eq!(record.root_session_id, PARENT_SESSION);
    assert_eq!(record.depth, 1);
    assert_eq!(record.category.as_deref(), Some("quick"));
}

#[test]
fn given_a_resolved_background_start_when_executed_then_resolved_metadata_and_background_mode_reach_result_details()
 {
    // given
    let harness = default_manager();

    // when
    let result = run_execute(
        &harness.manager,
        "call-resolved-bg",
        &params("sensitive prompt", "quick", Some(true)),
    );

    // then
    let record = harness
        .manager
        .get(&result.details.task_id)
        .expect("started task should be recorded");
    assert_eq!(result.details.resolved_model, record.resolved_model);
    assert_eq!(result.details.run_in_background, Some(true));
    assert!(!first_text(&result).contains("sensitive prompt"));
}

#[test]
fn given_config_default_execution_mode_when_spawning_without_an_agent_overlay_then_the_task_starts() {
    let harness = default_manager();

    let result = run_execute(&harness.manager, "c", &params("p", "quick", Some(true)));

    assert_eq!(result.details.status, "running");
    assert!(
        harness
            .manager
            .resident_task_ids()
            .contains(&result.details.task_id)
    );
}

#[test]
fn given_run_in_background_falsy_when_executed_then_it_composes_start_and_wait_for_and_returns_the_final_response_inline()
 {
    let harness = default_manager();

    let result = run_foreground(
        &harness.manager,
        "c",
        &params("p", "quick", None),
        |task_id| {
            harness
                .in_process
                .wait_handle(task_id)
                .complete("THE FINAL ANSWER");
        },
    );

    let text = first_text(&result);
    assert!(text.contains("THE FINAL ANSWER"), "{text}");
    assert!(text.contains(&result.details.task_id), "{text}");
    assert_eq!(result.details.status, "completed");
}

#[test]
fn given_a_resolved_foreground_record_when_execution_completes_then_resolved_metadata_raw_model_and_foreground_mode_reach_details()
 {
    // given
    let harness = default_manager();

    // when
    let result = run_foreground(
        &harness.manager,
        "call-resolved-fg",
        &params("finish", "quick", None),
        |task_id| {
            harness.in_process.wait_handle(task_id).complete("done");
        },
    );

    // then
    let record = harness
        .manager
        .get(&result.details.task_id)
        .expect("completed task should be recorded");
    assert_eq!(result.details.resolved_model, record.resolved_model);
    assert_eq!(result.details.model.as_deref(), Some(record.model.as_str()));
    assert_eq!(result.details.run_in_background, Some(false));
}

#[test]
fn held_single_spawn_returns_without_wait_and_forwards_parent_model() {
    use crate::manager::{start_admission::HeldStart, types::StartResult};
    use crate::tools::task::foreground_wait::ParentModel;
    let harness = default_manager();
    let tool = TaskToolDeps {
        start: Some(std::sync::Arc::new(|spec, _| {
            assert_eq!(spec.parent_model.as_deref(), Some("provider/parent"));
            StartResult::Held(HeldStart {
                kind: "held",
                task_id: "held-one".into(),
                pause_seq: 9,
                root_session_id: "parent".into(),
            })
        })),
        ..Default::default()
    };
    let mut input = params("work", "quick", Some(false));
    input.tools = Some(vec!["lookup".into()]);
    let context = TaskToolContext {
        model: Some(ParentModel {
            provider: "provider".into(),
            id: "parent".into(),
        }),
        ..ctx()
    };
    let result = build_task_execute(
        TaskExecuteDeps {
            manager: &harness.manager,
            tool: &tool,
            policy: &tool,
        },
        ForegroundWaitOptions::default(),
    )
    .execute("held", &input, None, None, &context)
    .expect("execute");
    assert_eq!(result.details.task_id, "held-one");
    assert_eq!(result.details.status, "paused");
    assert_eq!(result.details.pause_seq, Some(9));
    assert!(harness.manager.resident_task_ids().is_empty());
}
