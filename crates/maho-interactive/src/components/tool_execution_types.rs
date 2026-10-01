//! Port of `components/tool-execution-types.ts`.
use maho_tools::definition::ToolResult;

pub type ToolExecutionResult = ToolResult;

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
}
