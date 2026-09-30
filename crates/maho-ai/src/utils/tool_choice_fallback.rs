//! Port of senpi packages/ai/src/utils/tool-choice-fallback.ts.

use regex::Regex;
use serde_json::{Map, Value};
use std::sync::LazyLock;

static PATTERNS: LazyLock<[Regex; 3]> = LazyLock::new(|| {
    let compile = |p: &str| Regex::new(p).unwrap_or_else(|e| panic!("{e}"));
    [
        compile(r"(?is)tool[_\s-]?choices?\b.*?(not\s+compatible|incompatible|not\s+supported|unsupported)"),
        compile(r"(?is)forces?\s+tool\s+use.*?(not\s+compatible|incompatible|not\s+supported|unsupported)"),
        compile(r"(?is)does\s+not\s+support\s+forced\s+tool[_\s-]?choices?"),
    ]
});

/// A provider HTTP failure: status (or `response.status`) plus message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpFailure {
    pub status: Option<u16>,
    pub message: String,
}

pub fn is_forced_tool_choice_unsupported_error(error: &HttpFailure, sent_forced_tool_choice: bool) -> bool {
    if !sent_forced_tool_choice || error.status != Some(400) {
        return false;
    }
    PATTERNS.iter().any(|pattern| pattern.is_match(&error.message))
}

pub fn omit_tool_choice_param(params: &Map<String, Value>) -> Map<String, Value> {
    let mut next = params.clone();
    next.remove("tool_choice");
    next
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_forced_tool_choice_rejections() {
        let err = |status, message: &str| HttpFailure { status, message: message.into() };
        assert!(is_forced_tool_choice_unsupported_error(&err(Some(400), "tool_choice is not supported"), true));
        assert!(is_forced_tool_choice_unsupported_error(&err(Some(400), "Thinking forces tool use\nincompatible"), true));
        assert!(is_forced_tool_choice_unsupported_error(&err(Some(400), "model does not support forced tool choice"), true));
        assert!(!is_forced_tool_choice_unsupported_error(&err(Some(400), "tool_choice is not supported"), false));
        assert!(!is_forced_tool_choice_unsupported_error(&err(Some(500), "tool_choice is not supported"), true));
        let mut params = Map::new();
        params.insert("tool_choice".into(), Value::from("any"));
        params.insert("model".into(), Value::from("m"));
        assert_eq!(omit_tool_choice_param(&params).len(), 1);
    }
}
