//! Port of `components/tool-execution-types.ts`.
use maho_tools::definition::{ToolContent, ToolResult};
use serde_json::Value;

/// senpi's `ToolExecutionResult`: the agent result plus a top-level `isError`, which is what the
/// card's background and the grok row read. It is not a field of `details`.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolExecutionResult {
    pub content: Vec<ToolContent>,
    pub details: Option<Value>,
    pub is_error: bool,
}

impl ToolExecutionResult {
    pub fn as_tool_result(&self) -> ToolResult {
        ToolResult { content: self.content.clone(), details: self.details.clone() }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolExecutionRenderState {
    pub args: serde_json::Value,
    pub execution_started: bool,
    pub args_complete: bool,
    pub is_partial: bool,
    pub expanded: bool,
    pub show_images: bool,
    pub spinner_frame: Option<i64>,
    pub result: Option<ToolExecutionResult>,
    pub is_error: bool,
    /// Wall-clock milliseconds for the progress line's elapsed time. senpi reads `Date.now()` at
    /// render time; this port takes it from the host tick so a render is reproducible.
    pub now_ms: f64,
}
