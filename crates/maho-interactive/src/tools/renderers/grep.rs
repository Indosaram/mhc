//! Port of `core/tools/renderers/grep.ts`.
use std::cell::RefCell;
use std::rc::Rc;

use maho_tools::definition::ToolResult;
use maho_tools::grep::format::display_path;
use maho_tui::components::text::Text;
use serde_json::Value;

use super::{RenderedComponent, ToolRenderContext, ToolRenderResultOptions, ToolRenderers};
use crate::components::keybinding_hints::key_hint;
use crate::theme::{Theme, ThemeColor};
use crate::tools::render_utils::{get_text_output, invalid_arg_text, link_path, render_tool_path, str_value};

const COLLAPSED_LINE_BUDGET: usize = 15;

fn is_v1_details(details: &Value) -> bool {
    details.get("version").and_then(Value::as_u64) == Some(1)
        && details.get("matches").and_then(Value::as_array).is_some()
}

fn format_call_paths(path: Option<&Value>, theme: &Theme, cwd: &str) -> String {
    match path {
        Some(Value::Array(entries)) => {
            if entries.is_empty() || entries.iter().any(|entry| !entry.is_string()) {
                return invalid_arg_text(theme);
            }
            entries
                .iter()
                .filter_map(|entry| entry.as_str())
                .map(|entry| render_tool_path(Some(entry.to_owned()), theme, cwd, Some(".")))
                .collect::<Vec<_>>()
                .join(", ")
        }
        Some(value) => render_tool_path(str_value(Some(value)).flatten(), theme, cwd, Some(".")),
        None => render_tool_path(Some(String::new()), theme, cwd, Some(".")),
    }
}

fn format_call_globs(glob: Option<&Value>) -> Option<String> {
    let glob = glob?;
    let values: Vec<&Value> = match glob {
        Value::Array(values) => values.iter().collect(),
        value => vec![value],
    };
    let joined = values.iter().filter_map(|entry| entry.as_str()).collect::<Vec<_>>().join(",");
    if joined.is_empty() { None } else { Some(joined) }
}

fn format_grep_call(args: Option<&Value>, theme: &Theme, cwd: &str) -> String {
    let pattern = args.and_then(|args| str_value(args.get("pattern"))).flatten();
    let invalid_arg = invalid_arg_text(theme);
    let pattern_display = match &pattern {
        None => invalid_arg,
        Some(pattern) => theme.fg(ThemeColor::Accent, &format!("/{pattern}/")),
    };
    let mut text = format!(
        "{} {}{}{}",
        theme.fg(ThemeColor::ToolTitle, &theme.bold("grep")),
        pattern_display,
        theme.fg(ThemeColor::ToolOutput, " in "),
        format_call_paths(args.and_then(|args| args.get("path")), theme, cwd)
    );
    if let Some(glob) = format_call_globs(args.and_then(|args| args.get("glob"))) {
        text += &theme.fg(ThemeColor::ToolOutput, &format!(" {glob}"));
    }
    if let Some(mode) = args.and_then(|args| args.get("mode")).and_then(Value::as_str).filter(|mode| !mode.is_empty()) {
        text += &theme.fg(ThemeColor::ToolOutput, &format!(" {mode}"));
    }
    if let Some(skip) = args.and_then(|args| args.get("skip")).and_then(Value::as_u64) {
        text += &theme.fg(ThemeColor::ToolOutput, &format!(" skip {skip}"));
    }
    text
}

fn format_file_header(path: &str, theme: &Theme, cwd: &str) -> String {
    link_path(&theme.fg(ThemeColor::Accent, &display_path(path)), path, cwd)
}

fn format_match_row(row: &Value, theme: &Theme) -> String {
    let line = row.get("line").and_then(Value::as_u64).unwrap_or(0);
    let is_context = row.get("isContext").and_then(Value::as_bool).unwrap_or(false);
    let text = row.get("text").and_then(Value::as_str).unwrap_or_default();
    let row_text = format!("{line}{} {text}", if is_context { "-" } else { ":" });
    if is_context { theme.fg(ThemeColor::Dim, &row_text) } else { theme.fg(ThemeColor::Accent, &row_text) }
}

fn no_match_text(details: &Value) -> String {
    match details.get("status").and_then(Value::as_str) {
        Some("pageEnd") => format!("No more results (skip={})", details.get("skip").and_then(Value::as_u64).unwrap_or(0)),
        Some("partial") => String::from("No matches found in searched portion"),
        _ => String::from("No matches found"),
    }
}

struct Group {
    path: String,
    count: Option<u64>,
    rows: Vec<Value>,
}

fn groups_from_details(details: &Value) -> Vec<Group> {
    let file_matches = details.get("fileMatches").and_then(Value::as_array).cloned().unwrap_or_default();
    let matches = details.get("matches").and_then(Value::as_array).cloned().unwrap_or_default();
    if !file_matches.is_empty() {
        return file_matches
            .iter()
            .map(|file| {
                let path = file.get("path").and_then(Value::as_str).unwrap_or_default().to_owned();
                Group {
                    count: file.get("count").and_then(Value::as_u64),
                    rows: matches
                        .iter()
                        .filter(|row| row.get("path").and_then(Value::as_str) == Some(path.as_str()))
                        .cloned()
                        .collect(),
                    path,
                }
            })
            .collect();
    }
    let mut groups: Vec<Group> = Vec::new();
    for row in &matches {
        let path = row.get("path").and_then(Value::as_str).unwrap_or_default().to_owned();
        match groups.last_mut() {
            Some(last) if last.path == path => last.rows.push(row.clone()),
            _ => groups.push(Group { path, count: None, rows: vec![row.clone()] }),
        }
    }
    groups
}

fn overflow_hint(remaining: usize, unit: &str, theme: &Theme) -> String {
    format!(
        "{}{}{}",
        theme.fg(ThemeColor::Muted, &format!("... ({remaining} more {unit},")),
        key_hint("app.tools.expand", "to expand", theme),
        theme.fg(ThemeColor::Muted, ")")
    )
}

fn format_grouped_result(
    details: &Value,
    options: ToolRenderResultOptions,
    theme: &Theme,
    cwd: &str,
) -> String {
    let mut lines: Vec<String> = Vec::new();
    let groups = groups_from_details(details);
    if groups.is_empty() {
        lines.push(theme.fg(ThemeColor::ToolOutput, &no_match_text(details)));
    } else {
        let mut hidden_groups = 0;
        for (index, group) in groups.iter().enumerate() {
            let rows: Vec<&Value> = if options.expanded {
                group.rows.iter().collect()
            } else {
                group.rows.iter().filter(|row| !row.get("isContext").and_then(Value::as_bool).unwrap_or(false)).collect()
            };
            let mut block: Vec<String> = Vec::new();
            if !lines.is_empty() {
                block.push(String::new());
            }
            if rows.is_empty() && group.count.is_some() {
                block.push(format!(
                    "{}{}",
                    format_file_header(&group.path, theme, cwd),
                    theme.fg(ThemeColor::ToolOutput, &format!(": {}", group.count.unwrap_or(0)))
                ));
            } else {
                block.push(format_file_header(&group.path, theme, cwd));
                for row in rows {
                    block.push(format_match_row(row, theme));
                }
            }
            if !options.expanded && !lines.is_empty() && lines.len() + block.len() > COLLAPSED_LINE_BUDGET {
                hidden_groups = groups.len() - index;
                break;
            }
            lines.extend(block);
        }
        if hidden_groups > 0 {
            lines.push(overflow_hint(hidden_groups, if hidden_groups == 1 { "file" } else { "files" }, theme));
        }
    }
    format!("\n{}", lines.join("\n"))
}

fn format_text_fallback(
    result: &ToolResult,
    options: ToolRenderResultOptions,
    theme: &Theme,
    show_images: bool,
) -> String {
    let output = get_text_output(Some(result), show_images);
    let output = output.trim();
    if output.is_empty() {
        return String::new();
    }
    let lines: Vec<&str> = output.split('\n').collect();
    let max_lines = if options.expanded { lines.len() } else { COLLAPSED_LINE_BUDGET };
    let display_lines: Vec<&str> = lines.iter().take(max_lines).copied().collect();
    let remaining = lines.len().saturating_sub(max_lines);
    let mut text = format!(
        "\n{}",
        display_lines.iter().map(|line| theme.fg(ThemeColor::ToolOutput, line)).collect::<Vec<_>>().join("\n")
    );
    if remaining > 0 {
        text += &format!("\n{}", overflow_hint(remaining, if remaining == 1 { "line" } else { "lines" }, theme));
    }
    text
}

fn format_grep_result(
    result: &ToolResult,
    options: ToolRenderResultOptions,
    theme: &Theme,
    show_images: bool,
    cwd: &str,
) -> String {
    let details = result.details.clone().unwrap_or(Value::Null);
    if is_v1_details(&details) {
        return format_grouped_result(&details, options, theme, cwd);
    }
    format_text_fallback(result, options, theme, show_images)
}

#[derive(Default)]
pub struct GrepRenderers;

impl ToolRenderers for GrepRenderers {
    fn render_call(&mut self, theme: &Theme, context: &ToolRenderContext<'_>) -> Option<RenderedComponent> {
        let text = format_grep_call(Some(context.args), theme, context.cwd);
        Some(Rc::new(RefCell::new(Text::with_padding(text, 0, 0))))
    }

    fn render_result(
        &mut self,
        result: &ToolResult,
        options: ToolRenderResultOptions,
        theme: &Theme,
        context: &ToolRenderContext<'_>,
    ) -> Option<RenderedComponent> {
        let text = format_grep_result(result, options, theme, context.show_images, context.cwd);
        Some(Rc::new(RefCell::new(Text::with_padding(text, 0, 0))))
    }
}
