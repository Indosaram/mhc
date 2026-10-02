//! Port of `core/tools/renderers/bash.ts`.
use std::cell::RefCell;
use std::rc::Rc;

use maho_tools::definition::ToolResult;
use maho_tui::components::text::Text;
use maho_tui::tui::{Component, Container};
use maho_tui::utils::truncate_to_width;
use serde_json::Value;

use super::{RenderedComponent, ToolRenderContext, ToolRenderResultOptions, ToolRenderers};
use crate::components::keybinding_hints::key_hint;
use crate::components::visual_truncate::truncate_to_visual_lines;
use crate::theme::{Theme, ThemeColor};
use crate::tools::render_utils::{get_text_output, invalid_arg_text, normalize_display_text, replace_tabs, str_value};

const BASH_PREVIEW_LINES: usize = 5;

fn format_duration(ms: f64) -> String {
    let total_seconds = (ms.max(0.) / 1000.).floor() as u64;
    if total_seconds < 1 {
        return String::from("<1s");
    }
    let seconds = total_seconds % 60;
    let total_minutes = total_seconds / 60;
    if total_minutes < 1 {
        return format!("{seconds}s");
    }
    let minutes = total_minutes % 60;
    let hours = total_minutes / 60;
    if hours < 1 {
        return if seconds == 0 { format!("{minutes}m") } else { format!("{minutes}m {seconds}s") };
    }
    if minutes == 0 { format!("{hours}h") } else { format!("{hours}h {minutes}m") }
}

fn highlight_bash_command(command: &str, theme: &Theme) -> String {
    crate::theme::highlight_code(theme, &replace_tabs(&normalize_display_text(command)), Some("bash")).join("\n")
}

fn format_shell_call(args: Option<&Value>, theme: &Theme) -> String {
    let command = args.and_then(|args| str_value(args.get("command"))).flatten();
    let timeout = args.and_then(|args| args.get("timeout")).and_then(Value::as_f64);
    let timeout_suffix = match timeout {
        Some(timeout) if timeout != 0. => theme.fg(ThemeColor::Muted, &format!(" (timeout {}s)", format_number(timeout))),
        _ => String::new(),
    };
    let command_display = match &command {
        None => invalid_arg_text(theme),
        Some(command) if command.is_empty() => theme.fg(ThemeColor::ToolOutput, "..."),
        Some(command) => highlight_bash_command(command, theme),
    };
    format!("{}{command_display}{timeout_suffix}", theme.fg(ThemeColor::ToolTitle, &theme.bold("$ ")))
}

fn format_number(value: f64) -> String {
    if value.fract() == 0. { format!("{}", value as i64) } else { format!("{value}") }
}

#[derive(Default)]
struct BashResultState {
    cached_width: Option<usize>,
    cached_lines: Option<Vec<String>>,
    cached_skipped: Option<usize>,
}

struct CachedPreview {
    state: Rc<RefCell<BashResultState>>,
    styled_output: String,
    theme: Theme,
}

impl Component for CachedPreview {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut state = self.state.borrow_mut();
        if state.cached_lines.is_none() || state.cached_width != Some(width) {
            let preview = truncate_to_visual_lines(&self.styled_output, BASH_PREVIEW_LINES, width, 0);
            state.cached_lines = Some(preview.visual_lines);
            state.cached_skipped = Some(preview.skipped_count);
            state.cached_width = Some(width);
        }
        let skipped = state.cached_skipped.unwrap_or(0);
        let lines = state.cached_lines.clone().unwrap_or_default();
        if skipped > 0 {
            let hint = format!(
                "{} {}{}",
                self.theme.fg(ThemeColor::Muted, &format!("... ({skipped} earlier lines,")),
                key_hint("app.tools.expand", "to expand", &self.theme),
                self.theme.fg(ThemeColor::Muted, ")")
            );
            let mut out = vec![String::new(), truncate_to_width(&hint, width, "...", false)];
            out.extend(lines);
            return out;
        }
        let mut out = vec![String::new()];
        out.extend(lines);
        out
    }
    fn invalidate(&mut self) {
        let mut state = self.state.borrow_mut();
        state.cached_width = None;
        state.cached_lines = None;
        state.cached_skipped = None;
    }
}

#[derive(Default)]
pub struct ShellRenderers {
    started_at: Option<f64>,
    ended_at: Option<f64>,
}

impl ShellRenderers {
    pub fn new(_prompt: &str) -> Self {
        Self::default()
    }
}

impl ToolRenderers for ShellRenderers {
    fn is_built_in(&self) -> bool {
        true
    }
    fn render_call(&mut self, theme: &Theme, context: &ToolRenderContext<'_>) -> Option<RenderedComponent> {
        if context.execution_started && self.started_at.is_none() {
            self.started_at = Some(context.now_ms);
            self.ended_at = None;
        }
        let text = format_shell_call(Some(context.args), theme);
        Some(Rc::new(RefCell::new(Text::with_padding(text, 0, 0))))
    }

    fn render_result(
        &mut self,
        result: &ToolResult,
        options: ToolRenderResultOptions,
        theme: &Theme,
        context: &ToolRenderContext<'_>,
    ) -> Option<RenderedComponent> {
        if self.started_at.is_some() && options.is_partial {
            (context.invalidate)();
        }
        if !options.is_partial || context.is_error {
            self.ended_at.get_or_insert(context.now_ms);
        }

        let mut component = Container::new();
        let output = get_text_output(Some(result), context.show_images);
        let output = output.trim();
        if !output.is_empty() {
            let styled_output =
                output.split('\n').map(|line| theme.fg(ThemeColor::ToolOutput, line)).collect::<Vec<_>>().join("\n");
            if options.expanded {
                component.add_child(Rc::new(RefCell::new(Text::with_padding(format!("\n{styled_output}"), 0, 0))));
            } else {
                let state = Rc::new(RefCell::new(BashResultState::default()));
                component.add_child(Rc::new(RefCell::new(CachedPreview {
                    state,
                    styled_output,
                    theme: theme.clone(),
                })));
            }
        }

        if let Some(started_at) = self.started_at {
            let label = if options.is_partial { "Elapsed" } else { "Took" };
            let end_time = self.ended_at.unwrap_or(context.now_ms);
            component.add_child(Rc::new(RefCell::new(Text::with_padding(
                format!("\n{}", theme.fg(ThemeColor::Muted, &format!("{label} {}", format_duration(end_time - started_at)))),
                0,
                0,
            ))));
        }

        component.invalidate();
        Some(Rc::new(RefCell::new(component)))
    }
}
