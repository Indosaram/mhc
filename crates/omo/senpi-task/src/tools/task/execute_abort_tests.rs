//! `tools/task/execute-abort.test.ts`

use pretty_assertions::assert_eq;

use crate::manager::manager_tests::fakes::{default_manager, wait_terminal};
use crate::manager::{AbortSignal, TaskManager};
use crate::state::TaskStatus;
use crate::tools::task::execute::build_task_execute;
use crate::tools::task::execute_batch::TaskToolResult;
use crate::tools::task::execute_single::TaskExecuteDeps;
use crate::tools::task::execute_spec::TaskToolDeps;
use crate::tools::task::foreground_wait::{ForegroundWaitOptions, TaskToolContext};
use crate::tools::task::types::TaskToolMode;
use crate::tools::task::validation::SpawnParamsInput;

fn ctx() -> TaskToolContext {
    TaskToolContext {
        session_id: "parent-1".to_string(),
        cwd: "/tmp".into(),
        ..Default::default()
    }
}

fn params(run_in_background: Option<bool>) -> SpawnParamsInput {
    SpawnParamsInput {
        prompt: Some("work".to_string()),
        category: Some("quick".to_string()),
        run_in_background,
        ..Default::default()
    }
}

fn run(
    manager: &TaskManager,
    call_id: &str,
    input: &SpawnParamsInput,
    signal: Option<&AbortSignal>,
) -> TaskToolResult {
    let tool = TaskToolDeps::default();
    let deps = TaskExecuteDeps {
        manager,
        policy: manager,
        tool: &tool,
    };
    let execute = build_task_execute(deps, ForegroundWaitOptions::default());
    let context = ctx();
    let Ok(result) = execute.execute(call_id, input, signal, None, &context) else {
        panic!("execute failed");
    };
    result
}

/// Spins (no sleeping) until a resident task has a foreground waiter, returning its id.
fn wait_for_waiter(manager: &TaskManager) -> String {
    loop {
        if let Some(id) = manager
            .resident_task_ids()
            .into_iter()
            .find(|id| manager.waiter_count(id) > 0)
        {
            return id;
        }
        std::thread::yield_now();
    }
}

/// Spins (no sleeping) until a resident task exists, returning its id.
fn wait_for_resident(manager: &TaskManager) -> String {
    loop {
        if let Some(id) = manager.resident_task_ids().into_iter().next() {
            return id;
        }
        std::thread::yield_now();
    }
}

fn first_text(result: &TaskToolResult) -> String {
    result
        .content
        .first()
        .filter(|c| c.kind == "text")
        .map(|c| c.text.clone())
        .unwrap_or_default()
}

#[test]
fn w2sig_given_pre_aborted_signal_when_spawn_executes_then_returns_cancelled_without_starting_child()
{
    // given
    let harness = default_manager();
    let signal = AbortSignal::default();
    signal.abort();

    // when
    let result = run(
        &harness.manager,
        "call-pre-abort",
        &params(Some(true)),
        Some(&signal),
    );

    // then
    assert!(harness.manager.resident_task_ids().is_empty());
    assert_eq!(result.details.task_id, "");
    assert_eq!(result.details.status, "cancelled");
    assert!(matches!(result.details.mode, TaskToolMode::Spawn));
    assert_eq!(
        result.details.reason.as_deref(),
        Some("Parent aborted before spawn")
    );
}

#[test]
fn w2sig_given_sync_child_waiting_when_parent_aborts_then_cancels_once_and_reports_task_id() {
    // given
    let harness = default_manager();
    let signal = AbortSignal::default();
    let input = params(None);

    let (result, task_id) = std::thread::scope(|scope| {
        let execution =
            scope.spawn(|| run(&harness.manager, "call-mid-abort", &input, Some(&signal)));
        let task_id = wait_for_waiter(&harness.manager);

        // when
        signal.abort();
        let Ok(result) = execution.join() else {
            panic!("execution thread panicked");
        };
        (result, task_id)
    });

    // then
    assert_eq!(result.details.status, "cancelled");
    assert_eq!(result.details.task_id, task_id);
    assert!(first_text(&result).contains(&task_id));
    let record = harness.manager.get(&task_id).expect("record");
    assert_eq!(record.status, TaskStatus::Cancelled);
}

#[test]
fn w2sig_given_child_terminal_during_parent_abort_when_cancellation_is_noop_then_still_returns_cancelled()
 {
    // given
    let harness = default_manager();
    let signal = AbortSignal::default();
    let input = params(None);

    let (result, task_id) = std::thread::scope(|scope| {
        let execution =
            scope.spawn(|| run(&harness.manager, "call-terminal-race", &input, Some(&signal)));
        let task_id = wait_for_waiter(&harness.manager);

        // when
        signal.abort();
        let Ok(result) = execution.join() else {
            panic!("execution thread panicked");
        };
        (result, task_id)
    });
    // a late completion after the abort is a noop for the already-terminal record
    harness.in_process.wait_handle(&task_id).complete("done");

    // then
    assert_eq!(result.details.status, "cancelled");
    assert_eq!(result.details.task_id, task_id);
    let record = harness.manager.get(&task_id).expect("record");
    assert_eq!(record.status, TaskStatus::Cancelled);
}

#[test]
fn w2sig_given_background_child_started_when_parent_aborts_then_child_survives_without_cancellation()
{
    // given
    let harness = default_manager();
    let signal = AbortSignal::default();
    let result = run(
        &harness.manager,
        "call-background-abort",
        &params(Some(true)),
        Some(&signal),
    );

    // when
    signal.abort();

    // then
    assert_eq!(result.details.status, "running");
    assert_eq!(result.details.run_in_background, Some(true));
    let task_id = result.details.task_id.clone();
    let record = harness.manager.get(&task_id).expect("record");
    assert!(record.status != TaskStatus::Cancelled);

    harness.in_process.wait_handle(&task_id).complete("done");
    let settled = wait_terminal(&harness.manager, &task_id);
    assert_eq!(settled.status, TaskStatus::Completed);
}

#[test]
fn w2sig_given_sync_spawn_without_signal_when_child_completes_then_foreground_behavior_unchanged() {
    // given
    let harness = default_manager();
    let input = params(None);

    // when
    let (result, task_id) = std::thread::scope(|scope| {
        let execution = scope.spawn(|| run(&harness.manager, "call-no-signal", &input, None));
        let task_id = wait_for_resident(&harness.manager);
        harness.in_process.wait_handle(&task_id).complete("done");
        let Ok(result) = execution.join() else {
            panic!("execution thread panicked");
        };
        (result, task_id)
    });

    // then
    assert_eq!(result.details.status, "completed");
    assert_eq!(result.details.task_id, task_id);
    assert!(first_text(&result).contains("done"));
    let record = harness.manager.get(&task_id).expect("record");
    assert_eq!(record.status, TaskStatus::Completed);
}
