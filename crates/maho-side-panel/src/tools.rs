//! The recent tool-call log (spec 5 Tools).

use serde_json::Value;

/// Calls kept in memory.
pub const STORED: usize = 24;
/// Calls shown.
pub const SHOWN: usize = 6;
/// Argument keys checked, in order, for the call summary.
pub const ARG_KEYS: [&str; 6] = ["file_path", "path", "command", "pattern", "query", "url"];

/// One recorded call: tool name plus its first non-empty summary argument.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolCall {
    pub name: String,
    pub summary: Option<String>,
}

/// Tool calls since the last user input, newest last.
#[derive(Clone, Debug, Default)]
pub struct ToolLog {
    calls: Vec<ToolCall>,
    total: usize,
}

impl ToolLog {
    /// Records a call; keeps the newest [`STORED`].
    pub fn record(&mut self, name: &str, args: &Value) {
        let summary = ARG_KEYS.iter().find_map(|key| {
            args.get(key)
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(str::to_owned)
        });
        self.calls.push(ToolCall {
            name: name.to_owned(),
            summary,
        });
        self.total += 1;
        if self.calls.len() > STORED {
            let excess = self.calls.len() - STORED;
            self.calls.drain(..excess);
        }
    }

    /// User input clears the log.
    pub fn clear(&mut self) {
        self.calls.clear();
        self.total = 0;
    }

    pub fn stored(&self) -> &[ToolCall] {
        &self.calls
    }

    /// The newest [`SHOWN`] calls, oldest first.
    pub fn shown(&self) -> &[ToolCall] {
        &self.calls[self.calls.len().saturating_sub(SHOWN)..]
    }

    /// Stored calls not shown (`+N earlier`).
    pub fn earlier(&self) -> usize {
        self.calls.len().saturating_sub(SHOWN)
    }

    pub fn is_empty(&self) -> bool {
        self.calls.is_empty()
    }
}
