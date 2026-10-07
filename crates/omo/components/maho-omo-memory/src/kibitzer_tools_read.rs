//! The sidecar's `read` tool (latest `kibitzer/tools/read.ts`).

use std::path::Path;

use serde_json::Value;

use crate::kibitzer_tools_caps::KibitzerToolCaps;
use crate::kibitzer_tools_path_safety::{PathCheck, resolve_workspace_path};
use crate::kibitzer_tools_result::{KibitzerRejectionCode, KibitzerToolResult, bounded_text, ok_text, rejection};

pub const KIBITZER_READ_TOOL_NAME: &str = "read";

/// Reads a workspace file bounded to `caps.read_chars`, redacting secrets first.
pub fn execute_read(workspace_root: &Path, caps: KibitzerToolCaps, params: &Value) -> KibitzerToolResult {
    let Some(path) = params.get("path").and_then(Value::as_str) else {
        return rejection(KibitzerRejectionCode::MissingArgument, "read requires a path.", None);
    };
    let resolved = match resolve_workspace_path(workspace_root, path) {
        PathCheck::Ok { path } => path,
        PathCheck::Rejected { code, message } => return rejection(code, &message, Some(path)),
    };
    let Ok(info) = std::fs::metadata(&resolved) else {
        return rejection(KibitzerRejectionCode::NotFound, &format!("\"{path}\" does not exist."), Some(path));
    };
    if !info.is_file() {
        return rejection(KibitzerRejectionCode::NotAFile, &format!("\"{path}\" is not a regular file."), Some(path));
    }
    let Ok(content) = std::fs::read_to_string(&resolved) else {
        return rejection(KibitzerRejectionCode::NotFound, &format!("\"{path}\" could not be read."), Some(path));
    };
    let offset = params.get("offset").and_then(Value::as_u64).map(|value| value as usize);
    let limit = params.get("limit").and_then(Value::as_u64).map(|value| value as usize);
    ok_text(bounded_text(&slice_lines(&content, offset, limit), caps.read_chars))
}

/// `offset` is 1-based; absent `offset` and `limit` return the whole file, matching upstream.
fn slice_lines(content: &str, offset: Option<usize>, limit: Option<usize>) -> String {
    if offset.is_none() && limit.is_none() {
        return content.to_string();
    }
    let lines: Vec<&str> = content.split('\n').collect();
    let start = offset.unwrap_or(1).saturating_sub(1).min(lines.len());
    let end = limit
        .map(|limit| start.saturating_add(limit))
        .unwrap_or(lines.len())
        .min(lines.len());
    lines[start..end].join("\n")
}
#[cfg(test)]
mod tests {
    use super::execute_read;
    use crate::kibitzer_tools_caps::DEFAULT_KIBITZER_TOOL_CAPS;
    use serde_json::json;

    fn read(params: serde_json::Value) -> String {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::write(root.path().join("note.txt"), "l1\nl2\nl3\nl4\nl5").expect("write");
        let result = execute_read(root.path(), DEFAULT_KIBITZER_TOOL_CAPS, &params);
        assert!(!result.is_error, "read failed: {}", result.text);
        result.text
    }

    #[test]
    fn the_whole_file_is_returned_without_offset_or_limit() {
        assert_eq!(read(json!({ "path": "note.txt" })), "l1\nl2\nl3\nl4\nl5");
    }

    #[test]
    fn an_offset_windows_from_that_one_based_line() {
        assert_eq!(read(json!({ "path": "note.txt", "offset": 2 })), "l2\nl3\nl4\nl5");
    }

    #[test]
    fn a_limit_takes_that_many_lines_from_the_start() {
        assert_eq!(read(json!({ "path": "note.txt", "limit": 2 })), "l1\nl2");
    }

    #[test]
    fn offset_and_limit_together_return_that_window() {
        assert_eq!(read(json!({ "path": "note.txt", "offset": 2, "limit": 2 })), "l2\nl3");
    }

    #[test]
    fn an_offset_past_the_end_returns_empty_text() {
        assert_eq!(read(json!({ "path": "note.txt", "offset": 10 })), "");
    }

    #[test]
    fn a_zero_offset_starts_at_the_first_line() {
        assert_eq!(read(json!({ "path": "note.txt", "offset": 0 })), "l1\nl2\nl3\nl4\nl5");
    }

    #[test]
    fn a_usize_max_offset_returns_empty_text_without_panicking() {
        assert_eq!(read(json!({ "path": "note.txt", "offset": u64::MAX })), "");
    }

    #[test]
    fn offset_two_with_a_usize_max_limit_does_not_overflow_start_plus_limit() {
        assert_eq!(read(json!({ "path": "note.txt", "offset": 2, "limit": u64::MAX })), "l2\nl3\nl4\nl5");
    }
}
