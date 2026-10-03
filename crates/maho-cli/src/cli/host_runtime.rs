//! Port of senpi `packages/coding-agent/src/main.ts` `createCliRuntimeFactory` and its
//! `CliRuntimeConfiguration` argument: the CLI-owned session-runtime factory every mode mounts
//! (print, rpc, interactive, app-server and the shared session host).
//!
//! Adaptation (plan D-M5): `createAgentSessionRuntime` (senpi `core/agent-session-runtime.ts`)
//! builds its services and its session from one settings manager inside maho-core. maho-core exposes
//! `create_agent_session_services` and `sdk::create_agent_session` separately, and `SettingsManager`
//! is neither `Clone` nor `Default`, so the runtime here creates the cwd-bound services (which load
//! the same global/project settings files) and hands the caller's settings manager to the session;
//! one shared manager is a maho-core owner request recorded in `.omo/evidence/task-38-resume.md`.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use maho_core::agent_session::AgentSession;
use maho_core::agent_session_runtime::{AgentSessionLaunchProfile, AgentSessionRuntime};
use maho_core::agent_session_services::{
    create_agent_session_services, AgentSessionRuntimeDiagnostic, CreateAgentSessionServicesOptions,
};
use maho_core::model_runtime::ModelRuntime;
use maho_core::project_trust::AppMode;
use maho_core::session_manager::SessionManager;
use maho_core::settings_manager::SettingsManager;
use maho_ext_api::{FlagValue, SessionStartEvent};
use maho_ext_host::ExtensionRunner;

use super::args::Args;

/// senpi `CliRuntimeConfiguration`.
#[derive(Clone, Debug)]
pub struct CliRuntimeConfiguration {
    pub cwd: String,
    pub agent_dir: String,
    pub app_mode: AppMode,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub thinking: Option<String>,
    pub auto_title_sessions: Option<bool>,
    pub help: bool,
    pub list_models: bool,
    pub list_tips: bool,
    pub session_dir: Option<String>,
    pub extensions: Vec<String>,
    pub skills: Vec<String>,
    pub prompt_templates: Vec<String>,
    pub themes: Vec<String>,
    pub hooks: Vec<String>,
    pub additional_hook_paths: Vec<PathBuf>,
}

impl Default for CliRuntimeConfiguration {
    fn default() -> Self {
        Self {
            cwd: String::new(),
            agent_dir: String::new(),
            app_mode: AppMode::Print,
            provider: None,
            model: None,
            thinking: None,
            auto_title_sessions: None,
            help: false,
            list_models: false,
            list_tips: false,
            session_dir: None,
            extensions: Vec::new(),
            skills: Vec::new(),
            prompt_templates: Vec::new(),
            themes: Vec::new(),
            hooks: Vec::new(),
            additional_hook_paths: Vec::new(),
        }
    }
}

impl CliRuntimeConfiguration {
    pub fn from_parsed(parsed: &Args, cwd: &str, agent_dir: &str, app_mode: AppMode) -> Self {
        Self {
            cwd: cwd.to_owned(),
            agent_dir: agent_dir.to_owned(),
            app_mode,
            provider: parsed.provider.clone(),
            model: parsed.model.clone(),
            thinking: parsed.thinking.clone(),
            auto_title_sessions: parsed.auto_title_sessions.then_some(true),
            help: parsed.help,
            list_models: parsed.list_models.is_some(),
            list_tips: parsed.list_tips,
            session_dir: parsed.session_dir.clone(),
            extensions: parsed.extensions.clone(),
            skills: parsed.skills.clone(),
            prompt_templates: parsed.prompt_templates.clone(),
            themes: parsed.themes.clone(),
            hooks: Vec::new(),
            additional_hook_paths: Vec::new(),
        }
    }

    /// senpi `trustPromptMode`: metadata-only launches resolve trust as a print run.
    pub fn trust_prompt_mode(&self) -> AppMode {
        if self.help || self.list_models || self.list_tips { AppMode::Print } else { self.app_mode }
    }

    /// senpi `resolveCliPaths`.
    pub fn resolve_cli_paths(&self, paths: &[String]) -> Vec<String> {
        paths.iter().map(|path| resolve_cli_path(&self.cwd, path)).collect()
    }

    pub fn resolved_extension_paths(&self) -> Vec<String> {
        self.resolve_cli_paths(&self.extensions)
    }

    pub fn resolved_skill_paths(&self) -> Vec<String> {
        self.resolve_cli_paths(&self.skills)
    }

    pub fn resolved_prompt_template_paths(&self) -> Vec<String> {
        self.resolve_cli_paths(&self.prompt_templates)
    }

    pub fn resolved_theme_paths(&self) -> Vec<String> {
        self.resolve_cli_paths(&self.themes)
    }

    /// senpi `cliExtensionPaths.hooks` resolved against the launch cwd.
    pub fn resolved_hook_paths(&self) -> Vec<PathBuf> {
        self.resolve_cli_paths(&self.hooks).into_iter().map(PathBuf::from).collect()
    }

    /// senpi `runtimeConfiguration.launchProfile`.
    pub fn launch_profile(&self) -> AgentSessionLaunchProfile {
        AgentSessionLaunchProfile {
            cwd: self.cwd.clone(),
            permission_preset: None,
            creation_model: match (&self.provider, &self.model) {
                (Some(provider), Some(model)) if !provider.is_empty() && !model.is_empty() => {
                    Some((provider.clone(), model.clone()))
                }
                _ => None,
            },
            initial_thinking_level: self.thinking.clone(),
            auto_title: self.auto_title_sessions,
        }
    }
}

/// senpi `resolvePath`/`normalizePath` for a CLI path: absolute paths are returned as given and
/// relative paths resolve against the launch cwd.
pub fn resolve_cli_path(cwd: &str, path: &str) -> String {
    let candidate = Path::new(path);
    if candidate.is_absolute() { return path.to_owned(); }
    PathBuf::from(cwd).join(candidate).to_string_lossy().into_owned()
}

/// The per-runtime inputs a mode supplies when it mounts a session runtime.
#[derive(Default)]
pub struct CliRuntimeRequest {
    pub session_manager: Option<SessionManager>,
    pub session_start_event: Option<SessionStartEvent>,
    pub model: Option<maho_ai::model::Model>,
    pub thinking_level: Option<maho_ai::types::ThinkingLevel>,
    pub thinking_selection: Option<maho_ai::types::ThinkingSelection>,
    pub scoped_models: Vec<maho_core::agent_session::SessionModelEntry>,
    pub favorite_models: Vec<maho_core::agent_session::SessionModelEntry>,
    pub tools: Option<Vec<String>>,
    pub exclude_tools: Option<Vec<String>>,
    pub no_tools: Option<maho_core::sdk::NoToolsMode>,
    pub custom_tools: Vec<maho_ext_api::ToolDefinition>,
    pub extension_flag_values: Option<BTreeMap<String, FlagValue>>,
    pub auto_title_sessions: Option<bool>,
}

/// The mounted session runtime plus the diagnostics senpi reports before the first turn.
pub struct MountedRuntime {
    pub runtime: AgentSessionRuntime,
    pub diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
    pub model_fallback_message: Option<String>,
}

impl MountedRuntime {
    pub fn session(&self) -> &AgentSession {
        self.runtime.session()
    }

    pub fn has_errors(&self) -> bool {
        self.runtime
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.kind == maho_core::agent_session_services::AgentSessionRuntimeDiagnosticType::Error)
    }
}

/// senpi `createAgentSessionRuntime` with the CLI runtime factory: cwd-bound services, then the
/// session, then one runtime that owns both.
pub async fn mount_agent_session_runtime(
    config: &CliRuntimeConfiguration,
    request: CliRuntimeRequest,
    model_runtime: Option<ModelRuntime>,
    settings_manager: Option<SettingsManager>,
) -> Result<MountedRuntime, String> {
    let CliRuntimeRequest {
        session_manager,
        session_start_event,
        model,
        thinking_level,
        thinking_selection,
        scoped_models,
        favorite_models,
        tools,
        exclude_tools,
        no_tools,
        custom_tools,
        extension_flag_values,
        auto_title_sessions,
    } = request;
    let services = create_agent_session_services(CreateAgentSessionServicesOptions {
        cwd: config.cwd.clone(),
        agent_dir: Some(config.agent_dir.clone()),
        settings_manager,
        model_runtime,
        extension_flag_values,
    });
    let created = maho_core::agent_session_services::create_agent_session_from_services(
        services,
        maho_core::agent_session_services::CreateAgentSessionFromServicesOptions {
            session_manager,
            session_start_event,
            model,
            thinking_level,
            thinking_selection,
            scoped_models,
            favorite_models,
            tools,
            exclude_tools,
            no_tools: match no_tools { Some(maho_core::sdk::NoToolsMode::All) => Some(true), _ => None },
            custom_tools,
            auto_title_sessions,
        },
    )
    .await?;
    let diagnostics = created.services.diagnostics.clone();
    let model_fallback_message = created.model_fallback_message.clone();
    let runtime = AgentSessionRuntime::new(
        created.session,
        created.services,
        diagnostics.clone(),
        model_fallback_message.clone(),
        Some(config.launch_profile()),
    );
    Ok(MountedRuntime { runtime, diagnostics, model_fallback_message })
}

/// The session factory shape the host and app-server mount: one call per session, keyed by cwd and
/// session manager, returning a fully mounted session.
pub type CliSessionFactory = Arc<
    dyn Fn(CliSessionFactoryRequest) -> Pin<Box<dyn Future<Output = Result<MountedRuntime, String>> + Send>>
        + Send
        + Sync,
>;

pub struct CliSessionFactoryRequest {
    pub config: CliRuntimeConfiguration,
    pub session_manager: Option<SessionManager>,
    pub model_runtime: Option<ModelRuntime>,
    pub settings_manager: Option<SettingsManager>,
    pub session_start_event: Option<SessionStartEvent>,
}

/// Bind the loaded native extensions into a session: build the runner over the same factories the
/// registry assembled, then install it against the session's own `ExtensionContext` (no private
/// adapter reconstruction).
pub async fn bind_native_extensions(
    session: &AgentSession,
    ui: Arc<dyn maho_ext_api::ExtensionUi>,
    extensions: Vec<Box<dyn maho_ext_api::Extension>>,
) -> Result<(), String> {
    let context = session.extension_context(ui);
    let runner = ExtensionRunner::from_static(extensions, context);
    session.set_extension_runner(runner).await;
    Ok(())
}

/// senpi `createCliRuntimeFactory`: a factory that mounts one session per request against the
/// captured configuration.
pub fn create_cli_runtime_factory(config: CliRuntimeConfiguration) -> CliSessionFactory {
    Arc::new(move |request: CliSessionFactoryRequest| {
        let config = config.clone();
        Box::pin(async move {
            let model_runtime = request.model_runtime;
            let settings_manager = request.settings_manager;
            let mount_request = CliRuntimeRequest {
                session_manager: request.session_manager,
                session_start_event: request.session_start_event,
                auto_title_sessions: config.auto_title_sessions,
                ..Default::default()
            };
            mount_agent_session_runtime(&config, mount_request, model_runtime, settings_manager).await
        })
    })
}
