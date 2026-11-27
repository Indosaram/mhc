//! Port of `packages/coding-agent/src/core/tools/read-text.ts`.

use crate::definition::{ToolContent, ToolError, ToolFuture};
use crate::model_only_text::model_only_text;
use crate::truncate::{
    DEFAULT_MAX_BYTES, TruncationOptions, TruncationResult, format_size, select_lines,
    truncate_head,
};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;

pub struct ReadSummaryInput<'a> {
    pub path: &'a Path,
    pub text: &'a str,
    pub offset: Option<usize>,
    pub limit: Option<usize>,
    pub truncated: bool,
}

pub trait ReadFolder: Send + Sync {
    fn summarize<'a>(&'a self, input: ReadSummaryInput<'a>) -> ToolFuture<'a, Option<String>>;
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ReadToolDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncation: Option<TruncationResult>,
}

pub struct ShapedTextRead {
    pub content: Vec<ToolContent>,
    pub details: Option<Value>,
}

/// Format text read output: selected lines, optional structural folding, or head truncation with
/// a model-only continuation notice.
pub async fn shape_text_read(
    path: &str,
    absolute_path: &Path,
    text_content: &str,
    offset: Option<usize>,
    limit: Option<usize>,
    folder: Option<&Arc<dyn ReadFolder>>,
) -> Result<ShapedTextRead, ToolError> {
    let total_file_lines = text_content.split('\n').count();
    let start_line = offset.map_or(0, |o| o.saturating_sub(1));
    let start_line_display = start_line + 1;
    if start_line >= total_file_lines {
        return Err(ToolError::Message(format!(
            "Offset {} is beyond end of file ({} lines total)",
            offset.unwrap_or(1),
            total_file_lines
        )));
    }

    let end_line = limit.map_or(total_file_lines, |l| {
        start_line.saturating_add(l).min(total_file_lines)
    });
    let count = (end_line < total_file_lines).then_some(end_line - start_line);
    let selected_content = select_lines(text_content, start_line, count);
    let truncation = truncate_head(selected_content, TruncationOptions::default());

    let summary = if let Some(folder) = folder {
        folder
            .summarize(ReadSummaryInput {
                path: absolute_path,
                text: text_content,
                offset,
                limit,
                truncated: truncation.truncated,
            })
            .await?
    } else {
        None
    };

    let mut details: Option<Value> = None;
    let mut notice: Option<String> = None;
    let output_text = if let Some(summary_text) = summary {
        summary_text
    } else if truncation.first_line_exceeds_limit {
        let first_line_size = format_size(
            selected_content
                .find('\n')
                .unwrap_or(selected_content.len()),
        );
        notice = Some(format!(
            "[Line {start_line_display} is {first_line_size}, exceeds {} limit. Use bash: sed -n '{start_line_display}p' {path} | head -c {DEFAULT_MAX_BYTES}]",
            format_size(DEFAULT_MAX_BYTES)
        ));
        details = Some(json!({ "truncation": truncation }));
        String::new()
    } else if truncation.truncated {
        let end_line_display = start_line_display + truncation.output_lines - 1;
        let next_offset = end_line_display + 1;
        notice = Some(if truncation.truncated_by.as_deref() == Some("lines") {
            format!(
                "[Showing lines {start_line_display}-{end_line_display} of {total_file_lines}. Use offset={next_offset} to continue.]"
            )
        } else {
            format!(
                "[Showing lines {start_line_display}-{end_line_display} of {total_file_lines} ({} limit). Use offset={next_offset} to continue.]",
                format_size(DEFAULT_MAX_BYTES)
            )
        });
        details = Some(json!({ "truncation": truncation }));
        truncation.content
    } else {
        if limit.is_some() && end_line < total_file_lines {
            let remaining = total_file_lines - end_line;
            let next_offset = end_line + 1;
            notice = Some(format!(
                "[{remaining} more lines in file. Use offset={next_offset} to continue.]"
            ));
        }
        truncation.content
    };

    let mut content = Vec::new();
    if !output_text.is_empty() || notice.is_none() {
        content.push(ToolContent::text(format!(
            "{output_text}{}",
            if notice.is_some() { "\n" } else { "" }
        )));
    }
    if let Some(notice) = notice {
        content.push(model_only_text(notice));
    }
    Ok(ShapedTextRead { content, details })
}
