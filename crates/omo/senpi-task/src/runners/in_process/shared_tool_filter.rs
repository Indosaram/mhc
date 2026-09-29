//! Shared parent tool filtering (`runners/in-process/shared-tool-filter.ts`).

use std::sync::Arc;

use serde_json::Value;

use crate::host::HostError;

/// A host tool definition (`ToolDefinition` from the host runtime) handed to a child session.
pub trait ChildTool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    /// Runs the tool; the result is the host's `{ content: [...] }` tool-result JSON.
    fn execute(&self, tool_call_id: &str, input: &Value) -> Result<Value, HostError>;
}

pub type ChildToolRef = Arc<dyn ChildTool>;

pub fn is_task_or_team_family_tool(name: &str) -> bool {
    name == "dag" || name == "task" || name.starts_with("task_") || name.starts_with("team_")
}

pub fn filter_shared_parent_tools(
    tools: &[ChildToolRef],
    ui_only_tool_names: &[String],
) -> Vec<ChildToolRef> {
    tools
        .iter()
        .filter(|tool| {
            !is_task_or_team_family_tool(tool.name())
                && !ui_only_tool_names.iter().any(|name| name == tool.name())
        })
        .cloned()
        .collect()
}

/// Member-scoped tools are the sanctioned bypass of the task/team-family exclusion.
pub fn merge_child_custom_tools(
    shared_parent_tools: &[ChildToolRef],
    member_scoped_tools: Option<&[ChildToolRef]>,
    ui_only_tool_names: &[String],
) -> Vec<ChildToolRef> {
    let mut merged = filter_shared_parent_tools(shared_parent_tools, ui_only_tool_names);
    merged.extend(member_scoped_tools.unwrap_or_default().iter().cloned());
    merged
}
