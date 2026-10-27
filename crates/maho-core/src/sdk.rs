//! Port of senpi `packages/coding-agent/src/core/sdk.ts`.
//!
//! `createAgentSession` is ported; see `parity.d/21.md` for the remaining integration gaps.
//! The original resource-loader note is retained only as historical context:
//! the built-in tool factories (`tools/index.ts`, todo 18) and `AgentSession::_buildRuntime`, none
//! of which are ported yet, so only `clampThinkingLevelToModel` and the option/result shapes are
//! here. `setDefaultStreamFn(streamSimple)` at module load has no Rust counterpart because the
//! agent's default stream function is installed by the caller.

use maho_ai::model::Model;
use maho_ai::types::{ModelThinkingLevel, ThinkingLevel, ThinkingSelection};

use crate::agent_session::SessionModelEntry;
use crate::auth_storage::AuthStorage;
use crate::model_registry::ModelRegistry;
use crate::model_runtime::ModelRuntime;
use crate::session_manager::SessionManager;
use crate::settings_manager::SettingsManager;

pub(crate) mod extension_context;

#[derive(Default)]
pub struct CreateAgentSessionOptions {
    pub cwd: Option<String>,
    pub agent_dir: Option<String>,
    pub model_runtime: Option<ModelRuntime>,
    pub auth_storage: Option<std::sync::Arc<AuthStorage>>,
    pub model_registry: Option<ModelRegistry>,
    pub model: Option<Model>,
    pub initial_model_provenance: Option<String>,
    pub thinking_level: Option<ThinkingLevel>,
    pub thinking_selection: Option<ThinkingSelection>,
    pub scoped_models: Vec<SessionModelEntry>,
    pub favorite_models: Vec<SessionModelEntry>,
    pub no_tools: Option<NoToolsMode>,
    pub tools: Option<Vec<String>>,
    pub exclude_tools: Option<Vec<String>>,
    pub custom_tools: Vec<maho_ext_api::ToolDefinition>,
    pub context_files: Vec<maho_ext_api::ContextFile>,
    pub session_manager: Option<SessionManager>,
    pub settings_manager: Option<SettingsManager>,
    pub session_start_event: Option<maho_ext_api::SessionStartEvent>,
    pub auto_title_sessions: Option<bool>,
    pub system_prompt: Option<String>,
    pub append_system_prompt: Vec<String>,
    pub extension_factories: Vec<maho_ext_host::loader::NativeAsyncExtensionFactory>,
    pub loaded_extensions: Option<maho_ext_host::loader::LoadExtensionsResult>,
    pub hook_resources: Vec<crate::package_manager::ResolvedResource>,
    pub additional_hook_paths: Vec<String>,
    /// senpi `DefaultResourceLoader` CLI inputs (`-e`, `--skill`, `--prompt-template`, `--no-skills`,
    /// `--no-prompt-templates`); merged with `hook_resources` / `additional_hook_paths`.
    pub resource_inputs: crate::resource_loader::ResourceLoaderInputs,
    pub minimal_resources: bool,
    pub defer_extension_start: bool,
    pub session_profile: maho_ext_api::ExtensionSessionProfile,
    /// The first `bind_extensions` call's bindings, built over the created session before `session_start`.
    pub extension_bindings: Option<crate::agent_session::ExtensionBindingsFactory>,
    /// senpi `bindExtensions({ uiContext })`: the UI a mode binds before `session_start` runs.
    pub extension_ui: Option<std::sync::Arc<dyn maho_ext_api::ExtensionUi>>,
    /// senpi `toolRuntime`: where heavy tool work runs; `None` is `in-process`.
    pub tool_runtime: Option<crate::tool_dispatch::tool_dispatcher::ToolRuntimeKind>,
    /// senpi `promptSurface`: absent means `SENPI_PROMPT_SURFACE`.
    pub prompt_surface: Option<maho_ext_api::PromptSurface>,
    /// senpi `browserEngine`: absent means none was chosen.
    pub browser_engine: Option<String>,
}

/// `noTools` suppression mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoToolsMode {
    All,
    Builtin,
}

/// Result from `createAgentSession`.
pub struct CreateAgentSessionResult {
    pub session: crate::agent_session::AgentSession,
    pub model_fallback_message: Option<String>,
}

#[derive(Clone)]
pub struct HostRuntimeFactory {
    pub model_runtime: ModelRuntime,
    pub model_registry: ModelRegistry,
    pub extension_factories: Vec<maho_ext_host::loader::NativeAsyncExtensionFactory>,
}

impl HostRuntimeFactory {
    pub async fn create(
        &self,
        mut options: CreateAgentSessionOptions,
    ) -> Result<CreateAgentSessionResult, String> {
        options.model_runtime = Some(self.model_runtime.clone());
        options.model_registry = Some(self.model_registry.clone());
        options.extension_factories = self.extension_factories.clone();
        create_agent_session(options).await
    }
}

struct PendingSessionConstruction {
    session: Option<crate::agent_session::AgentSession>,
    runtime: tokio::runtime::Handle,
}

static READ_EXECUTION_POLICY: std::sync::LazyLock<maho_tools::read::ReadExecutionPolicy> =
    std::sync::LazyLock::new(maho_tools::read::ReadExecutionPolicy::default);

impl Drop for PendingSessionConstruction {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            self.runtime.spawn(async move {
                session.dispose().await;
            });
        }
    }
}

/// senpi `this._toolDispatcher.bashOperations({ shellPath })`, resolved per call: the built-in tools are built
/// before the session exists, so the session's dispatcher is reached through the accessor. A session that is gone
/// runs the local operations, the in-process runtime's choice.
struct DispatcherBashOperations {
    session: std::sync::Arc<dyn Fn() -> Option<crate::agent_session::AgentSession> + Send + Sync>,
    shell_path: Option<String>,
}

impl maho_tools::bash::BashOperations for DispatcherBashOperations {
    fn exec<'a>(
        &'a self,
        command: &'a str,
        cwd: &'a std::path::Path,
        options: maho_tools::bash::BashExecOptions,
    ) -> maho_tools::definition::ToolFuture<'a, maho_tools::bash::BashExit> {
        Box::pin(async move {
            let operations = match (self.session)() {
                Some(session) => session
                    .tool_dispatcher()
                    .bash_operations(self.shell_path.as_deref()),
                None => maho_tools::bash::create_local_bash_operations(self.shell_path.as_deref()),
            };
            operations.exec(command, cwd, options).await
        })
    }
}

/// senpi agent-session.ts `_buildRuntime`: the built-in tool options. The extensions' filesystem
/// policies are composed into the file tools (re-read per call, since native extensions may
/// register theirs at bind time), and the tools honour `images.autoResize`, `shellCommandPrefix`
/// and `shellPath` from settings. A cwd-changing switch rebuilds the tools with the same options.
pub fn built_in_tools_options(
    settings: &SettingsManager,
    session: std::sync::Arc<dyn Fn() -> Option<crate::agent_session::AgentSession> + Send + Sync>,
) -> maho_tools::index::ToolsOptions {
    use std::sync::Arc;
    let bash_session = session.clone();
    let filesystem_policy: maho_tools::FilesystemPolicyChecker = Arc::new(move |request| {
        let session = session();
        Box::pin(async move {
            let policies = match session {
                Some(session) => session.filesystem_policies().await,
                None => Vec::new(),
            };
            match maho_tools::filesystem_policy::compose_filesystem_policies(policies) {
                Some(check) => check(request).await,
                None => Ok(maho_tools::FilesystemPolicyDecision::Allow),
            }
        })
    });
    let shell_env: maho_tools::bash::ShellEnvProvider =
        Arc::new(crate::utils::shell::get_shell_env);
    maho_tools::index::ToolsOptions {
        read: maho_tools::read::ReadToolOptions {
            filesystem_policy: Some(filesystem_policy.clone()),
            auto_resize_images: Some(settings.get_image_auto_resize()),
            execution_policy: Some(READ_EXECUTION_POLICY.clone()),
            ..Default::default()
        },
        // The core bash tool's process work goes through the dispatcher's operations: the same local operations
        // under in-process, and the tool child's `user.bash` job under child (senpi#3566 todo 9), while truncation
        // and rendering stay on the server.
        bash: maho_tools::bash::BashToolOptions {
            command_prefix: settings.get_string("shellCommandPrefix"),
            shell_path: settings.get_shell_path(),
            shell_env: Some(shell_env.clone()),
            env_passthrough: settings.get_bash_env_scrub_passthrough(),
            sandbox: Some(settings.get_bash_sandbox()),
            sandbox_agent_dir: Some(std::path::PathBuf::from(settings.agent_dir())),
            operations: Some(Arc::new(DispatcherBashOperations {
                session: bash_session,
                shell_path: settings.get_shell_path(),
            })),
            ..Default::default()
        },
        powershell: maho_tools::bash::BashToolOptions {
            shell_env: Some(shell_env),
            env_passthrough: settings.get_bash_env_scrub_passthrough(),
            ..Default::default()
        },
        write: maho_tools::write::WriteToolOptions {
            filesystem_policy: Some(filesystem_policy.clone()),
            ..Default::default()
        },
        edit: maho_tools::edit::EditToolOptions {
            filesystem_policy: Some(filesystem_policy.clone()),
            ..Default::default()
        },
        grep: maho_tools::grep::index::GrepToolOptions {
            filesystem_policy: Some(filesystem_policy.clone()),
        },
        find: maho_tools::find::FindToolOptions {
            filesystem_policy: Some(filesystem_policy.clone()),
            ..Default::default()
        },
        glob: maho_tools::find::FindToolOptions {
            filesystem_policy: Some(filesystem_policy.clone()),
            ..Default::default()
        },
        ls: maho_tools::ls::LsToolOptions {
            filesystem_policy: Some(filesystem_policy),
            ..Default::default()
        },
    }
}

pub async fn create_agent_session(
    mut options: CreateAgentSessionOptions,
) -> Result<CreateAgentSessionResult, String> {
    use std::{collections::BTreeMap, sync::Arc};
    if options.minimal_resources {
        options.extension_factories.clear();
        options.loaded_extensions = None;
        options.hook_resources.clear();
        options.additional_hook_paths.clear();
        options.system_prompt = None;
        options.append_system_prompt.clear();
        options.context_files.clear();
    }
    let cwd = options.cwd.take().unwrap_or_else(|| {
        std::env::current_dir()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    let agent_dir = options
        .agent_dir
        .take()
        .unwrap_or_else(crate::config::get_agent_dir);
    let runtime = match options.model_runtime.take().or_else(|| {
        options
            .model_registry
            .as_ref()
            .map(|registry| registry.model_runtime.clone())
    }) {
        Some(runtime) => runtime,
        None => ModelRuntime::create_sync(crate::model_runtime::CreateModelRuntimeOptions {
            credentials: options.auth_storage.take(),
            models_path: Some(std::path::Path::new(&agent_dir).join("models.json")),
            auth_path: Some(std::path::Path::new(&agent_dir).join("auth.json")),
            providers: None,
            ..Default::default()
        })
        .map_err(|error| error.to_string())?,
    };
    let registry = options
        .model_registry
        .take()
        .unwrap_or_else(|| ModelRegistry::new(runtime.clone()));
    // senpi sdk.ts `SettingsManager.create(cwd, agentDir)`: `projectTrusted` defaults to true.
    let settings = options.settings_manager.take().unwrap_or_else(|| {
        SettingsManager::create(&cwd, &agent_dir, &crate::config::home_dir(), true)
    });
    runtime.set_settings_manager(&settings);
    let mut manager = options
        .session_manager
        .take()
        .unwrap_or_else(|| SessionManager::create(&cwd, None, None));
    crate::session_cwd::assert_session_cwd_exists(&manager, &cwd)
        .map_err(|error| error.to_string())?;
    let context = manager.current_context();
    let mut model_fallback_message: Option<String> = None;
    let mut model = options.model.take();
    // senpi sdk.ts:284-299: a resumed session's model is restored only when its provider has
    // configured auth; otherwise startup warns and falls back.
    if model.is_none()
        && !context.messages.is_empty()
        && let Some((provider, id)) = context.model.as_ref()
    {
        model = registry
            .find(provider, id)
            .filter(|restored| registry.has_configured_auth(restored));
        if model.is_none() {
            model_fallback_message = Some(format!("Could not restore model {provider}/{id}"));
        }
    }
    if model.is_none() {
        // senpi sdk.ts:333-345: findInitialModel picks the startup model and records its provenance,
        // which session_start carries so an automatic pick (settings, provider default, first
        // available) stays replaceable by a model profile.
        let default_provider = settings.get_string("defaultProvider");
        let default_model_id = settings.get_string("defaultModel");
        let initial = crate::model_resolver::find_initial_model(
            crate::model_resolver::InitialModelOptions {
                cli_provider: None,
                cli_model: None,
                scoped_models: &[],
                is_continuing: !context.messages.is_empty(),
                default_provider: default_provider.as_deref(),
                default_model_id: default_model_id.as_deref(),
                model_thinking_levels: None,
            },
            &runtime,
        )
        .await?;
        model = initial.parsed.model;
        if options.initial_model_provenance.is_none() {
            options.initial_model_provenance =
                Some(crate::model_resolver::initial_model_provenance_str(initial.provenance).to_owned());
        }
        // senpi sdk.ts:322-326: no model leaves a model-less session with the login guidance.
        match &model {
            None => {
                model_fallback_message =
                    Some(crate::auth_guidance::format_no_models_available_message())
            }
            Some(fallback) => {
                if let Some(message) = model_fallback_message.as_mut() {
                    message.push_str(&format!(". Using {}/{}", fallback.provider, fallback.id));
                }
            }
        }
    }
    let has_thinking_entry = manager
        .current_branch()
        .iter()
        .any(|entry| entry["type"] == "thinking_level_change");
    let model_thinking = model
        .as_ref()
        .and_then(|model| {
            settings
                .get_value("modelThinkingLevels")
                .and_then(|levels| levels.get(format!("{}/{}", model.provider, model.id)))
                .and_then(serde_json::Value::as_str)
                .and_then(ModelThinkingLevel::parse)
        });
    let configured_thinking = model_thinking;
    let explicit_thinking = options.thinking_level.is_some() || options.thinking_selection.is_some();
    let mut thinking_level_source = if explicit_thinking { "explicit" } else if has_thinking_entry { "session" } else if model_thinking.is_some() { "global" } else if model.as_ref().and_then(|m| m.default_thinking_level).is_some() { "model-default" } else { "default" };
    let requested_thinking = options
        .thinking_level
        .map(ModelThinkingLevel::from)
        .or_else(|| {
            options
                .thinking_selection
                .as_ref()
                .map(|selection| selection.level)
        })
        .or_else(|| {
            has_thinking_entry
                .then(|| ModelThinkingLevel::parse(&context.thinking_level))
                .flatten()
        })
        .or(configured_thinking)
        .or_else(|| model.as_ref().and_then(|m| m.default_thinking_level))
        .or_else(|| settings.get_string("defaultThinkingLevel").as_deref().and_then(ModelThinkingLevel::parse))
        .or(Some(ModelThinkingLevel::Medium));
    // senpi sdk.ts:360-365: without a model the level is "off" (`clamp_thinking_level_to_model(_, None)`).
    let thinking_level = match requested_thinking {
        Some(ModelThinkingLevel::Off) => ModelThinkingLevel::Off,
        Some(level) => clamp_thinking_level_to_model(
            serde_json::from_value(serde_json::Value::from(level.as_str())).ok(),
            model.as_ref(),
        ),
        None => clamp_thinking_level_to_model(None, model.as_ref()),
    };
    if requested_thinking != Some(thinking_level) && thinking_level_source != "default" { thinking_level_source = "clamped"; }
    let mut thinking_selection = options
        .thinking_selection
        .take()
        .or_else(|| {
            options.thinking_level.map(|_| ThinkingSelection {
                level: thinking_level,
                source: maho_ai::types::ThinkingSelectionSource::Explicit,
                legacy_variant_id: None,
            })
        })
        .or_else(|| {
            has_thinking_entry
                .then(|| context.thinking_selection.clone())
                .flatten()
                .and_then(|value| serde_json::from_value(value).ok())
        });
    if thinking_selection.is_none() && !has_thinking_entry && (configured_thinking.is_some() || settings.get_string("defaultThinkingLevel").is_some()) {
        thinking_selection = Some(ThinkingSelection {
            level: thinking_level,
            source: maho_ai::types::ThinkingSelectionSource::Explicit,
            legacy_variant_id: None,
        });
    }
    if let Some(selection) = thinking_selection.as_mut() {
        selection.level = thinking_level;
    }
    let session_accessor = Arc::new(std::sync::OnceLock::<
        Arc<dyn Fn() -> Option<crate::agent_session::AgentSession> + Send + Sync>,
    >::new());
    // senpi `_buildRuntime`: the built-in tools carry the composed filesystem policy and settings.
    let policy_accessor = session_accessor.clone();
    let options_session: Arc<dyn Fn() -> Option<crate::agent_session::AgentSession> + Send + Sync> =
        Arc::new(move || policy_accessor.get().and_then(|accessor| accessor()));
    let mut definitions = maho_tools::index::create_all_tool_definitions(
        std::path::Path::new(&cwd),
        built_in_tools_options(&settings, options_session),
    );
    for definition in &options.custom_tools {
        definitions.insert(definition.name.clone(), definition.clone());
    }
    let registered_definitions = definitions.clone();
    let context_accessor = session_accessor.clone();
    let context_factory: maho_tools::tool_definition_wrapper::ContextFactory =
        Arc::new(move |_, _| {
            let session = context_accessor
                .get()
                .expect("SDK session initialized before tool execution")(
            )
            .expect("SDK session retained during tool execution");
            Arc::new(extension_context::create(&session))
        });
    let mut base_tools = BTreeMap::new();
    for (name, definition) in definitions {
        let tool = maho_tools::tool_definition_wrapper::wrap_tool_definition(
            definition,
            Some(context_factory.clone()),
        );
        base_tools.insert(name, tool);
    }
    let configured_defaults: Option<Vec<String>> = settings
        .get_value("defaultTools")
        .and_then(serde_json::Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect()
        });
    let default_tool_names = if options.tools.is_none() && options.no_tools.is_none() {
        configured_defaults.clone()
    } else {
        None
    };
    let allowed_tool_names = options
        .tools
        .clone()
        .or_else(|| (options.no_tools == Some(NoToolsMode::All)).then(Vec::new));
    let mut selected = options.tools.clone().unwrap_or_else(|| {
        if options.no_tools.is_some() {
            Vec::new()
        } else {
            configured_defaults.clone().unwrap_or_else(|| {
                vec![
                    "read".to_owned(),
                    "bash".to_owned(),
                    "edit".to_owned(),
                    "write".to_owned(),
                    "grep".to_owned(),
                ]
            })
        }
    });
    if options.tools.is_none() && options.no_tools != Some(NoToolsMode::All) {
        for tool in &options.custom_tools {
            if matches!(
                tool.exposure.unwrap_or(maho_ext_api::ToolExposure::Direct),
                maho_ext_api::ToolExposure::Direct | maho_ext_api::ToolExposure::Eval
            ) && !selected.contains(&tool.name)
            {
                selected.push(tool.name.clone());
            }
        }
    }
    if let Some(excluded) = &options.exclude_tools {
        selected.retain(|name| !excluded.contains(name));
    }
    let active_tools = selected
        .iter()
        .filter_map(|name| base_tools.get(name).cloned())
        .collect();
    let runtime_for_stream = runtime.clone();
    let runtime_for_auth = runtime.clone();
    if context.messages.is_empty()
        && let Some(model) = &model
    {
        manager.append_model_change(&model.provider, &model.id, None, None);
    }
    if context.messages.is_empty() || !has_thinking_entry {
        manager.append_thinking_level_change_with_context(
            thinking_level.as_str(),
            thinking_selection.as_ref().map(|selection| {
                serde_json::to_value(selection).expect("thinking selection serializes")
            }),
            Some(thinking_level_source),
            model.as_ref(),
        );
    }
    let resumed = !context.messages.is_empty();
    let messages = context
        .messages
        .into_iter()
        .map(crate::agent_session::session_message_from_value)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    let provider_retry_settings = settings.get_provider_retry_settings();
    let agent = maho_agent::agent::Agent::new(maho_agent::agent::AgentOptions {
        initial_state: Some(maho_agent::agent::PartialAgentState {
            model,
            thinking_level: Some(thinking_level),
            thinking_selection,
            messages: Some(messages),
            tools: Some(active_tools),
            ..Default::default()
        }),
        stream_fn: Some(Arc::new(move |model, context, options| {
            runtime_for_stream.stream_simple(model, context, options.map(|options| options.simple))
        })),
        get_api_key: Some(Arc::new(move |provider| {
            let runtime = runtime_for_auth.clone();
            Box::pin(async move { api_key_from_auth(runtime.get_auth(&provider).await) })
        })),
        // senpi sdk.ts hands the Agent convertToLlmForTransport: custom, bash-execution and
        // summary messages become user turns (the agent-core default drops them, so a hidden
        // extension wake after an assistant reply left the provider request ending on that reply),
        // and the transport image policy applies. blockImages and maxHistoricalImages are read per
        // request so a mid-session settings change takes effect.
        convert_to_llm: Some({
            let settings_accessor = session_accessor.clone();
            Arc::new(move |messages: Vec<maho_agent::types::AgentMessage>| {
                let (block_images, max_historical_images) = settings_accessor
                    .get()
                    .and_then(|accessor| accessor())
                    .map(|session| {
                        session.with_settings_manager(|manager| {
                            (
                                manager.get_block_images(),
                                manager.get_max_historical_images(),
                            )
                        })
                    })
                    .unwrap_or((false, None));
                Box::pin(async move {
                    let options = crate::messages::TransportConvertOptions {
                        elide: crate::messages::ElideOldImagesOptions {
                            budget_bytes: Some(crate::messages::TRANSPORT_IMAGE_BUDGET_BYTES),
                            always_keep_newest: Some(1),
                            max_historical_images,
                        },
                        block_images,
                    };
                    // The typed agent messages and the JSON session messages share one serde shape;
                    // a message that cannot cross it keeps the typed conversion (without the image policy)
                    // rather than losing conversation content.
                    let values = messages
                        .iter()
                        .map(crate::agent_session::session_message_to_value)
                        .collect::<Result<Vec<_>, _>>();
                    let converted = values.and_then(|values| {
                        crate::messages::convert_to_llm_for_transport(&values, &options)
                            .into_iter()
                            .map(serde_json::from_value::<maho_ai::types::Message>)
                            .collect::<Result<Vec<_>, _>>()
                    });
                    converted
                        .unwrap_or_else(|_| maho_agent::harness::messages::convert_to_llm(messages))
                })
            })
        }),
        steering_mode: Some(settings.get_steering_mode()),
        follow_up_mode: Some(settings.get_follow_up_mode()),
        transport: Some(settings.get_transport()),
        thinking_budgets: settings.get_thinking_budgets(),
        timeout_ms: settings.get_agent_stream_idle_timeout_ms()?,
        stream_start_timeout_ms: settings.get_agent_stream_start_timeout_ms()?,
        max_retry_delay_ms: Some(provider_retry_settings.max_retry_delay_ms),
        ..Default::default()
    });
    let session_start_event = match options.initial_model_provenance.take() {
        Some(provenance) => {
            let mut event =
                options
                    .session_start_event
                    .take()
                    .unwrap_or(maho_ext_api::SessionStartEvent {
                        reason: maho_ext_api::SessionReason::Startup,
                        initial_model_provenance: None,
                        previous_session_file: None,
                        attended: None,
                    });
            event.initial_model_provenance = Some(provenance);
            Some(event)
        }
        None => options.session_start_event.take(),
    };
    // senpi reads `resourceLoader.getExtensions().runtime.flagValues` in the AgentSession constructor,
    // so CLI-applied extension flags (`--no-model-fallback`, `--no-ask-user`) reach the settings overrides.
    let flag_values = options
        .loaded_extensions
        .as_ref()
        .map(|loaded| loaded.runtime.flag_values())
        .unwrap_or_default();
    let session = crate::agent_session::AgentSession::new_with_tool_runtime(
        crate::agent_session::AgentSessionConfig {
            agent,
            session_manager: manager,
            settings_manager: settings,
            cwd: cwd.clone(),
            agent_dir: Some(agent_dir.clone()),
            fallback_now: None,
            retry_random: None,
            scoped_models: options.scoped_models,
            favorite_models: options.favorite_models,
            flag_values,
            custom_tools: options.custom_tools,
            model_runtime: Some(runtime),
            model_registry: Some(registry),
            // senpi sdk.ts:425 hands the Agent its own `streamFn` closure, so `agent.streamFunction ===
            // streamSimple` is false for SDK sessions (agent-session.ts:1243).
            uses_default_stream_function: Some(false),
            initial_active_tool_names: Some(selected),
            default_tool_names,
            eval_only_tool_names: None,
            allowed_tool_names,
            excluded_tool_names: options.exclude_tools,
            base_tools_override: Some(base_tools.clone()),
            session_start_event,
            auto_title_sessions: options.auto_title_sessions,
        },
        options.tool_runtime,
    )
    .map_err(|error| error.to_string())?;
    // senpi `new AgentSession({ promptSurface, browserEngine })`: set before the first prompt is built.
    if let Some(surface) = options.prompt_surface {
        session.set_prompt_surface(surface);
    }
    if let Some(engine) = options.browser_engine.clone() {
        session.set_browser_engine(engine);
    }
    // `modelRuntime.setSettingsManager(settings)`: provider concurrency limits follow live settings.
    session
        .with_settings_manager(|settings| session.model_runtime().set_settings_manager(settings));
    if options.minimal_resources {
        session.set_context_files_enabled(false);
    }
    // senpi sdk.ts hands the Agent its settings-aware `streamFn` up front, so attribution headers,
    // provider timeouts and the request service tier apply with or without an extension runner.
    session.install_default_stream_function();
    assert!(
        session_accessor.set(session.weak_accessor()).is_ok(),
        "SDK context initialized once"
    );
    for (name, definition) in registered_definitions {
        if let Some(tool) = base_tools.get(&name) {
            session.register_tool_definition(
                definition,
                maho_ext_api::SourceInfo {
                    source: "sdk".to_owned(),
                    ..Default::default()
                },
                tool.clone(),
            );
        }
    }
    if !options.minimal_resources {
        let mut inputs = options.resource_inputs;
        inputs
            .additional_hook_paths
            .extend(options.additional_hook_paths);
        inputs.hook_resources.extend(options.hook_resources);
        session.set_resource_loader_inputs(inputs);
        if let Err(error) = session.load_resources().await {
            session.dispose().await;
            return Err(error);
        }
    }
    session.set_system_prompt_sources(options.system_prompt, options.append_system_prompt);
    if let Err(error) = session.admit_sdk_model(resumed) {
        session.dispose().await;
        return Err(error);
    }
    if options.loaded_extensions.is_some() || !options.extension_factories.is_empty() {
        let mut construction = PendingSessionConstruction {
            session: Some(session.clone()),
            runtime: tokio::runtime::Handle::current(),
        };
        let context = extension_context::create(&session);
        let runner = if let Some(loaded) = options.loaded_extensions {
            maho_ext_host::runner::ExtensionRunner::from_loaded_extensions(
                loaded,
                options.extension_factories,
                context,
                options.session_profile.clone(),
            )
        } else {
            match maho_ext_host::runner::ExtensionRunner::from_async_factories(
                options.extension_factories,
                context,
                options.session_profile.clone(),
            )
            .await
            {
                Ok(runner) => runner,
                Err(error) => {
                    session.dispose().await;
                    drop(construction.session.take());
                    return Err(error.to_string());
                }
            }
        };
        session.set_extension_runner(runner).await;
        if !options.defer_extension_start {
            let mut bindings = options
                .extension_bindings
                .as_ref()
                .map(|factory| factory(&session))
                .unwrap_or_default();
            if bindings.ui_context.is_none() {
                bindings.ui_context = options.extension_ui.clone();
            }
            session.bind_extensions(bindings).await;
        }
        drop(construction.session.take());
    }
    session.set_cursor_exec_bridge_factory(Arc::new({
        let session_ref: crate::cursor_exec_bridge_session::SessionRef = session.weak_accessor();
        move |run_signal| {
            let Some(session) = session_ref() else {
                return Arc::new(Default::default());
            };
            Arc::new(
                crate::cursor_exec_bridge_session::create_session_cursor_exec_bridge(
                    session_ref.clone(),
                    session.agent().clone(),
                    run_signal,
                ),
            )
        }
    }));
    Ok(CreateAgentSessionResult {
        session,
        model_fallback_message,
    })
}

/// The default global config directory.
pub fn get_default_agent_dir() -> String {
    crate::config::get_agent_dir()
}

/// Clamp a requested thinking level to what the model supports, preferring lower levels.
pub fn clamp_thinking_level_to_model(
    level: Option<ThinkingLevel>,
    model: Option<&Model>,
) -> ModelThinkingLevel {
    let Some(model) = model else {
        return ModelThinkingLevel::Off;
    };
    if !model.reasoning {
        return ModelThinkingLevel::Off;
    }
    let requested = level.map_or(ModelThinkingLevel::Off, thinking_to_model_level);
    let available = crate::thinking_levels::get_supported_thinking_levels(model);
    if available.contains(&requested) {
        return requested;
    }
    let ordered = ModelThinkingLevel::ALL;
    let requested_index = ordered
        .iter()
        .position(|candidate| *candidate == requested)
        .unwrap_or(0);
    for candidate in ordered[..requested_index].iter().rev() {
        if available.contains(candidate) {
            return *candidate;
        }
    }
    for candidate in &ordered[requested_index + 1..] {
        if available.contains(candidate) {
            return *candidate;
        }
    }
    available
        .first()
        .copied()
        .unwrap_or(ModelThinkingLevel::Off)
}

fn api_key_from_auth(
    result: Result<Option<maho_ai::models::AuthResolution>, maho_ai::models::ModelsError>,
) -> Result<Option<String>, maho_ai::utils::event_stream::StreamError> {
    use maho_ai::models::ModelsErrorCode;
    use maho_ai::utils::event_stream::StreamError;

    result
        .map(|auth| auth.and_then(|auth| auth.auth.api_key))
        .map_err(|error| match error.code {
            ModelsErrorCode::OAuth => StreamError::oauth_refresh_unavailable(error.message),
            // Keep the exact consent text and `provider_consent_required` code (never rewritten
            // to `oauth_refresh_unavailable`).
            ModelsErrorCode::ConsentRequired => StreamError::provider_failure(
                error.message,
                maho_ai::types::ProviderDiagnostic {
                    category: maho_ai::types::ProviderDiagnosticCategory::Unknown,
                    http_status: None,
                    code: Some(error.code.as_str().to_owned()),
                    evidence: maho_ai::types::ProviderDiagnosticEvidence::StructuredCode,
                    resets_at: None,
                },
            ),
            ModelsErrorCode::ModelSource
            | ModelsErrorCode::ModelValidation
            | ModelsErrorCode::Provider
            | ModelsErrorCode::Stream
            | ModelsErrorCode::Auth => StreamError::new(error.message),
        })
}

fn thinking_to_model_level(level: ThinkingLevel) -> ModelThinkingLevel {
    match level {
        ThinkingLevel::Minimal => ModelThinkingLevel::Minimal,
        ThinkingLevel::Low => ModelThinkingLevel::Low,
        ThinkingLevel::Medium => ModelThinkingLevel::Medium,
        ThinkingLevel::High => ModelThinkingLevel::High,
        ThinkingLevel::Xhigh => ModelThinkingLevel::Xhigh,
        ThinkingLevel::Max => ModelThinkingLevel::Max,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_key_is_preserved_when_auth_lookup_succeeds() {
        // Given a successful auth lookup with a key.
        let resolution = maho_ai::models::AuthResolution {
            ambient: false,
            auth: maho_ai::models::ProviderAuthResult {
                api_key: Some("request-key".to_owned()),
                ..Default::default()
            },
            ..Default::default()
        };
        // When adapting the lookup for GetApiKey.
        let result = api_key_from_auth(Ok(Some(resolution)));
        // Then the key reaches the caller.
        assert_eq!(result, Ok(Some("request-key".to_owned())));
    }

    #[test]
    fn api_key_is_absent_when_auth_lookup_has_no_key() {
        // Given either an unconfigured provider or auth without an API key.
        for resolution in [None, Some(maho_ai::models::AuthResolution::default())] {
            // When adapting the successful lookup for GetApiKey.
            let result = api_key_from_auth(Ok(resolution));
            // Then absence remains a successful result.
            assert_eq!(result, Ok(None));
        }
    }

    #[test]
    fn consent_failure_keeps_message_and_code_through_api_key_lookup() {
        use maho_ai::models::{ModelsError, ModelsErrorCode};
        use maho_ai::utils::event_stream::StreamErrorKind;

        let error = api_key_from_auth(Err(ModelsError::new(ModelsErrorCode::ConsentRequired, "consent text")))
            .expect_err("consent error");
        assert_eq!(error.message, "consent text");
        let StreamErrorKind::ProviderFailure(diagnostic) = error.kind else {
            panic!("consent error must not become oauth_refresh_unavailable");
        };
        assert_eq!(diagnostic.code.as_deref(), Some("provider_consent_required"));
    }

    #[test]
    fn auth_failures_are_propagated_when_api_key_lookup_fails() {
        use maho_ai::models::{ModelsError, ModelsErrorCode};
        use maho_ai::utils::event_stream::{StreamError, StreamErrorKind};

        // Given each possible lookup error classification.
        for (code, kind) in [
            (
                ModelsErrorCode::OAuth,
                StreamErrorKind::OAuthRefreshUnavailable,
            ),
            (ModelsErrorCode::Auth, StreamErrorKind::Other),
            (ModelsErrorCode::ModelSource, StreamErrorKind::Other),
            (ModelsErrorCode::ModelValidation, StreamErrorKind::Other),
            (ModelsErrorCode::Provider, StreamErrorKind::Other),
            (ModelsErrorCode::Stream, StreamErrorKind::Other),
        ] {
            // When adapting the failed lookup for GetApiKey.
            let result = api_key_from_auth(Err(ModelsError::new(code, "lookup-failure")));
            // Then the failure and its OAuth classification survive the boundary.
            assert_eq!(
                result,
                Err(StreamError {
                    message: "lookup-failure".to_owned(),
                    kind
                })
            );
        }
    }

    #[test]
    fn a_non_reasoning_model_clamps_to_off() {
        let mut model = test_model();
        model.reasoning = false;
        assert_eq!(
            clamp_thinking_level_to_model(Some(ThinkingLevel::High), Some(&model)),
            ModelThinkingLevel::Off
        );
    }

    #[test]
    fn an_unsupported_level_falls_back_to_a_lower_supported_one() {
        let model = test_model();
        assert_eq!(
            clamp_thinking_level_to_model(Some(ThinkingLevel::Xhigh), Some(&model)),
            ModelThinkingLevel::High
        );
    }

    #[test]
    fn a_missing_model_clamps_to_off() {
        assert_eq!(
            clamp_thinking_level_to_model(Some(ThinkingLevel::High), None),
            ModelThinkingLevel::Off
        );
    }

    #[tokio::test]
    async fn sdk_records_initial_model_and_thinking_for_resume() {
        let dir = tempfile::tempdir().expect("directory");
        let cwd = dir.path().to_string_lossy().into_owned();
        let created = create_agent_session(CreateAgentSessionOptions {
            cwd: Some(cwd.clone()),
            agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
            model: Some(test_model()),
            thinking_level: Some(ThinkingLevel::Low),
            session_manager: Some(SessionManager::in_memory(&cwd, None, None)),
            tools: Some(Vec::new()),
            ..Default::default()
        })
        .await
        .expect("SDK session");
        let context = created
            .session
            .with_session_manager(|manager| manager.current_context());
        let entries = created
            .session
            .with_session_manager(|manager| manager.entries());
        created.session.dispose().await;
        assert_eq!(
            context.model,
            Some(("faux".to_owned(), "faux-1".to_owned()))
        );
        assert_eq!(context.thinking_level, "low");
        assert_eq!(
            context.thinking_selection,
            Some(serde_json::json!({"level":"low","source":"explicit"}))
        );
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry["type"] == "model_change")
                .count(),
            1
        );
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry["type"] == "thinking_level_change")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn sdk_records_only_missing_thinking_metadata_for_existing_conversations() {
        for saved_thinking in [false, true] {
            let dir = tempfile::tempdir().expect("directory");
            let cwd = dir.path().to_string_lossy().into_owned();
            let mut manager = SessionManager::in_memory(&cwd, None, None);
            manager.append_model_change("faux", "saved-model", None, None);
            if saved_thinking {
                manager.append_thinking_level_change("high", None);
            }
            manager.append_message(
                serde_json::json!({"role":"user","content":"existing task","timestamp":0}),
            );
            let created = create_agent_session(CreateAgentSessionOptions {
                cwd: Some(cwd.clone()),
                agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
                model: Some(test_model()),
                thinking_level: Some(ThinkingLevel::Low),
                session_manager: Some(manager),
                tools: Some(Vec::new()),
                ..Default::default()
            })
            .await
            .expect("existing SDK conversation");
            let context = created
                .session
                .with_session_manager(|manager| manager.current_context());
            let entries = created
                .session
                .with_session_manager(|manager| manager.entries());
            let effective = created.session.thinking_level();
            created.session.dispose().await;
            assert_eq!(effective, ModelThinkingLevel::Low);
            assert_eq!(
                context.model,
                Some(("faux".to_owned(), "saved-model".to_owned()))
            );
            assert_eq!(
                context.thinking_level,
                if saved_thinking { "high" } else { "low" }
            );
            assert_eq!(
                entries
                    .iter()
                    .filter(|entry| entry["type"] == "model_change")
                    .count(),
                1
            );
            assert_eq!(
                entries
                    .iter()
                    .filter(|entry| entry["type"] == "thinking_level_change")
                    .count(),
                1
            );
            assert_eq!(context.messages.len(), 1);
        }
    }

    #[tokio::test]
    async fn sdk_rejects_a_fresh_model_without_startup_context_budget() {
        let dir = tempfile::tempdir().expect("directory");
        let cwd = dir.path().to_string_lossy().into_owned();
        let mut model = test_model();
        model.context_window = 1;
        model.max_tokens = 1;
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            create_agent_session(CreateAgentSessionOptions {
                cwd: Some(cwd.clone()),
                agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
                model: Some(model),
                session_manager: Some(SessionManager::in_memory(&cwd, None, None)),
                tools: Some(Vec::new()),
                ..Default::default()
            }),
        )
        .await;
        if let Ok(Ok(created)) = &result {
            created.session.dispose().await;
        }
        assert!(
            result.expect("bounded SDK startup admission").is_err(),
            "SDK construction must reject an unusable fresh model before any prompt"
        );
    }

    #[tokio::test]
    async fn sdk_rejects_unrepairable_restored_context_when_compaction_is_disabled() {
        let dir = tempfile::tempdir().expect("directory");
        let cwd = dir.path().to_string_lossy().into_owned();
        let agent_dir = dir.path().join("agent").to_string_lossy().into_owned();
        let mut settings = SettingsManager::create(&cwd, &agent_dir, &cwd, false);
        settings.apply_overrides(&serde_json::Map::from_iter([(
            "compaction".to_owned(),
            serde_json::json!({"enabled":false}),
        )]));
        let mut manager = SessionManager::in_memory(&cwd, None, None);
        manager.append_message(serde_json::json!({"role":"user","content":"restored context ".repeat(10000),"timestamp":0}));
        let mut model = test_model();
        model.context_window = 16000;
        model.max_tokens = 1024;
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            create_agent_session(CreateAgentSessionOptions {
                cwd: Some(cwd),
                agent_dir: Some(agent_dir),
                model: Some(model),
                session_manager: Some(manager),
                settings_manager: Some(settings),
                tools: Some(Vec::new()),
                ..Default::default()
            }),
        )
        .await;
        if let Ok(Ok(created)) = &result {
            created.session.dispose().await;
        }
        assert!(
            result.expect("bounded SDK resume admission").is_err(),
            "disabled compaction cannot repair an oversized restored conversation"
        );
    }

    #[tokio::test]
    async fn sdk_reduces_oversized_resume_without_losing_transcript_messages() {
        let dir = tempfile::tempdir().expect("directory");
        let cwd = dir.path().to_string_lossy().into_owned();
        let mut manager = SessionManager::in_memory(&cwd, None, None);
        for index in 0..20 {
            manager.append_message(serde_json::json!({"role":"user",
                "content":[{"type":"text","text":"restored context ".repeat(2500)}],"timestamp":index}));
        }
        let before = manager
            .entries()
            .into_iter()
            .filter(|entry| entry["type"] == "message")
            .collect::<Vec<_>>();
        let live_before: u64 = manager
            .current_context()
            .messages
            .iter()
            .map(crate::compaction::compaction::estimate_tokens)
            .sum();
        let mut model = test_model();
        model.context_window = 128000;
        model.max_tokens = 4096;
        assert!(
            live_before > model.context_window,
            "fixture crosses resume reduction boundary"
        );
        let created = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            create_agent_session(CreateAgentSessionOptions {
                cwd: Some(cwd.clone()),
                agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
                model: Some(model.clone()),
                session_manager: Some(manager),
                tools: Some(Vec::new()),
                ..Default::default()
            }),
        )
        .await
        .expect("bounded oversized SDK resume")
        .expect("repairable resume");
        let context = created
            .session
            .with_session_manager(|manager| manager.current_context());
        let entries = created
            .session
            .with_session_manager(|manager| manager.entries());
        let live_after: u64 = context
            .messages
            .iter()
            .map(crate::compaction::compaction::estimate_tokens)
            .sum();
        let admission = created.session.assert_model_usable(&model, live_after);
        created.session.dispose().await;
        assert!(live_after < live_before);
        assert!(live_after <= model.context_window - model.max_tokens);
        assert!(
            admission.is_ok(),
            "reduced resume must fit an ordinary request"
        );
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry["type"] == "message")
                .cloned()
                .collect::<Vec<_>>(),
            before
        );
        let reductions = entries
            .iter()
            .filter(|entry| entry["type"] == "compaction")
            .collect::<Vec<_>>();
        assert_eq!(reductions.len(), 1);
        assert_eq!(reductions[0]["details"]["origin"], "resume-admission");
    }

    #[tokio::test]
    async fn sdk_restores_saved_thinking_without_overriding_explicit_launch_level() {
        let dir = tempfile::tempdir().expect("directory");
        for explicit in [None, Some(ThinkingLevel::Low)] {
            let mut manager = SessionManager::in_memory(&dir.path().to_string_lossy(), None, None);
            manager.append_thinking_level_change(
                "high",
                Some(serde_json::json!({"level":"high","source":"explicit"})),
            );
            let created = create_agent_session(CreateAgentSessionOptions {
                cwd: Some(dir.path().to_string_lossy().into_owned()),
                agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
                model: Some(test_model()),
                session_manager: Some(manager),
                thinking_level: explicit,
                tools: Some(Vec::new()),
                ..Default::default()
            })
            .await
            .expect("restored SDK session");
            let expected = if explicit.is_some() {
                ModelThinkingLevel::Low
            } else {
                ModelThinkingLevel::High
            };
            let level = created.session.thinking_level();
            let selection = created.session.thinking_selection();
            created.session.dispose().await;
            assert_eq!(level, expected);
            assert_eq!(selection.expect("provenance").level, expected);
        }
    }

    #[tokio::test]
    async fn sdk_uses_remembered_thinking_before_configured_and_shipped_defaults() {
        let dir = tempfile::tempdir().expect("directory");
        for (configured, remembered, expected, provenance) in [
            (None, None, ModelThinkingLevel::Medium, false),
            (Some("high"), None, ModelThinkingLevel::High, true),
            (Some("high"), Some("low"), ModelThinkingLevel::Low, true),
            (Some("high"), Some("off"), ModelThinkingLevel::Off, true),
        ] {
            let cwd = dir.path().to_string_lossy().into_owned();
            let agent_dir = dir.path().join("agent").to_string_lossy().into_owned();
            let mut settings = SettingsManager::create(&cwd, &agent_dir, &cwd, false);
            let mut overrides = serde_json::Map::new();
            if let Some(level) = configured {
                overrides.insert("defaultThinkingLevel".to_owned(), level.into());
            }
            if let Some(level) = remembered {
                overrides.insert(
                    "modelThinkingLevels".to_owned(),
                    serde_json::json!({"faux/faux-1":level}),
                );
            }
            settings.apply_overrides(&overrides);
            let created = create_agent_session(CreateAgentSessionOptions {
                cwd: Some(cwd.clone()),
                agent_dir: Some(agent_dir),
                model: Some(test_model()),
                settings_manager: Some(settings),
                session_manager: Some(SessionManager::in_memory(&cwd, None, None)),
                tools: Some(Vec::new()),
                ..Default::default()
            })
            .await
            .expect("SDK session");
            let level = created.session.thinking_level();
            let has_selection = created.session.thinking_selection().is_some();
            created.session.dispose().await;
            assert_eq!(level, expected);
            assert_eq!(has_selection, provenance);
        }
    }

    #[tokio::test]
    async fn sdk_applies_loaded_extension_flag_values_to_session_settings() {
        let dir = tempfile::tempdir().expect("directory");
        let loaded = maho_ext_host::loader::load_extensions_async(
            Vec::new(),
            dir.path(),
            Default::default(),
        )
        .await;
        loaded
            .runtime
            .set_flag("no-model-fallback", maho_ext_api::FlagValue::Boolean(true));
        loaded
            .runtime
            .set_flag("no-ask-user", maho_ext_api::FlagValue::Boolean(true));
        let created = create_agent_session(CreateAgentSessionOptions {
            cwd: Some(dir.path().to_string_lossy().into_owned()),
            agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
            model: Some(test_model()),
            session_manager: Some(SessionManager::in_memory(
                &dir.path().to_string_lossy(),
                None,
                None,
            )),
            tools: Some(Vec::new()),
            loaded_extensions: Some(loaded),
            defer_extension_start: true,
            ..Default::default()
        })
        .await
        .expect("SDK session");
        let (fallback, ask_user) = created.session.with_settings_manager(|manager| {
            (
                manager
                    .get_value("retry")
                    .and_then(|retry| retry.get("modelFallback"))
                    .cloned(),
                manager
                    .get_value("askUser")
                    .and_then(|ask| ask.get("enabled"))
                    .cloned(),
            )
        });
        created.session.dispose().await;
        assert_eq!(fallback, Some(serde_json::Value::Bool(false)));
        assert_eq!(ask_user, Some(serde_json::Value::Bool(false)));
    }

    #[tokio::test]
    async fn sdk_creates_session_with_working_builtin_tool() {
        let dir = tempfile::tempdir().expect("directory");
        let created = create_agent_session(CreateAgentSessionOptions {
            cwd: Some(dir.path().to_string_lossy().into_owned()),
            agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
            model: Some(test_model()),
            session_manager: Some(SessionManager::in_memory(
                &dir.path().to_string_lossy(),
                None,
                None,
            )),
            tools: Some(vec!["bash".to_owned()]),
            ..Default::default()
        })
        .await
        .expect("SDK session");
        let tools = created.session.get_active_tool_names();
        let result = created
            .session
            .execute_tool(
                "bash",
                serde_json::json!({"command":"printf sdk"}),
                Default::default(),
            )
            .await;
        created.session.dispose().await;
        assert_eq!(tools, vec!["bash"]);
        let result = result.expect("tool");
        assert!(maho_ai::utils::text::content_text(&result.content, "").contains("sdk"));
    }

    #[tokio::test]
    async fn sdk_bash_tool_honours_shell_command_prefix_setting() {
        let dir = tempfile::tempdir().expect("directory");
        let settings = crate::settings_manager::SettingsManager::in_memory(
            serde_json::Map::from_iter([(
                "shellCommandPrefix".to_owned(),
                serde_json::json!("SDK_PREFIX=prefixed"),
            )]),
            false,
        );
        let created = create_agent_session(CreateAgentSessionOptions {
            cwd: Some(dir.path().to_string_lossy().into_owned()),
            agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
            model: Some(test_model()),
            session_manager: Some(SessionManager::in_memory(
                &dir.path().to_string_lossy(),
                None,
                None,
            )),
            settings_manager: Some(settings),
            tools: Some(vec!["bash".to_owned()]),
            ..Default::default()
        })
        .await
        .expect("SDK session");
        let result = created
            .session
            .execute_tool(
                "bash",
                serde_json::json!({"command":"printf \"$SDK_PREFIX\""}),
                Default::default(),
            )
            .await;
        created.session.dispose().await;
        assert!(
            maho_ai::utils::text::content_text(&result.expect("tool").content, "")
                .contains("prefixed")
        );
    }

    #[tokio::test]
    async fn sdk_agent_projects_custom_messages_into_provider_user_turns() {
        use maho_agent::harness::messages::{CustomMessage, CustomMessageContent};
        use maho_agent::types::{AgentContext, AgentMessage, CustomAgentMessage};
        use maho_ai::types::{Message, UserContent, UserMessage};
        let dir = tempfile::tempdir().expect("directory");
        let cwd = dir.path().to_string_lossy().into_owned();
        let created = create_agent_session(CreateAgentSessionOptions {
            cwd: Some(cwd.clone()),
            agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
            model: Some(test_model()),
            session_manager: Some(SessionManager::in_memory(&cwd, None, None)),
            tools: Some(Vec::new()),
            ..Default::default()
        })
        .await
        .expect("session");
        let messages = vec![
            AgentMessage::Llm(Message::User(UserMessage {
                content: UserContent::Text("hi".into()),
                timestamp: 1,
            })),
            AgentMessage::Custom(CustomAgentMessage::Custom(CustomMessage {
                role: "custom".into(),
                custom_type: "omo-senpi:wake".into(),
                content: CustomMessageContent::Text("Continue".into()),
                display: false,
                details: None,
                timestamp: 2,
            })),
        ];
        let context = created
            .session
            .agent()
            .build_provider_context(
                &AgentContext {
                    declared_tools: None,
                    system_prompt: String::new(),
                    messages,
                    tools: None,
                },
                None,
            )
            .await;
        created.session.dispose().await;
        assert_eq!(
            context.messages.len(),
            2,
            "the hidden custom message must not be dropped"
        );
        let Some(Message::User(last)) = context.messages.last() else {
            panic!("custom message must reach the provider as a user turn")
        };
        let UserContent::Blocks(blocks) = &last.content else {
            panic!("custom text projects to content blocks")
        };
        assert_eq!(maho_ai::utils::text::content_text(blocks, ""), "Continue");
    }

    #[tokio::test]
    async fn sdk_agent_receives_settings_driven_stream_options() {
        let dir = tempfile::tempdir().expect("directory");
        let cwd = dir.path().to_string_lossy().into_owned();
        let mut global = crate::settings_manager::Settings::new();
        global.insert("httpIdleTimeoutMs".to_owned(), serde_json::json!(60_000));
        global.insert("followUpMode".to_owned(), serde_json::json!("all"));
        let created = create_agent_session(CreateAgentSessionOptions {
            cwd: Some(cwd.clone()),
            agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
            model: Some(test_model()),
            session_manager: Some(SessionManager::in_memory(&cwd, None, None)),
            settings_manager: Some(SettingsManager::in_memory(global, false)),
            tools: Some(Vec::new()),
            ..Default::default()
        })
        .await
        .expect("session");
        let agent = created.session.agent().clone();
        created.session.dispose().await;
        assert_eq!(
            (agent.timeout_ms(), agent.stream_start_timeout_ms()),
            (Some(60_000), Some(60_000))
        );
        assert_eq!(agent.max_retry_delay_ms(), Some(60_000));
        assert_eq!(agent.transport(), Some(maho_ai::types::Transport::Auto));
        assert_eq!(agent.steering_mode(), maho_agent::types::QueueMode::All);
        assert_eq!(agent.follow_up_mode(), maho_agent::types::QueueMode::All);
    }

    #[tokio::test]
    async fn sdk_agent_applies_the_block_images_setting_at_the_transport_boundary() {
        use maho_agent::types::{AgentContext, AgentMessage};
        use maho_ai::types::{ContentBlock, ImageContent, Message, UserContent, UserMessage};
        let dir = tempfile::tempdir().expect("directory");
        let cwd = dir.path().to_string_lossy().into_owned();
        let agent_dir = dir.path().join("agent").to_string_lossy().into_owned();
        let mut settings = SettingsManager::create(&cwd, &agent_dir, &cwd, false);
        settings.apply_overrides(&serde_json::Map::from_iter([(
            "images".to_owned(),
            serde_json::json!({"blockImages":true}),
        )]));
        let created = create_agent_session(CreateAgentSessionOptions {
            cwd: Some(cwd.clone()),
            agent_dir: Some(agent_dir),
            model: Some(test_model()),
            settings_manager: Some(settings),
            session_manager: Some(SessionManager::in_memory(&cwd, None, None)),
            tools: Some(Vec::new()),
            ..Default::default()
        })
        .await
        .expect("session");
        let messages = vec![AgentMessage::Llm(Message::User(UserMessage {
            content: UserContent::Blocks(vec![ContentBlock::Image(ImageContent {
                data: "aGk=".into(),
                mime_type: "image/png".into(),
            })]),
            timestamp: 1,
        }))];
        let context = created
            .session
            .agent()
            .build_provider_context(
                &AgentContext {
                    declared_tools: None,
                    system_prompt: String::new(),
                    messages,
                    tools: None,
                },
                None,
            )
            .await;
        created.session.dispose().await;
        let Some(Message::User(user)) = context.messages.first() else {
            panic!("user turn reaches the provider")
        };
        let UserContent::Blocks(blocks) = &user.content else {
            panic!("blocks content")
        };
        assert!(
            !blocks
                .iter()
                .any(|block| matches!(block, ContentBlock::Image(_))),
            "blockImages strips every image"
        );
        assert_eq!(
            maho_ai::utils::text::content_text(blocks, ""),
            crate::messages::BLOCKED_IMAGE_PLACEHOLDER
        );
    }

    fn test_model() -> Model {
        serde_json::from_value(serde_json::json!({
            "id": "faux-1", "name": "faux-1", "api": "faux", "provider": "faux",
            "baseUrl": "", "reasoning": true, "input": [], "contextWindow": 128000, "maxTokens": 4096,
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }
        }))
        .expect("model")
    }
}
