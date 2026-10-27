//! Port of senpi `packages/coding-agent/src/core/compaction/utils.ts`.
//!
//! `extract_patched_paths` mirrors the helper the gpt-apply-patch extension owns in senpi
//! (`extensions/builtin/gpt-apply-patch/text.ts`); the Rust extension crates depend on maho-core,
//! so the two copies must stay in sync.

use indexmap::IndexSet;
use serde_json::Value;

/// `FileOperations`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileOperations {
    pub read: IndexSet<String>,
    pub written: IndexSet<String>,
    pub edited: IndexSet<String>,
}

/// `createFileOps`.
pub fn create_file_ops() -> FileOperations {
    FileOperations::default()
}

/// `normalizePatchText`.
pub fn normalize_patch_text(patch_text: &str) -> String {
    patch_text.replace("\r\n", "\n").replace('\r', "\n")
}

/// JS `\s` / `String.prototype.trim` whitespace.
fn js_whitespace(character: char) -> bool {
    matches!(character, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')
}

/// `stripHeredoc`: `^(?:cat\s+)?<<['"]?(\w+)['"]?\s*\n([\s\S]*?)\n\1\s*$` (JS `\w` is ASCII-only).
/// Same algorithm as the gpt-apply-patch extension's `text::strip_heredoc`.
pub fn strip_heredoc(input: &str) -> String {
    let mut header = input;
    if let Some(rest) = header.strip_prefix("cat") {
        if !rest.starts_with(js_whitespace) {
            return input.to_string();
        }
        header = rest.trim_start_matches(js_whitespace);
    }
    let Some(rest) = header.strip_prefix("<<") else {
        return input.to_string();
    };
    let rest = rest.strip_prefix(['\'', '"']).unwrap_or(rest);
    let length = rest
        .bytes()
        .take_while(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        .count();
    if length == 0 {
        return input.to_string();
    }
    let tag = &rest[..length];
    let rest = rest[length..].strip_prefix(['\'', '"']).unwrap_or(&rest[length..]);
    let newlines = rest
        .char_indices()
        .take_while(|(_, character)| js_whitespace(*character))
        .filter(|(_, character)| *character == '\n')
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    for newline in newlines.into_iter().rev() {
        let body = &rest[newline + 1..];
        for (index, _) in body.match_indices('\n') {
            let tail = &body[index + 1..];
            if tail
                .strip_prefix(tag)
                .is_some_and(|suffix| suffix.chars().all(js_whitespace))
            {
                return body[..index].to_string();
            }
        }
    }
    input.to_string()
}

/// `extractPatchedPaths` (gpt-apply-patch `text.ts`): headers are matched on the trimmed line.
pub fn extract_patched_paths(patch_text: &str) -> Vec<String> {
    strip_heredoc(&normalize_patch_text(patch_text))
        .split(['\n', '\u{2028}', '\u{2029}'])
        .filter_map(|line| {
            let trimmed = line.trim_matches(js_whitespace);
            ["*** Add File: ", "*** Delete File: ", "*** Update File: ", "*** Move to: "]
                .iter()
                .find_map(|marker| trimmed.strip_prefix(marker))
                .map(str::to_owned)
        })
        .collect()
}

/// `extractFileOpsFromMessage`: tool calls in an assistant message, or the nested calls a codemode
/// script recorded on its tool result.
pub fn extract_file_ops_from_message(message: &Value, file_ops: &mut FileOperations) {
    match message.get("role").and_then(Value::as_str) {
        Some("toolResult") => {
            let calls = message
                .get("nestedCalls")
                .and_then(|nested| nested.get("calls"))
                .and_then(Value::as_array);
            for call in calls.into_iter().flatten() {
                if let Some(name) = call.get("name").and_then(Value::as_str) {
                    add_file_op(name, call.get("arguments"), file_ops);
                }
            }
        }
        Some("assistant") => {
            let Some(content) = message.get("content").and_then(Value::as_array) else {
                return;
            };
            for block in content {
                if block.get("type").and_then(Value::as_str) != Some("toolCall") {
                    continue;
                }
                if let Some(name) = block.get("name").and_then(Value::as_str) {
                    add_file_op(name, block.get("arguments"), file_ops);
                }
            }
        }
        _ => {}
    }
}

/// `addFileOp`.
fn add_file_op(tool_name: &str, arguments: Option<&Value>, file_ops: &mut FileOperations) {
    let Some(arguments) = arguments.filter(|arguments| !arguments.is_null()) else {
        return;
    };
    let path = || arguments.get("path").and_then(Value::as_str).map(str::to_string);
    match tool_name {
        "read" => file_ops.read.extend(path()),
        "write" => file_ops.written.extend(path()),
        "edit" => file_ops.edited.extend(path()),
        "apply_patch" => {
            if let Some(input) = arguments.get("input").and_then(Value::as_str) {
                file_ops.edited.extend(extract_patched_paths(input));
            }
        }
        _ => {}
    }
}

/// `computeFileLists`: files only read, and files modified.
pub fn compute_file_lists(file_ops: &FileOperations) -> (Vec<String>, Vec<String>) {
    let modified: IndexSet<String> = file_ops.edited.iter().chain(file_ops.written.iter()).cloned().collect();
    let mut read_only: Vec<String> = file_ops.read.iter().filter(|path| !modified.contains(*path)).cloned().collect();
    read_only.sort();
    let mut modified_files: Vec<String> = modified.into_iter().collect();
    modified_files.sort();
    (read_only, modified_files)
}

/// `formatFileOperations`.
pub fn format_file_operations(read_files: &[String], modified_files: &[String]) -> String {
    let mut sections: Vec<String> = Vec::new();
    if !read_files.is_empty() {
        sections.push(format!("<read-files>\n{}\n</read-files>", read_files.join("\n")));
    }
    if !modified_files.is_empty() {
        sections.push(format!("<modified-files>\n{}\n</modified-files>", modified_files.join("\n")));
    }
    if sections.is_empty() {
        return String::new();
    }
    format!("\n\n{}", sections.join("\n\n"))
}

/// `TOOL_RESULT_MAX_CHARS`.
pub const TOOL_RESULT_MAX_CHARS: usize = 2000;

/// `truncateForSummary`.
pub fn truncate_for_summary(text: &str, max_chars: usize) -> String {
    let length = text.chars().count();
    if length <= max_chars {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max_chars).collect();
    format!("{truncated}\n\n[... {} more characters truncated]", length - max_chars)
}

/// `contentTextForSummary`: provider-native replay blocks are dropped before the join.
pub fn content_text_for_summary(content: &Value, separator: &str) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) != Some("providerNative"))
            .filter_map(|block| match block.get("type").and_then(Value::as_str) {
                Some("text") => block.get("text").and_then(Value::as_str),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(separator),
        _ => String::new(),
    }
}

/// `serializeConversation`.
pub fn serialize_conversation(messages: &[Value]) -> String {
    let mut parts: Vec<String> = Vec::new();

    for message in messages {
        match message.get("role").and_then(Value::as_str) {
            Some("user") => {
                let content = content_text_for_summary(message.get("content").unwrap_or(&Value::Null), "");
                if !content.is_empty() {
                    parts.push(format!("[User]: {content}"));
                }
            }
            Some("assistant") => {
                let mut thinking_parts: Vec<String> = Vec::new();
                let mut tool_calls: Vec<String> = Vec::new();
                let mut has_text = false;

                if let Some(blocks) = message.get("content").and_then(Value::as_array) {
                    for block in blocks {
                        match block.get("type").and_then(Value::as_str) {
                            Some("thinking") => {
                                if let Some(thinking) = block.get("thinking").and_then(Value::as_str) {
                                    thinking_parts.push(thinking.to_string());
                                }
                            }
                            Some("text") => has_text = true,
                            Some("toolCall") => {
                                let name = block.get("name").and_then(Value::as_str).unwrap_or_default();
                                let args_str = match block.get("arguments") {
                                    Some(Value::Object(arguments)) => arguments
                                        .iter()
                                        .map(|(key, value)| format!("{key}={}", compact_json(value)))
                                        .collect::<Vec<_>>()
                                        .join(", "),
                                    _ => String::new(),
                                };
                                tool_calls.push(format!("{name}({args_str})"));
                            }
                            _ => {}
                        }
                    }
                }

                if !thinking_parts.is_empty() {
                    parts.push(format!("[Assistant thinking]: {}", thinking_parts.join("\n")));
                }
                if has_text {
                    parts.push(format!("[Assistant]: {}", content_text_for_summary(message.get("content").unwrap_or(&Value::Null), "\n")));
                }
                if !tool_calls.is_empty() {
                    parts.push(format!("[Assistant tool calls]: {}", tool_calls.join("; ")));
                }
            }
            Some("toolResult") => {
                let content = content_text_for_summary(message.get("content").unwrap_or(&Value::Null), "");
                if !content.is_empty() {
                    parts.push(format!("[Tool result]: {}", truncate_for_summary(&content, TOOL_RESULT_MAX_CHARS)));
                }
            }
            _ => {}
        }
    }

    parts.join("\n\n")
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

/// `SUMMARIZATION_SYSTEM_PROMPT`.
pub const SUMMARIZATION_SYSTEM_PROMPT: &str = "You are a context summarization assistant. Your task is to read a conversation between a user and an AI assistant, then produce a structured summary following the exact format specified.

Do NOT continue the conversation. Do NOT respond to any questions in the conversation. ONLY output the structured summary.";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn assistant_with_tool_call(name: &str, arguments: Value) -> Value {
        json!({ "role": "assistant", "content": [{ "type": "toolCall", "name": name, "arguments": arguments }] })
    }

    #[test]
    fn tool_calls_record_read_write_and_edit_paths() {
        let mut file_ops = create_file_ops();
        extract_file_ops_from_message(&assistant_with_tool_call("read", json!({ "path": "/a" })), &mut file_ops);
        extract_file_ops_from_message(&assistant_with_tool_call("write", json!({ "path": "/b" })), &mut file_ops);
        extract_file_ops_from_message(&assistant_with_tool_call("edit", json!({ "path": "/c" })), &mut file_ops);
        extract_file_ops_from_message(&assistant_with_tool_call("read", json!({ "path": 5 })), &mut file_ops);
        assert_eq!(file_ops.read.iter().collect::<Vec<_>>(), vec![&"/a".to_string()]);
        assert_eq!(file_ops.written.iter().collect::<Vec<_>>(), vec![&"/b".to_string()]);
        assert_eq!(file_ops.edited.iter().collect::<Vec<_>>(), vec![&"/c".to_string()]);
    }

    #[test]
    fn an_apply_patch_records_every_patched_path() {
        let patch = "*** Begin Patch\n*** Update File: /one.ts\n*** Add File: /two.ts\n*** Move to: /three.ts\n*** End Patch";
        let mut file_ops = create_file_ops();
        extract_file_ops_from_message(&assistant_with_tool_call("apply_patch", json!({ "input": patch })), &mut file_ops);
        assert_eq!(file_ops.edited.iter().cloned().collect::<Vec<_>>(), vec!["/one.ts", "/two.ts", "/three.ts"]);
    }

    #[test]
    fn a_heredoc_wrapped_patch_is_unwrapped_first() {
        let patch = "cat <<'EOF'\n*** Update File: /wrapped.ts\nEOF";
        assert_eq!(extract_patched_paths(patch), vec!["/wrapped.ts"]);
    }

    #[test]
    fn non_assistant_messages_contribute_nothing() {
        let mut file_ops = create_file_ops();
        extract_file_ops_from_message(&json!({ "role": "user", "content": [{ "type": "toolCall", "name": "read", "arguments": { "path": "/a" } }] }), &mut file_ops);
        assert!(file_ops.read.is_empty());
    }

    #[test]
    fn file_lists_split_read_only_from_modified_and_sort() {
        let mut file_ops = create_file_ops();
        file_ops.read.insert("/z".to_string());
        file_ops.read.insert("/a".to_string());
        file_ops.read.insert("/m".to_string());
        file_ops.edited.insert("/m".to_string());
        file_ops.written.insert("/b".to_string());
        let (read_files, modified_files) = compute_file_lists(&file_ops);
        assert_eq!(read_files, vec!["/a".to_string(), "/z".to_string()]);
        assert_eq!(modified_files, vec!["/b".to_string(), "/m".to_string()]);
    }

    #[test]
    fn the_file_operation_block_renders_xml_sections() {
        let (read_files, modified_files) = (vec!["/a".to_string()], vec!["/b".to_string()]);
        let rendered = format_file_operations(&read_files, &modified_files);
        assert_eq!(rendered, "\n\n<read-files>\n/a\n</read-files>\n\n<modified-files>\n/b\n</modified-files>");
        assert_eq!(format_file_operations(&[], &[]), "");
        assert_eq!(format_file_operations(&["/a".to_string()], &[]), "\n\n<read-files>\n/a\n</read-files>");
    }

    #[test]
    fn truncation_keeps_the_head_and_counts_the_rest() {
        assert_eq!(truncate_for_summary("short", 10), "short");
        assert_eq!(truncate_for_summary("abcdef", 3), "abc\n\n[... 3 more characters truncated]");
    }

    #[test]
    fn provider_native_blocks_are_dropped_from_the_summary_text() {
        let content = json!([
            { "type": "text", "text": "kept" },
            { "type": "providerNative", "provider": "anthropic", "data": {} },
            { "type": "text", "text": "also kept" }
        ]);
        assert_eq!(content_text_for_summary(&content, "|"), "kept|also kept");
        assert_eq!(content_text_for_summary(&json!("plain"), "|"), "plain");
    }

    #[test]
    fn serialization_labels_every_role() {
        let messages = vec![
            json!({ "role": "user", "content": "hello" }),
            json!({ "role": "assistant", "content": [
                { "type": "thinking", "thinking": "hmm" },
                { "type": "text", "text": "answer" },
                { "type": "toolCall", "name": "read", "arguments": { "path": "/a", "limit": 1 } }
            ] }),
            json!({ "role": "toolResult", "content": [{ "type": "text", "text": "file body" }] }),
        ];
        let serialized = serialize_conversation(&messages);
        assert_eq!(
            serialized,
            "[User]: hello\n\n[Assistant thinking]: hmm\n\n[Assistant]: answer\n\n[Assistant tool calls]: read(path=\"/a\", limit=1)\n\n[Tool result]: file body"
        );
    }

    #[test]
    fn a_tool_result_is_truncated_at_the_summary_limit() {
        let long = "x".repeat(TOOL_RESULT_MAX_CHARS + 5);
        let serialized = serialize_conversation(&[json!({ "role": "toolResult", "content": [{ "type": "text", "text": long }] })]);
        assert!(serialized.ends_with("[... 5 more characters truncated]"));
    }

    #[test]
    fn an_assistant_message_without_text_omits_the_assistant_line() {
        let serialized = serialize_conversation(&[json!({ "role": "assistant", "content": [{ "type": "toolCall", "name": "read", "arguments": {} }] })]);
        assert_eq!(serialized, "[Assistant tool calls]: read()");
    }

    #[test]
    fn file_ops_include_nested_codemode_calls_and_trimmed_patch_headers() {
        let mut file_ops = create_file_ops();
        extract_file_ops_from_message(
            &json!({ "role": "toolResult", "nestedCalls": { "complete": true, "calls": [
                { "id": "1", "name": "edit", "arguments": { "path": "a.rs" }, "status": "ok" },
                { "id": "2", "name": "apply_patch", "arguments": { "input": "  *** Update File: b.rs  \n" }, "status": "ok" }
            ] } }),
            &mut file_ops,
        );
        assert_eq!(file_ops.edited.iter().collect::<Vec<_>>(), ["a.rs", "b.rs"]);
    }

    #[test]
    fn heredoc_with_non_ascii_delimiter_passes_through() {
        assert_eq!(strip_heredoc("cat <<é\na\né"), "cat <<é\na\né");
        assert_eq!(strip_heredoc("cat <<'PATCH'\na\nPATCH\n"), "a");
    }
}
