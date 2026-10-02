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

pub fn enabled_languages_from(settings: &crate::config::settings::CodemodeSettings, availability: &crate::interpreters::detect::InterpreterAvailability) -> crate::config::settings::Languages {
    use crate::{interpreters::detect::InterpreterDetection, tool::types::EvalLanguage};
    let detected = |language| availability.iter().any(|(candidate, status)| *candidate == language && matches!(status.detected, InterpreterDetection::Detected { .. }));
    crate::config::settings::Languages {
        py: settings.languages.py && detected(EvalLanguage::Py),
        js: settings.languages.js && detected(EvalLanguage::Js),
        rb: settings.languages.rb && detected(EvalLanguage::Rb),
        jl: settings.languages.jl && detected(EvalLanguage::Jl),
    }
}

pub fn session_id_from(event: &JsonValue) -> String {
    event.get("sessionId").and_then(JsonValue::as_str).map_or_else(|| uuid::Uuid::new_v4().to_string(), str::to_owned)
}

pub struct RuntimeExecuteTool {
    pub api: std::sync::Arc<ExtensionApi>,
    pub active_tools: Vec<String>,
}

impl crate::bridges::output_bridge::OutputExecuteTool for RuntimeExecuteTool {
    fn is_tool_available(&self, name: &str) -> Option<bool> {
        Some(self.active_tools.iter().any(|tool| tool == name))
    }

    fn execute_tool<'a>(&'a self, name: &'a str, params: JsonValue, options: ExecuteToolOptions) -> maho_ext_api::ExecuteToolFuture<'a> {
        Box::pin(execute_tool(&self.api, name, params, options))
    }
}
