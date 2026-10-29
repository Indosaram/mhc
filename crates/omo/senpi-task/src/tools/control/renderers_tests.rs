//! `tools/control/renderers.test.ts`

use pretty_assertions::assert_eq;
use regex::Regex;
use unicode_width::UnicodeWidthStr;

use crate::state::TaskStatus;
use crate::tools::control::cancel::TaskCancelInput;
use crate::tools::control::renderers::{
    ToolRenderResultOptions, render_task_cancel_call, render_task_cancel_result, render_task_send_call,
};
use crate::tools::control::send_schema::{StructuredMessageInput, TaskSendInput, TaskSendMessage};
use crate::tools::control::tool_result::tool_result;
use crate::tools::control::types::CancelResultDetails;
use crate::tools::render::{LinesComponent, RendererTheme, ThemeColor};

fn color_name(color: ThemeColor) -> &'static str {
    match color {
        ThemeColor::ToolTitle => "toolTitle",
        ThemeColor::Warning => "warning",
        ThemeColor::Error => "error",
        ThemeColor::Success => "success",
        ThemeColor::Muted => "muted",
        #[allow(unreachable_patterns)]
        _ => "other",
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

fn visible_width(value: &str) -> usize {
    let ansi = Regex::new(r"\x1b\[[0-9;]*[A-Za-z]").expect("valid regex");
    UnicodeWidthStr::width(ansi.replace_all(value, "").as_ref())
}

fn send_input(to: &str, message: Option<TaskSendMessage>, team_run_id: Option<&str>) -> TaskSendInput {
    TaskSendInput {
        to: to.to_string(),
        message,
        team_run_id: team_run_id.map(str::to_string),
        summary: None,
        all_scope: None,
    }
}

fn plain(text: &str) -> Option<TaskSendMessage> {
    Some(TaskSendMessage::Plain(text.to_string()))
}

fn shutdown_request(reason: Option<&str>) -> Option<TaskSendMessage> {
    Some(TaskSendMessage::Structured(StructuredMessageInput::ShutdownRequest {
        reason: reason.map(str::to_string),
    }))
}

fn shutdown_response(request_id: &str, approve: bool, reason: Option<&str>) -> Option<TaskSendMessage> {
    Some(TaskSendMessage::Structured(StructuredMessageInput::ShutdownResponse {
        request_id: Some(request_id.to_string()),
        approve,
        reason: reason.map(str::to_string),
    }))
}

fn cancel_input(task_id: Option<&str>, name: Option<&str>, reason: Option<&str>) -> TaskCancelInput {
    TaskCancelInput {
        task_id: task_id.map(str::to_string),
        name: name.map(str::to_string),
        reason: reason.map(str::to_string),
    }
}

#[test]
fn given_plain_task_send_message_when_rendering_call_then_shows_concise_target_and_width_safe_excerpt() {
    let theme = AnsiTheme;
    let input = send_input(
        "st_00000001",
        plain("Please inspect the database migration and report only the risky steps. tail-marker"),
        None,
    );
    let line = first_line(&render_task_send_call(&input, &theme), 96);

    assert!(line.contains("task_send to:st_00000001"), "{line}");
    assert!(!line.contains("deliver:"));
    assert!(!line.contains("operation:"));
    assert!(!line.contains("target:"));
    assert!(!line.contains("delivery:"));
    assert!(line.contains("Please inspect"), "{line}");
    assert!(!line.contains("tail-marker"), "{line}");
}

#[test]
fn given_long_multiline_korean_and_english_when_rendering_with_ansi_at_width_72_then_normalized_truncated_column_safe() {
    let theme = AnsiTheme;
    let input = send_input(
        "atlas",
        plain("한국어 안내가 아주 길게 이어집니다.\nEnglish guidance also continues long enough to require truncation safely."),
        None,
    );
    let line = first_line(&render_task_send_call(&input, &theme), 72);

    assert!(!line.contains('\n'));
    assert!(line.contains("한국어 안내"), "{line}");
    assert!(line.contains("..."), "{line}");
    assert!(visible_width(&line) <= 72, "{line}");
}

#[test]
fn given_long_korean_continuation_when_rendering_task_send_then_excerpt_ends_at_word_boundary() {
    // given / when
    let theme = AnsiTheme;
    let input = send_input(
        "st_1",
        plain("한국어로 긴 후속 작업 지시를 작성하고 동일한 세션의 맥락을 검증하세요."),
        None,
    );
    let line = first_line(&render_task_send_call(&input, &theme), 72);

    // then
    assert!(line.contains("\"한국어로 긴 후속 작업 지시를 작성하고...\""), "{line}");
    assert!(!line.contains("지..."), "{line}");
    assert!(visible_width(&line) <= 72, "{line}");
}

#[test]
fn given_structured_shutdown_messages_when_rendering_calls_then_summaries_name_request_approve_reject_and_reason() {
    let theme = TestTheme;
    let request = first_line(
        &render_task_send_call(
            &send_input("atlas", shutdown_request(Some("done for today")), Some("team-9")),
            &theme,
        ),
        120,
    );
    let approve = first_line(
        &render_task_send_call(&send_input("atlas", shutdown_response("req-1", true, None), None), &theme),
        120,
    );
    let reject = first_line(
        &render_task_send_call(
            &send_input("atlas", shutdown_response("req-2", false, Some("still testing")), None),
            &theme,
        ),
        120,
    );

    assert!(request.contains("task_send shutdown:request to:atlas team:team-9"), "{request}");
    assert!(request.contains("reason:"), "{request}");
    assert!(approve.contains("task_send shutdown:approve to:atlas"), "{approve}");
    assert!(approve.contains("request:req-1"), "{approve}");
    assert!(reject.contains("task_send shutdown:reject to:atlas"), "{reject}");
    assert!(reject.contains("reason:"), "{reject}");
    let joined = [request, approve, reject].join("\n");
    assert!(!joined.contains("deliver:"));
    assert!(!joined.contains("[object Object]"));
}

#[test]
fn given_shutdown_request_with_meaningful_reason_when_rendering_at_normal_width_then_reason_visible() {
    let theme = TestTheme;
    let input = send_input(
        "member-with-long-readable-name",
        shutdown_request(Some("Renderer QA request after the mixed Korean and English edge pass")),
        Some("team-run-with-readable-context"),
    );
    let line = first_line(&render_task_send_call(&input, &theme), 160);

    assert!(line.contains("task_send shutdown:request"), "{line}");
    assert!(line.contains("to:member-with-long-readable-name"), "{line}");
    assert!(line.contains("team:team-run-with-readable-context"), "{line}");
    assert!(line.contains("reason:"), "{line}");
    assert!(line.contains("Renderer QA request"), "{line}");
}

#[test]
fn given_shutdown_request_with_no_room_for_reason_when_rendering_at_edge_width_then_reason_omitted() {
    let theme = AnsiTheme;
    let input = send_input(
        "edge-member",
        shutdown_request(Some("Renderer QA request after the mixed Korean and English edge pass")),
        Some("edge-team-72"),
    );
    let line = first_line(&render_task_send_call(&input, &theme), 73);

    assert!(line.contains("task_send shutdown:request"), "{line}");
    assert!(line.contains("to:edge-member"), "{line}");
    assert!(line.contains("team:edge-team-72"), "{line}");
    assert!(!line.contains("reason:"), "{line}");
    assert!(!line.contains("reason:\".\""), "{line}");
    assert!(visible_width(&line) <= 73, "{line}");
}

#[test]
fn given_task_send_without_message_when_rendering_call_then_meaningful_without_empty_message_label() {
    let theme = TestTheme;
    let line = first_line(&render_task_send_call(&send_input("atlas", None, None), &theme), 80);

    assert!(line.contains("task_send to:atlas"), "{line}");
    assert!(!line.contains("deliver:"));
    assert!(!line.contains("message:"));
}

#[test]
fn given_whitespace_only_control_text_when_rendering_calls_then_empty_message_and_reason_labels_omitted() {
    let theme = TestTheme;
    let send = first_line(&render_task_send_call(&send_input("atlas", plain(" \n\t "), None), &theme), 80);
    let shutdown = first_line(
        &render_task_send_call(&send_input("atlas", shutdown_request(Some(" \n\t ")), None), &theme),
        80,
    );
    let cancel = first_line(
        &render_task_cancel_call(&cancel_input(Some("st_1"), None, Some(" \n\t ")), &theme),
        80,
    );

    assert!(!send.contains("message:"), "{send}");
    assert!(!shutdown.contains("reason:"), "{shutdown}");
    assert!(!cancel.contains("reason:"), "{cancel}");
    assert!(!format!("{send}\n{shutdown}\n{cancel}").contains("[object Object]"));
}

#[test]
fn given_task_cancel_arguments_and_result_variants_when_rendering_then_rows_concise() {
    let theme = TestTheme;
    let call = first_line(
        &render_task_cancel_call(&cancel_input(None, Some("alpha"), Some("no longer needed")), &theme),
        80,
    );
    let details = vec![
        CancelResultDetails::Cancelled {
            task_id: "st_1".to_string(),
            previous_status: TaskStatus::Running,
            status: TaskStatus::Cancelled,
        },
        CancelResultDetails::Noop {
            task_id: "st_1".to_string(),
            status: TaskStatus::Cancelled,
            reason: "Already cancelled.".to_string(),
        },
        CancelResultDetails::NotFound {
            reason: "No task found.".to_string(),
        },
        CancelResultDetails::InvalidArguments {
            reason: "Provide task_id or name.".to_string(),
        },
    ];

    let lines: Vec<String> = details
        .into_iter()
        .map(|detail| {
            first_line(
                &render_task_cancel_result(&tool_result("ok", detail), &RESULT_OPTIONS, &theme),
                100,
            )
        })
        .collect();
    assert_eq!(lines.len(), 4);

    assert!(call.contains("target:alpha"), "{call}");
    assert!(call.contains("reason:"), "{call}");
    assert!(call.contains("[warning]"), "{call}");
    assert!(!call.contains("[toolTitle]"), "{call}");
    let joined = lines.join("\n");
    assert!(joined.contains("cancelled st_1"), "{joined}");
    assert!(joined.contains("[warning]"), "{joined}");
    assert!(joined.contains("[error]"), "{joined}");
}
