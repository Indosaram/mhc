//! `tools/control/renderers-result.test.ts`

use pretty_assertions::assert_eq;

use crate::state::TaskStatus;
use crate::tools::control::renderers::{
    ToolRenderResultOptions, render_task_cancel_result, render_task_send_result,
};
use crate::tools::control::tool_result::tool_result;
use crate::tools::control::types::{
    CancelResultDetails, SendResultDetails, ShutdownFailedCode, ShutdownOperation,
};
use crate::tools::render::{LinesComponent, RendererTheme, ThemeColor};
use crate::tools::team::messaging::TeamSendDetails;

/// Renders a theme color the way senpi names it (e.g. `Accent` -> `accent`, `ToolTitle` -> `toolTitle`).
fn color_name(color: ThemeColor) -> String {
    let debug = format!("{color:?}");
    let mut chars = debug.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

struct TestTheme;

impl RendererTheme for TestTheme {
    fn fg(&self, color: ThemeColor, text: &str) -> String {
        let name = color_name(color);
        format!("[{name}]{text}[/{name}]")
    }

    fn italic(&self, text: &str) -> String {
        format!("<i>{text}</i>")
    }
}

struct AnsiTheme;

impl RendererTheme for AnsiTheme {
    fn fg(&self, _color: ThemeColor, text: &str) -> String {
        format!("\u{1b}[33m{text}\u{1b}[0m")
    }

    fn italic(&self, text: &str) -> String {
        format!("\u{1b}[3m{text}\u{1b}[0m")
    }
}

const RESULT_OPTIONS: ToolRenderResultOptions = ToolRenderResultOptions {
    expanded: false,
    is_partial: false,
};

fn first_line(component: &dyn LinesComponent, width: usize) -> String {
    component.render(width).into_iter().next().unwrap_or_default()
}

fn expect_no_terminal_controls(value: &str) {
    let found = value
        .chars()
        .any(|c| ('\u{0}'..='\u{1f}').contains(&c) || ('\u{7f}'..='\u{9f}').contains(&c));
    assert!(!found, "unexpected terminal control in {value:?}");
}

fn assert_send_case(details: SendResultDetails, expected: &str) {
    let component = render_task_send_result(&tool_result("ok", details), &RESULT_OPTIONS, &TestTheme);
    let line = first_line(&component, 120);
    assert_eq!(line, expected);
}

#[test]
fn given_the_steered_task_send_result_mapping_when_rendering_then_the_exact_themed_row_is_stable() {
    assert_send_case(
        SendResultDetails::Steered {
            task_id: "st_1".to_string(),
            status: TaskStatus::Running,
            delivered: "steer".to_string(),
        },
        "[accent]task_send delivered st_1 as steer (running)[/accent]",
    );
}

#[test]
fn given_the_revived_task_send_result_mapping_when_rendering_then_the_exact_themed_row_is_stable() {
    assert_send_case(
        SendResultDetails::Revived {
            task_id: "st_1".to_string(),
            run_epoch: 2,
        },
        "[success]task_send revived st_1 epoch 2[/success]",
    );
}

#[test]
fn given_the_queued_task_send_result_mapping_when_rendering_then_the_exact_themed_row_is_stable() {
    assert_send_case(
        SendResultDetails::Queued {
            task_id: "st_1".to_string(),
            queue_position: 3,
        },
        "[muted]task_send queued st_1 position 3[/muted]",
    );
}

#[test]
fn given_the_not_continuable_task_send_result_mapping_when_rendering_then_the_exact_themed_row_is_stable() {
    assert_send_case(
        SendResultDetails::NotContinuable {
            task_id: "st_1".to_string(),
            reason: "Task is cancelled.".to_string(),
            suggestion: "Start a new task.".to_string(),
        },
        "[warning]task_send not continuable st_1: Task is cancelled. Start a new task.[/warning]",
    );
}

#[test]
fn given_the_scope_denied_task_send_result_mapping_when_rendering_then_the_exact_themed_row_is_stable() {
    assert_send_case(
        SendResultDetails::ScopeDenied {
            task_id: "st_1".to_string(),
            owning_session_id: "owner".to_string(),
            reason: "Denied.".to_string(),
        },
        "[error]task_send denied st_1 owner:owner[/error]",
    );
}

#[test]
fn given_the_one_shot_agent_task_send_result_mapping_when_rendering_then_the_exact_themed_row_is_stable() {
    assert_send_case(
        SendResultDetails::OneShotAgent {
            task_id: "st_1".to_string(),
            agent: "momus".to_string(),
            message: "reminder".to_string(),
        },
        "[error]task_send denied st_1 one-shot:momus[/error]",
    );
}

#[test]
fn given_the_not_found_task_send_result_mapping_when_rendering_then_the_exact_themed_row_is_stable() {
    assert_send_case(
        SendResultDetails::NotFound {
            reason: "No task.".to_string(),
            known_tasks: vec!["alpha".to_string()],
        },
        "[error]task_send not found: No task. known:alpha[/error]",
    );
}

#[test]
fn given_the_invalid_arguments_task_send_result_mapping_when_rendering_then_the_exact_themed_row_is_stable() {
    assert_send_case(
        SendResultDetails::InvalidArguments {
            reason: "message is required".to_string(),
        },
        "[error]task_send invalid: message is required[/error]",
    );
}

#[test]
fn given_the_team_message_task_send_result_mapping_when_rendering_then_the_exact_themed_row_is_stable() {
    assert_send_case(
        SendResultDetails::TeamMessage {
            team: TeamSendDetails::ToLead {
                message_id: "msg-1".to_string(),
            },
        },
        "[success]task_send team message msg-1 enqueued to lead[/success]",
    );
}

#[test]
fn given_the_shutdown_requested_task_send_result_mapping_when_rendering_then_the_exact_themed_row_is_stable() {
    assert_send_case(
        SendResultDetails::ShutdownRequested {
            team_run_id: "team-1".to_string(),
            member: "atlas".to_string(),
        },
        "[warning]task_send shutdown requested team-1 member:atlas[/warning]",
    );
}

#[test]
fn given_the_shutdown_responded_task_send_result_mapping_when_rendering_then_the_exact_themed_row_is_stable() {
    assert_send_case(
        SendResultDetails::ShutdownResponded {
            team_run_id: "team-1".to_string(),
            member: "atlas".to_string(),
            approved: false,
        },
        "[warning]task_send shutdown rejected team-1 member:atlas[/warning]",
    );
}

#[test]
fn given_the_shutdown_failed_task_send_result_mapping_when_rendering_then_the_exact_themed_row_is_stable() {
    assert_send_case(
        SendResultDetails::ShutdownFailed {
            operation: ShutdownOperation::Reject,
            team_run_id: "team-1".to_string(),
            member: "atlas".to_string(),
            code: ShutdownFailedCode::TeamStateMissing,
            reason: "Team state is unavailable.".to_string(),
        },
        "[error]task_send shutdown reject failed team-1 member:atlas: Team state is unavailable.[/error]",
    );
}

#[test]
fn given_a_structured_shutdown_failure_when_rendering_the_result_then_it_shows_concise_safe_context_with_the_error_theme()
{
    let details = SendResultDetails::ShutdownFailed {
        operation: ShutdownOperation::Approve,
        team_run_id: "team-9".to_string(),
        member: "atlas".to_string(),
        code: ShutdownFailedCode::TeamStateMissing,
        reason: "Team state is unavailable.".to_string(),
    };
    let component = render_task_send_result(&tool_result("safe", details), &RESULT_OPTIONS, &TestTheme);
    let line = first_line(&component, 120);

    assert_eq!(
        line,
        "[error]task_send shutdown approve failed team-9 member:atlas: Team state is unavailable.[/error]"
    );
    assert!(!line.contains("ENOENT"));
    assert!(!line.contains("/private/secret"));
    assert!(!line.contains("state.json"));
}

#[test]
fn given_injected_controls_in_task_send_and_task_cancel_results_when_rendered_then_dynamic_controls_are_removed_before_trusted_theme_styling()
 {
    // given
    let send_details = SendResultDetails::InvalidArguments {
        reason: "잘못됨 \u{1b}[31m빨강\u{1b}[0m\u{7}".to_string(),
    };
    let cancel_details = CancelResultDetails::NotFound {
        reason: "없음 \u{1b}]8;;https://example.com\u{1b}\\링크\u{1b}]8;;\u{1b}\\\u{7f}".to_string(),
    };

    // when
    let send = first_line(
        &render_task_send_result(
            &tool_result("ignored", send_details.clone()),
            &RESULT_OPTIONS,
            &AnsiTheme,
        ),
        120,
    );
    let cancel = first_line(
        &render_task_cancel_result(
            &tool_result("ignored", cancel_details.clone()),
            &RESULT_OPTIONS,
            &AnsiTheme,
        ),
        120,
    );
    let plain_send = first_line(
        &render_task_send_result(&tool_result("ignored", send_details), &RESULT_OPTIONS, &TestTheme),
        120,
    );
    let plain_cancel = first_line(
        &render_task_cancel_result(&tool_result("ignored", cancel_details), &RESULT_OPTIONS, &TestTheme),
        120,
    );

    // then
    assert!(send.starts_with("\u{1b}[33m"), "send line: {send:?}");
    assert!(cancel.starts_with("\u{1b}[33m"), "cancel line: {cancel:?}");
    assert!(!send.contains("\u{1b}[31m"));
    assert!(!cancel.contains("https://example.com"));
    expect_no_terminal_controls(&plain_send);
    expect_no_terminal_controls(&plain_cancel);
}
