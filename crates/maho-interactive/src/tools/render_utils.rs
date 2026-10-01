//! Port of `core/tools/render-utils.ts`.
use maho_agent::harness::utils::shell_output::sanitize_binary_output;
use maho_core::paths::{PathInputOptions, resolve_path, shorten_path as shorten_path_string};
use maho_tui::terminal_image::{get_capabilities, get_image_dimensions, hyperlink, image_fallback, path_to_file_url};
use maho_tui::utils::strip_terminal_sequences;
use maho_tools::definition::{ToolContent, ToolResult};
use maho_tools::model_only_text::is_model_only_text;
use serde_json::Value;

use crate::theme::{Theme, ThemeColor};

pub fn shorten_path(path: Option<&Value>) -> String {
    let Some(text) = path.and_then(Value::as_str) else {
        return String::new();
    };
    shorten_path_string(text)
}

pub fn link_path(styled_text: &str, raw_path: &str, cwd: &str) -> String {
    if !get_capabilities().hyperlinks {
        return styled_text.to_owned();
    }
    let absolute_path = resolve_path(raw_path, cwd, &PathInputOptions::default());
    hyperlink(styled_text, &path_to_file_url(&absolute_path))
}

/// senpi's `str`: a string passes through, `null`/`undefined` become the empty string, and any
/// other type is `null` (the "invalid arg" signal).
pub fn str_value(value: Option<&Value>) -> Option<Option<String>> {
    match value {
        None | Some(Value::Null) => Some(Some(String::new())),
        Some(Value::String(text)) => Some(Some(text.clone())),
        Some(_) => None,
    }
}

pub fn replace_tabs(text: &str) -> String {
    text.replace('\t', "   ")
}

pub fn normalize_display_text(text: &str) -> String {
    text.replace('\r', "")
}

pub fn get_text_output(result: Option<&ToolResult>, show_images: bool) -> String {
    let Some(result) = result else {
        return String::new();
    };

    let text_blocks: Vec<&ToolContent> = result
        .content
        .iter()
        .filter(|content| matches!(content, ToolContent::Text { .. }) && !is_model_only_text(content))
        .collect();
    let image_blocks: Vec<&ToolContent> = result
        .content
        .iter()
        .filter(|content| matches!(content, ToolContent::Image { .. }))
        .collect();

    let mut output = text_blocks
        .iter()
        .map(|content| match content {
            ToolContent::Text { text, .. } => {
                sanitize_binary_output(&strip_terminal_sequences(text)).replace('\r', "")
            }
            ToolContent::Image { .. } => String::new(),
        })
        .collect::<Vec<_>>()
        .join("\n");

    let capabilities = get_capabilities();
    if !image_blocks.is_empty() && (capabilities.images.is_none() || !show_images) {
        let indicators = image_blocks
            .iter()
            .map(|content| match content {
                ToolContent::Image { data, mime_type } => {
                    let dimensions = get_image_dimensions(data, mime_type);
                    image_fallback(mime_type, dimensions, None)
                }
                ToolContent::Text { .. } => String::new(),
            })
            .collect::<Vec<_>>()
            .join("\n");
        output = if output.is_empty() { indicators } else { format!("{output}\n{indicators}") };
    }

    output
}

pub fn invalid_arg_text(theme: &Theme) -> String {
    theme.fg(ThemeColor::Error, "[invalid arg]")
}

pub fn render_tool_path(
    raw_path: Option<String>,
    theme: &Theme,
    cwd: &str,
    empty_fallback: Option<&str>,
) -> String {
    let Some(raw_path) = raw_path else {
        return invalid_arg_text(theme);
    };
    let value = if raw_path.is_empty() { empty_fallback } else { Some(raw_path.as_str()) };
    let Some(value) = value else {
        return theme.fg(ThemeColor::ToolOutput, "...");
    };
    link_path(&theme.fg(ThemeColor::Accent, &shorten_path_string(value)), value, cwd)
}
