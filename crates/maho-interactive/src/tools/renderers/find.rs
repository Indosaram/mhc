//! Port of `core/tools/renderers/find.ts`.
use std::cell::RefCell;
use std::rc::Rc;

use maho_tools::definition::ToolResult;
use maho_tools::truncate::{DEFAULT_MAX_BYTES, format_size};
use maho_tui::components::text::Text;
use serde_json::Value;

use super::{RenderedComponent, ToolRenderContext, ToolRenderResultOptions, ToolRenderers};
use crate::components::keybinding_hints::key_hint;
use crate::theme::{Theme, ThemeColor};
use crate::tools::render_utils::{get_text_output, invalid_arg_text, shorten_path, str_value};

fn format_find_call(args: Option<&Value>, theme: &Theme) -> String {
    let pattern = args.and_then(|args| str_value(args.get("pattern"))).flatten();
    let raw_path = args.and_then(|args| str_value(args.get("path"))).flatten();
    let path = raw_path.as_deref().map(|value| shorten_path(Some(&Value::String(value.to_owned()))));
    let limit = args.and_then(|args| args.get("limit")).and_then(Value::as_u64);
    let invalid_arg = invalid_arg_text(theme);
    let pattern_display = match &pattern {
        None => invalid_arg.clone(),
        Some(pattern) => theme.fg(ThemeColor::Accent, pattern),
    };
    let path_display = match &path {
        None => invalid_arg,
        Some(path) if path.is_empty() => shorten_path(Some(&Value::String(String::from(".")))),
        Some(path) => path.clone(),
    };
    let mut text = format!(
        "{} {}{}",
        theme.fg(ThemeColor::ToolTitle, &theme.bold("find")),
        pattern_display,
        theme.fg(ThemeColor::ToolOutput, &format!(" in {path_display}"))
    );
    if let Some(limit) = limit {
        text += &theme.fg(ThemeColor::ToolOutput, &format!(" (limit {limit})"));
    }
    text
}

fn format_find_result(
    result: &ToolResult,
    options: ToolRenderResultOptions,
    theme: &Theme,
    show_images: bool,
) -> String {
    let output = get_text_output(Some(result), show_images);
    let output = output.trim();
    let mut text = String::new();
    if !output.is_empty() {
        let lines: Vec<&str> = output.split('\n').collect();
        let max_lines = if options.expanded { lines.len() } else { 20 };
        let display_lines: Vec<&str> = lines.iter().take(max_lines).copied().collect();
        let remaining = lines.len().saturating_sub(max_lines);
        text += &format!(
            "\n{}",
            display_lines.iter().map(|line| theme.fg(ThemeColor::ToolOutput, line)).collect::<Vec<_>>().join("\n")
        );
        if remaining > 0 {
            text += &format!(
                "{} {}{}",
                theme.fg(ThemeColor::Muted, &format!("\n... ({remaining} more lines,")),
                key_hint("app.tools.expand", "to expand", theme),
                theme.fg(ThemeColor::Muted, ")")
            );
        }
    }

    let details = result.details.clone().unwrap_or(Value::Null);
    let result_limit = details.get("resultLimitReached").and_then(Value::as_u64);
    let truncation = details.get("truncation");
    let truncated = truncation.and_then(|value| value.get("truncated")).and_then(Value::as_bool).unwrap_or(false);
    if result_limit.is_some() || truncated {
        let mut warnings: Vec<String> = Vec::new();
        if let Some(result_limit) = result_limit {
            warnings.push(format!("{result_limit} results limit"));
        }
        if truncated {
            let max_bytes = truncation
                .and_then(|value| value.get("maxBytes"))
                .and_then(Value::as_u64)
                .map_or(DEFAULT_MAX_BYTES, |value| value as usize);
            warnings.push(format!("{} limit", format_size(max_bytes)));
        }
        text += &format!("\n{}", theme.fg(ThemeColor::Warning, &format!("[Truncated: {}]", warnings.join(", "))));
    }
    text
}

#[derive(Default)]
pub struct FindRenderers;

impl ToolRenderers for FindRenderers {
    fn is_built_in(&self) -> bool {
        true
    }
    fn render_call(&mut self, theme: &Theme, context: &ToolRenderContext<'_>) -> Option<RenderedComponent> {
        let text = format_find_call(Some(context.args), theme);
        Some(Rc::new(RefCell::new(Text::with_padding(text, 0, 0))))
    }

    fn render_result(
        &mut self,
        result: &ToolResult,
        options: ToolRenderResultOptions,
        theme: &Theme,
        context: &ToolRenderContext<'_>,
    ) -> Option<RenderedComponent> {
        let text = format_find_result(result, options, theme, context.show_images);
        Some(Rc::new(RefCell::new(Text::with_padding(text, 0, 0))))
    }
}
