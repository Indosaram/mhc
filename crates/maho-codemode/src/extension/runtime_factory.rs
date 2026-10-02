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

pub struct RuntimePreparationOptions<'a> {
    pub cwd: &'a std::path::Path,
    pub home_dir: &'a std::path::Path,
    pub environment: &'a crate::config::settings::Environment,
    pub event: &'a JsonValue,
    pub session_file: Option<&'a std::path::Path>,
    pub js_runtime: crate::tool::types::EvalRuntimeInfo,
}

pub struct PreparedRuntime {
    pub session_id: String,
    pub cwd: std::path::PathBuf,
    pub parallel_pool_width: u64,
    pub enabled_languages: crate::config::settings::Languages,
    pub availability: crate::interpreters::detect::InterpreterAvailability,
    pub runtimes: Vec<(crate::tool::types::EvalLanguage, crate::tool::types::EvalRuntimeInfo)>,
    pub settings: crate::config::settings::CodemodeSettings,
    pub artifacts: crate::output::output_meta::SessionArtifactsDir,
}

pub async fn prepare_runtime(options: RuntimePreparationOptions<'_>, detector: &mut crate::interpreters::detect::InterpreterDetector) -> Result<PreparedRuntime, std::io::Error> {
    use crate::{config::settings::{load_codemode_settings, resolve_enabled_languages}, interpreters::detect::get_interpreter_availability};
    let mut settings = load_codemode_settings(options.cwd, options.home_dir).await?.settings;
    settings.languages = resolve_enabled_languages(&settings, options.environment);
    let availability = get_interpreter_availability(&settings, detector).await;
    let enabled_languages = enabled_languages_from(&settings, &availability);
    let artifacts = crate::output::output_meta::resolve_session_artifacts_dir(options.session_file)?;
    let configured_width = settings.parallel_pool_width;
    let parallel_pool_width = if configured_width.is_finite() { configured_width.trunc().max(1.0) as u64 } else { 1 };
    let detections = availability.iter().map(|(language, status)| (*language, status.detected.clone())).collect::<Vec<_>>();
    let runtimes = super::runtime_info::runtimes_from_availability(&detections, options.js_runtime);
    Ok(PreparedRuntime {session_id: session_id_from(options.event), cwd: options.cwd.into(), parallel_pool_width, enabled_languages, availability, runtimes, settings, artifacts})
}
