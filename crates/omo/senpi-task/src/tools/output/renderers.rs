//! Port of `tools/output/renderers.ts`.

use crate::renderer_text::ELLIPSIS;
use crate::status_line::{StatusTargetInput, TaskIdentityInput, format_status_target, task_identity_label};
use crate::tools::control::tool_result::AgentToolResult;
use crate::tools::render::{LinesComponent, RendererTheme, ThemeColor, status_theme_color, truncate_to_width};
use crate::tools::run_stats_format::run_stats_suffix;
use crate::tools::task::renderers::{
    excerpt_renderer_text, join_renderer_tokens, normalize_renderer_text, renderer_visible_width,
};

use crate::tools::output::output::TaskOutputInput;
use crate::tools::output::types::{TaskOutputDetails, TaskSnapshot};

/// `Pick<Theme, "fg">`.
pub use crate::tools::render::RendererTheme as OutputRenderTheme;

/// Minimal model of senpi's `ToolRenderResultOptions`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ToolRenderResultOptions {
    pub expanded: bool,
    pub is_partial: bool,
}

struct ResultRow {
    color: ThemeColor,
    text: String,
}

const DEFAULT_TAIL_LINES: usize = 60;
const TARGET_EXCERPT_MAX: usize = 56;

/// A single-line call component rendered lazily for the available width.
pub struct OutputCallComponent<'a> {
    args: TaskOutputInput,
    theme: &'a dyn RendererTheme,
}

impl OutputCallComponent<'_> {
    pub fn invalidate(&self) {}
}

impl LinesComponent for OutputCallComponent<'_> {
    fn render(&self, width: usize) -> Vec<String> {
        let line = self.theme.fg(ThemeColor::ToolTitle, &task_output_call_line(&self.args, width));
        vec![truncate_to_width(&line, width, ELLIPSIS)]
    }
}

/// A fixed-lines result component.
pub struct OutputLinesComponent {
    lines: Vec<String>,
}

impl OutputLinesComponent {
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn invalidate(&self) {}
}

impl LinesComponent for OutputLinesComponent {
    fn render(&self, width: usize) -> Vec<String> {
        self.lines
            .iter()
            .map(|line| truncate_to_width(line, width, ELLIPSIS))
            .collect()
    }
}

pub fn render_task_output_call<'a>(args: &TaskOutputInput, theme: &'a dyn RendererTheme) -> OutputCallComponent<'a> {
    OutputCallComponent {
        args: args.clone(),
        theme,
    }
}

pub fn render_task_output_result(
    result: &AgentToolResult<TaskOutputDetails>,
    _options: &ToolRenderResultOptions,
    theme: &dyn RendererTheme,
) -> OutputLinesComponent {
    let row = task_output_result_row(&result.details);
    OutputLinesComponent {
        lines: vec![theme.fg(row.color, &normalize_renderer_text(&row.text))],
    }
}

fn task_output_call_line(args: &TaskOutputInput, width: usize) -> String {
    let mode = args.mode.map_or("status", |mode| mode.as_str());
    let tail = (mode == "tail").then(|| format!("tail_lines:{}", args.tail_lines.unwrap_or(DEFAULT_TAIL_LINES)));
    let before_target = "task_output target:";
    let mode_token = format!("mode:{mode}");
    let after_target = join_renderer_tokens(&[Some(mode_token.as_str()), Some("peek"), tail.as_deref()]);
    let available = TARGET_EXCERPT_MAX.min(
        width
            .saturating_sub(renderer_visible_width(before_target))
            .saturating_sub(renderer_visible_width(&after_target))
            .saturating_sub(1),
    );
    let raw_target = args
        .task_id
        .as_deref()
        .or(args.name.as_deref())
        .unwrap_or("<missing>");
    let target = excerpt_renderer_text(raw_target, Some(available));
    let head = format!("{before_target}{target}");
    join_renderer_tokens(&[Some(head.as_str()), Some(after_target.as_str())])
}

fn task_output_result_row(details: &TaskOutputDetails) -> ResultRow {
    match details {
        TaskOutputDetails::Status { snapshot } => {
            let identity = snapshot_identity(snapshot);
            let id_suffix = (identity != snapshot.task_id).then(|| format!("({})", snapshot.task_id));
            let target = format_status_target(&StatusTargetInput {
                category: snapshot.category.as_deref(),
                agent_type: snapshot.agent_type.as_deref(),
                resolved_model: snapshot.resolved_model.as_ref(),
                model: None,
                fallback_count: None,
            })
            .unwrap_or_else(|| format!("model:{}", normalize_renderer_text(&snapshot.model)));
            let status_label = if snapshot.suspended.is_some() {
                "suspended".to_string()
            } else {
                normalize_renderer_text(snapshot.status.as_str())
            };
            let head = format!("task_output {identity}");
            let tokens = join_renderer_tokens(&[
                Some(head.as_str()),
                id_suffix.as_deref(),
                Some(status_label.as_str()),
            ]);
            ResultRow {
                color: status_theme_color(snapshot.status.as_str()),
                text: format!(
                    "{tokens} · {target}{}",
                    run_stats_suffix(snapshot.run_stats.as_ref())
                ),
            }
        }
        TaskOutputDetails::Transcript {
            mode,
            source,
            truncated,
            snapshot,
            ..
        } => {
            let head = format!("task_output transcript {}", snapshot_identity(snapshot));
            let mode_token = format!("mode:{}", mode.as_str());
            let source_token = format!("source:{}", source.as_str());
            ResultRow {
                color: status_theme_color(snapshot.status.as_str()),
                text: join_renderer_tokens(&[
                    Some(head.as_str()),
                    Some(mode_token.as_str()),
                    Some(source_token.as_str()),
                    truncated.then_some("truncated"),
                ]),
            }
        }
        TaskOutputDetails::NotFound { reason, known_tasks } => ResultRow {
            color: ThemeColor::Error,
            text: not_found_row(reason, known_tasks),
        },
        TaskOutputDetails::InvalidArguments { reason } => ResultRow {
            color: ThemeColor::Error,
            text: format!("task_output invalid: {reason}"),
        },
    }
}

fn not_found_row(reason: &str, known_tasks: &[String]) -> String {
    let known = (!known_tasks.is_empty())
        .then(|| format!("known:{}", excerpt_renderer_text(&known_tasks.join(","), None)));
    let head = format!("task_output not found: {reason}");
    join_renderer_tokens(&[Some(head.as_str()), known.as_deref()])
}

/// Model-facing model summary for the task_output status view text (TUI rows use format_status_target).
pub fn task_output_model_text(snapshot: &TaskSnapshot) -> String {
    let resolved = snapshot.resolved_model.as_ref();
    let display = non_empty(resolved.map(|model| model.display.as_str()));
    let model = normalize_renderer_text(&snapshot.model);
    let reasoning = non_empty(resolved.and_then(|model| model.reasoning.as_deref()))
        .or_else(|| non_empty(resolved.and_then(|model| model.reasoning_effort.as_deref())));
    let variant = non_empty(resolved.and_then(|model| model.variant.as_deref()));
    let details: Vec<String> = [
        reasoning.map(|reasoning| format!("reasoning {reasoning}")),
        variant.map(|variant| format!("variant {variant}")),
    ]
    .into_iter()
    .flatten()
    .collect();
    let suffix = if details.is_empty() {
        String::new()
    } else {
        format!(" ({})", details.join(", "))
    };
    format!("model {}{suffix}", display.unwrap_or(model))
}

fn non_empty(value: Option<&str>) -> Option<String> {
    let normalized = excerpt_renderer_text(value?, None);
    (!normalized.is_empty()).then_some(normalized)
}

fn snapshot_identity(snapshot: &TaskSnapshot) -> String {
    task_identity_label(&TaskIdentityInput {
        task_id: &snapshot.task_id,
        name: snapshot.name.as_deref(),
        description: snapshot.description.as_deref(),
        task_summary: snapshot.task_summary.as_deref(),
    })
}
