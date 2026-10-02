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

pub struct RuntimeHostOptions {
    pub executor: std::sync::Arc<dyn crate::bridges::output_bridge::OutputExecuteTool>,
    pub active_tools: Vec<String>,
    pub list_tools: Option<crate::bridges::schema_bridge::EvalToolCatalog>,
    pub complete: crate::bridge::http_server::BridgeCompletionHandler,
    pub session_env: crate::kernels::session_env::SessionEnvironment,
}

pub struct SessionRuntime {
    pub session_id: String,
    pub cwd: std::path::PathBuf,
    pub parallel_pool_width: u64,
    pub manager: std::sync::Arc<super::session_manager::CodemodeSessionManager>,
    pub enabled_languages: crate::config::settings::Languages,
    pub runtimes: Vec<(crate::tool::types::EvalLanguage, crate::tool::types::EvalRuntimeInfo)>,
    pub settings: crate::config::settings::CodemodeSettings,
    pub artifacts_dir: std::path::PathBuf,
    pub executor: std::sync::Arc<dyn crate::bridges::output_bridge::OutputExecuteTool>,
    pub spawns: bool,
}

pub async fn create_runtime(prepared: PreparedRuntime, host: RuntimeHostOptions) -> Result<SessionRuntime, std::io::Error> {
    let spawns = host.active_tools.iter().any(|name| name == &prepared.settings.task_tools.task);
    let manager = super::session_manager::CodemodeSessionManager::start(super::session_manager::CreateCodemodeSessionManagerOptions {
        session_id: prepared.session_id.clone(), cwd: prepared.cwd.clone(), settings: prepared.settings.clone(),
        availability: prepared.availability, local_roots: None, artifacts_dir: Some(prepared.artifacts.dir.clone()),
        session_env: Some(host.session_env), executor: host.executor.clone(), list_tools: host.list_tools, complete: host.complete,
    }).await?;
    Ok(SessionRuntime {session_id: prepared.session_id, cwd: prepared.cwd, parallel_pool_width: prepared.parallel_pool_width,
        manager: std::sync::Arc::new(manager), enabled_languages: prepared.enabled_languages, runtimes: prepared.runtimes,
        settings: prepared.settings, artifacts_dir: prepared.artifacts.dir, executor: host.executor, spawns})
}

pub fn runtime_host_from_api(api: std::sync::Arc<ExtensionApi>, context: &dyn maho_ext_api::ToolContext, complete: crate::bridge::http_server::BridgeCompletionHandler) -> Result<RuntimeHostOptions, ExtensionFailure> {
    let active_tools = api.get_active_tools()?;
    let executor = std::sync::Arc::new(RuntimeExecuteTool {api: api.clone(), active_tools: active_tools.clone()});
    let list_tools = std::sync::Arc::new(move || api.get_all_tools().map(|tools| tools.into_iter().map(|tool| crate::bridges::schema_bridge::EvalSchemaToolInfo {
        name: tool.name, description: Some(tool.description), parameters: Some(tool.parameters),
    }).collect()).map_err(|error| error.to_string()));
    Ok(RuntimeHostOptions {executor, active_tools, list_tools: Some(list_tools), complete, session_env: crate::kernels::session_env::session_environment_from_context(context)})
}
