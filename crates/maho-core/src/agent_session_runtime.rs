//! Port of senpi `packages/coding-agent/src/core/agent-session-runtime.ts`.
//!

use crate::agent_session::AgentSession;
use crate::agent_session_services::{AgentSessionRuntimeDiagnostic, MountedAgentSessionServices};
use crate::session_cwd::assert_session_cwd_exists;
use crate::session_holders::{SessionHolderEndpoint, hold_active_session, was_flushed};

pub struct ExtensionModelRuntimeActions(pub std::sync::Mutex<crate::model_runtime::ModelRuntime>);
impl maho_ext_api::ExtensionProviderActions for ExtensionModelRuntimeActions {
    fn register_provider(
        &self,
        registration: maho_ext_api::ProviderRegistration,
        _: &str,
    ) -> Result<(), maho_ext_api::ExtensionFailure> {
        let mut runtime = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (name, config, model_options, retry_policy) = match registration {
            maho_ext_api::ProviderRegistration::Native(provider) => {
                runtime.register_native_provider(provider);
                return Ok(());
            }
            maho_ext_api::ProviderRegistration::Config { name, config } => {
                (name, config, std::collections::BTreeMap::new(), None)
            }
            maho_ext_api::ProviderRegistration::ConfigOptions { name, options } => (
                name,
                Box::new(options.config),
                options.model_options,
                options.retry_policy,
            ),
        };
        let apply_options = move |model: maho_ext_api::ProviderModelConfig| {
            let options = model_options.get(&model.id).cloned().unwrap_or_default();
            let mut model = extension_provider_model(model)?;
            model.service_tier = options.service_tier;
            model.prompt_preset = options.prompt_preset;
            model.sampling_params = options
                .sampling_params
                .map(|params| params.into_iter().collect());
            model.cache_retention = options.cache_retention;
            Ok::<_, maho_ext_api::ExtensionFailure>(model)
        };
        let models = config
            .models
            .map(|models| {
                models
                    .into_iter()
                    .map(&apply_options)
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?;
        let refresh_models =
            config
                .refresh_models
                .map(|refresh| -> crate::provider_composer::ExtensionRefresh {
                    std::sync::Arc::new(move |context| {
                        let future = refresh(context);
                        let apply_options = apply_options.clone();
                        Box::pin(async move {
                            future
                                .await
                                .and_then(|models| models.into_iter().map(apply_options).collect())
                                .map_err(|error| error.message)
                        })
                    })
                });
        let input = crate::provider_composer::ProviderConfigInput {
            config: crate::model_config_schema::ModelsJsonProvider {
                name: config.name,
                base_url: config.base_url,
                api_key: config.api_key,
                api: config.api,
                headers: config.headers,
                extra_body: config
                    .extra_body
                    .and_then(|value| value.as_object().cloned()),
                auth_header: config.auth_header,
                models,
                ..Default::default()
            },
            stream_simple: config.stream_simple,
            oauth: config.oauth,
            fallback_eligible: config.fallback_eligible,
            owns_account_selection: config.owns_account_selection,
            refresh_models,
            retry_policy,
            ambient_auth: config.ambient_auth,
            images: config.images.into_iter().collect(),
            classifiers: config.classifiers.into_iter().collect(),
        };
        runtime
            .register_provider(&name, input)
            .map_err(maho_ext_api::ExtensionFailure::new)
    }
    fn unregister_provider(
        &self,
        name: &str,
        _: &str,
    ) -> Result<(), maho_ext_api::ExtensionFailure> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .unregister_provider(name);
        Ok(())
    }
}
fn extension_provider_model(
    model: maho_ext_api::ProviderModelConfig,
) -> Result<crate::model_config_schema::ModelsJsonModel, maho_ext_api::ExtensionFailure> {
    let input_limits = model
        .input_limits
        .map(|limits| serde_json::to_value(limits).and_then(serde_json::from_value))
        .transpose()
        .map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?;
    let prompt_cache = model
        .prompt_cache
        .map(|cache| serde_json::to_value(cache).and_then(serde_json::from_value))
        .transpose()
        .map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?;
    Ok(crate::model_config_schema::ModelsJsonModel {
        id: model.id,
        name: Some(model.name),
        upstream_model_id: model.upstream_model_id,
        api: model.api,
        base_url: model.base_url,
        reasoning: Some(model.reasoning),
        recover_text_tool_calls: model.recover_text_tool_calls,
        thinking_level_map: model.thinking_level_map,
        input: Some(model.input),
        cost: Some(model.cost),
        context_window: Some(model.context_window as f64),
        max_tokens: Some(model.max_tokens as f64),
        headers: model.headers,
        extra_body: model
            .extra_body
            .and_then(|value| value.as_object().cloned()),
        compat: model.compat.map(|compat| compat.0),
        input_limits,
        prompt_cache,
        default_thinking_level: model.default_thinking_level,
        model_type: model.model_type,
        output: model.output,
        image_cost: model.image_cost,
        supports_assistant_prefill: model.supports_assistant_prefill,
        ..Default::default()
    })
}

/// Thrown when `/import` references a JSONL file path that does not exist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionImportFileNotFoundError {
    pub file_path: String,
}

impl std::fmt::Display for SessionImportFileNotFoundError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "File not found: {}", self.file_path)
    }
}

impl std::error::Error for SessionImportFileNotFoundError {}

/// The errors senpi's runtime replacement methods throw, kept typed so a caller can tell an
/// import of a missing file or a session whose cwd is gone from any other failure. Every variant
/// displays the exact upstream message text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentSessionRuntimeError {
    /// senpi `SessionImportFileNotFoundError`.
    ImportFileNotFound(SessionImportFileNotFoundError),
    /// senpi `MissingSessionCwdError` from `assertSessionCwdExists`.
    MissingSessionCwd(crate::session_cwd::MissingSessionCwdError),
    /// Any other upstream `Error` (for example `Invalid entry ID for forking`).
    Other(String),
}

impl std::fmt::Display for AgentSessionRuntimeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ImportFileNotFound(error) => error.fmt(formatter),
            Self::MissingSessionCwd(error) => error.fmt(formatter),
            Self::Other(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for AgentSessionRuntimeError {}

impl From<SessionImportFileNotFoundError> for AgentSessionRuntimeError {
    fn from(error: SessionImportFileNotFoundError) -> Self {
        Self::ImportFileNotFound(error)
    }
}
impl From<crate::session_cwd::MissingSessionCwdError> for AgentSessionRuntimeError {
    fn from(error: crate::session_cwd::MissingSessionCwdError) -> Self {
        Self::MissingSessionCwd(error)
    }
}
impl From<String> for AgentSessionRuntimeError {
    fn from(message: String) -> Self {
        Self::Other(message)
    }
}
impl From<&str> for AgentSessionRuntimeError {
    fn from(message: &str) -> Self {
        Self::Other(message.to_owned())
    }
}
impl From<std::io::Error> for AgentSessionRuntimeError {
    fn from(error: std::io::Error) -> Self {
        Self::Other(error.to_string())
    }
}
impl From<maho_ext_api::ExtensionFailure> for AgentSessionRuntimeError {
    fn from(error: maho_ext_api::ExtensionFailure) -> Self {
        Self::Other(error.message)
    }
}

/// Options of senpi `AgentSessionRuntime.switchSession`.
#[derive(Clone, Default)]
pub struct SwitchSessionOptions {
    pub cwd_override: Option<String>,
    pub with_session: Option<maho_ext_api::WithSession>,
}

/// Immutable flags selected when a runtime is first launched.
#[derive(Clone, Debug, Default)]
pub struct AgentSessionLaunchProfile {
    pub cwd: String,
    pub permission_preset: Option<String>,
    pub creation_model: Option<(String, String)>,
    pub initial_thinking_level: Option<String>,
    pub auto_title: Option<bool>,
    pub prompt_size_inventory:
        Option<std::sync::Arc<dyn crate::prompt_size_inventory::PromptSizeInventory>>,
}

impl PartialEq for AgentSessionLaunchProfile {
    fn eq(&self, other: &Self) -> bool {
        self.cwd == other.cwd && self.permission_preset == other.permission_preset
            && self.creation_model == other.creation_model && self.initial_thinking_level == other.initial_thinking_level
            && self.auto_title == other.auto_title
            && match (&self.prompt_size_inventory, &other.prompt_size_inventory) {
                (None, None) => true,
                (Some(left), Some(right)) => std::sync::Arc::ptr_eq(left, right),
                _ => false,
            }
    }
}

/// senpi `applyRetryFallbackProfile`: overlays `{ modelFallback, fallbackChains }` on the session's
/// settings as an in-memory override that is never written to a settings file.
pub fn apply_retry_fallback_profile(
    settings: &mut crate::settings_manager::SettingsManager,
    profile: &serde_json::Value,
) {
    let mut retry = serde_json::Map::new();
    if let Some(model_fallback) = profile.get("modelFallback") {
        retry.insert("modelFallback".to_owned(), model_fallback.clone());
    }
    if let Some(chains) = profile.get("fallbackChains") {
        retry.insert("fallbackChains".to_owned(), chains.clone());
    }
    settings.apply_overrides(&serde_json::Map::from_iter([(
        "retry".to_owned(),
        serde_json::Value::Object(retry),
    )]));
}

/// Result returned by runtime creation.
pub struct CreateAgentSessionRuntimeResult {
    pub session: AgentSession,
    pub services: MountedAgentSessionServices,
    pub diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
    pub model_fallback_message: Option<String>,
}

/// The attachment transitions a shared host reports to a live session's extensions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachmentEvent {
    SessionParked,
    SessionResumed,
}

pub type RebindSession = Box<dyn Fn(&AgentSession) + Send + Sync>;
pub type SessionInvalidateHook = Box<dyn Fn() + Send + Sync>;

/// Owns the current AgentSession plus its cwd-bound services.
pub struct AgentSessionRuntime {
    rebind_session: Option<RebindSession>,
    before_session_invalidate: Option<SessionInvalidateHook>,
    session: AgentSession,
    services: MountedAgentSessionServices,
    diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
    model_fallback_message: Option<String>,
    launch_profile: Option<AgentSessionLaunchProfile>,
    /// Host-owned public endpoint evidence the holder record names (senpi `launchProfile.holderEndpoint`; kept
    /// beside the profile so existing profile literals stay valid). Never sourced from client session context.
    holder_endpoint: Option<SessionHolderEndpoint>,
    /// senpi `launchProfile.promptSurface`: forwarded to the live session and re-applied on replacement.
    prompt_surface: Option<maho_ext_api::PromptSurface>,
    /// senpi `launchProfile.browserEngine` (`connected` / `builtin` / `none`); same caveat as `prompt_surface`.
    browser_engine: Option<String>,
    /// The hold on the active session's file; released when the session is replaced or disposed.
    session_hold: std::sync::Mutex<Option<crate::schedule::runner_lease::SessionHold>>,
}

impl AgentSessionRuntime {
    pub fn new(
        session: AgentSession,
        services: impl Into<MountedAgentSessionServices>,
        diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
        model_fallback_message: Option<String>,
        launch_profile: Option<AgentSessionLaunchProfile>,
    ) -> Self {
        let mut services = services.into();
        services.settings_manager = session.shared_settings_manager();
        Self {
            rebind_session: None,
            before_session_invalidate: None,
            session,
            services,
            diagnostics,
            model_fallback_message,
            launch_profile,
            holder_endpoint: None,
            prompt_surface: None,
            browser_engine: None,
            session_hold: std::sync::Mutex::new(None),
        }
    }

    /// Names the host endpoint on this runtime's holder record and takes the hold for the active session
    /// (senpi: the constructor's `holdActiveSession(..., launchProfile?.holderEndpoint)`). Later replacements
    /// hold their session under the same endpoint.
    pub fn set_holder_endpoint(
        &mut self,
        endpoint: Option<SessionHolderEndpoint>,
    ) -> Result<(), String> {
        self.holder_endpoint = endpoint;
        let hold = self.session.with_session_manager(|manager| {
            hold_active_session(manager, was_flushed(manager), self.holder_endpoint.as_ref())
        })?;
        *self.lock_hold() = hold;
        Ok(())
    }

    /// senpi `setPromptSurface` (the runtime half): later replacements keep it.
    pub fn set_prompt_surface(&mut self, surface: maho_ext_api::PromptSurface) {
        self.prompt_surface = Some(surface);
        self.session.set_prompt_surface(surface);
    }

    pub fn prompt_surface(&self) -> Option<maho_ext_api::PromptSurface> {
        self.prompt_surface
    }

    /// senpi `setBrowserEngine` (the runtime half): later replacements keep it.
    pub fn set_browser_engine(&mut self, engine: String) {
        self.session.set_browser_engine(engine.clone());
        self.browser_engine = Some(engine);
    }

    pub fn browser_engine(&self) -> Option<&str> {
        self.browser_engine.as_deref()
    }

    /// senpi `setRetryFallback`: overlays the fallback policy on this session's settings in memory.
    pub fn set_retry_fallback(&self, profile: &serde_json::Value) {
        self.session.with_settings_manager_mut(|settings| {
            apply_retry_fallback_profile(settings, profile);
        });
    }

    pub fn holder_endpoint(&self) -> Option<&SessionHolderEndpoint> {
        self.holder_endpoint.as_ref()
    }

    /// senpi `releaseSessionHold`.
    pub fn release_session_hold(&self) {
        *self.lock_hold() = None;
    }

    pub fn has_session_hold(&self) -> bool {
        self.lock_hold().is_some()
    }

    fn lock_hold(
        &self,
    ) -> std::sync::MutexGuard<'_, Option<crate::schedule::runner_lease::SessionHold>> {
        self.session_hold
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn services(&self) -> &MountedAgentSessionServices {
        &self.services
    }

    pub fn session(&self) -> &AgentSession {
        &self.session
    }

    pub fn cwd(&self) -> &str {
        &self.services.cwd
    }

    pub fn diagnostics(&self) -> &[AgentSessionRuntimeDiagnostic] {
        &self.diagnostics
    }

    pub fn model_fallback_message(&self) -> Option<&str> {
        self.model_fallback_message.as_deref()
    }

    pub fn launch_profile(&self) -> Option<&AgentSessionLaunchProfile> {
        self.launch_profile.as_ref()
    }

    pub fn set_rebind_session(&mut self, rebind: Option<RebindSession>) {
        self.rebind_session = rebind;
    }

    pub fn set_before_session_invalidate(&mut self, callback: Option<SessionInvalidateHook>) {
        self.before_session_invalidate = callback;
    }

    /// Attachment transitions are ordered by the RPC entry's lifecycle owner.
    pub async fn emit_attachment_event(&self, event: AttachmentEvent) {
        self.session
            .runtime_emit_attachment(match event {
                AttachmentEvent::SessionParked => maho_ext_api::ExtensionEvent::SessionParked,
                AttachmentEvent::SessionResumed => maho_ext_api::ExtensionEvent::SessionResumed,
            })
            .await;
    }

    /// senpi `finishSessionReplacement`: rebind the host to the new session, then run the
    /// caller's `withSession` against a context bound to it.
    async fn finish_session_replacement(
        &self,
        with_session: Option<maho_ext_api::WithSession>,
    ) -> Result<(), AgentSessionRuntimeError> {
        if let Some(rebind) = &self.rebind_session {
            rebind(&self.session);
        }
        if with_session.is_some() {
            self.session
                .run_replacement_callbacks(None, with_session)
                .await?;
        }
        Ok(())
    }

    /// Returns `Ok(false)` when `session_before_switch` cancelled (senpi `{ cancelled: true }`).
    pub async fn switch_session(
        &mut self,
        session_path: &str,
        options: SwitchSessionOptions,
    ) -> Result<bool, AgentSessionRuntimeError> {
        if self
            .session
            .runtime_before_switch(
                maho_ext_api::SessionReason::Resume,
                Some(session_path.to_owned()),
            )
            .await?
        {
            return Ok(false);
        }
        let manager = crate::session_manager::SessionManager::open(
            session_path,
            None,
            options.cwd_override.as_deref(),
            None,
        )?;
        assert_session_cwd_exists(&manager, &self.services.cwd)?;
        let cwd = manager.cwd().to_owned();
        self.apply_replacement(manager, cwd, maho_ext_api::SessionReason::Resume)
            .await?;
        self.finish_session_replacement(options.with_session)
            .await?;
        Ok(true)
    }

    /// Returns `Ok(false)` when `session_before_switch` cancelled (senpi `{ cancelled: true }`).
    pub async fn new_session(
        &mut self,
        options: maho_ext_api::NewSessionOptions,
    ) -> Result<bool, AgentSessionRuntimeError> {
        if self
            .session
            .runtime_before_switch(maho_ext_api::SessionReason::New, None)
            .await?
        {
            return Ok(false);
        }
        let cwd = self.services.cwd.clone();
        let new_options = options.parent_session.map(|parent_session| {
            crate::session_manager::NewSessionOptions {
                id: None,
                parent_session: Some(parent_session),
                forked_from_leaf_id: None,
                spawned_by: None,
            }
        });
        let manager = self.session.with_session_manager(|current| {
            if current.is_persisted() {
                crate::session_manager::SessionManager::create(
                    &cwd,
                    Some(current.session_dir()),
                    new_options,
                )
            } else {
                crate::session_manager::SessionManager::in_memory(&cwd, new_options, None)
            }
        });
        self.apply_replacement(manager, cwd, maho_ext_api::SessionReason::New)
            .await?;
        // senpi runs `setup` on the new manager, then rebuilds the agent messages from it.
        if options.setup.is_some() {
            self.session
                .run_replacement_callbacks(options.setup, None)
                .await?;
        }
        self.finish_session_replacement(options.with_session)
            .await?;
        Ok(true)
    }

    pub async fn fork(
        &mut self,
        entry_id: &str,
        options: maho_ext_api::ForkOptions,
    ) -> Result<crate::agent_session::AssistantEditResult, AgentSessionRuntimeError> {
        let include_entry = options
            .position
            .unwrap_or(maho_ext_api::ForkPosition::Before)
            == maho_ext_api::ForkPosition::At;
        if self
            .session
            .runtime_before_fork(entry_id, include_entry)
            .await?
        {
            return Ok(crate::agent_session::AssistantEditResult {
                cancelled: true,
                ..Default::default()
            });
        }
        let entry = self
            .session
            .with_session_manager(|manager| manager.entry(entry_id))
            .ok_or_else(|| "Invalid entry ID for forking".to_owned())?;
        if !include_entry && (entry["type"] != "message" || entry["message"]["role"] != "user") {
            return Err("Invalid entry ID for forking".into());
        }
        let leaf = if include_entry {
            Some(entry_id)
        } else {
            entry["parentId"].as_str()
        };
        let previous = self.session.session_file();
        if self
            .session
            .with_session_manager(|manager| manager.is_persisted())
        {
            let file = previous
                .as_deref()
                .ok_or("Persisted session is missing a session file")?;
            if leaf.is_some() && !std::path::Path::new(file).exists() {
                return Err("This session has not been saved yet. Wait for the first assistant response before cloning or forking it.".into());
            }
        }
        let (manager, cwd) = self
            .session
            .with_session_manager(|current| -> Result<_, String> {
                let options = Some(crate::session_manager::NewSessionOptions {
                    parent_session: previous.clone(),
                    ..Default::default()
                });
                let Some(leaf) = leaf else {
                    let manager = if current.is_persisted() {
                        crate::session_manager::SessionManager::create(
                            &self.services.cwd,
                            Some(current.session_dir()),
                            options,
                        )
                    } else {
                        crate::session_manager::SessionManager::in_memory(
                            &self.services.cwd,
                            options,
                            None,
                        )
                    };
                    return Ok((manager, self.services.cwd.clone()));
                };
                if current.is_persisted() {
                    let file = previous
                        .as_deref()
                        .ok_or("Persisted session is missing a session file")?;
                    let mut manager = crate::session_manager::SessionManager::open(
                        file,
                        Some(current.session_dir()),
                        None,
                        None,
                    )?;
                    manager
                        .create_branched_session(leaf)?
                        .ok_or("Failed to create forked session")?;
                    let cwd = manager.cwd().to_owned();
                    Ok((manager, cwd))
                } else {
                    let mut manager = current.clone();
                    manager.create_branched_session(leaf)?;
                    Ok((manager, self.services.cwd.clone()))
                }
            })?;
        self.apply_replacement(manager, cwd, maho_ext_api::SessionReason::Fork)
            .await?;
        self.finish_session_replacement(options.with_session)
            .await?;
        Ok(crate::agent_session::AssistantEditResult {
            editor_text: (!include_entry)
                .then(|| extract_user_message_text(&entry["message"]["content"])),
            ..Default::default()
        })
    }

    /// Import a session JSONL file and switch runtime state to the imported session.
    ///
    /// Returns `Ok(false)` when cancelled by `session_before_switch`. Fails with
    /// [`AgentSessionRuntimeError::ImportFileNotFound`] when the input path does not exist and
    /// [`AgentSessionRuntimeError::MissingSessionCwd`] when the imported session cwd cannot be
    /// resolved and no override is provided.
    pub async fn import_from_jsonl(
        &mut self,
        input_path: &str,
        cwd_override: Option<&str>,
    ) -> Result<bool, AgentSessionRuntimeError> {
        let base = std::env::current_dir()?;
        let resolved = crate::paths::resolve_path(
            input_path,
            &base.to_string_lossy(),
            &crate::paths::PathInputOptions::expanding_tilde(crate::config::home_dir()),
        );
        let source = std::path::Path::new(&resolved);
        if !source.exists() {
            return Err(SessionImportFileNotFoundError {
                file_path: resolved,
            }
            .into());
        }
        let directory = self
            .session
            .with_session_manager(|manager| manager.session_dir().to_owned());
        std::fs::create_dir_all(&directory)?;
        let filename = source.file_name().ok_or("Invalid import filename")?;
        let mut destination = std::path::Path::new(&directory).join(filename);
        let stored = crate::paths::resolve_path(
            &destination.to_string_lossy(),
            &base.to_string_lossy(),
            &crate::paths::PathInputOptions::default(),
        ) == resolved;
        if !stored {
            let stem = source.file_stem().unwrap_or_default().to_string_lossy();
            let extension = source
                .extension()
                .map(|extension| format!(".{}", extension.to_string_lossy()))
                .unwrap_or_default();
            let mut suffix = 1;
            while destination.exists() {
                destination =
                    std::path::Path::new(&directory).join(format!("{stem}-{suffix}{extension}"));
                suffix += 1;
            }
        }
        let destination = destination.to_string_lossy().into_owned();
        if self
            .session
            .runtime_before_switch(
                maho_ext_api::SessionReason::Resume,
                Some(destination.clone()),
            )
            .await?
        {
            return Ok(false);
        }
        // senpi takes the shared host's write grant for the destination before copying into it.
        crate::session_write_reservation::reserve_session_write(&destination);
        if !stored {
            let (source, target) = (source.to_path_buf(), destination.clone());
            tokio::task::spawn_blocking(move || -> std::io::Result<u64> {
                let mut input = std::fs::File::open(source)?;
                let mut output = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&target)?;
                std::io::copy(&mut input, &mut output)
            })
            .await
            .map_err(std::io::Error::other)??;
        }
        let manager = crate::session_manager::SessionManager::open(
            &destination,
            Some(&directory),
            cwd_override,
            None,
        )?;
        assert_session_cwd_exists(&manager, &self.services.cwd)?;
        let cwd = manager.cwd().to_owned();
        self.apply_replacement(manager, cwd, maho_ext_api::SessionReason::Resume)
            .await?;
        self.finish_session_replacement(None).await?;
        Ok(true)
    }

    async fn apply_replacement(
        &mut self,
        manager: crate::session_manager::SessionManager,
        cwd: String,
        reason: maho_ext_api::SessionReason,
    ) -> Result<(), String> {
        let settings = crate::settings_manager::SettingsManager::create(
            &cwd,
            &self.services.agent_dir,
            &std::env::var("HOME").unwrap_or_default(),
            self.services
                .settings_manager
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_project_trusted(),
        );
        let (system_prompt, append_system_prompt) = self.session.system_prompt_sources();
        let target_session_file = manager.session_file().map(str::to_owned);
        // The new session's file is held before anything is torn down (senpi switch path): a file that was
        // moved away after it was read refuses here, with the current session untouched.
        let next_hold = hold_active_session(
            &manager,
            matches!(reason, maho_ext_api::SessionReason::Resume) && was_flushed(&manager),
            self.holder_endpoint.as_ref(),
        )?;
        let context = manager.current_context();
        let restored_model = context
            .model
            .as_ref()
            .and_then(|(provider, id)| self.services.model_registry.find(provider, id));
        let launch_model = self
            .launch_profile
            .as_ref()
            .and_then(|profile| profile.creation_model.as_ref())
            .and_then(|(provider, id)| self.services.model_registry.find(provider, id));
        let launch_thinking = self
            .launch_profile
            .as_ref()
            .and_then(|profile| profile.initial_thinking_level.as_deref())
            .and_then(maho_ai::types::ModelThinkingLevel::parse);
        let effective_model = launch_model
            .or(restored_model)
            .or_else(|| Some(self.session.model()));
        let mut session_thinking = None;
        if let Some(ref m) = effective_model {
            let levels = crate::session_thinking_memory::derive_session_model_thinking_levels(&manager.current_branch());
            if let Some(level_str) = levels.get(&format!("{}/{}", m.provider, m.id)) {
                session_thinking = maho_ai::types::ModelThinkingLevel::parse(level_str);
            }
        }
        let thinking_selection = if manager
            .current_branch()
            .iter()
            .any(|entry| entry["type"] == "thinking_level_change")
        {
            session_thinking.map(|level| maho_ai::types::ThinkingSelection {
                level,
                source: maho_ai::types::ThinkingSelectionSource::Explicit,
                legacy_variant_id: None,
            })
        } else {
            self.session.thinking_selection()
        };
        let options = crate::sdk::CreateAgentSessionOptions {
            system_prompt,
            append_system_prompt,
            cwd: Some(cwd.clone()),
            agent_dir: Some(self.services.agent_dir.clone()),
            model_runtime: Some(self.services.model_runtime().clone()),
            model_registry: Some(self.services.model_registry.clone()),
            model: effective_model,
            thinking_level: launch_thinking
                .or(session_thinking)
                .and_then(|level| {
                    serde_json::from_value(serde_json::Value::from(level.as_str())).ok()
                }),
            thinking_selection: launch_thinking
                .map(|level| maho_ai::types::ThinkingSelection {
                    level,
                    source: maho_ai::types::ThinkingSelectionSource::Explicit,
                    legacy_variant_id: None,
                })
                .or(thinking_selection),
            scoped_models: self.session.scoped_models(),
            favorite_models: self.session.favorite_models(),
            session_manager: Some(manager),
            settings_manager: Some(settings),
            tools: Some(self.session.get_active_tool_names()),
            custom_tools: self.session.replacement_custom_tools(),
            resource_inputs: self.session.resource_loader_inputs(),
            auto_title_sessions: self
                .launch_profile
                .as_ref()
                .and_then(|profile| profile.auto_title)
                .or(Some(self.session.replacement_auto_title())),
            session_start_event: Some(maho_ext_api::SessionStartEvent {
                reason,
                initial_model_provenance: None,
                previous_session_file: self.session.session_file(),
                attended: None,
            }),
            ..Default::default()
        };
        self.session.abort().await;
        self.session
            .runtime_shutdown_to(reason, target_session_file)
            .await;
        if let Some(before) = &self.before_session_invalidate {
            before();
        }
        self.session.dispose().await;
        let created = crate::sdk::create_agent_session(options).await?;
        // senpi `apply`: the launch profile's surface and engine follow the session replacement.
        if let Some(surface) = self.prompt_surface {
            created.session.set_prompt_surface(surface);
        }
        if let Some(engine) = &self.browser_engine {
            created.session.set_browser_engine(engine.clone());
        }
        let replacement = match self
            .session
            .replacement_extension_runner(&created.session)
            .await
        {
            Ok(replacement) => replacement,
            Err(error) => {
                created.session.dispose().await;
                return Err(error);
            }
        };
        if let Some(runner) = replacement {
            self.session
                .report_removed_extensions(&runner, reason)
                .await;
            created.session.set_extension_runner(runner).await;
            let inventory = self
                .launch_profile
                .as_ref()
                .and_then(|p| p.prompt_size_inventory.clone());
            created
                .session
                .bind_extensions(crate::agent_session::ExtensionBindings {
                    prompt_size_inventory: inventory,
                    ..Default::default()
                })
                .await;
        }
        self.session = created.session;
        *self.lock_hold() = next_hold;
        self.services.cwd = cwd;
        self.services.settings_manager = self.session.shared_settings_manager();
        self.model_fallback_message = created.model_fallback_message;
        Ok(())
    }

    /// `process_exit`: the caller exits the process right after, so quit-time handlers share the total quit
    /// budget counted from this request (senpi#3711). An RPC host closing one session leaves it `false` and
    /// keeps only the per-handler limits.
    pub async fn dispose(&self) {
        self.dispose_with(DisposeOptions::default()).await;
    }

    /// `dispose({ processExit })`: `process_exit` arms the runner's total quit budget.
    pub async fn dispose_with(&self, options: DisposeOptions) {
        let DisposeOptions { process_exit } = options;
        self.session
            .emit_session_shutdown_for_quit(process_exit)
            .await;
        if let Some(callback) = &self.before_session_invalidate {
            callback();
        }
        self.session.dispose().await;
        self.release_session_hold();
    }
}

/// senpi `AgentSessionRuntime.dispose` options.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DisposeOptions {
    pub process_exit: bool,
}

/// Reject a session manager whose cwd does not exist, exactly as the runtime factory does.
pub fn assert_runtime_cwd(
    manager: &crate::session_manager::SessionManager,
    cwd: &str,
) -> Result<(), crate::session_cwd::MissingSessionCwdError> {
    assert_session_cwd_exists(manager, cwd)
}

/// Extract the text of a user message's content, for `fork`'s selected text.
pub fn extract_user_message_text(content: &serde_json::Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_owned();
    }
    content
        .as_array()
        .map(|parts| {
            parts
                .iter()
                .filter(|part| part.get("type").and_then(serde_json::Value::as_str) == Some("text"))
                .filter_map(|part| part.get("text").and_then(serde_json::Value::as_str))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_registration_preserves_nonchat_catalogs_and_prefill() {
        use maho_ext_api::*;
        let runtime = crate::model_runtime::ModelRuntime::create_sync(
            crate::model_runtime::CreateModelRuntimeOptions {
                providers: Some(Vec::new()),
                ..Default::default()
            },
        )
        .expect("valid runtime");
        let actions = ExtensionModelRuntimeActions(std::sync::Mutex::new(runtime.clone()));
        let definition = |id: &str, kind| ProviderModelConfig {
            model_type: Some(kind),
            output: Some(vec![maho_ai::types::ImagesOutputModality::Image]),
            image_cost: Some(Default::default()),
            input_limits: None,
            prompt_cache: None,
            supports_assistant_prefill: Some(true),
            default_thinking_level: None,
            id: id.into(),
            name: id.into(),
            upstream_model_id: None,
            api: Some(
                match kind {
                    maho_ai::types::ModelType::Image => "fixture-image",
                    maho_ai::types::ModelType::Classifier => "fixture-classifier",
                    maho_ai::types::ModelType::Chat => "fixture-chat",
                }
                .into(),
            ),
            base_url: None,
            reasoning: false,
            recover_text_tool_calls: None,
            thinking_level_map: None,
            input: vec![maho_ai::types::InputModality::Text],
            cost: Default::default(),
            context_window: 1024,
            max_tokens: 128,
            headers: None,
            extra_body: None,
            compat: None,
        };
        // Given
        let config = ProviderConfig {
            base_url: Some("https://example.test".into()),
            models: Some(vec![
                definition("chat", maho_ai::types::ModelType::Chat),
                definition("image", maho_ai::types::ModelType::Image),
                definition("classifier", maho_ai::types::ModelType::Classifier),
            ]),
            ..Default::default()
        };
        // When
        actions
            .register_provider(
                ProviderRegistration::Config {
                    name: "fixture".into(),
                    config: Box::new(config),
                },
                "fixture",
            )
            .expect("register");
        // Then
        let stored = runtime
            .get_registered_provider_config("fixture")
            .expect("config");
        let provider = crate::provider_composer::compose_model_provider(
            "fixture",
            None,
            &crate::model_config::ModelConfig::default(),
            Some(&stored),
        )
        .expect("compose");
        let models = provider.get_all_models();
        assert_eq!(models.len(), 3);
        assert!(models.iter().any(
            |model| matches!(model, maho_ai::types::AnyModel::Image(model) if model.id == "image")
        ));
        assert!(models.iter().any(|model| matches!(model, maho_ai::types::AnyModel::Classifier(model) if model.id == "classifier")));
        assert_eq!(
            provider.get_models()[0].supports_assistant_prefill,
            Some(true)
        );
    }

    #[test]
    fn extension_provider_registration_reaches_the_live_session_catalog() {
        use maho_ext_api::*;
        let runtime = crate::model_runtime::ModelRuntime::create_sync(
            crate::model_runtime::CreateModelRuntimeOptions {
                providers: Some(vec![]),
                ..Default::default()
            },
        )
        .expect("valid model runtime");
        let actions = std::sync::Arc::new(ExtensionModelRuntimeActions(std::sync::Mutex::new(
            runtime.clone(),
        )));
        let extension_runtime = ExtensionRuntime::default();
        let api = ExtensionApi::new(
            LoadedExtension::new("fixture", "/tmp".into(), SourceInfo::default()),
            ExtensionSessionProfile::default(),
            EventBus::default(),
            extension_runtime.clone(),
        );
        let model = ProviderModelConfig {
            model_type: None,
            output: None,
            image_cost: None,
            input_limits: None,
            prompt_cache: None,
            supports_assistant_prefill: None,
            default_thinking_level: None,
            id: "fixture-model".into(),
            name: "Fixture".into(),
            upstream_model_id: None,
            api: None,
            base_url: None,
            reasoning: false,
            recover_text_tool_calls: None,
            thinking_level_map: None,
            input: vec![maho_ai::types::InputModality::Text],
            cost: Default::default(),
            context_window: 4096,
            max_tokens: 512,
            headers: None,
            extra_body: None,
            compat: None,
        };
        api.register_provider_with_options(
            "fixture",
            ProviderConfigOptions {
                config: ProviderConfig {
                    base_url: Some("https://example.test/v1".into()),
                    api: Some("openai-completions".into()),
                    models: Some(vec![model]),
                    ..Default::default()
                },
                model_options: std::collections::BTreeMap::from([(
                    "fixture-model".into(),
                    ProviderModelOptions {
                        service_tier: Some(maho_ai::types::ServiceTierPreference::Flex),
                        prompt_preset: Some("fixture-prompt".into()),
                        sampling_params: Some(
                            serde_json::from_value(serde_json::json!({"temperature": 0.2}))
                                .unwrap(),
                        ),
                        cache_retention: Some(maho_ai::types::CacheRetention::Long),
                    },
                )]),
                retry_policy: Some(
                    maho_ai::utils::retry_profile::profiles::SENPI_DEFAULT_RETRY_PROFILE.clone(),
                ),
            },
        )
        .unwrap();
        assert!(runtime.get_model("fixture", "fixture-model").is_none());
        extension_runtime.bind_providers(actions).unwrap();
        let model = runtime.get_model("fixture", "fixture-model").unwrap();
        assert_eq!(model.context_window, 4096);
        assert_eq!(model.base_url, "https://example.test/v1");
        assert_eq!(
            model.cache_retention,
            Some(maho_ai::types::CacheRetention::Long)
        );
        assert_eq!(
            model.sampling_params.as_ref().unwrap()["temperature"],
            serde_json::json!(0.2)
        );
        let config = runtime.get_registered_provider_config("fixture").unwrap();
        assert_eq!(config.retry_policy.as_ref().unwrap().id, "senpi-default");
        let definition = &config.models.as_ref().unwrap()[0];
        assert_eq!(
            definition.service_tier,
            Some(maho_ai::types::ServiceTierPreference::Flex)
        );
        assert_eq!(definition.prompt_preset.as_deref(), Some("fixture-prompt"));
        api.unregister_provider("fixture").unwrap();
        assert!(runtime.get_model("fixture", "fixture-model").is_none());
    }

    #[test]
    fn a_missing_import_file_names_the_path() {
        let error = SessionImportFileNotFoundError {
            file_path: "/tmp/missing.jsonl".to_owned(),
        };
        assert_eq!(error.to_string(), "File not found: /tmp/missing.jsonl");
    }

    #[tokio::test]
    async fn persisted_fork_reads_durable_history_instead_of_modified_mirror() {
        let dir = tempfile::tempdir().expect("directory");
        let cwd = dir.path().to_string_lossy().into_owned();
        let services = crate::agent_session_services::create_agent_session_services(
            crate::agent_session_services::CreateAgentSessionServicesOptions {
                cwd: cwd.clone(),
                agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
                ..Default::default()
            },
        )
        .await
        .expect("valid session services");
        let provider = maho_ai::providers::faux::faux_provider(Default::default())
            .expect("valid faux provider");
        let mut manager = crate::session_manager::SessionManager::create(
            &cwd,
            dir.path().join("sessions").to_str(),
            None,
        );
        let user = manager
            .append_message(serde_json::json!({"role":"user","content":"durable","timestamp":0}));
        let assistant = manager.append_message(
            serde_json::to_value(maho_ai::providers::faux::faux_assistant_message(
                "answer",
                Default::default(),
            ))
            .expect("assistant"),
        );
        let mut durable = crate::session_manager::SessionManager::open(
            manager.session_file().expect("file"),
            None,
            None,
            None,
        )
        .expect("open");
        durable
            .append_label(user["id"].as_str().expect("user"), Some("durable label"))
            .expect("label");
        let created = crate::sdk::create_agent_session(crate::sdk::CreateAgentSessionOptions {
            cwd: Some(cwd.clone()),
            agent_dir: Some(services.agent_dir.clone()),
            model: provider.get_model(Some("faux-1")),
            session_manager: Some(manager),
            tools: Some(Vec::new()),
            ..Default::default()
        })
        .await
        .expect("session");
        let mut runtime =
            AgentSessionRuntime::new(created.session, services, Vec::new(), None, None);
        runtime
            .fork(
                assistant["id"].as_str().expect("assistant"),
                maho_ext_api::ForkOptions {
                    position: Some(maho_ext_api::ForkPosition::At),
                    with_session: None,
                },
            )
            .await
            .expect("persisted fork");
        assert_eq!(
            runtime.session().with_session_manager(|manager| manager
                .label(user["id"].as_str().expect("user"))
                .map(str::to_owned)),
            Some("durable label".to_owned())
        );
    }

    #[tokio::test]
    async fn runtime_factory_constructs_a_runtime_from_the_session_factory() {
        let dir = tempfile::tempdir().expect("directory");
        let cwd = dir.path().to_string_lossy().into_owned();
        let agent_dir = dir.path().join("agent").to_string_lossy().into_owned();
        let services = crate::agent_session_services::create_agent_session_services(
            crate::agent_session_services::CreateAgentSessionServicesOptions {
                cwd: cwd.clone(),
                agent_dir: Some(agent_dir.clone()),
                ..Default::default()
            },
        )
        .await
        .expect("valid session services");
        let provider = maho_ai::providers::faux::faux_provider(Default::default())
            .expect("valid faux provider");
        let manager = crate::session_manager::SessionManager::create(&cwd, None, None);
        let created = crate::sdk::create_agent_session(crate::sdk::CreateAgentSessionOptions {
            cwd: Some(cwd.clone()),
            agent_dir: Some(agent_dir),
            model: provider.get_model(Some("faux-1")),
            session_manager: Some(manager),
            tools: Some(Vec::new()),
            ..Default::default()
        })
        .await
        .expect("session");
        let runtime = AgentSessionRuntime::new(created.session, services, Vec::new(), None, None);
        assert_eq!(runtime.cwd(), cwd);
    }

    #[tokio::test]
    async fn unsaved_persisted_fork_rejects_without_replacing_session() {
        let dir = tempfile::tempdir().expect("directory");
        let cwd = dir.path().to_string_lossy().into_owned();
        let services = crate::agent_session_services::create_agent_session_services(
            crate::agent_session_services::CreateAgentSessionServicesOptions {
                cwd: cwd.clone(),
                agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
                ..Default::default()
            },
        )
        .await
        .expect("valid session services");
        let provider = maho_ai::providers::faux::faux_provider(Default::default())
            .expect("valid faux provider");
        let mut manager = crate::session_manager::SessionManager::create(
            &cwd,
            dir.path().join("sessions").to_str(),
            None,
        );
        let entry = manager
            .append_message(serde_json::json!({"role":"user","content":"unsaved","timestamp":0}));
        let created = crate::sdk::create_agent_session(crate::sdk::CreateAgentSessionOptions {
            cwd: Some(cwd.clone()),
            agent_dir: Some(services.agent_dir.clone()),
            model: provider.get_model(Some("faux-1")),
            session_manager: Some(manager),
            tools: Some(Vec::new()),
            ..Default::default()
        })
        .await
        .expect("session");
        let mut runtime =
            AgentSessionRuntime::new(created.session, services, Vec::new(), None, None);
        let id = runtime.session().session_id();
        assert!(!std::path::Path::new(&runtime.session().session_file().expect("path")).exists());
        let error = runtime
            .fork(
                entry["id"].as_str().expect("entry"),
                maho_ext_api::ForkOptions {
                    position: Some(maho_ext_api::ForkPosition::At),
                    with_session: None,
                },
            )
            .await
            .expect_err("unsaved fork");
        assert!(
            error
                .to_string()
                .starts_with("This session has not been saved yet.")
        );
        assert_eq!(runtime.session().session_id(), id);
        assert_eq!(runtime.session().messages().len(), 1);
    }

    #[tokio::test]
    async fn replacement_restores_target_model_and_thinking() {
        let dir = tempfile::tempdir().expect("directory");
        let cwd = dir.path().to_string_lossy().into_owned();
        let services = crate::agent_session_services::create_agent_session_services(
            crate::agent_session_services::CreateAgentSessionServicesOptions {
                cwd: cwd.clone(),
                agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
                ..Default::default()
            },
        )
        .await
        .expect("valid session services");
        let provider = maho_ai::providers::faux::faux_provider(
            maho_ai::providers::faux::RegisterFauxProviderOptions {
                models: Some(vec![
                    maho_ai::providers::faux::FauxModelDefinition {
                        id: "original".to_owned(),
                        ..Default::default()
                    },
                    maho_ai::providers::faux::FauxModelDefinition {
                        id: "saved".to_owned(),
                        reasoning: Some(true),
                        ..Default::default()
                    },
                ]),
                ..Default::default()
            },
        )
        .expect("valid faux provider");
        let mut model_runtime = services.model_runtime().clone();
        model_runtime.register_native_provider(provider.provider.clone());
        let created = crate::sdk::create_agent_session(crate::sdk::CreateAgentSessionOptions {
            cwd: Some(cwd.clone()),
            agent_dir: Some(services.agent_dir.clone()),
            model: provider.get_model(Some("original")),
            model_runtime: Some(model_runtime),
            session_manager: Some(crate::session_manager::SessionManager::in_memory(
                &cwd, None, None,
            )),
            tools: Some(Vec::new()),
            ..Default::default()
        })
        .await
        .expect("session");
        let mut runtime =
            AgentSessionRuntime::new(created.session, services, Vec::new(), None, None);
        let mut target = crate::session_manager::SessionManager::in_memory(&cwd, None, None);
        target.append_model_change("faux", "saved", None, None);
        target.append_thinking_level_change(
            "high",
            Some(serde_json::json!({"level":"high","source":"explicit"})),
        );
        runtime
            .apply_replacement(target, cwd.clone(), maho_ext_api::SessionReason::Resume)
            .await
            .expect("replacement");
        assert_eq!(runtime.session().model().id, "saved");
        assert_eq!(
            runtime.session().thinking_level(),
            maho_ai::types::ModelThinkingLevel::High
        );
        assert_eq!(
            runtime
                .session()
                .thinking_selection()
                .expect("selection")
                .level,
            maho_ai::types::ModelThinkingLevel::High
        );
        runtime.launch_profile = Some(AgentSessionLaunchProfile {
            cwd: cwd.clone(),
            creation_model: Some(("faux".to_owned(), "original".to_owned())),
            initial_thinking_level: Some("off".to_owned()),
            auto_title: Some(false),
            ..Default::default()
        });
        let mut target = crate::session_manager::SessionManager::in_memory(&cwd, None, None);
        target.append_model_change("faux", "saved", None, None);
        target.append_thinking_level_change(
            "high",
            Some(serde_json::json!({"level":"high","source":"explicit"})),
        );
        runtime
            .apply_replacement(target, cwd, maho_ext_api::SessionReason::Resume)
            .await
            .expect("launch replacement");
        assert_eq!(runtime.session().model().id, "original");
        assert_eq!(
            runtime.session().thinking_level(),
            maho_ai::types::ModelThinkingLevel::Off
        );
        assert!(!runtime.session().replacement_auto_title());
    }

    #[tokio::test]
    async fn replacing_session_preserves_custom_tools_and_auto_title_selection() {
        let dir = crate::test_support::isolated_tempdir();
        let cwd = dir.path().to_string_lossy().into_owned();
        let agent_dir = dir.path().join("agent").to_string_lossy().into_owned();
        let services = crate::agent_session_services::create_agent_session_services(
            crate::agent_session_services::CreateAgentSessionServicesOptions {
                cwd: cwd.clone(),
                agent_dir: Some(agent_dir.clone()),
                ..Default::default()
            },
        )
        .await
        .expect("valid session services");
        let model = serde_json::from_value(serde_json::json!({
            "id":"faux-1", "name":"faux-1", "provider":"faux", "api":"faux", "baseUrl":"",
            "reasoning":false, "input":[], "contextWindow":128000, "maxTokens":4096,
            "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}
        }))
        .expect("model");
        let custom = maho_ext_api::ToolDefinition::new(
            "retained",
            "custom tool",
            serde_json::json!({"type":"object"}),
            std::sync::Arc::new(|_| {
                Box::pin(async { Ok(maho_tools::definition::ToolResult::text("retained")) })
            }),
        );
        let created = crate::sdk::create_agent_session(crate::sdk::CreateAgentSessionOptions {
            cwd: Some(cwd.clone()),
            agent_dir: Some(agent_dir),
            model: Some(model),
            model_runtime: Some(services.model_runtime().clone()),
            session_manager: Some(crate::session_manager::SessionManager::in_memory(
                &cwd, None, None,
            )),
            custom_tools: vec![custom],
            tools: Some(vec!["retained".to_owned()]),
            auto_title_sessions: Some(false),
            ..Default::default()
        })
        .await
        .expect("session");
        let mut runtime =
            AgentSessionRuntime::new(created.session, services, Vec::new(), None, None);
        let prompt_path = dir.path().join("system.md");
        std::fs::write(&prompt_path, "original prompt").expect("prompt fixture");
        runtime.session().set_system_prompt_sources(
            Some(prompt_path.to_string_lossy().into_owned()),
            Vec::new(),
        );
        runtime.set_before_session_invalidate(Some(Box::new(move || {
            std::fs::write(&prompt_path, "prompt after teardown").expect("updated prompt fixture");
        })));
        assert!(
            runtime
                .new_session(Default::default())
                .await
                .expect("replacement")
        );
        // senpi `_rebuildSystemPrompt`: a loader system prompt is used verbatim (agent-session.ts:3580).
        assert_eq!(runtime.session().system_prompt(), "prompt after teardown");
        runtime.set_before_session_invalidate(None);
        assert!(
            !runtime
                .session()
                .with_session_manager(|manager| manager.is_persisted())
        );
        assert!(runtime.session().get_tool_definition("retained").is_some());
        assert!(!runtime.session().replacement_auto_title());
        let result = runtime
            .session()
            .execute_tool("retained", serde_json::json!({}), Default::default())
            .await
            .expect("tool");
        assert_eq!(
            maho_ai::utils::text::content_text(&result.content, ""),
            "retained"
        );
        let entry = runtime.session().with_session_manager_mut(|manager| {
            manager.append_message(serde_json::json!({
                "role":"user","content":[{"type":"text","text":"selected text"}],"timestamp":0
            }))
        });
        let before = runtime
            .fork(
                entry["id"].as_str().unwrap(),
                maho_ext_api::ForkOptions {
                    position: Some(maho_ext_api::ForkPosition::Before),
                    with_session: None,
                },
            )
            .await
            .expect("fork before");
        assert!(!before.cancelled);
        assert_eq!(before.editor_text.as_deref(), Some("selected text"));
        assert!(runtime.session().messages().is_empty());
        let entry = runtime.session().with_session_manager_mut(|manager| {
            manager.append_message(serde_json::json!({
                "role":"user","content":[{"type":"text","text":"retained text"}],"timestamp":0
            }))
        });
        let at = runtime
            .fork(
                entry["id"].as_str().unwrap(),
                maho_ext_api::ForkOptions {
                    position: Some(maho_ext_api::ForkPosition::At),
                    with_session: None,
                },
            )
            .await
            .expect("fork at");
        assert!(!at.cancelled);
        assert!(at.editor_text.is_none());
        assert_eq!(runtime.session().messages().len(), 1);
        assert!(runtime.session().get_tool_definition("retained").is_some());
        let target_id = runtime
            .session()
            .with_session_manager(|manager| manager.leaf_id().unwrap().to_owned());
        let label = runtime
            .session()
            .with_session_manager_mut(|manager| manager.append_label(&target_id, Some("old label")))
            .expect("label");
        let descendant = runtime.session().with_session_manager_mut(|manager| {
            manager.append_message(serde_json::json!({
                "role":"user","content":[{"type":"text","text":"descendant"}],"timestamp":0
            }))
        });
        runtime
            .session()
            .with_session_manager_mut(|manager| {
                manager.append_label(&target_id, Some("resolved label"))
            })
            .expect("label");
        let forked = runtime
            .fork(
                descendant["id"].as_str().unwrap(),
                maho_ext_api::ForkOptions {
                    position: Some(maho_ext_api::ForkPosition::At),
                    with_session: None,
                },
            )
            .await
            .expect("labeled fork");
        assert!(!forked.cancelled);
        runtime.session().with_session_manager(|manager| {
            assert_eq!(manager.label(&target_id), Some("resolved label"));
            assert!(manager.entry(label["id"].as_str().unwrap()).is_none());
            let child = manager.entry(descendant["id"].as_str().unwrap()).unwrap();
            assert_eq!(child["parentId"], target_id);
            assert_eq!(
                manager
                    .current_branch()
                    .iter()
                    .filter(|entry| entry["type"] == "message")
                    .count(),
                2
            );
        });
        assert!(
            matches!(runtime.import_from_jsonl("/missing-import-fixture.jsonl", None).await.unwrap_err(),
            AgentSessionRuntimeError::ImportFileNotFound(error) if error.to_string().contains("missing-import-fixture.jsonl"))
        );
        let source = dir.path().join("import-fixture.jsonl");
        runtime
            .session()
            .export_to_jsonl(source.to_str())
            .expect("export fixture");
        let import_directory = dir.path().join("imported");
        runtime.session().with_session_manager_mut(|manager| {
            *manager = crate::session_manager::SessionManager::create(
                &cwd,
                import_directory.to_str(),
                None,
            );
        });
        assert!(
            runtime
                .import_from_jsonl(source.to_str().unwrap(), None)
                .await
                .expect("import")
        );
        let first_import = runtime.session().session_file().expect("imported path");
        assert!(first_import.ends_with("import-fixture.jsonl"));
        assert_eq!(runtime.session().messages().len(), 2);
        assert!(
            runtime
                .import_from_jsonl(source.to_str().unwrap(), None)
                .await
                .expect("collision import")
        );
        assert!(
            runtime
                .session()
                .session_file()
                .unwrap()
                .ends_with("import-fixture-1.jsonl")
        );
    }

    fn recording_extension(
        session: &AgentSession,
        path: &str,
        kinds: &[maho_ext_api::EventKind],
        seen: std::sync::Arc<std::sync::Mutex<Vec<maho_ext_api::EventKind>>>,
    ) -> maho_ext_api::LoadedExtension {
        let mut extension =
            maho_ext_api::LoadedExtension::new(path, session.cwd().into(), Default::default());
        for kind in kinds {
            let seen = seen.clone();
            let kind = *kind;
            extension.handlers.insert(
                kind,
                vec![std::sync::Arc::new(move |_, _| {
                    seen.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(kind);
                    Box::pin(async { Ok(maho_ext_api::EventResult::None) })
                })],
            );
        }
        extension
    }

    async fn runtime_fixture() -> (tempfile::TempDir, AgentSessionRuntime) {
        let dir = crate::test_support::isolated_tempdir();
        let cwd = dir.path().to_string_lossy().into_owned();
        let services = crate::agent_session_services::create_agent_session_services(
            crate::agent_session_services::CreateAgentSessionServicesOptions {
                cwd: cwd.clone(),
                agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
                ..Default::default()
            },
        )
        .await
        .expect("valid session services");
        let provider = maho_ai::providers::faux::faux_provider(Default::default())
            .expect("valid faux provider");
        let created = crate::sdk::create_agent_session(crate::sdk::CreateAgentSessionOptions {
            cwd: Some(cwd.clone()),
            agent_dir: Some(services.agent_dir.clone()),
            model: provider.get_model(Some("faux-1")),
            session_manager: Some(crate::session_manager::SessionManager::in_memory(
                &cwd, None, None,
            )),
            tools: Some(Vec::new()),
            ..Default::default()
        })
        .await
        .expect("session");
        (
            dir,
            AgentSessionRuntime::new(created.session, services, Vec::new(), None, None),
        )
    }

    #[tokio::test]
    async fn attachment_events_reach_registered_extensions() {
        let (_dir, runtime) = runtime_fixture().await;
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let extension = recording_extension(
            runtime.session(),
            "<inline:attachment>",
            &[
                maho_ext_api::EventKind::SessionParked,
                maho_ext_api::EventKind::SessionResumed,
            ],
            seen.clone(),
        );
        let context = crate::sdk::extension_context::create(runtime.session());
        runtime
            .session()
            .set_extension_runner(maho_ext_host::ExtensionRunner::new(
                vec![extension],
                Default::default(),
                Default::default(),
                context,
            ))
            .await;
        runtime
            .emit_attachment_event(AttachmentEvent::SessionParked)
            .await;
        runtime
            .emit_attachment_event(AttachmentEvent::SessionResumed)
            .await;
        assert_eq!(
            *seen.lock().expect("seen"),
            [
                maho_ext_api::EventKind::SessionParked,
                maho_ext_api::EventKind::SessionResumed
            ]
        );
    }

    #[tokio::test]
    async fn replacement_reports_extensions_the_new_runner_no_longer_loads() {
        let (_dir, mut runtime) = runtime_fixture().await;
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let extension = recording_extension(
            runtime.session(),
            "<inline:removed>",
            &[maho_ext_api::EventKind::SessionExtensionsRemoved],
            seen.clone(),
        );
        let context = crate::sdk::extension_context::create(runtime.session());
        let mut runner = maho_ext_host::ExtensionRunner::new(
            vec![extension],
            Default::default(),
            Default::default(),
            context,
        );
        runner.set_runtime_factory(std::sync::Arc::new(|context| {
            Box::pin(async move {
                Ok(maho_ext_host::ExtensionRunner::new(
                    Vec::new(),
                    Default::default(),
                    Default::default(),
                    context,
                ))
            })
        }));
        runtime.session().set_extension_runner(runner).await;
        assert!(
            runtime
                .new_session(Default::default())
                .await
                .expect("replacement")
        );
        assert_eq!(
            *seen.lock().expect("seen"),
            [maho_ext_api::EventKind::SessionExtensionsRemoved]
        );
    }

    #[tokio::test]
    async fn with_session_runs_against_the_replacement_and_switch_cwd_errors_stay_typed() {
        let (dir, mut runtime) = runtime_fixture().await;
        let context = crate::sdk::extension_context::create(runtime.session());
        let mut runner = maho_ext_host::ExtensionRunner::new(
            Vec::new(),
            Default::default(),
            Default::default(),
            context,
        );
        runner.set_runtime_factory(std::sync::Arc::new(|context| {
            Box::pin(async move {
                Ok(maho_ext_host::ExtensionRunner::new(
                    Vec::new(),
                    Default::default(),
                    Default::default(),
                    context,
                ))
            })
        }));
        runtime.session().set_extension_runner(runner).await;
        let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
        let captured = seen.clone();
        let with_session: maho_ext_api::WithSession = std::sync::Arc::new(move |ctx| {
            *captured
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                Some(ctx.session_manager.session_id().to_owned());
            Box::pin(async { Ok(()) })
        });
        let previous = runtime.session().session_id();
        assert!(
            runtime
                .new_session(maho_ext_api::NewSessionOptions {
                    with_session: Some(with_session),
                    ..Default::default()
                })
                .await
                .expect("new")
        );
        assert_ne!(runtime.session().session_id(), previous);
        assert_eq!(
            *seen.lock().expect("seen"),
            Some(runtime.session().session_id())
        );

        let gone = dir.path().join("gone");
        let mut stored = crate::session_manager::SessionManager::create(
            &gone.to_string_lossy(),
            dir.path().join("stored").to_str(),
            None,
        );
        stored.append_message(serde_json::json!({"role":"user","content":"saved","timestamp":0}));
        stored.append_message(
            serde_json::to_value(maho_ai::providers::faux::faux_assistant_message(
                "answer",
                Default::default(),
            ))
            .expect("assistant"),
        );
        let path = stored.session_file().expect("file").to_owned();
        let error = runtime
            .switch_session(&path, SwitchSessionOptions::default())
            .await
            .expect_err("missing cwd");
        assert!(matches!(
            error,
            AgentSessionRuntimeError::MissingSessionCwd(_)
        ));
        let cwd = runtime.cwd().to_owned();
        assert!(
            runtime
                .switch_session(
                    &path,
                    SwitchSessionOptions {
                        cwd_override: Some(cwd.clone()),
                        with_session: None
                    }
                )
                .await
                .expect("override")
        );
        assert_eq!(
            runtime.session().session_file().as_deref(),
            Some(path.as_str())
        );
        assert_eq!(runtime.session().cwd(), cwd);
    }

    #[test]
    fn user_message_text_joins_text_parts_and_skips_others() {
        let content = serde_json::json!([
            { "type": "text", "text": "hello " },
            { "type": "image", "data": "x" },
            { "type": "text", "text": "world" }
        ]);
        assert_eq!(extract_user_message_text(&content), "hello world");
    }

    #[test]
    fn a_string_content_is_returned_verbatim() {
        assert_eq!(
            extract_user_message_text(&serde_json::json!("plain")),
            "plain"
        );
    }
}
