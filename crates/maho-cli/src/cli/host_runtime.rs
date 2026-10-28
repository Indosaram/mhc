//! Port of senpi `packages/coding-agent/src/main.ts` `createCliRuntimeFactory` and its
//! `CliRuntimeConfiguration` argument: the CLI-owned session-runtime factory every mode mounts
//! (print, rpc, interactive and the shared session host).
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
    AgentSessionRuntimeDiagnostic, CreateAgentSessionServicesOptions, create_agent_session_services,
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
    pub no_extensions: bool,
    pub skills: Vec<String>,
    pub prompt_templates: Vec<String>,
    pub themes: Vec<String>,
    pub hooks: Vec<String>,
    pub additional_hook_paths: Vec<PathBuf>,
    /// senpi `parsed.projectTrustOverride` (`--approve` / `--no-approve`).
    pub project_trust_override: Option<bool>,
    /// senpi `projectTrustByCwd`: one decision per cwd for every runtime this factory creates.
    pub project_trust_by_cwd: Arc<std::sync::Mutex<BTreeMap<String, bool>>>,
    /// senpi `parsed.unknownFlags`: extension flag values every mounted session applies.
    pub extension_flag_values: Option<BTreeMap<String, FlagValue>>,
    /// senpi `toolRuntime`: the resolved `--tool-runtime` (flag, then env); absent means `in-process`.
    /// A worker isolate never re-reads env, so it travels in `to_worker_json`.
    pub tool_runtime: Option<super::tool_runtime::ToolRuntimeKind>,
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
            no_extensions: false,
            skills: Vec::new(),
            prompt_templates: Vec::new(),
            themes: Vec::new(),
            hooks: Vec::new(),
            additional_hook_paths: Vec::new(),
            project_trust_override: None,
            project_trust_by_cwd: Arc::default(),
            extension_flag_values: None,
            tool_runtime: None,
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
            no_extensions: parsed.no_extensions,
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
            project_trust_override: parsed.project_trust_override,
            project_trust_by_cwd: Arc::default(),
            tool_runtime: parsed.tool_runtime,
            extension_flag_values: (!parsed.unknown_flags.is_empty()).then(|| {
                parsed
                    .unknown_flags
                    .iter()
                    .map(|(name, value)| {
                        (
                            name.clone(),
                            match value {
                                super::args::FlagValue::Boolean(value) => {
                                    FlagValue::Boolean(*value)
                                }
                                super::args::FlagValue::String(value) => {
                                    FlagValue::String(value.clone())
                                }
                            },
                        )
                    })
                    .collect()
            }),
        }
    }

    /// The configuration a session worker receives in its \`prepare\` (senpi posts the
    /// \`CliRuntimeConfiguration\` object; a structured clone keeps no shared trust cache, so the
    /// worker starts its own per-cwd decisions).
    pub fn to_worker_json(&self) -> serde_json::Value {
        serde_json::json!({
            "cwd": self.cwd, "agentDir": self.agent_dir, "appMode": self.app_mode.as_str(),
            "provider": self.provider, "model": self.model, "models": self.models, "thinking": self.thinking,
            "autoTitleSessions": self.auto_title_sessions, "help": self.help, "listModels": self.list_models, "listTips": self.list_tips,
            "sessionDir": self.session_dir, "extensions": self.extensions, "noExtensions": self.no_extensions,
            "skills": self.skills, "promptTemplates": self.prompt_templates, "themes": self.themes, "hooks": self.hooks,
            "additionalHookPaths": self.additional_hook_paths.iter().map(|path| path.to_string_lossy().into_owned()).collect::<Vec<_>>(),
            "projectTrustOverride": self.project_trust_override,
            "toolRuntime": self.tool_runtime.map(|kind| kind.as_str()),
            "extensionFlagValues": self.extension_flag_values.as_ref().map(|flags| flags.iter().map(|(name, value)| (name.clone(), match value {
                FlagValue::Boolean(value) => serde_json::json!(value),
                FlagValue::String(value) => serde_json::json!(value),
            })).collect::<serde_json::Map<_, _>>()),
        })
    }

    pub fn from_worker_json(value: &serde_json::Value) -> Result<Self, String> {
        let text = |key: &str| {
            value
                .get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        };
        let list = |key: &str| {
            value
                .get(key)
                .and_then(serde_json::Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str().map(str::to_owned))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        let flag = |key: &str| {
            value
                .get(key)
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        };
        let app_mode = match text("appMode").as_deref() {
            Some("interactive") => AppMode::Interactive,
            Some("print") => AppMode::Print,
            Some("json") => AppMode::Json,
            Some("rpc") => AppMode::Rpc,
            other => return Err(format!("Invalid worker configuration appMode: {other:?}")),
        };
        Ok(Self {
            cwd: text("cwd").ok_or("Invalid worker configuration: cwd")?,
            agent_dir: text("agentDir").ok_or("Invalid worker configuration: agentDir")?,
            app_mode,
            provider: text("provider"),
            model: text("model"),
            models: list("models"),
            thinking: text("thinking"),
            auto_title_sessions: value
                .get("autoTitleSessions")
                .and_then(serde_json::Value::as_bool),
            help: flag("help"),
            list_models: flag("listModels"),
            list_tips: flag("listTips"),
            session_dir: text("sessionDir"),
            extensions: list("extensions"),
            no_extensions: flag("noExtensions"),
            skills: list("skills"),
            prompt_templates: list("promptTemplates"),
            themes: list("themes"),
            hooks: list("hooks"),
            additional_hook_paths: list("additionalHookPaths")
                .into_iter()
                .map(PathBuf::from)
                .collect(),
            project_trust_override: value
                .get("projectTrustOverride")
                .and_then(serde_json::Value::as_bool),
            project_trust_by_cwd: Arc::default(),
            tool_runtime: text("toolRuntime")
                .as_deref()
                .and_then(super::tool_runtime::ToolRuntimeKind::parse),
            extension_flag_values: value
                .get("extensionFlagValues")
                .and_then(serde_json::Value::as_object)
                .map(|flags| {
                    flags
                        .iter()
                        .filter_map(|(name, value)| {
                            Some((
                                name.clone(),
                                match value {
                                    serde_json::Value::Bool(value) => FlagValue::Boolean(*value),
                                    serde_json::Value::String(value) => {
                                        FlagValue::String(value.clone())
                                    }
                                    _ => return None,
                                },
                            ))
                        })
                        .collect()
                }),
        })
    }

    /// senpi `trustPromptMode`: metadata-only launches resolve trust as a print run.
    pub fn trust_prompt_mode(&self) -> AppMode {
        if self.help || self.list_models || self.list_tips {
            AppMode::Print
        } else {
            to_project_trust_mode(self.app_mode)
        }
    }

    /// senpi `resolveCliPaths`.
    pub fn resolve_cli_paths(&self, paths: &[String]) -> Vec<String> {
        paths
            .iter()
            .map(|path| resolve_cli_path(&self.cwd, path))
            .collect()
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

    /// senpi `DefaultResourceLoader` CLI inputs: `-e` sources, `--skill`, `--prompt-template` and hook
    /// paths, resolved against the launch cwd.
    pub fn resource_loader_inputs(&self) -> maho_core::resource_loader::ResourceLoaderInputs {
        maho_core::resource_loader::ResourceLoaderInputs {
            additional_extension_paths: self.resolved_extension_paths(),
            additional_skill_paths: self.resolved_skill_paths(),
            additional_prompt_template_paths: self.resolved_prompt_template_paths(),
            ..Default::default()
        }
    }

    /// senpi `cliExtensionPaths.hooks` resolved against the launch cwd.
    pub fn resolved_hook_paths(&self) -> Vec<PathBuf> {
        self.resolve_cli_paths(&self.hooks)
            .into_iter()
            .map(PathBuf::from)
            .collect()
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
            prompt_size_inventory: None,
        }
    }
}

/// senpi `toProjectTrustMode`.
pub fn to_project_trust_mode(mode: AppMode) -> AppMode {
    mode
}

/// senpi `createCliRuntimeFactory`'s project-trust decision for one runtime cwd: a cached decision
/// wins, then `resolveProjectTrusted` (override, no trust-requiring resources, the pre-trust
/// extensions' `project_trust` handlers, saved `trust.json`, global `defaultProjectTrust`, then the
/// prompt when `has_ui`). A decision reached through the resolver is cached per cwd. Extension load
/// and handler failures are reported on stderr as warnings.
pub async fn resolve_runtime_project_trust(
    config: &CliRuntimeConfiguration,
    cwd: &str,
    mode: AppMode,
    has_ui: bool,
    selector: Box<super::project_trust::Selector>,
) -> bool {
    let (trusted, warnings) =
        resolve_runtime_project_trust_with_warnings(config, cwd, mode, has_ui, selector).await;
    for warning in warnings {
        maho_core::output_guard::maho_write_stderr(&format!("Warning: {warning}\n"));
    }
    trusted
}

/// [`resolve_runtime_project_trust`] returning senpi's `projectTrustDiagnostics` (main.ts:769-775)
/// for the caller to report with its runtime diagnostics.
pub async fn resolve_runtime_project_trust_with_warnings(
    config: &CliRuntimeConfiguration,
    cwd: &str,
    mode: AppMode,
    has_ui: bool,
    selector: Box<super::project_trust::Selector>,
) -> (bool, Vec<String>) {
    let cached = config
        .project_trust_by_cwd
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(cwd)
        .copied();
    if let Some(trusted) = cached {
        return (trusted, Vec::new());
    }
    let should_resolve = config.project_trust_override.is_none()
        && maho_core::trust_manager::has_trust_requiring_project_resources(cwd, None);
    let bootstrap = SettingsManager::create(
        cwd,
        &config.agent_dir,
        &maho_core::config::home_dir(),
        false,
    );
    let default_project_trust = bootstrap
        .get_global()
        .get("defaultProjectTrust")
        .and_then(serde_json::Value::as_str)
        .and_then(maho_core::project_trust::DefaultProjectTrust::parse);
    let trust_store = maho_core::trust_manager::ProjectTrustStore::new(&config.agent_dir);
    let selector: Arc<super::project_trust::Selector> = Arc::from(selector);
    let mut warnings = Vec::new();
    let extension_trust = if should_resolve {
        let startup_ui = (
            bootstrap.get_string("theme"),
            bootstrap.get_show_hardware_cursor(),
            bootstrap.get_clear_on_shrink(),
        );
        let ui = Arc::new(super::project_trust::ProjectTrustUi {
            mode,
            has_ui,
            select: selector.clone(),
            input: Arc::new(move |title: &str, placeholder: Option<&str>| {
                match super::startup_ui::show_startup_input(
                    startup_ui.0.as_deref(),
                    startup_ui.1,
                    startup_ui.2,
                    title,
                    placeholder,
                ) {
                    Ok(value) => value,
                    Err(error) => {
                        maho_core::output_guard::maho_write_stderr(&format!("{error}\n"));
                        None
                    }
                }
            }),
        });
        let resolved = super::project_trust::resolve_extension_project_trust(
            cwd,
            &config.agent_dir,
            &config.resolved_extension_paths(),
            config.no_extensions,
            mode,
            ui,
        )
        .await;
        warnings.extend(resolved.load_warnings);
        warnings.extend(resolved.trust_warnings);
        resolved.result
    } else {
        None
    };
    let emit = move |_: &str| -> Result<
        Option<maho_ext_api::ProjectTrustEventResult>,
        maho_ext_api::ExtensionFailure,
    > { Ok(extension_trust.clone()) };
    let context_selector = selector.clone();
    let context = super::project_trust::CliProjectTrustContext {
        cwd: cwd.to_owned(),
        mode,
        ui_available: has_ui,
        selector: Box::new(move |title, options| context_selector(title, options)),
    };
    let trusted = maho_core::project_trust::resolve_project_trusted(
        maho_core::project_trust::ResolveProjectTrustedOptions {
            cwd,
            trust_store: &trust_store,
            trust_override: config.project_trust_override,
            default_project_trust,
            emit_project_trust: Some(&emit),
            project_trust_context: &context,
            on_extension_error: None,
        },
    )
    .await;
    if should_resolve {
        config
            .project_trust_by_cwd
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(cwd.to_owned(), trusted);
    }
    (trusted, warnings)
}

/// senpi `resolveCliPaths` entry: package sources and remote URLs (`isLocalPath` false) are kept
/// verbatim; local values go through `resolvePath` (`~` and `file://` expansion) against the cwd.
pub fn resolve_cli_path(cwd: &str, path: &str) -> String {
    if !maho_core::paths::is_local_path(path) {
        return path.to_owned();
    }
    let normalized = crate::utils::paths::normalize_path(
        path,
        &crate::utils::paths::PathInputOptions::default(),
    )
    .unwrap_or_else(|_| path.to_owned());
    maho_core::paths::resolve_path(
        &normalized,
        cwd,
        &maho_core::paths::PathInputOptions::default(),
    )
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
    /// senpi `mcpRegistry`: the multi-session host's one shared MCP registry.
    pub mcp_registry: Option<Arc<maho_ext_mcp::host_registry::HostMcpRegistry>>,
    /// Mode-specific bindings for the first `bind_extensions`; the mount still sets the mode.
    pub extension_bindings: Option<maho_core::agent_session::ExtensionBindingsFactory>,
    pub extension_ui: Option<Arc<dyn maho_ext_api::ExtensionUi>>,
    /// senpi `createAgentSession({ toolRuntime })`: a caller-chosen runtime wins over the configuration's.
    pub tool_runtime: Option<maho_core::tool_dispatch::tool_dispatcher::ToolRuntimeKind>,
}

/// The mounted session runtime plus the diagnostics senpi reports before the first turn.
pub struct MountedRuntime {
    pub runtime: AgentSessionRuntime,
    pub diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
    pub model_fallback_message: Option<String>,
    pub widget_requests:
        tokio::sync::mpsc::UnboundedReceiver<maho_interactive::interactive_extension_ui::UiRequest>,
}

impl MountedRuntime {
    pub fn session(&self) -> &AgentSession {
        self.runtime.session()
    }

    pub fn has_errors(&self) -> bool {
        self.runtime.diagnostics().iter().any(|diagnostic| {
            diagnostic.kind
                == maho_core::agent_session_services::AgentSessionRuntimeDiagnosticType::Error
        })
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
        mcp_registry,
        extension_bindings,
        extension_ui,
        tool_runtime,
    } = request;
    let cwd = cwd.unwrap_or_else(|| config.cwd.clone());
    let mut launch_config = config.clone();
    launch_config.cwd = cwd.clone();
    let mut trust_warnings = Vec::new();
    let settings_manager = match settings_manager {
        Some(settings_manager) => settings_manager,
        None => {
            let (trusted, warnings) = resolve_runtime_project_trust_with_warnings(
                config,
                &cwd,
                to_project_trust_mode(config.app_mode),
                false,
                Box::new(|_, _| None),
            )
            .await;
            trust_warnings = warnings;
            SettingsManager::create(
                &cwd,
                &config.agent_dir,
                &maho_core::config::home_dir(),
                trusted,
            )
        }
    };
    let settings_manager = Some(settings_manager);
    let mut services = create_agent_session_services(CreateAgentSessionServicesOptions {
        cwd: cwd.clone(),
        agent_dir: Some(config.agent_dir.clone()),
        settings_manager,
        model_runtime,
    })
    .await
    .map_err(|error| error.to_string())?;
    let (widget_sender, widget_requests) = tokio::sync::mpsc::unbounded_channel();
    let reload_widget_sender = widget_sender.clone();
    let reload_mcp_registry = mcp_registry.clone();
    let task_parent = Arc::new(std::sync::OnceLock::new());
    let extension_factories = super::default_extensions::assembled_factories_with_mcp_registry(
        widget_sender,
        task_parent.clone(),
        mcp_registry,
        &services.settings_manager,
        Path::new(&cwd),
        &config.agent_dir,
    );
    let (extension_factories, extension_errors) = super::default_extensions::with_user_extensions(
        extension_factories,
        &mut services.settings_manager,
        &cwd,
        &config.agent_dir,
        &config.resolved_extension_paths(),
        config.no_extensions,
    )
    .await?;
    // senpi main.ts:129-136 `collectExtensionLoadDiagnostics`: a failed extension load is a warning
    // and the session still opens. senpi main.ts:769-775 `projectTrustDiagnostics` lead the list.
    services.diagnostics.splice(
        0..0,
        trust_warnings
            .into_iter()
            .map(|message| AgentSessionRuntimeDiagnostic {
                kind: maho_core::agent_session_services::AgentSessionRuntimeDiagnosticType::Warning,
                message,
            }),
    );
    services
        .diagnostics
        .extend(
            extension_errors
                .into_iter()
                .map(|error| {
                    AgentSessionRuntimeDiagnostic {
            kind: maho_core::agent_session_services::AgentSessionRuntimeDiagnosticType::Warning,
            message: super::default_extensions::extension_load_failure_message(
                &error.path,
                &error.message,
            ),
        }
                }),
        );
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
            // senpi main.ts:1091 `...(toolRuntime !== undefined ? { toolRuntime } : {})`.
            tool_runtime: tool_runtime.or_else(|| {
                config
                    .tool_runtime
                    .map(super::tool_runtime::to_core_tool_runtime)
            }),
            extension_factories,
            session_profile: session_profile.clone(),
            extension_flag_values,
            defer_extension_start: true,
            resource_inputs: launch_config.resource_loader_inputs(),
            ..Default::default()
        },
    )
    .await?;
    let mut theme_paths = Vec::new();
    let cwd = created.session.cwd();
    let agent_dir = created.session.agent_dir();
    let mut theme_settings = maho_core::settings_manager::SettingsManager::in_memory(
        created
            .session
            .with_settings_manager(|settings| settings.get_global().clone()),
        false,
    );
    let packages = maho_core::package_manager::DefaultPackageManager::new(
        maho_core::package_manager::PackageManagerOptions {
            cwd: &cwd,
            agent_dir: &agent_dir,
            settings_manager: &mut theme_settings,
        },
    );
    match packages.resolve(None).await {
        Ok(paths) => theme_paths.extend(
            paths
                .themes
                .into_iter()
                .filter(|resource| resource.enabled)
                .map(|resource| PathBuf::from(resource.path)),
        ),
        Err(error) => {
            maho_core::output_guard::maho_write_stderr(&format!("package themes: {error}\n"));
        }
    }
    theme_paths.extend(config.resolved_theme_paths().into_iter().map(PathBuf::from));
    super::default_extensions::configure_rpc_html_theme_with_paths(&created.session, theme_paths);
    super::memory_report_registry::register_session(&created.session);
    task_parent
        .set(created.session.weak_accessor())
        .map_err(|_| "Task parent already bound".to_owned())?;
    created
        .session
        .set_extension_runtime_factory(super::default_extensions::reloading_runtime_factory(
            reload_widget_sender,
            task_parent.clone(),
            reload_mcp_registry,
            config.resolved_extension_paths(),
            config.no_extensions,
            session_profile,
        ))
        .await;
    let hooks = created.session.with_settings_manager(|settings| {
        super::hook_sources::build_loaded_hook_sources(&launch_config, settings)
    });
    created.session.set_hook_sources(Some(hooks));
    let mut bindings = extension_bindings
        .map(|factory| factory(&created.session))
        .unwrap_or_default();
    if extension_ui.is_some() {
        bindings.ui_context = extension_ui;
    }
    created
        .session
        .bind_extensions(maho_core::agent_session::ExtensionBindings {
            mode: Some(match config.app_mode {
                AppMode::Interactive => maho_ext_api::ExtensionMode::Tui,
                AppMode::Rpc => maho_ext_api::ExtensionMode::Rpc,
                AppMode::Json => maho_ext_api::ExtensionMode::Json,
                AppMode::Print => maho_ext_api::ExtensionMode::Print,
            }),
            ..bindings
        })
        .await;
    let diagnostics = created.services.diagnostics.clone();
    let model_fallback_message = created.model_fallback_message.clone();
    let runtime = AgentSessionRuntime::new(
        created.session,
        created.services,
        diagnostics.clone(),
        model_fallback_message.clone(),
        Some(launch_config.launch_profile()),
    );
    Ok(MountedRuntime {
        runtime,
        diagnostics,
        model_fallback_message,
        widget_requests,
    })
}

/// The session factory shape the host mounts: one call per session, keyed by cwd and
/// session manager, returning a fully mounted session.
pub type CliSessionFactory = Arc<
    dyn Fn(
            CliSessionFactoryRequest,
        ) -> Pin<Box<dyn Future<Output = Result<MountedRuntime, String>> + Send>>
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
                extension_flag_values: config.extension_flag_values.clone(),
                ..Default::default()
            };
            mount_agent_session_runtime(&config, mount_request, model_runtime, settings_manager)
                .await
        })
    })
}

/// The shared host's process-local core, built from the CLI configuration and the host's cwd
/// (senpi `createHostCore` + the `runMultiSessionHost` argument object).
pub fn create_host_core(
    config: CliRuntimeConfiguration,
    cwd: &str,
    listen: Option<&str>,
) -> Result<std::sync::Arc<maho_rpc::multi_session_host::HostCore>, maho_ai::models::ModelsError> {
    // One model runtime for every session this host opens (senpi `hostModelRuntime`, senpi#1844):
    // the sessions share one agent dir, so a per-open rebuild would pay the same catalog load once
    // per open instead of once per host.
    let model_runtime = host_model_runtime(&config)?;
    let tool_runtime = config
        .tool_runtime
        .unwrap_or(super::tool_runtime::ToolRuntimeKind::InProcess);
    let create_runtime = host_runtime_factory_with_mcp_registry(
        config.clone(),
        model_runtime,
        Arc::new(maho_ext_mcp::host_registry::HostMcpRegistry::default()),
    );
    // A socket host answers each connection through its own writer, so its fallback writer is a
    // sink; the stdio host answers on stdout (senpi `runSocketHost` vs `runStdioHost`).
    let writer = match listen {
        None | Some("stdio://") => std::sync::Arc::new(
            maho_rpc::session_event_writer::SessionWriterActor::new(tokio::io::stdout()),
        ),
        Some(_) => std::sync::Arc::new(maho_rpc::session_event_writer::SessionWriterActor::new(
            tokio::io::sink(),
        )),
    };
    Ok(std::sync::Arc::new(
        maho_rpc::multi_session_host::HostCore::new(
            maho_rpc::multi_session_host::HostCoreOptions {
                agent_dir: PathBuf::from(&config.agent_dir),
                cwd: cwd.to_owned(),
                create_runtime,
                capabilities: maho_rpc::custom_capability::parse_client_capabilities(
                    maho_core::brand::env_value(
                        "RPC_CLIENT_CAPABILITIES",
                        &maho_core::config::current_env(),
                    )
                    .as_deref(),
                ),
                close_grace_ms: host_close_grace_ms(),
            },
            writer,
        )
        .with_tool_runtime(tool_runtime),
    ))
}

/// senpi main.ts:1112-1154 with \`resolveSessionRuntime(parsed) === "worker"\`: the host passes its
/// configuration as \`workerConfiguration\`, so \`createHostCore\` selects the worker registry and every
/// session runs in its own isolated worker process. No shared model runtime is built: each worker
/// builds its own (an object cannot cross the process boundary).
pub fn create_worker_host_core(
    config: CliRuntimeConfiguration,
    cwd: &str,
    listen: Option<&str>,
    executable: PathBuf,
) -> std::sync::Arc<maho_rpc::multi_session_host::HostCore> {
    let create_runtime: maho_rpc::multi_session_host::HostRuntimeFactory = Arc::new(
        |_profile: maho_rpc::session_registry::RpcSessionLaunchProfile| {
            Box::pin(async {
                Err("a worker-runtime host opens every session in its own worker".to_owned())
            })
                as Pin<Box<dyn Future<Output = Result<AgentSessionRuntime, String>> + Send>>
        },
    );
    let writer = match listen {
        None | Some("stdio://") => std::sync::Arc::new(
            maho_rpc::session_event_writer::SessionWriterActor::new(tokio::io::stdout()),
        ),
        Some(_) => std::sync::Arc::new(maho_rpc::session_event_writer::SessionWriterActor::new(
            tokio::io::sink(),
        )),
    };
    let close_grace_ms = host_close_grace_ms();
    let tool_runtime = config
        .tool_runtime
        .unwrap_or(super::tool_runtime::ToolRuntimeKind::InProcess);
    let workers = maho_rpc::worker_session_registry::WorkerSessionRegistry::new(
        config.to_worker_json(),
        session_worker_spawner(executable),
        close_grace_ms,
    );
    std::sync::Arc::new(
        maho_rpc::multi_session_host::HostCore::new(
            maho_rpc::multi_session_host::HostCoreOptions {
                agent_dir: PathBuf::from(&config.agent_dir),
                cwd: cwd.to_owned(),
                create_runtime,
                capabilities: maho_rpc::custom_capability::parse_client_capabilities(
                    maho_core::brand::env_value(
                        "RPC_CLIENT_CAPABILITIES",
                        &maho_core::config::current_env(),
                    )
                    .as_deref(),
                ),
                close_grace_ms,
            },
            writer,
        )
        .with_worker_registry(workers)
        .with_tool_runtime(tool_runtime),
    )
}

/// One isolated session worker: this binary re-entered under the internal rpc-session-worker role
/// (senpi \`new Worker(session-worker.ts)\`), speaking the worker protocol on stdin/stdout.
pub fn session_worker_spawner(
    executable: PathBuf,
) -> maho_rpc::session_worker_client::SpawnSessionWorker {
    Arc::new(move || {
        let mut command = tokio::process::Command::new(&executable);
        command
            .env(
                crate::experimental::process::INTERNAL_PROCESS_ENV,
                crate::experimental::process::RPC_SESSION_WORKER_ROLE,
            )
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true);
        command.spawn()
    })
}

/// senpi \`session-worker.ts\` \`commit\`: \`createCliRuntimeFactory(prepared.configuration)\` inside the worker.
pub fn session_worker_configuration(
    value: &serde_json::Value,
) -> Result<maho_rpc::session_worker::SessionWorkerConfiguration, String> {
    let config = CliRuntimeConfiguration::from_worker_json(value)?;
    let model_runtime = host_model_runtime(&config).map_err(|error| error.to_string())?;
    Ok(maho_rpc::session_worker::SessionWorkerConfiguration {
        agent_dir: config.agent_dir.clone(),
        session_dir: config.session_dir.clone(),
        create_runtime: host_runtime_factory(config, model_runtime),
    })
}

/// senpi's shared host model runtime (`hostModelRuntime`): one catalog and one auth store behind
/// every in-process session the host opens.
fn host_model_runtime(
    config: &CliRuntimeConfiguration,
) -> Result<ModelRuntime, maho_ai::models::ModelsError> {
    ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {
        models_path: Some(Path::new(&config.agent_dir).join("models.json")),
        auth_path: Some(Path::new(&config.agent_dir).join("auth.json")),
        ..Default::default()
    })
}

/// senpi `closeGraceMs`: the configured grace, else ten seconds.
fn host_close_grace_ms() -> u64 {
    maho_rpc::host_lifecycle::parse_idle_exit_ms(
        std::env::var(maho_rpc::multi_session_host::RPC_CLOSE_GRACE_MS_ENV)
            .ok()
            .as_deref(),
    )
    .map_or(10_000, |milliseconds| {
        if milliseconds.is_finite() && milliseconds > 0.0 {
            milliseconds as u64
        } else {
            10_000
        }
    })
}

/// Serve the shared host (senpi `runMultiSessionHost`).
pub async fn serve_multi_session_host(
    core: std::sync::Arc<maho_rpc::multi_session_host::HostCore>,
    listen: Option<String>,
) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        maho_rpc::multi_session_host::set_host_signal_cleanup(
            crate::utils::shell::kill_tracked_detached_children,
        );
    }
    maho_rpc::multi_session_host::run_multi_session_host(core, listen).await
}

/// senpi `createCliRuntimeFactory`'s `HostRuntimeFactory` adapter: one fully mounted session runtime
/// per `open_session`, built by the same CLI mount path every other mode uses (the seam
/// `maho_rpc::multi_session_host::HostRuntimeFactory` exists for).
pub fn host_runtime_factory(
    config: CliRuntimeConfiguration,
    model_runtime: ModelRuntime,
) -> maho_rpc::multi_session_host::HostRuntimeFactory {
    host_runtime_factory_with_mcp_registry(
        config,
        model_runtime,
        Arc::new(maho_ext_mcp::host_registry::HostMcpRegistry::default()),
    )
}

/// `host_runtime_factory` over the host's one `HostMcpRegistry` (senpi `createHostCore`
/// `mcpRegistry: new HostMcpRegistry()`), injected into every session's `mcp` builtin.
pub fn host_runtime_factory_with_mcp_registry(
    config: CliRuntimeConfiguration,
    model_runtime: ModelRuntime,
    mcp_registry: Arc<maho_ext_mcp::host_registry::HostMcpRegistry>,
) -> maho_rpc::multi_session_host::HostRuntimeFactory {
    std::sync::Arc::new(
        move |profile: maho_rpc::session_registry::RpcSessionLaunchProfile| {
            let config = config.clone();
            let model_runtime = model_runtime.clone();
            let mcp_registry = mcp_registry.clone();
            Box::pin(async move {
                let session_manager = open_profile_session_manager(&config, &profile)?;
                // The profile's per-session startup choices feed the same resolver as
                // `--provider/--model/--thinking`: an open that names none keeps the CLI's own
                // (senpi `runtimeParsed` inside `createCliRuntimeFactory`).
                let thinking = profile
                    .runtime
                    .initial_thinking_level
                    .clone()
                    .or_else(|| config.thinking.clone())
                    .and_then(|level| maho_ai::types::ThinkingLevel::parse(&level));
                let creation_model = profile
                    .runtime
                    .creation_model
                    .clone()
                    .or_else(|| config.provider.clone().zip(config.model.clone()));
                // senpi `resolveModelScope`: scope patterns resolve against authenticated models.
                let scope_models = if config.models.is_empty() {
                    Vec::new()
                } else {
                    model_runtime
                        .get_available(None)
                        .await
                        .map_err(|error| error.to_string())?
                };
                let scoped = maho_core::model_resolver::resolve_model_scope_from_models(
                    &config.models,
                    &scope_models,
                );
                let trusted = resolve_runtime_project_trust(
                    &config,
                    &profile.runtime.cwd,
                    to_project_trust_mode(config.app_mode),
                    false,
                    Box::new(|_, _| None),
                )
                .await;
                let settings = SettingsManager::create(
                    &profile.runtime.cwd,
                    &config.agent_dir,
                    &maho_core::config::home_dir(),
                    trusted,
                );
                let is_continuing = !session_manager.current_context().messages.is_empty();
                let initial = resolve_cli_initial_model(
                    creation_model,
                    thinking,
                    is_continuing,
                    &settings,
                    &model_runtime,
                    &scoped.scoped_models,
                )
                .await?;
                let session_start_event = initial.provenance.map(|provenance| SessionStartEvent {
                    reason: maho_ext_api::SessionReason::Startup,
                    initial_model_provenance: Some(provenance.to_owned()),
                    previous_session_file: None,
                    attended: None,
                });
                let request = CliRuntimeRequest {
                    cwd: Some(profile.runtime.cwd.clone()),
                    session_manager: Some(session_manager),
                    session_start_event,
                    model: initial.model,
                    thinking_level: thinking.or(initial
                        .thinking_level
                        .and_then(|level| maho_ai::types::ThinkingLevel::parse(level.as_str()))),
                    thinking_selection: initial.thinking_selection,
                    scoped_models: super::startup::session_model_entries(scoped.scoped_models)?,
                    auto_title_sessions: profile.runtime.auto_title.or(config.auto_title_sessions),
                    session_profile: maho_ext_api::ExtensionSessionProfile {
                        shared_host_enabled: true,
                        session_kind: profile.session_kind.unwrap_or_default(),
                        session_context: profile.session_context.clone().unwrap_or_default(),
                    },
                    mcp_registry: Some(mcp_registry),
                    extension_flag_values: config.extension_flag_values.clone(),
                    ..Default::default()
                };
                let mounted = mount_agent_session_runtime(
                    &config,
                    request,
                    Some(model_runtime),
                    Some(settings),
                )
                .await?;
                let mut runtime = mounted.runtime;
                apply_launch_profile(&mut runtime, &profile)?;
                Ok(runtime)
            })
                as std::pin::Pin<
                    Box<
                        dyn std::future::Future<Output = Result<AgentSessionRuntime, String>>
                            + Send,
                    >,
                >
        },
    )
}

/// senpi `AgentSessionRuntime` constructor + `apply`: the per-open profile values a launch carries.
/// The holder record names the host endpoint, the retry-fallback policy overlays this session's
/// settings in memory, and the prompt surface / browser engine are kept on the runtime.
fn apply_launch_profile(
    runtime: &mut AgentSessionRuntime,
    profile: &maho_rpc::session_registry::RpcSessionLaunchProfile,
) -> Result<(), String> {
    if let Some(retry_fallback) = &profile.retry_fallback {
        runtime.set_retry_fallback(retry_fallback);
    }
    if let Some(surface) = profile
        .prompt_surface
        .as_deref()
        .and_then(|surface| match surface {
            "terminal" => Some(maho_ext_api::PromptSurface::Terminal),
            "app" => Some(maho_ext_api::PromptSurface::App),
            "chat" => Some(maho_ext_api::PromptSurface::Chat),
            _ => None,
        })
    {
        runtime.set_prompt_surface(surface);
    }
    if let Some(engine) = &profile.browser_engine {
        runtime.set_browser_engine(engine.clone());
    }
    runtime.set_holder_endpoint(profile.holder_endpoint.clone())
}

/// Initial model and senpi origin forwarded to a fresh session's startup event.
pub struct CliInitialModel {
    pub model: Option<maho_ai::model::Model>,
    pub thinking_level: Option<maho_ai::types::ModelThinkingLevel>,
    pub provenance: Option<&'static str>,
    pub thinking_selection: Option<maho_ai::types::ThinkingSelection>,
}

pub async fn resolve_cli_initial_model(
    creation_model: Option<(String, String)>,
    thinking: Option<maho_ai::types::ThinkingLevel>,
    is_continuing: bool,
    settings: &SettingsManager,
    model_runtime: &ModelRuntime,
    scoped: &[maho_core::model_resolver::ScopedModel],
) -> Result<CliInitialModel, String> {
    if is_continuing {
        return Ok(CliInitialModel {
            model: None,
            thinking_level: None,
            provenance: None,
            thinking_selection: None,
        });
    }
    if let Some((provider, model_id)) = creation_model {
        let resolved = maho_core::model_resolver::resolve_cli_model(
            Some(provider.as_str()),
            Some(model_id.as_str()),
            thinking.map(Into::into),
            model_runtime,
        );
        return Ok(CliInitialModel {
            model: resolved.parsed.model,
            thinking_level: resolved.parsed.thinking_level,
            provenance: Some("cli"),
            thinking_selection: None,
        });
    }
    let default_provider = settings.get_string("defaultProvider");
    let default_model_id = settings.get_string("defaultModel");
    let resolved = maho_core::model_resolver::find_initial_model(
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
    )
    .await?;
    Ok(CliInitialModel {
        model: resolved.parsed.model,
        thinking_level: resolved.parsed.thinking_level,
        provenance: Some(maho_core::model_resolver::initial_model_provenance_str(resolved.provenance)),
        thinking_selection: resolved.parsed.thinking_selection,
    })
}

/// senpi's per-open session manager: an explicit `sessionPath` opens it with the profile's cwd.
pub fn open_profile_session_manager(
    config: &CliRuntimeConfiguration,
    profile: &maho_rpc::session_registry::RpcSessionLaunchProfile,
) -> Result<SessionManager, String> {
    let identity = profile.durable_session_id.clone().map(|id| {
        maho_core::session_manager::NewSessionOptions {
            id: Some(id),
            parent_session: None,
            ..Default::default()
        }
    });
    match profile.session_path.as_deref() {
        Some(path) => SessionManager::open(
            path,
            config.session_dir.as_deref(),
            Some(profile.runtime.cwd.as_str()),
            identity,
        ),
        None => Ok(SessionManager::create(
            &profile.runtime.cwd,
            config.session_dir.as_deref(),
            identity,
        )),
    }
}
