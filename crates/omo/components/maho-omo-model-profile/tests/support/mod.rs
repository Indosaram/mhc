use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use maho_ai::model::Model;
use maho_ai::types::{ModelThinkingLevel, ThinkingLevel};
use maho_ext_api::{
    BuildSystemPromptOptions, ComponentFactory, CredentialAccountSummary, CustomMessage,
    CustomUiOptions, EventBus, EventKind, EventResult, ExecuteToolFuture, ExecuteToolOptions,
    ExecOptions, ExecResult, ExtensionActions, ExtensionApi, ExtensionContext, ExtensionEvent,
    ExtensionFailure, ExtensionFuture, ExtensionMode, ExtensionRuntime,
    ExtensionSessionActions, ExtensionSessionProfile, ExtensionUi, ExtensionUiDialogOptions,
    ExtensionWidgetOptions, JsonValue, LazyToolActivator, LoadedExtension, ModelRegistry,
    NotificationType, ProviderAuthStatus, ResolvedRequestAuth, SendMessageOptions, SendUserMessageOptions, SessionEntry,
    SessionManager, SessionReason, SlashCommandInfo, SourceInfo, Theme, ToolInfo,
    ToolSessionManager, UiFuture, WidgetContent,
};

pub const PRIVATE_MARKER: &str = "SYNTHETIC_PRIVATE_MARKER_9f8e7d6c5b4a";
pub const SHORT_SECRET: &str = "sk-short-7q";

/// The pinned engine's builtin provider ids (port of senpi `builtinProviders()`), pending an engine
/// registry accessor on this side. A builtin profile may name only these or an allow-listed alias.
pub const ENGINE_BUILTIN_PROVIDER_IDS: &[&str] = &[
    "anthropic",
    "chatgpt-subscription",
    "github-copilot",
    "moonshotai",
    "openai",
    "opencode",
    "opencode-go",
    "zai",
    "zai-coding-cn",
];

/// Provider ids the builtin profiles may name even though they are not engine builtins. A new
/// unknown id fails the test unless it is added here with a reason. OpenCode's own tables are not
/// shared with these arrays; each keep is for a leftover OpenCode-id key or a senpi extension lane.
pub const CHAIN_PROVIDER_ID_ALLOWLIST: &[(&str, &str)] = &[
    // senpi Claude Pro/Max extension lane; not a pi-ai builtin.
    ("anthropic-subscription", "senpi Claude subscription extension"),
    // OpenCode/models.dev Anthropic API-key id. setup maps it to `anthropic`; Claude rungs keep it
    // so a leftover key still matches.
    ("anthropic-api", "OpenCode anthropic API-key alias"),
    // senpi-only Kimi engine lane.
    ("kimi-coding", "senpi Kimi coding engine lane"),
    // OpenCode/models.dev Kimi Code id. senpi-task category chains carry both `kimi-coding` (engine)
    // and this alias; builtin profiles copy that pair.
    ("kimi-for-coding", "OpenCode kimi alias kept next to engine kimi-coding"),
];

pub fn is_known_provider(provider: &str) -> bool {
    ENGINE_BUILTIN_PROVIDER_IDS.contains(&provider)
        || CHAIN_PROVIDER_ID_ALLOWLIST.iter().any(|(id, _)| *id == provider)
}

pub fn model(provider: &str, id: &str) -> Model {
    serde_json::from_value(serde_json::json!({
        "id": id, "name": id, "api": "openai-completions", "provider": provider,
        "baseUrl": "https://example.invalid", "reasoning": false, "input": ["text"],
        "cost": {"input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0},
        "contextWindow": 1, "maxTokens": 1
    }))
    .expect("model fixture")
}

pub fn selector(provider: &str, id: &str) -> String {
    format!("{provider}/{id}")
}

#[derive(Default)]
pub struct TestRegistry {
    pub models: Vec<Model>,
    /// Provider-scope failures keyed by provider, then by slot name (`None` = the flat/default
    /// credential, senpi's `{}`). This is the REAL slot rotation the probe must exercise: a named
    /// slot can fail while a sibling resolves.
    pub dead_slots: BTreeMap<String, BTreeMap<Option<String>, ExtensionFailure>>,
    pub broken_models: BTreeMap<String, ExtensionFailure>,
    pub accounts: BTreeMap<String, Vec<CredentialAccountSummary>>,
    pub auth_status: BTreeMap<String, ProviderAuthStatus>,
    pub calls: Mutex<Vec<String>>,
}

// senpi maps a rejected refresh onto ModelsError code "oauth" whose message carries the exchange
// detail (request headers, URL, response body).
pub fn refresh_rejected(provider: &str) -> String {
    format!(
        "OAuth refresh failed for {provider}: Authorization: Bearer {SHORT_SECRET} {{\"access_token\":\"{SHORT_SECRET}\"}} url=https://example.invalid/oauth/token?refresh_token={SHORT_SECRET}; details=Error: body={{\"error\":\"invalid_grant\",\"marker\":\"{PRIVATE_MARKER}\"}}"
    )
}

// A model header whose `!command` fails: senpi quotes the command in the message.
pub fn request_configuration_failed(provider: &str, id: &str) -> String {
    format!("Failed to resolve model \"{provider}/{id}\" header x-token from shell command: echo {PRIVATE_MARKER} {SHORT_SECRET}")
}

/// senpi's `ModelsError` with code `oauth`: the typed class/code the fingerprint must read.
pub fn oauth_failure(message: impl Into<String>) -> ExtensionFailure {
    ExtensionFailure {
        class: Some("ModelsError".to_owned()),
        code: Some("oauth".to_owned()),
        ..ExtensionFailure::new(message)
    }
}

/// A `ModelsError` without the `oauth` code: any other resolution failure.
pub fn credential_failure(message: impl Into<String>) -> ExtensionFailure {
    ExtensionFailure { class: Some("ModelsError".to_owned()), ..ExtensionFailure::new(message) }
}

pub fn account(name: &str, pinned: bool) -> CredentialAccountSummary {
    CredentialAccountSummary {
        name: name.to_owned(),
        display_name: None,
        source: maho_ext_api::CredentialAccountSource::Login,
        blocked: false,
        pinned,
    }
}

impl TestRegistry {
    pub fn new(models: Vec<Model>) -> Self {
        Self { models, ..Default::default() }
    }

    /// Every slot of the provider fails, the flat/default credential included.
    pub fn dead_provider(mut self, provider: &str) -> Self {
        self.dead_slots.insert(
            provider.to_owned(),
            BTreeMap::from([(Option::<String>::None, oauth_failure(refresh_rejected(provider)))]),
        );
        self
    }

    /// Only one named slot fails; the flat/default credential and every sibling stay healthy.
    pub fn dead_slot(mut self, provider: &str, slot: &str) -> Self {
        self.dead_slots
            .entry(provider.to_owned())
            .or_default()
            .insert(Some(slot.to_owned()), oauth_failure(refresh_rejected(provider)));
        self
    }

    pub fn broken_model(mut self, provider: &str, id: &str) -> Self {
        self.broken_models
            .insert(selector(provider, id), credential_failure(request_configuration_failed(provider, id)));
        self
    }

    pub fn with_accounts(mut self, provider: &str, accounts: Vec<CredentialAccountSummary>) -> Self {
        self.accounts.insert(provider.to_owned(), accounts);
        self
    }

    /// senpi `registry.getProviderAuthStatus(provider)`: `source: "runtime"` turns rotation off.
    pub fn with_auth_status(mut self, provider: &str, status: ProviderAuthStatus) -> Self {
        self.auth_status.insert(provider.to_owned(), status);
        self
    }

    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }
}

impl ModelRegistry for TestRegistry {
    fn get_all(&self) -> Vec<Model> {
        self.models.clone()
    }
    fn get_available(&self) -> Vec<Model> {
        self.models.clone()
    }
    fn find(&self, provider: &str, id: &str) -> Option<Model> {
        self.models
            .iter()
            .find(|model| model.provider == provider && model.id == id)
            .cloned()
    }
    fn has_configured_auth(&self, _model: &Model) -> bool {
        true
    }
    fn get_api_key_for_provider<'a>(&'a self, _provider: &'a str) -> ExtensionFuture<'a, Option<String>> {
        Box::pin(async { Ok(None) })
    }
    /// senpi `runtime.getAuth(provider, { slotName })`: the probe's provider scope. A named slot
    /// records `provider#slot`; the flat credential records `provider`.
    fn get_provider_auth_for_slot<'a>(
        &'a self,
        provider: &'a str,
        slot_name: Option<&'a str>,
    ) -> ExtensionFuture<'a, Option<maho_ai::models::AuthResolution>> {
        let call = slot_name.map_or_else(|| provider.to_owned(), |slot| format!("{provider}#{slot}"));
        self.calls.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(call);
        let failure = self
            .dead_slots
            .get(provider)
            .and_then(|slots| slots.get(&slot_name.map(str::to_owned)))
            .cloned();
        Box::pin(async move {
            match failure {
                Some(error) => Err(error),
                None => Ok(None),
            }
        })
    }
    /// senpi `runtime.getAuth(model, { slotName })`: the model scope, carrying the resolved slot.
    fn get_api_key_and_headers_for_slot<'a>(
        &'a self,
        model: &'a Model,
        slot_name: Option<&'a str>,
    ) -> ExtensionFuture<'a, ResolvedRequestAuth> {
        let key = selector(&model.provider, &model.id);
        let call = slot_name.map_or_else(|| key.clone(), |slot| format!("{key}#{slot}"));
        self.calls.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(call);
        let failure = self.broken_models.get(&key).cloned();
        Box::pin(async move {
            match failure {
                Some(error) => Err(error),
                None => Ok(ResolvedRequestAuth {
                    auth: maho_ai::models::ProviderAuthResult::default(),
                    extra_body: None,
                    upstream_model_id: None,
                    service_tier: None,
                    env: None,
                }),
            }
        })
    }
    fn get_credential_accounts<'a>(
        &'a self,
        provider: &'a str,
    ) -> ExtensionFuture<'a, Vec<CredentialAccountSummary>> {
        let accounts = self.accounts.get(provider).cloned().unwrap_or_default();
        Box::pin(async move { Ok(accounts) })
    }
    /// senpi `registry.getProviderAuthStatus(provider)`.
    fn get_provider_auth_status(&self, provider: &str) -> ProviderAuthStatus {
        self.auth_status.get(provider).cloned().unwrap_or_default()
    }
}

struct TestSession {
    id: String,
}
impl ToolSessionManager for TestSession {
    fn session_id(&self) -> &str {
        &self.id
    }
    fn session_file(&self) -> Option<&Path> {
        None
    }
}
impl SessionManager for TestSession {
    fn get_entries(&self) -> Vec<SessionEntry> {
        Vec::new()
    }
    fn get_branch(&self) -> Vec<SessionEntry> {
        Vec::new()
    }
    fn get_leaf_id(&self) -> Option<String> {
        None
    }
    fn get_session_name(&self) -> Option<String> {
        None
    }
}

#[derive(Default)]
pub struct RecordingSessionActions {
    pub session_models: Mutex<Vec<Model>>,
    pub session_thinking_levels: Mutex<Vec<ModelThinkingLevel>>,
    pub durable_thinking_levels: Mutex<Vec<ModelThinkingLevel>>,
}
impl ExtensionSessionActions for RecordingSessionActions {
    fn set_session_name(&self, _: &str) -> Result<(), ExtensionFailure> {
        Err("unused".into())
    }
    fn get_session_name(&self) -> Result<Option<String>, ExtensionFailure> {
        Err("unused".into())
    }
    fn set_label(&self, _: &str, _: Option<&str>) -> Result<(), ExtensionFailure> {
        Err("unused".into())
    }
    fn execute_tool<'a>(&'a self, name: &'a str, _: JsonValue, _: ExecuteToolOptions) -> ExecuteToolFuture<'a> {
        Box::pin(async move {
            Err(maho_ext_api::ExecuteToolError {
                code: maho_ext_api::ExecuteToolErrorCode::UnknownTool,
                tool_name: name.into(),
                message: "unused".into(),
                active_tools: Vec::new(),
            })
        })
    }
    fn get_active_tools(&self) -> Result<Vec<String>, ExtensionFailure> {
        Ok(Vec::new())
    }
    fn set_active_tools(&self, _: Vec<String>) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn refresh_tools(&self) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn register_removed_tool_hint(&self, _: &str, _: &str) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn register_lazy_tool_activator(&self, _: LazyToolActivator) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn get_commands(&self) -> Result<Vec<SlashCommandInfo>, ExtensionFailure> {
        Ok(Vec::new())
    }
    fn set_model(&self, _: Model) -> ExtensionFuture<'_, bool> {
        Box::pin(async { Err("unused".into()) })
    }
    fn get_thinking_level(&self) -> Result<ThinkingLevel, ExtensionFailure> {
        Ok(ThinkingLevel::Medium)
    }
    fn set_thinking_level(&self, _: ThinkingLevel) -> Result<(), ExtensionFailure> {
        Err("unused".into())
    }
    fn set_session_model(&self, model: Model) -> ExtensionFuture<'_, bool> {
        self.session_models.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(model);
        Box::pin(async { Ok(true) })
    }
    fn set_session_thinking_level(&self, _: ThinkingLevel) -> Result<(), ExtensionFailure> {
        Err("unused".into())
    }
    fn set_model_thinking_level(&self, level: ModelThinkingLevel) -> Result<(), ExtensionFailure> {
        self.durable_thinking_levels.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(level);
        Ok(())
    }
    fn set_session_model_thinking_level(&self, level: ModelThinkingLevel) -> Result<(), ExtensionFailure> {
        self.session_thinking_levels.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(level);
        Ok(())
    }
    fn set_session_fast_mode(&self, _: bool) -> Result<(), ExtensionFailure> {
        Err("unused".into())
    }
    fn exec<'a>(&'a self, _: &'a str, _: &'a [String], _: &'a Path, _: ExecOptions) -> ExtensionFuture<'a, ExecResult> {
        Box::pin(async { Err("unused".into()) })
    }
}

#[derive(Default)]
pub struct RecordingActions {
    pub messages: Mutex<Vec<CustomMessage>>,
}
impl ExtensionActions for RecordingActions {
    fn send_message(&self, message: CustomMessage, _: SendMessageOptions) -> Result<(), ExtensionFailure> {
        self.messages.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(message);
        Ok(())
    }
    fn send_user_message(&self, _: maho_ext_api::UserMessageContent, _: SendUserMessageOptions) -> Result<(), ExtensionFailure> {
        Err("unused".into())
    }
    fn append_entry(&self, _: &str, _: Option<JsonValue>) -> Result<(), ExtensionFailure> {
        Err("unused".into())
    }
    fn get_all_tools(&self) -> Result<Vec<ToolInfo>, ExtensionFailure> {
        Ok(Vec::new())
    }
}

struct TestUi;
impl ExtensionUi for TestUi {
    fn select<'a>(&'a self, _: &'a str, _: &'a [String], _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn confirm<'a>(&'a self, _: &'a str, _: &'a str, _: ExtensionUiDialogOptions) -> UiFuture<'a, bool> {
        Box::pin(async { false })
    }
    fn input<'a>(&'a self, _: &'a str, _: Option<&'a str>, _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn notify(&self, _: &str, _: NotificationType) {}
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(&self, _: &str, _: Option<WidgetContent>, _: ExtensionWidgetOptions) {}
    fn set_header(&self, _: Option<ComponentFactory>) {}
    fn set_footer(&self, _: Option<ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String {
        String::new()
    }
    fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> {
        Box::pin(async { Err("UI not available".into()) })
    }
    fn theme(&self) -> Theme {
        Theme::default()
    }
}

#[derive(Default)]
pub struct RecordingLogger {
    pub lines: Mutex<Vec<String>>,
}
impl maho_ext_api::ComponentLogger for RecordingLogger {
    fn info(&self, message: &str, _: Option<&JsonValue>) {
        self.lines.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(format!("info:{message}"));
    }
    fn warn(&self, message: &str, _: Option<&JsonValue>) {
        self.lines.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(format!("warn:{message}"));
    }
    fn error(&self, message: &str, _: Option<&JsonValue>) {
        self.lines.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(format!("error:{message}"));
    }
}
impl RecordingLogger {
    pub fn lines(&self) -> Vec<String> {
        self.lines.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }
}

pub struct Harness {
    pub api: ExtensionApi,
    pub ctx: ExtensionContext,
    pub registry: Arc<TestRegistry>,
    pub session_actions: Arc<RecordingSessionActions>,
    pub actions: Arc<RecordingActions>,
    pub logger: Arc<RecordingLogger>,
    pub agent_dir: PathBuf,
    pub agent_dir_guard: Option<tempfile::TempDir>,
}

pub fn context_with(
    registry: Arc<TestRegistry>,
    session_id: &str,
    mode: ExtensionMode,
    cwd: &str,
    agent_dir: PathBuf,
    logger: Arc<RecordingLogger>,
) -> ExtensionContext {
    ExtensionContext {
        ui: Arc::new(TestUi),
        mode,
        has_ui: false,
        cwd: cwd.into(),
        agent_dir,
        session_manager: Arc::new(TestSession { id: session_id.to_owned() }),
        model_registry: registry,
        model: None,
        thinking_level: None,
        service_tier: None,
        effective_service_tier: None,
        scoped_models: Vec::new(),
        goal_store_file: None,
        loaded_extension_paths: Vec::new(),
        signal: None,
        steering_signal: None,
        is_idle_fn: Arc::new(|| true),
        wait_for_idle_fn: Arc::new(|| Box::pin(async {})),
        is_project_trusted_fn: Arc::new(|| true),
        is_compacting_fn: Arc::new(|| false),
        get_system_prompt_fn: Arc::new(|| "base".into()),
        get_system_prompt_options_fn: Arc::new(|| BuildSystemPromptOptions { cwd: "/tmp".into(), ..Default::default() }),
        registered_mcp_servers: Vec::new(),
        update_tool_hook_status: None,
        idle_coordinator: None,
        logger: Some(logger),
        defer_macrotask: None,
        compaction_signal: Default::default(),
    }
}

pub fn harness(
    component: maho_omo_model_profile::ModelProfileComponent,
    registry: Arc<TestRegistry>,
    session_id: &str,
    mode: ExtensionMode,
    cwd: &str,
    agent_dir: PathBuf,
) -> Harness {
    let runtime = ExtensionRuntime::default();
    let session_actions = Arc::new(RecordingSessionActions::default());
    let actions = Arc::new(RecordingActions::default());
    runtime.bind_session_actions(session_actions.clone());
    runtime.bind(actions.clone());
    let mut api = ExtensionApi::new(
        LoadedExtension::new("model-profile", "/tmp".into(), SourceInfo::default()),
        ExtensionSessionProfile::default(),
        EventBus::default(),
        runtime,
    );
    use maho_ext_api::Extension;
    component.register(&mut api);
    let logger = Arc::new(RecordingLogger::default());
    let ctx = context_with(registry.clone(), session_id, mode, cwd, agent_dir.clone(), logger.clone());
    Harness { api, ctx, registry, session_actions, actions, logger, agent_dir, agent_dir_guard: None }
}

pub fn harness_in(
    component: maho_omo_model_profile::ModelProfileComponent,
    registry: Arc<TestRegistry>,
    session_id: &str,
    mode: ExtensionMode,
    cwd: &str,
    agent_dir: tempfile::TempDir,
) -> Harness {
    let agent_dir_path = agent_dir.path().to_path_buf();
    let mut harness = harness(component, registry, session_id, mode, cwd, agent_dir_path);
    harness.agent_dir_guard = Some(agent_dir);
    harness
}

impl Harness {
    pub async fn start(&self, reason: SessionReason, provenance: Option<&str>) -> EventResult {
        let mut event = ExtensionEvent::SessionStart(maho_ext_api::SessionStartEvent {
            reason,
            initial_model_provenance: provenance.map(str::to_owned),
            previous_session_file: None,
        });
        self.api.registered.handlers[&EventKind::SessionStart][0](&mut event, &self.ctx)
            .await
            .expect("test dispatch")
    }

    pub fn session_models(&self) -> Vec<Model> {
        self.session_actions.session_models.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    pub fn session_thinking_levels(&self) -> Vec<ModelThinkingLevel> {
        self.session_actions.session_thinking_levels.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    pub fn durable_thinking_levels(&self) -> Vec<ModelThinkingLevel> {
        self.session_actions.durable_thinking_levels.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    pub fn messages(&self) -> Vec<CustomMessage> {
        self.actions.messages.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    pub fn first_details(&self) -> JsonValue {
        self.messages().first().and_then(|message| message.details.clone()).unwrap_or(JsonValue::Null)
    }

    pub fn first_custom_type(&self) -> String {
        self.messages().first().map(|message| message.custom_type.clone()).unwrap_or_default()
    }
}

pub fn config_result(value: JsonValue) -> maho_omo_config_resolution::SenpiOmoConfigResult {
    maho_omo_config_resolution::SenpiOmoConfigResult {
        loaded: omo_config_core::LoadOmoConfigResult {
            config: serde_json::Map::new(),
            diagnostics: Vec::new(),
            layers: Vec::new(),
            profile: None,
            sources: Vec::new(),
        },
        config: value,
        diagnostics: Vec::new(),
    }
}
