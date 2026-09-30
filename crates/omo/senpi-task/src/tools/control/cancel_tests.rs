//! `tools/control/cancel.test.ts`

use std::sync::Arc;

use pretty_assertions::assert_eq;

use crate::manager::manager_tests::fakes::{Harness, base_spec, default_manager, started};
use crate::manager::types::ManagerStartSpec;
use crate::tools::control::cancel::{
    TaskCancelDeps, TaskCancelInput, TaskCancelTool, create_task_cancel_tool, run_task_cancel,
};
use crate::tools::control::send::run_task_send;
use crate::tools::control::send_schema::{TaskSendInput, TaskSendMessage};
use crate::tools::control::types::{CancelManager, CancelResultDetails, SendResultDetails};

fn start_running(harness: &Harness) -> String {
    let spec = ManagerStartSpec {
        parent_session_id: "p1".to_string(),
        ..base_spec()
    };
    let task = started(harness.manager.start(&spec));
    harness.in_process.wait_handle(&task.task_id);
    task.task_id
}

fn status_str(harness: &Harness, task_id: &str) -> Option<&'static str> {
    harness.manager.get(task_id).map(|record| record.status.as_str())
}

fn cancel_by_id(harness: &Harness, task_id: &str) -> crate::tools::control::types::CancelToolResult {
    run_task_cancel(
        &harness.manager,
        &TaskCancelInput {
            task_id: Some(task_id.to_string()),
            ..TaskCancelInput::default()
        },
    )
}

#[test]
fn given_the_task_cancel_tool_when_created_then_it_exposes_custom_call_and_result_renderers() {
    let harness = default_manager();

    let tool = create_task_cancel_tool(TaskCancelDeps {
        manager: Arc::new(harness.manager.clone()) as Arc<dyn CancelManager>,
    });

    // Rust analogue of `typeof tool.renderCall === "function"`: the methods exist and resolve.
    let _render_call = TaskCancelTool::render_call;
    let _render_result = TaskCancelTool::render_result;
    assert_eq!(tool.name, "task_cancel");
}

#[test]
fn given_a_running_child_when_cancelled_with_a_reason_then_the_exact_post_state_is_reported() {
    let harness = default_manager();
    let task_id = start_running(&harness);

    let result = run_task_cancel(
        &harness.manager,
        &TaskCancelInput {
            task_id: Some(task_id.clone()),
            reason: Some("no longer needed".to_string()),
            ..TaskCancelInput::default()
        },
    );

    assert_eq!(result.details.kind(), "cancelled");
    let CancelResultDetails::Cancelled {
        previous_status,
        status,
        ..
    } = &result.details
    else {
        panic!("expected cancelled");
    };
    assert_eq!(previous_status.as_str(), "running");
    assert_eq!(status.as_str(), "cancelled");
    assert_eq!(status_str(&harness, &task_id), Some("cancelled"));
}

#[test]
fn given_a_cancelled_child_when_task_send_follows_up_then_the_child_is_not_continuable() {
    let harness = default_manager();
    let task_id = start_running(&harness);
    let cancelled = cancel_by_id(&harness, &task_id);
    assert_eq!(cancelled.details.kind(), "cancelled");

    let result = run_task_send(
        &harness.manager,
        &TaskSendInput {
            to: task_id.clone(),
            message: Some(TaskSendMessage::Plain("continue anyway".to_string())),
            team_run_id: None,
            summary: None,
            all_scope: None,
        },
        Some("p1"),
        None,
    )
    .expect("send succeeds");

    assert_eq!(result.details.kind(), "not_continuable");
    assert!(matches!(
        result.details,
        SendResultDetails::NotContinuable { .. }
    ));
    assert_eq!(status_str(&harness, &task_id), Some("cancelled"));
}

#[test]
fn given_an_already_cancelled_child_when_cancelled_again_then_it_is_a_noop_with_the_cancelled_status() {
    let harness = default_manager();
    let task_id = start_running(&harness);
    cancel_by_id(&harness, &task_id);

    let result = cancel_by_id(&harness, &task_id);

    assert_eq!(result.details.kind(), "noop");
    let CancelResultDetails::Noop { status, .. } = &result.details else {
        panic!("expected noop");
    };
    assert_eq!(status.as_str(), "cancelled");
}

#[test]
fn given_an_unknown_id_when_cancelled_then_not_found_is_returned() {
    let harness = default_manager();

    let result = cancel_by_id(&harness, "st_deadbeef");

    assert_eq!(result.details.kind(), "not_found");
}

#[test]
fn given_no_identifier_when_cancelled_then_invalid_arguments_is_returned() {
    let harness = default_manager();

    let result = run_task_cancel(&harness.manager, &TaskCancelInput::default());

    assert_eq!(result.details.kind(), "invalid_arguments");
}

#[test]
fn given_the_task_cancel_tool_when_reading_its_description_then_it_names_the_terminal_contract_without_stale_revive_wording()
 {
    let harness = default_manager();

    let description = create_task_cancel_tool(TaskCancelDeps {
        manager: Arc::new(harness.manager.clone()) as Arc<dyn CancelManager>,
    })
    .description;

    assert!(description.contains("NOT resumable"));
    assert!(!description.contains("task_interrupt"));
    assert!(!description.to_lowercase().contains("revive"));
}
