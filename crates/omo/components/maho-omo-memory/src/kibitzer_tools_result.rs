//! Tool results and rejections (latest `kibitzer/tools/result.ts`).
//!
//! Upstream returns inline `isError` / `terminate`. This port's `ToolResult` carries only
//! `content` + `details`, so `is_error` maps to `Err(ToolError::Message(..))` at the tool
//! boundary and `terminate` is carried here for the caller to act on.

use memory_core::sync::redact::redact_url;
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct KibitzerToolResult {
    pub text: String,
    pub is_error: bool,
    pub terminate: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KibitzerRejectionCode {
    ToolBudgetExceeded,
    PathEscape,
    PathTraversal,
    PathAbsolute,
    PathSeparator,
    PathEmpty,
    SystemPath,
    NotFound,
    NotAFile,
    NotCommitted,
    InvalidPattern,
    UnsupportedOperation,
    MissingArgument,
    InvalidArgument,
}

impl KibitzerRejectionCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ToolBudgetExceeded => "tool_budget_exceeded",
            Self::PathEscape => "path_escape",
            Self::PathTraversal => "path_traversal",
            Self::PathAbsolute => "path_absolute",
            Self::PathSeparator => "path_separator",
            Self::PathEmpty => "path_empty",
            Self::SystemPath => "system_path",
            Self::NotFound => "not_found",
            Self::NotAFile => "not_a_file",
            Self::NotCommitted => "not_committed",
            Self::InvalidPattern => "invalid_pattern",
            Self::UnsupportedOperation => "unsupported_operation",
            Self::MissingArgument => "missing_argument",
            Self::InvalidArgument => "invalid_argument",
        }
    }
}

pub fn rejection(code: KibitzerRejectionCode, message: &str, path: Option<&str>) -> KibitzerToolResult {
    let mut body = serde_json::json!({ "rejected": code.as_str(), "message": message });
    if let Some(path) = path {
        body["path"] = Value::String(path.to_string());
    }
    KibitzerToolResult { text: body.to_string(), is_error: true, terminate: false }
}

pub fn ok_text(text: String) -> KibitzerToolResult {
    KibitzerToolResult { text, is_error: false, terminate: false }
}

pub fn ok_json(value: &Value) -> KibitzerToolResult {
    ok_text(value.to_string())
}

/// Redaction FIRST (a secret split by truncation would escape the pattern), then the cap in UTF-16
/// units, matching JS `String.length` / `slice`.
pub fn bounded_text(text: &str, cap: usize) -> String {
    let redacted = redact_url(text);
    let length = redacted.encode_utf16().count();
    if length <= cap {
        return redacted;
    }
    let units: Vec<u16> = redacted.encode_utf16().take(cap).collect();
    format!("{}\n[truncated: {cap} of {length} chars]", String::from_utf16_lossy(&units))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn given_text_over_the_cap_when_bounded_then_it_is_cut_with_the_marker() {
        let capped = bounded_text("abcdef", 3);
        assert!(capped.starts_with("abc"));
        assert!(capped.contains("[truncated: 3 of 6 chars]"));
    }

    #[test]
    fn given_text_at_the_cap_when_bounded_then_it_is_unchanged() {
        assert_eq!(bounded_text("abc", 3), "abc");
    }

    #[test]
    fn given_an_astral_character_when_bounded_then_the_cut_counts_utf16_units() {
        let capped = bounded_text("a\u{1f600}b", 3);
        assert!(capped.starts_with("a\u{1f600}"));
    }

    #[test]
    fn given_a_rejection_when_built_then_it_is_an_error_carrying_code_and_path() {
        let result = rejection(KibitzerRejectionCode::NotFound, "gone", Some("a.md"));
        assert!(result.is_error);
        assert!(result.text.contains("\"rejected\":\"not_found\""));
        assert!(result.text.contains("\"path\":\"a.md\""));
    }
}
