use super::{
    image::{detect_supported_image_mime_type, encode_base64},
    path_utils::resolve_read_tool_path,
    tool_context::HasExecutionToolContext,
};
use crate::{
    harness::{
        context::Context,
        types::AgentHarnessTool,
        utils::truncate::{
            DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, TruncatedBy, TruncationOptions, TruncationResult,
            format_size, truncate_head,
        },
    },
    types::AgentToolResult,
};
use maho_ai::types::{BoxFuture, ContentBlock, ImageContent, Tool};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadToolInput {
    pub path: String,
    pub offset: Option<f64>,
    pub limit: Option<f64>,
}
#[derive(Debug, Clone)]
pub struct ReadToolDetails {
    pub truncation: Option<TruncationResult>,
}
pub enum ReadImageProcessorResult {
    Ok {
        data: String,
        mime_type: String,
        hints: Vec<String>,
    },
    Err {
        message: String,
    },
}
#[derive(Debug, Clone, Copy)]
pub struct ReadImageProcessorOptions {
    pub auto_resize_images: bool,
}
pub type ReadImageProcessor = Arc<
    dyn Fn(
            Vec<u8>,
            String,
            ReadImageProcessorOptions,
            Context,
        ) -> BoxFuture<'static, ReadImageProcessorResult>
        + Send
        + Sync,
>;
#[derive(Clone, Default)]
pub struct ReadToolOptions {
    pub auto_resize_images: Option<bool>,
    pub image_processor: Option<ReadImageProcessor>,
}
fn truncation_json(t: &TruncationResult) -> serde_json::Value {
    json!({"content":t.content,"truncated":t.truncated,"truncatedBy": t.truncated_by.map(|by| match by { TruncatedBy::Lines => "lines", TruncatedBy::Bytes => "bytes" }),"totalLines":t.total_lines,"totalBytes":t.total_bytes,"outputLines":t.output_lines,"outputBytes":t.output_bytes,"lastLinePartial":t.last_line_partial,"firstLineExceedsLimit":t.first_line_exceeds_limit,"maxLines":t.max_lines,"maxBytes":t.max_bytes})
}
fn image_text(text: impl Into<String>) -> AgentToolResult {
    let mut result = AgentToolResult::text(text);
    result.details = serde_json::Value::Null;
    result
}
pub fn create_read_tool<T: HasExecutionToolContext>(
    options: ReadToolOptions,
) -> AgentHarnessTool<T> {
    let options = Arc::new(options);
    AgentHarnessTool {
        label: "read".into(),
        prepare_arguments: None,
        replay: None,
        tool: Tool {
            name: "read".into(),
            description: format!(
                "Read the contents of a file. Supports text files and images (jpg, png, gif, webp, bmp). Images are sent as attachments. For text files, output is truncated to {DEFAULT_MAX_LINES} lines or {}KB (whichever is hit first). Use offset/limit for large files. When you need the full file, continue with offset until complete.",
                DEFAULT_MAX_BYTES / 1024
            ),
            parameters: json!({"type":"object","properties":{"path":{"type":"string","description":"Path to the file to read (relative or absolute)"},"offset":{"type":"number","description":"Line number to start reading from (1-indexed)"},"limit":{"type":"number","description":"Maximum number of lines to read"}},"required":["path"]}),
            freeform: None,
            constrained_sampling: None,
        },
        execute: Arc::new(move |_, input, _, turn: T, _, context| {
            let options = options.clone();
            Box::pin(async move {
                let input: ReadToolInput =
                    serde_json::from_value(input).map_err(|e| e.to_string())?;
                let env = &turn.execution_tool_context().env;
                let absolute = resolve_read_tool_path(env.as_ref(), &input.path, &context).await?;
                let bytes = env
                    .read_binary_file(&absolute, &context)
                    .await
                    .map_err(|e| e.to_string())?;
                if context.is_aborted() {
                    return Err("Operation aborted".into());
                }
                if let Some(mime) = detect_supported_image_mime_type(&bytes) {
                    if let Some(processor) = &options.image_processor {
                        return Ok(
                            match processor(
                                bytes,
                                mime.into(),
                                ReadImageProcessorOptions {
                                    auto_resize_images: options.auto_resize_images.unwrap_or(true),
                                },
                                context,
                            )
                            .await
                            {
                                ReadImageProcessorResult::Err { message } => {
                                    image_text(format!("Read image file [{mime}]\n{message}"))
                                }
                                ReadImageProcessorResult::Ok {
                                    data,
                                    mime_type,
                                    hints,
                                } => {
                                    let mut result = image_text(format!(
                                        "Read image file [{mime_type}]{}",
                                        if hints.is_empty() {
                                            String::new()
                                        } else {
                                            format!("\n{}", hints.join("\n"))
                                        }
                                    ));
                                    result.content.push(ContentBlock::Image(ImageContent {
                                        data,
                                        mime_type,
                                    }));
                                    result
                                }
                            },
                        );
                    }
                    if mime == "image/bmp" {
                        return Ok(image_text(
                            "Read image file [image/bmp]\n[Image omitted: configure an imageProcessor to convert BMP images.]",
                        ));
                    }
                    let mut result = image_text(format!("Read image file [{mime}]"));
                    result.content.push(ContentBlock::Image(ImageContent {
                        data: encode_base64(&bytes),
                        mime_type: mime.into(),
                    }));
                    return Ok(result);
                }
                let text = String::from_utf8_lossy(&bytes);
                let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
                let lines: Vec<_> = text.split('\n').collect();
                let start = input
                    .offset
                    .filter(|v| *v != 0.0)
                    .map_or(0.0, |v| (v - 1.0).max(0.0));
                if start >= lines.len() as f64 {
                    return Err(format!(
                        "Offset {} is beyond end of file ({} lines total)",
                        input.offset.unwrap_or(0.0),
                        lines.len()
                    ));
                }
                let slice_start = start as usize;
                let end = input
                    .limit
                    .map(|limit| (start + limit).min(lines.len() as f64));
                let slice_end = end.map_or(lines.len(), |end| {
                    if end < 0.0 {
                        (lines.len() as f64 + end).max(0.0) as usize
                    } else {
                        end as usize
                    }
                });
                let selected = if slice_end < slice_start {
                    String::new()
                } else {
                    lines[slice_start..slice_end].join("\n")
                };
                let t = truncate_head(&selected, TruncationOptions::default());
                let mut details = serde_json::Value::Null;
                let display = start + 1.0;
                let output = if t.first_line_exceeds_limit {
                    details = json!({"truncation":truncation_json(&t)});
                    format!(
                        "[Line {display} is {}, exceeds {} limit. Use bash: sed -n '{display}p' {} | head -c {DEFAULT_MAX_BYTES}]",
                        format_size(lines[slice_start].len() as u64),
                        format_size(DEFAULT_MAX_BYTES),
                        input.path
                    )
                } else if t.truncated {
                    details = json!({"truncation":truncation_json(&t)});
                    let end = display + t.output_lines as f64 - 1.0;
                    let next = end + 1.0;
                    let limit = if t.truncated_by == Some(TruncatedBy::Lines) {
                        String::new()
                    } else {
                        format!(" ({} limit)", format_size(DEFAULT_MAX_BYTES))
                    };
                    format!(
                        "{}\n\n[Showing lines {display}-{end} of {}{limit}. Use offset={next} to continue.]",
                        t.content,
                        lines.len()
                    )
                } else if let Some(end) = end.filter(|end| *end < lines.len() as f64) {
                    format!(
                        "{}\n\n[{} more lines in file. Use offset={} to continue.]",
                        t.content,
                        lines.len() as f64 - end,
                        end + 1.0
                    )
                } else {
                    t.content
                };
                let mut result = AgentToolResult::text(output);
                result.details = details;
                Ok(result)
            })
        }),
    }
}
