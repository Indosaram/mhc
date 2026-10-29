//! `tools/task/renderers.ts`: the `task` tool result rows.

use crate::renderer_text::{ELLIPSIS, optional_renderer_text};
use crate::state::ResolvedModelRecord;
use crate::status_line::{StatusTargetInput, format_target_with_model};
use crate::tools::render::{LinesComponent, RendererTheme, truncate_to_width};
use crate::tools::run_stats_format::run_stats_result_tokens;
use crate::tools::task::types::{TaskToolDetails, TaskToolItemDetail};

pub use super::call_renderer::{
    format_task_mode, format_task_target, render_task_call_lines, task_call_lines,
};
pub use crate::renderer_text::{
    excerpt_renderer_prompt_text, excerpt_renderer_text, join_renderer_tokens, normalize_renderer_text,
    renderer_visible_width,
};
pub use crate::tools::render::{lines_component, status_theme_color};

const TASK_REASON_EXCERPT_WIDTH: usize = 40;

pub fn format_task_status(status: &str) -> String {
    normalize_renderer_text(status)
}

pub fn task_result_lines(details: &TaskToolDetails) -> Vec<String> {
    let mode = details.run_in_background.map(|bg| format_task_mode(Some(bg)).to_string());
    result_lines(details, mode.as_deref())
}

pub fn render_task_result_lines(details: &TaskToolDetails, theme: &dyn RendererTheme) -> Vec<String> {
    let mode = details.run_in_background.map(|bg| theme.italic(format_task_mode(Some(bg))));
    result_lines(details, mode.as_deref())
}

fn result_lines(details: &TaskToolDetails, mode: Option<&str>) -> Vec<String> {
    let mut lines = vec![task_result_line(details, mode)];
    lines.extend(details.items.iter().flatten().map(task_item_result_line));
    lines
}

pub struct TaskResultComponent<'a> {
    details: TaskToolDetails,
    theme: &'a dyn RendererTheme,
}

pub fn render_task_result_component<'a>(
    details: &TaskToolDetails,
    theme: &'a dyn RendererTheme,
) -> TaskResultComponent<'a> {
    TaskResultComponent { details: details.clone(), theme }
}

impl LinesComponent for TaskResultComponent<'_> {
    fn render(&self, width: usize) -> Vec<String> {
        if width == 0 {
            return vec![String::new()];
        }
        let details = &self.details;
        let mode = details.run_in_background.map(|bg| self.theme.italic(format_task_mode(Some(bg))));
        let line = task_result_line_for_width(details, mode.as_deref(), width);
        let aggregate =
            truncate_to_width(&self.theme.fg(status_theme_color(&details.status), &line), width, ELLIPSIS);
        let mut lines = vec![aggregate];
        lines.extend(details.items.iter().flatten().map(|item| {
            truncate_to_width(
                &self.theme.fg(status_theme_color(&item.status), &task_item_result_line(item)),
                width,
                ELLIPSIS,
            )
        }));
        lines
    }
}

struct TargetIdentity<'a> {
    category: Option<&'a str>,
    subagent_type: Option<&'a str>,
    model: Option<&'a str>,
    resolved_model: Option<&'a ResolvedModelRecord>,
}

fn details_target(details: &TaskToolDetails) -> TargetIdentity<'_> {
    TargetIdentity {
        category: details.category.as_deref(),
        subagent_type: details.subagent_type.as_deref(),
        model: details.model.as_deref(),
        resolved_model: details.resolved_model.as_ref(),
    }
}

fn item_target(item: &TaskToolItemDetail) -> TargetIdentity<'_> {
    TargetIdentity {
        category: item.category.as_deref(),
        subagent_type: item.subagent_type.as_deref(),
        model: item.model.as_deref(),
        resolved_model: item.resolved_model.as_ref(),
    }
}

// One target token per row, in the shared status-line grammar.
fn task_target_token(target: &TargetIdentity<'_>) -> Option<String> {
    let token = format_target_with_model(&StatusTargetInput {
        category: target.category,
        agent_type: target.subagent_type,
        resolved_model: target.resolved_model,
        model: target.model,
        fallback_count: None,
    })
    .unwrap_or_else(|| "task".to_string());
    (token != "task").then_some(token)
}

fn fallback_count_token(details: &TaskToolDetails) -> Option<String> {
    let count = details.fallback_attempts.as_ref().map_or(0, Vec::len);
    (count > 0).then(|| format!("fallback:{count}"))
}

fn task_result_line(details: &TaskToolDetails, mode: Option<&str>) -> String {
    let target = task_target_token(&details_target(details));
    let fallback = fallback_count_token(details);
    let status = format_task_status(&details.status);
    let mut tokens: Vec<Option<String>> =
        vec![Some("task".to_string()), target, fallback, mode.map(str::to_string), Some(status)];
    tokens.extend(task_result_optional_tokens(details).into_iter().map(Some));
    join_owned(&tokens)
}

fn task_item_result_line(item: &TaskToolItemDetail) -> String {
    let task_id = optional_renderer_text(Some(&item.task_id));
    let name = optional_renderer_text(item.name.as_deref());
    let error = optional_renderer_text(item.error_message.as_deref());
    join_owned(&[
        Some("item".to_string()),
        name.map(|name| format!("name:{name}")),
        task_target_token(&item_target(item)),
        Some(format_task_status(&item.status)),
        task_id.map(|id| format!("id:{id}")),
        item.queue_position.map(|queue| format!("queue:{queue}")),
        error.map(|error| format!("error:{}", excerpt_renderer_text(&error, Some(TASK_REASON_EXCERPT_WIDTH)))),
    ])
}

fn task_result_line_for_width(details: &TaskToolDetails, mode: Option<&str>, width: usize) -> String {
    let fallback = fallback_count_token(details);
    let status = format_task_status(&details.status);
    let required_without_target: Vec<&str> =
        [Some("task"), fallback.as_deref(), mode, Some(status.as_str())].into_iter().flatten().collect();
    let required_spaces = required_without_target.len() + 1;
    let used: usize = required_without_target.iter().map(|token| renderer_visible_width(token)).sum();
    let target_width = width.saturating_sub(used + required_spaces);
    let target = compact_target_token(&details_target(details), target_width);
    let required: Vec<&str> =
        [Some("task"), target.as_deref(), fallback.as_deref(), mode, Some(status.as_str())]
            .into_iter()
            .flatten()
            .collect();
    let mut line = required.join(" ");
    for token in task_result_optional_tokens(details) {
        let candidate = format!("{line} {token}");
        if renderer_visible_width(&candidate) > width {
            break;
        }
        line = candidate;
    }
    line
}

fn compact_target_token(target: &TargetIdentity<'_>, max_width: usize) -> Option<String> {
    let token = task_target_token(target)?;
    if renderer_visible_width(&token) <= max_width {
        return Some(token);
    }
    Some(excerpt_renderer_text(&token, Some(max_width)))
}

fn task_result_optional_tokens(details: &TaskToolDetails) -> Vec<String> {
    let task_id = optional_renderer_text(Some(&details.task_id));
    let reason = optional_renderer_text(details.reason.as_deref());
    let mut tokens = Vec::new();
    if let Some(id) = task_id {
        tokens.push(format!("id:{id}"));
    }
    tokens.extend(run_stats_result_tokens(details.run_stats.as_ref()));
    if let Some(queue) = details.queue_position {
        tokens.push(format!("queue:{queue}"));
    }
    if let Some(reason) = reason {
        tokens.push(format!("reason:{}", excerpt_renderer_text(&reason, Some(TASK_REASON_EXCERPT_WIDTH))));
    }
    tokens
}

fn join_owned(tokens: &[Option<String>]) -> String {
    let borrowed: Vec<Option<&str>> = tokens.iter().map(Option::as_deref).collect();
    join_renderer_tokens(&borrowed)
}
