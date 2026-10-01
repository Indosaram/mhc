use crate::theme::{Theme, ThemeColor};
use maho_tools::definition::{ToolContent, ToolResult};
pub use maho_tools::write::{WriteBaseline, WriteToolDetails, create_write_details, read_local_write_baseline};

pub fn format_write_result(result: &ToolResult, is_error: bool, theme: &Theme) -> Option<String> {
    if !is_error { return None; }
    let output = result.content.iter().filter_map(|content| match content {
        ToolContent::Text { text, .. } => Some(text.as_str()),
        ToolContent::Image { .. } => None,
    }).collect::<Vec<_>>().join("\n");
    if output.is_empty() { None } else { Some(format!("\n{}", theme.fg(ThemeColor::Error, &output))) }
}
