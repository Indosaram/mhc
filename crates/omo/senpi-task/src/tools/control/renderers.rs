//! Port of `tools/control/renderers.ts`.

use crate::renderer_text::{
    excerpt_renderer_prompt_text, excerpt_renderer_text, join_renderer_tokens, normalize_renderer_text,
    renderer_visible_width,
};
use crate::tools::control::cancel::TaskCancelInput;
use crate::tools::control::send_schema::{
    MemberScopedTaskSendInput, StructuredMessageInput, TaskSendInput, TaskSendMessage,
};
use crate::tools::control::tool_result::AgentToolResult;
use crate::tools::control::types::{CancelResultDetails, SendResultDetails};
use crate::tools::render::{LinesComponent, RendererTheme, ThemeColor, status_theme_color, truncate_to_width};
use crate::tools::team::messaging::TeamSendDetails;

const MESSAGE_EXCERPT_MAX: usize = 56;
const REASON_EXCERPT_MAX: usize = 40;
const ELLIPSIS: &str = "...";
const MIN_MEANINGFUL_TRUNCATED_EXCERPT_WIDTH: usize = 8;

/// Minimal model of senpi's `ToolRenderResultOptions`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ToolRenderResultOptions {
    pub expanded: bool,
    pub is_partial: bool,
}

/// A single-line call component rendered lazily for the available width.
pub struct ControlCallComponent<'a> {
    render_line: Box<dyn Fn(usize) -> String + 'a>,
}

impl ControlCallComponent<'_> {
    pub fn invalidate(&self) {}
}

impl LinesComponent for ControlCallComponent<'_> {
    fn render(&self, width: usize) -> Vec<String> {
        vec![truncate_to_width(&(self.render_line)(width), width, ELLIPSIS)]
    }
}

/// A fixed-lines result component.
pub struct ControlLinesComponent {
    lines: Vec<String>,
}

impl ControlLinesComponent {
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn invalidate(&self) {}
}

impl LinesComponent for ControlLinesComponent {
    fn render(&self, width: usize) -> Vec<String> {
        self.lines
            .iter()
            .map(|line| truncate_to_width(line, width, ELLIPSIS))
            .collect()
    }
}

struct ResultRow {
    color: ThemeColor,
    text: String,
}

fn row(color: ThemeColor, text: String) -> ResultRow {
    ResultRow { color, text }
}

pub fn render_task_send_call<'a>(args: &TaskSendInput, theme: &'a dyn RendererTheme) -> ControlCallComponent<'a> {
    let args = args.clone();
    ControlCallComponent {
        render_line: Box::new(move |width| {
            theme.fg(ThemeColor::ToolTitle, &task_send_call_line(&args, theme, width))
        }),
    }
}

pub fn render_member_scoped_task_send_call<'a>(
    args: &MemberScopedTaskSendInput,
    theme: &'a dyn RendererTheme,
) -> ControlCallComponent<'a> {
    let converted = TaskSendInput {
        to: args.to.clone(),
        message: Some(TaskSendMessage::Plain(args.message.clone())),
        team_run_id: None,
        summary: args.summary.clone(),
        all_scope: None,
    };
    render_task_send_call(&converted, theme)
}

pub fn render_task_send_result(
    result: &AgentToolResult<SendResultDetails>,
    _options: &ToolRenderResultOptions,
    theme: &dyn RendererTheme,
) -> ControlLinesComponent {
    let row = task_send_result_row(&result.details);
    ControlLinesComponent {
        lines: vec![theme.fg(row.color, &normalize_renderer_text(&row.text))],
    }
}

pub fn render_task_cancel_call<'a>(args: &TaskCancelInput, theme: &'a dyn RendererTheme) -> ControlCallComponent<'a> {
    let args = args.clone();
    ControlCallComponent {
        render_line: Box::new(move |width| {
            theme.fg(ThemeColor::Warning, &task_cancel_call_line(&args, theme, width))
        }),
    }
}

pub fn render_task_cancel_result(
    result: &AgentToolResult<CancelResultDetails>,
    _options: &ToolRenderResultOptions,
    theme: &dyn RendererTheme,
) -> ControlLinesComponent {
    let row = task_cancel_result_row(&result.details);
    ControlLinesComponent {
        lines: vec![theme.fg(row.color, &normalize_renderer_text(&row.text))],
    }
}

fn task_send_call_line(args: &TaskSendInput, theme: &dyn RendererTheme, width: usize) -> String {
    if let Some(TaskSendMessage::Structured(message)) = &args.message {
        return shutdown_call_line(args, message, theme, width);
    }
    let target = format!("to:{}", normalize_renderer_text(&args.to));
    let base = join_renderer_tokens(&[Some("task_send"), Some(target.as_str())]);
    if let Some(TaskSendMessage::Plain(message)) = &args.message {
        return with_excerpt(&base, "message", message, theme, width);
    }
    base
}

fn shutdown_call_line(
    args: &TaskSendInput,
    message: &StructuredMessageInput,
    theme: &dyn RendererTheme,
    width: usize,
) -> String {
    let target = format!("to:{}", normalize_renderer_text(&args.to));
    let team = optional_token("team", args.team_run_id.as_deref());
    match message {
        StructuredMessageInput::ShutdownRequest { reason } => {
            let base = join_renderer_tokens(&[
                Some("task_send shutdown:request"),
                Some(target.as_str()),
                team.as_deref(),
            ]);
            match reason {
                None => base,
                Some(reason) => with_excerpt(&base, "reason", reason, theme, width),
            }
        }
        StructuredMessageInput::ShutdownResponse {
            request_id,
            approve,
            reason,
        } => {
            let action = if *approve { "approve" } else { "reject" };
            let head = format!("task_send shutdown:{action}");
            let request = optional_token("request", request_id.as_deref());
            let base = join_renderer_tokens(&[
                Some(head.as_str()),
                Some(target.as_str()),
                team.as_deref(),
                request.as_deref(),
            ]);
            match reason {
                None => base,
                Some(reason) => with_excerpt(&base, "reason", reason, theme, width),
            }
        }
    }
}

fn task_cancel_call_line(args: &TaskCancelInput, theme: &dyn RendererTheme, width: usize) -> String {
    let raw_target = args
        .task_id
        .as_deref()
        .or(args.name.as_deref())
        .unwrap_or("<missing>");
    let target = format!("target:{}", normalize_renderer_text(raw_target));
    let base = join_renderer_tokens(&[Some("task_cancel"), Some(target.as_str())]);
    match &args.reason {
        None => base,
        Some(reason) => with_excerpt(&base, "reason", reason, theme, width),
    }
}

fn with_excerpt(base: &str, label: &str, value: &str, theme: &dyn RendererTheme, width: usize) -> String {
    let label_token = format!("{label}:");
    let prefix = join_renderer_tokens(&[Some(base), Some(label_token.as_str())]);
    let quote_overhead = 2usize;
    let max_excerpt = if label == "reason" {
        REASON_EXCERPT_MAX
    } else {
        MESSAGE_EXCERPT_MAX
    };
    let normalized = normalize_renderer_text(value);
    if normalized.is_empty() {
        return base.to_string();
    }
    let available = max_excerpt.min(
        width
            .saturating_sub(renderer_visible_width(&prefix))
            .saturating_sub(quote_overhead),
    );
    if renderer_visible_width(&normalized) > available && available < MIN_MEANINGFUL_TRUNCATED_EXCERPT_WIDTH {
        return base.to_string();
    }
    let excerpt = if label == "message" {
        excerpt_renderer_prompt_text(&normalized, Some(available))
    } else {
        excerpt_renderer_text(&normalized, Some(available))
    };
    format!("{prefix}{}", theme.italic(&format!("\"{excerpt}\"")))
}

fn optional_token(label: &str, value: Option<&str>) -> Option<String> {
    let normalized = normalize_renderer_text(value?);
    if normalized.is_empty() {
        None
    } else {
        Some(format!("{label}:{normalized}"))
    }
}

fn task_send_result_row(details: &SendResultDetails) -> ResultRow {
    match details {
        SendResultDetails::Steered {
            task_id,
            status,
            delivered,
        } => row(
            status_theme_color(status.as_str()),
            format!("task_send delivered {task_id} as {delivered} ({})", status.as_str()),
        ),
        SendResultDetails::Revived { task_id, run_epoch } => row(
            ThemeColor::Success,
            format!("task_send revived {task_id} epoch {run_epoch}"),
        ),
        SendResultDetails::Queued {
            task_id,
            queue_position,
        } => row(
            ThemeColor::Muted,
            format!("task_send queued {task_id} position {queue_position}"),
        ),
        SendResultDetails::NotContinuable {
            task_id,
            reason,
            suggestion,
        } => row(
            ThemeColor::Warning,
            format!("task_send not continuable {task_id}: {reason} {suggestion}"),
        ),
        SendResultDetails::OneShotAgent { task_id, agent, .. } => row(
            ThemeColor::Error,
            format!("task_send denied {task_id} one-shot:{agent}"),
        ),
        SendResultDetails::ScopeDenied {
            task_id,
            owning_session_id,
            ..
        } => row(
            ThemeColor::Error,
            format!("task_send denied {task_id} owner:{owning_session_id}"),
        ),
        SendResultDetails::NotFound { reason, known_tasks } => {
            row(ThemeColor::Error, not_found_text(reason, known_tasks))
        }
        SendResultDetails::InvalidArguments { reason } => {
            row(ThemeColor::Error, format!("task_send invalid: {reason}"))
        }
        SendResultDetails::TeamMessage { team } => team_message_row(team),
        SendResultDetails::ShutdownRequested { team_run_id, member } => row(
            ThemeColor::Warning,
            format!("task_send shutdown requested {team_run_id} member:{member}"),
        ),
        SendResultDetails::ShutdownResponded {
            team_run_id,
            member,
            approved,
        } => row(
            if *approved {
                ThemeColor::Success
            } else {
                ThemeColor::Warning
            },
            format!(
                "task_send shutdown {} {team_run_id} member:{member}",
                if *approved { "approved" } else { "rejected" }
            ),
        ),
        SendResultDetails::ShutdownFailed {
            operation,
            team_run_id,
            member,
            reason,
            ..
        } => row(
            ThemeColor::Error,
            format!(
                "task_send shutdown {} failed {team_run_id} member:{member}: {reason}",
                operation.as_str()
            ),
        ),
    }
}

fn not_found_text(reason: &str, known_tasks: &[String]) -> String {
    if known_tasks.is_empty() {
        return format!("task_send not found: {reason}");
    }
    format!("task_send not found: {reason} known:{}", known_tasks.join(","))
}

fn team_message_row(details: &TeamSendDetails) -> ResultRow {
    match details {
        TeamSendDetails::ToLead { message_id } => row(
            ThemeColor::Success,
            format!("task_send team message {message_id} enqueued to lead"),
        ),
        TeamSendDetails::ToMembers { message_id, recipients } => row(
            ThemeColor::Success,
            format!(
                "task_send team message {message_id} enqueued to {} member(s)",
                recipients.len()
            ),
        ),
        TeamSendDetails::Mailbox { kind, to, reason } => row(
            ThemeColor::Error,
            format!("task_send team {} to:{to}: {reason}", kind.as_str()),
        ),
    }
}

fn task_cancel_result_row(details: &CancelResultDetails) -> ResultRow {
    match details {
        CancelResultDetails::Cancelled {
            task_id,
            previous_status,
            status,
        } => row(
            status_theme_color(status.as_str()),
            format!(
                "task_cancel cancelled {task_id} ({} -> {})",
                previous_status.as_str(),
                status.as_str()
            ),
        ),
        CancelResultDetails::Noop {
            task_id,
            status,
            reason,
        } => row(
            status_theme_color(status.as_str()),
            format!("task_cancel no change {task_id} ({}): {reason}", status.as_str()),
        ),
        CancelResultDetails::NotFound { reason } => {
            row(ThemeColor::Error, format!("task_cancel not found: {reason}"))
        }
        CancelResultDetails::InvalidArguments { reason } => {
            row(ThemeColor::Error, format!("task_cancel invalid: {reason}"))
        }
    }
}
