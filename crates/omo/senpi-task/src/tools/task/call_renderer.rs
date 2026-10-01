//! `tools/task/call-renderer.ts`: the one-line `task` tool call row.

use crate::renderer_text::{
    ELLIPSIS, excerpt_renderer_prompt_text, join_renderer_tokens, optional_renderer_text,
    renderer_visible_width,
};
use crate::status_line::format_target_identity;
use crate::tools::render::{RendererTheme, truncate_to_width};

const TASK_PROMPT_EXCERPT_WIDTH: usize = 80;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskCallArgs {
    pub prompt: Option<String>,
    pub task_summary: Option<String>,
    pub category: Option<String>,
    pub subagent_type: Option<String>,
    pub run_in_background: Option<bool>,
}

pub fn format_task_target(category: Option<&str>, subagent_type: Option<&str>) -> String {
    format_target_identity(category, subagent_type).unwrap_or_else(|| "task".to_string())
}

pub fn format_task_mode(run_in_background: Option<bool>) -> &'static str {
    if run_in_background == Some(true) { "background" } else { "foreground" }
}

pub fn task_call_lines(args: &TaskCallArgs) -> Vec<String> {
    vec![task_call_line(args, format_task_mode(args.run_in_background))]
}

pub fn render_task_call_lines(
    args: &TaskCallArgs,
    theme: &dyn RendererTheme,
    width: Option<usize>,
) -> Vec<String> {
    let plain_mode = format_task_mode(args.run_in_background);
    let mode = theme.italic(plain_mode);
    match width {
        None => vec![task_call_line(args, &mode)],
        Some(width) => vec![task_call_line_for_width(args, &mode, plain_mode, width)],
    }
}

// The call row is label-only: the task_summary, when present, replaces the truncated prompt so
// the row reads as WHAT was delegated instead of the prompt's first words.
fn call_row_text(args: &TaskCallArgs) -> Option<String> {
    optional_renderer_text(args.task_summary.as_deref())
        .or_else(|| optional_renderer_text(args.prompt.as_deref()))
}

fn task_call_line(args: &TaskCallArgs, mode: &str) -> String {
    let prompt = prompt_token(call_row_text(args).as_deref());
    join_renderer_tokens(&[Some("task"), prompt.as_deref(), Some(mode)])
}

fn task_call_line_for_width(args: &TaskCallArgs, mode: &str, plain_mode: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let fixed_width = renderer_visible_width("task") + renderer_visible_width(plain_mode) + 4;
    let available = TASK_PROMPT_EXCERPT_WIDTH.min(width.saturating_sub(fixed_width));
    let prompt = match call_row_text(args) {
        Some(normalized) if available > 0 => {
            Some(format!("\"{}\"", excerpt_renderer_prompt_text(&normalized, Some(available))))
        }
        _ => None,
    };
    truncate_to_width(
        &join_renderer_tokens(&[Some("task"), prompt.as_deref(), Some(mode)]),
        width,
        ELLIPSIS,
    )
}

fn prompt_token(text: Option<&str>) -> Option<String> {
    optional_renderer_text(text).map(|normalized| {
        format!("\"{}\"", excerpt_renderer_prompt_text(&normalized, Some(TASK_PROMPT_EXCERPT_WIDTH)))
    })
}
