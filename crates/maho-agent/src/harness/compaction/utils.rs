//! Port of senpi packages/agent/src/harness/compaction/utils.ts.

use std::collections::BTreeSet;

use maho_ai::types::{ContentBlock, Message, UserContent};
use maho_ai::utils::text::content_text;
use serde_json::Value;

use crate::types::AgentMessage;

/// File paths touched by a session branch or compaction range.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileOperations {
    pub read: BTreeSet<String>,
    pub written: BTreeSet<String>,
    pub edited: BTreeSet<String>,
}

/// Create an empty file-operation accumulator.
pub fn create_file_ops() -> FileOperations {
    FileOperations::default()
}

/// Add file operations from assistant tool calls to an accumulator.
pub fn extract_file_ops_from_message(message: &AgentMessage, file_ops: &mut FileOperations) {
    let AgentMessage::Llm(Message::Assistant(assistant)) = message else {
        return;
    };
    for block in &assistant.content {
        let ContentBlock::ToolCall(call) = block else {
            continue;
        };
        let Some(path) = call.arguments.get("path").and_then(Value::as_str) else {
            continue;
        };
        match call.name.as_str() {
            "read" => {
                file_ops.read.insert(path.to_owned());
            }
            "write" => {
                file_ops.written.insert(path.to_owned());
            }
            "edit" => {
                file_ops.edited.insert(path.to_owned());
            }
            _ => {}
        }
    }
}

/// Compute sorted read-only and modified file lists from accumulated operations.
pub fn compute_file_lists(file_ops: &FileOperations) -> (Vec<String>, Vec<String>) {
    let modified: BTreeSet<&String> = file_ops.edited.iter().chain(file_ops.written.iter()).collect();
    let read_only: Vec<String> = file_ops
        .read
        .iter()
        .filter(|path| !modified.contains(path))
        .cloned()
        .collect();
    let modified_files: Vec<String> = modified.into_iter().cloned().collect();
    (read_only, modified_files)
}

/// Format file lists as summary metadata tags.
pub fn format_file_operations(read_files: &[String], modified_files: &[String]) -> String {
    let mut sections: Vec<String> = Vec::new();
    if !read_files.is_empty() {
        sections.push(format!("<read-files>\n{}\n</read-files>", read_files.join("\n")));
    }
    if !modified_files.is_empty() {
        sections.push(format!(
            "<modified-files>\n{}\n</modified-files>",
            modified_files.join("\n")
        ));
    }
    if sections.is_empty() {
        return String::new();
    }
    format!("\n\n{}", sections.join("\n\n"))
}

const TOOL_RESULT_MAX_CHARS: usize = 2000;

fn safe_json_stringify(value: &Value) -> String {
    value.to_string()
}

fn truncate_for_summary(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let truncated_chars = text.chars().count() - max_chars;
    let head: String = text.chars().take(max_chars).collect();
    format!("{head}\n\n[... {truncated_chars} more characters truncated]")
}

fn portable_content(content: &[ContentBlock]) -> Vec<ContentBlock> {
    content
        .iter()
        .filter(|block| !matches!(block, ContentBlock::ProviderNative(_)))
        .cloned()
        .collect()
}

/// Extract text from content that may retain provider-native replay blocks.
pub fn content_text_for_summary(content: &[ContentBlock], separator: &str) -> String {
    content_text(&portable_content(content), separator)
}

fn message_content_text(content: &UserContent, separator: &str) -> String {
    match content {
        UserContent::Text(text) => text.clone(),
        UserContent::Blocks(blocks) => content_text_for_summary(blocks, separator),
    }
}

/// Serialize LLM messages to plain text for summarization prompts.
pub fn serialize_conversation(messages: &[Message]) -> String {
    let mut parts: Vec<String> = Vec::new();

    for message in messages {
        match message {
            Message::User(user) => {
                let content = message_content_text(&user.content, "");
                if !content.is_empty() {
                    parts.push(format!("[User]: {content}"));
                }
            }
            Message::Assistant(assistant) => {
                let mut thinking_parts: Vec<String> = Vec::new();
                let mut tool_calls: Vec<String> = Vec::new();

                for block in &assistant.content {
                    match block {
                        ContentBlock::Thinking(thinking) => thinking_parts.push(thinking.thinking.clone()),
                        ContentBlock::ToolCall(call) => {
                            let args = call
                                .arguments
                                .iter()
                                .map(|(key, value)| format!("{key}={}", safe_json_stringify(value)))
                                .collect::<Vec<String>>()
                                .join(", ");
                            tool_calls.push(format!("{}({args})", call.name));
                        }
                        _ => {}
                    }
                }

                if !thinking_parts.is_empty() {
                    parts.push(format!("[Assistant thinking]: {}", thinking_parts.join("\n")));
                }
                if assistant
                    .content
                    .iter()
                    .any(|block| matches!(block, ContentBlock::Text(_)))
                {
                    parts.push(format!(
                        "[Assistant]: {}",
                        content_text_for_summary(&assistant.content, "\n")
                    ));
                }
                if !tool_calls.is_empty() {
                    parts.push(format!("[Assistant tool calls]: {}", tool_calls.join("; ")));
                }
            }
            Message::ToolResult(result) => {
                let content = content_text_for_summary(&result.content, "");
                if !content.is_empty() {
                    parts.push(format!(
                        "[Tool result]: {}",
                        truncate_for_summary(&content, TOOL_RESULT_MAX_CHARS)
                    ));
                }
            }
            Message::ConfigurationUpdate(_) => {}
        }
    }

    parts.join("\n\n")
}
