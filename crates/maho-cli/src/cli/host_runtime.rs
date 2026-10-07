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
    pub models: Vec<String>,
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
            models: Vec::new(),
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
            models: parsed.models.clone().unwrap_or_default(),
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

    // RESERVED (assembly, sequential): the input/resources seam for the component owners - an owner
    // that ships resources (skills, prompt templates, extensions) declares them here once its source
    // receipt lands. Do not integrate from a summary.
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
    pub cwd: Option<String>,
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
    pub session_profile: maho_ext_api::ExtensionSessionProfile,
}

/// The mounted session runtime plus the diagnostics senpi reports before the first turn.
pub struct MountedRuntime {
    pub runtime: AgentSessionRuntime,
    pub diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
    pub model_fallback_message: Option<String>,
    pub widget_requests: tokio::sync::mpsc::UnboundedReceiver<maho_interactive::interactive_extension_ui::UiRequest>,
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
        cwd,
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
        session_profile,
    } = request;
    let cwd = cwd.unwrap_or_else(|| config.cwd.clone());
    let mut launch_config = config.clone();
    launch_config.cwd = cwd.clone();
    let services = create_agent_session_services(CreateAgentSessionServicesOptions {
        cwd,
        agent_dir: Some(config.agent_dir.clone()),
        settings_manager,
        model_runtime,
        extension_flag_values,
    });
    let (widget_sender, widget_requests) = tokio::sync::mpsc::unbounded_channel();
    let task_parent = Arc::new(std::sync::OnceLock::new());
    let extension_factories = super::default_extensions::assembled_factories(widget_sender, task_parent.clone());
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
            no_tools,
            custom_tools,
            auto_title_sessions,
            extension_factories,
            session_profile,
            defer_extension_start: true,
            ..Default::default()
        },
    )
    .await?;
    task_parent.set(created.session.weak_accessor()).map_err(|_| "Task parent already bound".to_owned())?;
    let hooks = created.session.with_settings_manager(|settings| super::hook_sources::build_loaded_hook_sources(&launch_config, settings));
    created.session.set_hook_sources(Some(hooks));
    created.session.bind_extensions(maho_core::agent_session::ExtensionBindings {
        mode: Some(match config.app_mode {
            AppMode::Interactive => maho_ext_api::ExtensionMode::Tui,
            AppMode::Rpc => maho_ext_api::ExtensionMode::Rpc,
            AppMode::AppServer => maho_ext_api::ExtensionMode::AppServer,
            AppMode::Json => maho_ext_api::ExtensionMode::Json,
            AppMode::Print => maho_ext_api::ExtensionMode::Print,
        }),
        ..Default::default()
    }).await;
    let diagnostics = created.services.diagnostics.clone();
    let model_fallback_message = created.model_fallback_message.clone();
    let runtime = AgentSessionRuntime::new(
        created.session,
        created.services,
        diagnostics.clone(),
        model_fallback_message.clone(),
        Some(launch_config.launch_profile()),
    );
    Ok(MountedRuntime { runtime, diagnostics, model_fallback_message, widget_requests })
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

/// The shared host's process-local core, built from the CLI configuration and the host's cwd
/// (senpi `createHostCore` + the `runMultiSessionHost` argument object).
pub fn create_host_core(config: CliRuntimeConfiguration, cwd: &str, listen: Option<&str>) -> std::sync::Arc<maho_rpc::multi_session_host::HostCore> {
    // One model runtime for every session this host opens (senpi `hostModelRuntime`, senpi#1844):
    // the sessions share one agent dir, so a per-open rebuild would pay the same catalog load once
    // per open instead of once per host.
    let model_runtime = host_model_runtime(&config);
    let create_runtime = host_runtime_factory(config.clone(), model_runtime);
    // A socket host answers each connection through its own writer, so its fallback writer is a
    // sink; the stdio host answers on stdout (senpi `runSocketHost` vs `runStdioHost`).
    let writer = match listen {
        None | Some("stdio://") => std::sync::Arc::new(maho_rpc::session_event_writer::SessionWriterActor::new(tokio::io::stdout())),
        Some(_) => std::sync::Arc::new(maho_rpc::session_event_writer::SessionWriterActor::new(tokio::io::sink())),
    };
    std::sync::Arc::new(maho_rpc::multi_session_host::HostCore::new(
        maho_rpc::multi_session_host::HostCoreOptions {
            agent_dir: PathBuf::from(&config.agent_dir),
            cwd: cwd.to_owned(),
            create_runtime,
            capabilities: maho_rpc::custom_capability::parse_client_capabilities(
                maho_core::brand::env_value("RPC_CLIENT_CAPABILITIES", &maho_core::config::current_env()).as_deref(),
            ),
            close_grace_ms: host_close_grace_ms(),
        },
        writer,
    ))
}

/// senpi's shared host model runtime (`hostModelRuntime`): one catalog and one auth store behind
/// every in-process session the host opens.
fn host_model_runtime(config: &CliRuntimeConfiguration) -> ModelRuntime {
    ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {
        models_path: Some(Path::new(&config.agent_dir).join("models.json")),
        auth_path: Some(Path::new(&config.agent_dir).join("auth.json")),
        ..Default::default()
    })
}

/// senpi `closeGraceMs`: the configured grace, else ten seconds.
fn host_close_grace_ms() -> u64 {
    maho_rpc::host_lifecycle::parse_idle_exit_ms(std::env::var(maho_rpc::multi_session_host::RPC_CLOSE_GRACE_MS_ENV).ok().as_deref())
        .map_or(10_000, |milliseconds| if milliseconds.is_finite() && milliseconds > 0.0 { milliseconds as u64 } else { 10_000 })
}

/// Serve the shared host (senpi `runMultiSessionHost`).
pub async fn serve_multi_session_host(
    core: std::sync::Arc<maho_rpc::multi_session_host::HostCore>,
    listen: Option<String>,
) -> std::io::Result<()> {
    maho_rpc::multi_session_host::run_multi_session_host(core, listen).await
}

/// senpi `createCliRuntimeFactory`'s `HostRuntimeFactory` adapter: one fully mounted session runtime
/// per `open_session`, built by the same CLI mount path every other mode uses (the seam
/// `maho_rpc::multi_session_host::HostRuntimeFactory` exists for).
pub fn host_runtime_factory(config: CliRuntimeConfiguration, model_runtime: ModelRuntime) -> maho_rpc::multi_session_host::HostRuntimeFactory {
    std::sync::Arc::new(move |profile: maho_rpc::session_registry::RpcSessionLaunchProfile| {
        let config = config.clone();
        let model_runtime = model_runtime.clone();
        Box::pin(async move {
            let session_manager = open_profile_session_manager(&config, &profile);
            // The profile's per-session startup choices feed the same resolver as
            // `--provider/--model/--thinking`: an open that names none keeps the CLI's own
            // (senpi `runtimeParsed` inside `createCliRuntimeFactory`).
            let thinking = profile.runtime.initial_thinking_level.clone().or_else(|| config.thinking.clone())
                .and_then(|level| maho_ai::types::ThinkingLevel::parse(&level));
            let creation_model = profile.runtime.creation_model.clone().or_else(|| config.provider.clone().zip(config.model.clone()));
            let scoped = maho_core::model_resolver::resolve_model_scope_from_models(&config.models, &model_runtime.get_models(None));
            let settings = SettingsManager::create(&profile.runtime.cwd, &config.agent_dir, &maho_core::config::home_dir(), false);
            let is_continuing = !session_manager.build_context(session_manager.leaf_id()).messages.is_empty();
            let initial = resolve_cli_initial_model(creation_model, thinking, is_continuing, &settings, &model_runtime, &scoped.scoped_models).await;
            let session_start_event = initial.provenance.map(|provenance| SessionStartEvent {
                reason: maho_ext_api::SessionReason::Startup,
                initial_model_provenance: Some(provenance.to_owned()),
                previous_session_file: None,
            });
            let request = CliRuntimeRequest {
                cwd: Some(profile.runtime.cwd.clone()),
                session_manager: Some(session_manager),
                session_start_event,
                model: initial.model,
                thinking_level: thinking.or(initial.thinking_level.and_then(|level| maho_ai::types::ThinkingLevel::parse(level.as_str()))),
                thinking_selection: initial.thinking_selection,
                scoped_models: super::startup::session_model_entries(scoped.scoped_models)?,
                auto_title_sessions: profile.runtime.auto_title.or(config.auto_title_sessions),
                session_profile: maho_ext_api::ExtensionSessionProfile {
                    shared_host_enabled: true,
                    session_kind: profile.session_kind.unwrap_or_default(),
                    session_context: profile.session_context.clone().unwrap_or_default(),
                },
                ..Default::default()
            };
            let mounted = mount_agent_session_runtime(&config, request, Some(model_runtime), None).await?;
            Ok(mounted.runtime)
        }) as std::pin::Pin<Box<dyn std::future::Future<Output = Result<AgentSessionRuntime, String>> + Send>>
    })
}

/// Initial model and senpi origin forwarded to a fresh session's startup event.
pub struct CliInitialModel {
    pub model: Option<maho_ai::model::Model>,
    pub thinking_level: Option<maho_ai::types::ModelThinkingLevel>,
    pub provenance: Option<&'static str>,
    pub thinking_selection: Option<maho_ai::types::ThinkingSelection>,
}

fn provenance_str(provenance: maho_core::model_resolver::InitialModelProvenance) -> &'static str {
    use maho_core::model_resolver::InitialModelProvenance as P;
    match provenance {
        P::Cli => "cli",
        P::Scoped => "scoped",
        P::Settings => "settings",
        P::ProviderDefault => "provider-default",
        P::FirstAvailable => "first-available",
    }
}

pub async fn resolve_cli_initial_model(
    creation_model: Option<(String, String)>,
    thinking: Option<maho_ai::types::ThinkingLevel>,
    is_continuing: bool,
    settings: &SettingsManager,
    model_runtime: &ModelRuntime,
    scoped: &[maho_core::model_resolver::ScopedModel],
) -> CliInitialModel {
    if is_continuing {
        return CliInitialModel { model: None, thinking_level: None, provenance: None, thinking_selection: None };
    }
    if let Some((provider, model_id)) = creation_model {
        let resolved = maho_core::model_resolver::resolve_cli_model(Some(provider.as_str()), Some(model_id.as_str()), thinking.map(Into::into), model_runtime);
        return CliInitialModel { model: resolved.parsed.model, thinking_level: resolved.parsed.thinking_level, provenance: Some("cli"), thinking_selection: None };
    }
    let default_provider = settings.get_string("defaultProvider");
    let default_model_id = settings.get_string("defaultModel");
    match maho_core::model_resolver::find_initial_model(
        maho_core::model_resolver::InitialModelOptions {
            cli_provider: None,
            cli_model: None,
            scoped_models: scoped,
            is_continuing: false,
            default_provider: default_provider.as_deref(),
            default_model_id: default_model_id.as_deref(),
            model_thinking_levels: None,
        },
        model_runtime,
    ).await {
        Ok(resolved) => CliInitialModel {
            model: resolved.parsed.model,
            thinking_level: resolved.parsed.thinking_level,
            provenance: Some(provenance_str(resolved.provenance)),
            thinking_selection: resolved.parsed.thinking_selection,
        },
        Err(_) => CliInitialModel { model: None, thinking_level: None, provenance: None, thinking_selection: None },
    }
}

/// senpi's per-open session manager: an explicit `sessionPath` opens it with the profile's cwd.
pub fn open_profile_session_manager(config: &CliRuntimeConfiguration, profile: &maho_rpc::session_registry::RpcSessionLaunchProfile) -> SessionManager {
    let identity = profile.durable_session_id.clone().map(|id| maho_core::session_manager::NewSessionOptions { id: Some(id), parent_session: None });
    match profile.session_path.as_deref() {
        Some(path) => SessionManager::open(path, config.session_dir.as_deref(), Some(profile.runtime.cwd.as_str()), identity),
        None => SessionManager::create(&profile.runtime.cwd, config.session_dir.as_deref(), identity),
    }
}
