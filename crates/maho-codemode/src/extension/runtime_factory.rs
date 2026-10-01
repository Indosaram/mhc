use maho_ext_api::{AgentToolResult, ExecuteToolError, ExecuteToolOptions, ExtensionApi, ExtensionFailure, JsonValue};

pub async fn execute_tool(api: &ExtensionApi, tool_name: &str, params: JsonValue, mut options: ExecuteToolOptions) -> Result<AgentToolResult, ExecuteToolError> {
    options.activate_inactive_tool = Some(true);
    api.execute_tool(tool_name, params, options).await
}

pub fn is_tool_available(api: &ExtensionApi, name: &str, active_tools: Option<&[String]>) -> Result<bool, ExtensionFailure> {
    match active_tools {
        Some(tools) => Ok(tools.iter().any(|tool| tool == name)),
        None => Ok(api.get_active_tools()?.iter().any(|tool| tool == name)),
    }
}
