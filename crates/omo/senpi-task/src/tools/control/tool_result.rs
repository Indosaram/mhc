//! Port of `tools/control/tool-result.ts`.

use serde::Serialize;

use crate::state::TaskStatus;

const TERMINAL_STATUSES: [&str; 5] = ["completed", "error", "cancelled", "interrupted", "lost"];

pub fn is_terminal_status(status: TaskStatus) -> bool {
    TERMINAL_STATUSES.contains(&status.as_str())
}

const FINAL_RESPONSE_HEAD_MAX: usize = 400;

/// JS `String.prototype.trim` whitespace set (Unicode White_Space plus BOM).
fn is_js_whitespace(c: char) -> bool {
    c.is_whitespace() || c == '\u{feff}'
}

/// Length and slicing follow JS semantics (UTF-16 code units).
pub fn final_response_head(text: Option<&str>) -> Option<String> {
    let trimmed = text?.trim_matches(is_js_whitespace);
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.encode_utf16().count() <= FINAL_RESPONSE_HEAD_MAX {
        return Some(trimmed.to_string());
    }
    let mut units = 0usize;
    let mut head = String::new();
    for c in trimmed.chars() {
        let width = c.len_utf16();
        if units + width > FINAL_RESPONSE_HEAD_MAX {
            break;
        }
        units += width;
        head.push(c);
    }
    Some(format!("{head}..."))
}

/// Content block of an agent tool result.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ToolResultContent {
    Text { text: String },
}

/// Minimal model of senpi's `AgentToolResult<TDetails>`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentToolResult<TDetails> {
    pub content: Vec<ToolResultContent>,
    pub details: TDetails,
}

/// Result convention (pi-task task-status/task-cancel): typed structured `details` for the model to
/// branch on, plus a short human-readable `content` line. Never prose-only.
pub fn tool_result<TDetails>(text: &str, details: TDetails) -> AgentToolResult<TDetails> {
    AgentToolResult {
        content: vec![ToolResultContent::Text {
            text: text.to_string(),
        }],
        details,
    }
}
