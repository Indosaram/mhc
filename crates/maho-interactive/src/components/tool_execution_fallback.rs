//! Port of `components/tool-execution-fallback.ts`.
use std::cell::RefCell;
use std::rc::Rc;

use maho_tools::definition::ToolResult;
use maho_tui::components::text::Text;
use maho_tui::tui::Component;
use maho_tui::utils::strip_terminal_sequences;
use serde_json::Value;

use crate::theme::{Theme, ThemeColor};
use crate::tools::render_utils::get_text_output;

const FALLBACK_STRING_MAX_LENGTH: usize = 160;
const FALLBACK_JSON_MAX_LENGTH: usize = 2000;
const JSON_VIEW_MAX_DEPTH: usize = 3;
const JSON_VIEW_MAX_ROWS: usize = 24;
const JSON_VIEW_MAX_VALUE_LENGTH: usize = 100;

fn sanitize_fallback_string(value: &str, max_length: usize) -> String {
    let stripped = strip_terminal_sequences(value);
    let mut sanitized = String::with_capacity(stripped.len());
    for character in stripped.chars() {
        if character <= '\u{1f}' || ('\u{7f}'..='\u{9f}').contains(&character) {
            sanitized.push(' ');
        } else {
            sanitized.push(character);
        }
    }
    let collapsed = sanitized.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max_length {
        return collapsed;
    }
    let taken: String = collapsed.chars().take(max_length.saturating_sub(3)).collect();
    format!("{taken}...")
}

fn sanitize_fallback_json_value(value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(sanitize_fallback_string(text, FALLBACK_STRING_MAX_LENGTH)),
        other => other.clone(),
    }
}

fn parse_renderable_json(text: &str) -> Option<Value> {
    let trimmed = text.trim();
    if !trimmed.starts_with('{') && !trimmed.starts_with('[') {
        return None;
    }
    let parsed: Value = serde_json::from_str(trimmed).ok()?;
    if parsed.is_object() || parsed.is_array() { Some(parsed) } else { None }
}

fn truncate_json_view_value(value: &str) -> String {
    if value.chars().count() > JSON_VIEW_MAX_VALUE_LENGTH {
        format!("{}…", value.chars().take(JSON_VIEW_MAX_VALUE_LENGTH).collect::<String>())
    } else {
        value.to_owned()
    }
}

fn style_json_primitive(value: &Value, theme: &Theme) -> String {
    match value {
        Value::String(text) => theme.fg(ThemeColor::ToolOutput, &truncate_json_view_value(text)),
        Value::Number(_) | Value::Bool(_) => theme.fg(ThemeColor::Accent, &value.to_string()),
        Value::Null => theme.fg(ThemeColor::Dim, "null"),
        other => theme.fg(ThemeColor::ToolOutput, &truncate_json_view_value(&other.to_string())),
    }
}

fn style_json_compact(value: &Value, theme: &Theme) -> String {
    theme.fg(ThemeColor::ToolOutput, &truncate_json_view_value(&value.to_string()))
}

fn collect_json_view_rows(value: &Value, theme: &Theme) -> (Vec<String>, usize) {
    let mut rows: Vec<String> = Vec::new();
    let mut omitted = 0usize;

    fn walk(node: &Value, depth: usize, rows: &mut Vec<String>, omitted: &mut usize, theme: &Theme) {
        let entries: Vec<(String, &Value)> = match node {
            Value::Array(items) => items.iter().enumerate().map(|(index, item)| (index.to_string(), item)).collect(),
            Value::Object(map) => map.iter().map(|(key, item)| (key.clone(), item)).collect(),
            _ => return,
        };
        let indent = "  ".repeat(depth);
        for (key, entry_value) in entries {
            if rows.len() >= JSON_VIEW_MAX_ROWS {
                *omitted += 1;
                continue;
            }
            let label = format!("{indent}{}", theme.fg(ThemeColor::Muted, &format!("{key}:")));
            if entry_value.is_object() || entry_value.is_array() {
                if depth + 1 >= JSON_VIEW_MAX_DEPTH {
                    rows.push(format!("{label} {}", style_json_compact(entry_value, theme)));
                    continue;
                }
                rows.push(label);
                walk(entry_value, depth + 1, rows, omitted, theme);
                continue;
            }
            rows.push(format!("{label} {}", style_json_primitive(entry_value, theme)));
        }
    }

    walk(value, 0, &mut rows, &mut omitted, theme);
    (rows, omitted)
}

fn render_json_view(value: &Value, theme: &Theme) -> String {
    let (mut rows, omitted) = collect_json_view_rows(value, theme);
    if rows.is_empty() {
        return theme.fg(ThemeColor::Dim, "(empty)");
    }
    if omitted > 0 {
        rows.push(theme.fg(ThemeColor::Dim, &format!("… {omitted} more")));
    }
    rows.join("\n")
}

pub fn create_tool_call_fallback(tool_name: &str, theme: &Theme) -> Rc<RefCell<dyn Component>> {
    Rc::new(RefCell::new(Text::with_padding(theme.fg(ThemeColor::ToolTitle, &theme.bold(tool_name)), 0, 0)))
}

pub fn create_tool_result_fallback(
    result: Option<&ToolResult>,
    show_images: bool,
    theme: &Theme,
) -> Option<Rc<RefCell<dyn Component>>> {
    let output = get_text_output(result, show_images);
    if output.is_empty() {
        return None;
    }
    match parse_renderable_json(&output) {
        Some(parsed) => Some(Rc::new(RefCell::new(Text::with_padding(render_json_view(&parsed, theme), 0, 0)))),
        None => Some(Rc::new(RefCell::new(Text::with_padding(theme.fg(ThemeColor::ToolOutput, &output), 0, 0)))),
    }
}

pub fn format_tool_execution_fallback(
    tool_name: &str,
    args: &Value,
    result: Option<&ToolResult>,
    show_images: bool,
    theme: &Theme,
) -> String {
    let mut text = theme.fg(ThemeColor::ToolTitle, &theme.bold(&sanitize_fallback_string(tool_name, FALLBACK_STRING_MAX_LENGTH)));
    let sanitized_args = match args {
        Value::Object(map) => {
            Value::Object(map.iter().map(|(key, value)| (key.clone(), sanitize_fallback_json_value(value))).collect())
        }
        other => sanitize_fallback_json_value(other),
    };
    let content = serde_json::to_string_pretty(&sanitized_args).unwrap_or_default();
    if !content.is_empty() {
        let bounded = if content.chars().count() > FALLBACK_JSON_MAX_LENGTH {
            format!("{}...", content.chars().take(FALLBACK_JSON_MAX_LENGTH - 3).collect::<String>())
        } else {
            content
        };
        text += &format!("\n\n{bounded}");
    }
    let output = get_text_output(result, show_images);
    if !output.is_empty() {
        text += &format!("\n{}", sanitize_fallback_string(&output, FALLBACK_JSON_MAX_LENGTH));
    }
    text
}
