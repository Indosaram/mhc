//! Port of senpi `packages/coding-agent/src/core/agent-session.ts`.
//!
//! Adaptations forced by the Rust tree (see AGENTS.md, plan D-M5):
//! - `AgentSessionEvent` is owned by `maho-ext-api` and imported, never redefined here.
//! - senpi's `resourceLoader` (the TS extension/resource host) has no Rust counterpart because
//!   extensions are native crates registered statically in maho-cli; the constructor reads the
//!   runtime flag values from [`AgentSessionConfig::flag_values`] instead.
//! - The class is a `&self` API over interior mutability so the agent event listener can call back
//!   into the session, as the TS arrow-function handlers do.
//!
//! Ported in slices S1..S8; the `agent-session.ts` ledger row in `parity.d/21.md` records what is
//! still outstanding.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use maho_agent::tool_name_alias::resolve_tool_name_alias;
use maho_agent::types::{AgentMessage, AgentTool, AgentToolResult, AgentState};
use maho_agent::Agent;
use maho_ai::model::Model;
use maho_ai::models::models_are_equal;
use maho_ai::types::{
    ImageContent, ModelThinkingLevel, ProviderEnv, ProviderHeaders, ServiceTierPreference, StopReason,
    ThinkingLevel, ThinkingSelection,
};
use maho_ext_api::{
    AgentSessionEvent, CompactionRejectionCause, ExtensionError, ExtensionMode, ExtensionUi, FlagValue,
    InputSource, ServiceTier, SessionReason, SessionStartEvent, SourceInfo, SourceOrigin, SourceScope,
    StreamingBehavior, ToolCallEvent, ToolDefinition, ToolExposure, ToolInfo, normalize_tool_exposure,
};
use maho_ext_host::ExtensionRunner;
use serde_json::{Map, Value};

use crate::event_bus::{EventBus, EventHandler, EventSubscription};

use crate::model_registry::ModelRegistry;
use crate::model_runtime::ModelRuntime;
use crate::session_activity::{SessionActivitySnapshot, WakeSourceTracker, is_session_busy_snapshot};
use crate::session_log::{SessionLogger, SessionLoggerOptions};
use crate::session_manager::SessionManager;
use crate::settings_manager::SettingsManager;

/// Sample eval-cell call for an eval-only tool, using the argument name that tool actually takes.
/// Sample eval-cell call for an eval-only tool, using the argument name that tool actually takes.
const EVAL_ONLY_TOOL_NAMES: [&str; 2] = ["workflow", "monitor"];

#[allow(dead_code)] // consumed by the eval-only hint publisher in the tool-registry slice
fn eval_helper_call(name: &str) -> String {
    match name {
        "bash" | "powershell" => format!("tool.{name}({{ command: \"...\" }})"),
        "grep" => "tool.grep({ pattern: \"...\", path: \"...\" })".to_owned(),
        "workflow" => format!("tool.{name}({{ action: \"...\" }})"),
        "monitor" => "tool.monitor({ description: \"...\", command: \"...\", filter: \"...\" })".to_owned(),
        _ => format!("tool.{name}({{ ... }})"),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandInvocationSource {
    Extension,
    Prompt,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CommandInvocation {
    pub name: String,
    pub source: CommandInvocationSource,
    pub source_info: SourceInfo,
}

#[derive(Clone, Debug)]
pub struct SessionModelEntry {
    pub model: Model,
    pub thinking_level: Option<ThinkingLevel>,
    pub thinking_selection: Option<ThinkingSelection>,
    pub service_tier: Option<ServiceTier>,
}

pub struct AgentSessionConfig {
    pub agent: Agent,
    pub session_manager: SessionManager,
    pub settings_manager: SettingsManager,
    pub cwd: String,
    pub agent_dir: Option<String>,
    pub fallback_now: Option<Arc<dyn Fn() -> f64 + Send + Sync>>,
    pub retry_random: Option<Arc<dyn Fn() -> f64 + Send + Sync>>,
    pub scoped_models: Vec<SessionModelEntry>,
    pub favorite_models: Vec<SessionModelEntry>,
    pub flag_values: BTreeMap<String, FlagValue>,
    pub custom_tools: Vec<ToolDefinition>,
    pub model_runtime: Option<ModelRuntime>,
    pub model_registry: Option<ModelRegistry>,
    /// Whether the agent streams through the default `streamSimple` (senpi reads
    /// `agent.streamFunction === streamSimple`; the Rust agent exposes no getter, so the host
    /// states it). Defaults to true, matching senpi's default stream function.
    pub uses_default_stream_function: Option<bool>,
    pub initial_active_tool_names: Option<Vec<String>>,
    pub default_tool_names: Option<Vec<String>>,
    pub eval_only_tool_names: Option<Vec<String>>,
    pub allowed_tool_names: Option<Vec<String>>,
    pub excluded_tool_names: Option<Vec<String>>,
    pub base_tools_override: Option<BTreeMap<String, AgentTool>>,
    pub session_start_event: Option<SessionStartEvent>,
    pub auto_title_sessions: Option<bool>,
}

pub type ExtensionErrorListener = Arc<dyn Fn(&ExtensionError) + Send + Sync>;

#[derive(Clone, Default)]
pub struct ExtensionBindings {
    pub ui_context: Option<Arc<dyn ExtensionUi>>,
    pub mode: Option<ExtensionMode>,
    pub abort_handler: Option<Arc<dyn Fn() + Send + Sync>>,
    pub on_error: Option<ExtensionErrorListener>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeNavigationIntent {
    Select,
    Resume,
}

#[derive(Clone, Debug, Default)]
pub struct TreeNavigationOptions {
    pub intent: Option<TreeNavigationIntent>,
    pub summarize: Option<bool>,
    pub custom_instructions: Option<String>,
    pub replace_instructions: Option<bool>,
    pub label: Option<String>,
    pub expected_leaf_id: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AssistantEditResult {
    pub editor_text: Option<String>,
    pub cancelled: bool,
    pub aborted: Option<bool>,
    pub summary_entry: Option<Value>,
    pub unchanged: Option<bool>,
    pub entry_id: Option<String>,
}

pub type UserEditResult = AssistantEditResult;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptDisposition {
    Handled,
    Queued,
    Started,
}

#[derive(Clone, Debug, PartialEq)]
pub struct QueuedInput {
    pub text: String,
    pub mode: StreamingBehavior,
    pub enqueue_order: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClearedQueue {
    pub steering: Vec<String>,
    pub follow_up: Vec<String>,
    pub ordered: Vec<QueuedInput>,
}

#[derive(Clone, Debug, Default)]
pub struct QueuedInputOptions {
    pub enqueue_order: Option<u64>,
    pub source: Option<InputSource>,
}

#[derive(Clone)]
pub struct ToolDefinitionEntry {
    pub definition: ToolDefinition,
    pub source_info: SourceInfo,
}

pub type LazyToolActivator = Arc<dyn Fn(&str) -> bool + Send + Sync>;

pub type AgentSessionEventListener = Arc<dyn Fn(&AgentSessionEvent) + Send + Sync>;

/// Unsubscribes its listener when dropped, matching the function `subscribe` returns in senpi.
pub struct AgentSessionSubscription {
    listeners: Arc<Mutex<Vec<(u64, AgentSessionEventListener)>>>,
    id: u64,
}

impl Drop for AgentSessionSubscription {
    fn drop(&mut self) {
        self.listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|(id, _)| *id != self.id);
    }
}

#[derive(Clone, Default)]
pub struct ExecuteToolOptions {
    pub signal: Option<maho_ai::utils::abort::AbortSignal>,
    pub activate_inactive_tool: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecuteToolError {
    pub code: String,
    pub tool_name: String,
    pub message: String,
    pub active_tools: Vec<String>,
}

impl std::fmt::Display for ExecuteToolError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ExecuteToolError {}

#[derive(Clone, Default)]
pub struct PromptOptions {
    pub expand_prompt_templates: Option<bool>,
    pub images: Option<Vec<ImageContent>>,
    pub streaming_behavior: Option<StreamingBehavior>,
    pub thinking_level: Option<ThinkingLevel>,
    pub source: Option<InputSource>,
    pub signal: Option<maho_ai::utils::abort::AbortSignal>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SystemPromptChangeEvent {
    pub system_prompt: String,
    pub previous_system_prompt: String,
    pub system_prompt_name: Option<String>,
    pub model: Model,
    pub previous_model: Option<Model>,
}

#[derive(Clone, Debug)]
pub struct ModelCycleResult {
    pub model: Model,
    pub thinking_level: ThinkingLevel,
    pub is_scoped: bool,
    pub skipped_models: Vec<Model>,
    pub system_prompt_change: Option<SystemPromptChangeEvent>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContextUsage {
    pub tokens: Option<u64>,
    pub context_window: u64,
    pub percent: Option<f64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SessionStatsTokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub total: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionStats {
    pub session_file: Option<String>,
    pub session_id: String,
    pub user_messages: usize,
    pub assistant_messages: usize,
    pub tool_calls: usize,
    pub tool_results: usize,
    pub total_messages: usize,
    pub tokens: SessionStatsTokens,
    pub cost: f64,
    pub context_usage: Option<ContextUsage>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompactionRejectedError {
    pub rejection_cause: CompactionRejectionCause,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompactionCancelledError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompactionExecutionError {
    pub message: String,
    pub owns_terminal_transition: bool,
    pub aborted: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequiredCompactionError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MissingModelAccessError;

impl std::fmt::Display for MissingModelAccessError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("AgentSession requires modelRuntime or modelRegistry")
    }
}

impl std::error::Error for MissingModelAccessError {}

/// Human-readable message for a rejection cause; exhaustive over the union so a new cause cannot
/// reintroduce a silent failure at the `compaction_end` UI seam.
pub fn describe_compaction_rejection(cause: CompactionRejectionCause) -> &'static str {
    use CompactionRejectionCause as Cause;
    match cause {
        Cause::WouldOverflow => "Compaction rejected: the produced summary would still overflow the model context window. Reduce context (e.g. /new, drop attachments) or switch to a larger-context model.",
        Cause::CancelledByExtension => "Compaction rejected: cancelled by an extension.",
        Cause::ExternalOwner => "Compaction rejected: the active provider owns compaction for this session.",
        Cause::CircuitBreaker => "Compaction rejected: the compaction circuit breaker is open after repeated failures. Wait for the cooldown and retry.",
        Cause::PerTurnCap => "Compaction rejected: absolute compaction cap reached for this session.",
        Cause::StaleRevision => "Compaction rejected: the session changed while the summary was being prepared. Retry compaction against the latest context.",
    }
}

pub fn provider_retry_watchdog_abort_message(
    retry_timeout_ms: Option<u64>,
    stream_start_timeout_ms: Option<u64>,
) -> String {
    let timeout = retry_timeout_ms.map_or_else(|| "undefined".to_owned(), |value| value.to_string());
    match stream_start_timeout_ms {
        None => format!(
            "Provider retry continuation watchdog timed out after {timeout}ms (stream-start guard disabled; raise retry.provider.streamStartTimeoutMs, 0 disables)"
        ),
        Some(stream_start) => format!(
            "Provider retry continuation watchdog timed out after {timeout}ms (stream-start guard: {stream_start}ms; raise retry.provider.streamStartTimeoutMs, 0 disables)"
        ),
    }
}

// Staged for slices S2..S8: each field is populated by construction and consumed by the slice that
// owns the corresponding TS member; the attribute is dropped once every slice lands.
#[allow(dead_code)]
struct AgentSessionState {
    scoped_models: Vec<SessionModelEntry>,
    favorite_models: Vec<SessionModelEntry>,
    cwd: String,
    agent_dir: String,
    custom_tools: Vec<ToolDefinition>,
    initial_active_tool_names: Option<Vec<String>>,
    default_tool_names: Option<BTreeSet<String>>,
    eval_only_tool_names: Option<BTreeSet<String>>,
    allowed_tool_names: Option<BTreeSet<String>>,
    excluded_tool_names: Option<BTreeSet<String>>,
    base_tools_override: Option<BTreeMap<String, AgentTool>>,
    base_tool_definitions: BTreeMap<String, ToolDefinition>,
    tool_registry: BTreeMap<String, AgentTool>,
    tool_definitions: BTreeMap<String, ToolDefinitionEntry>,
    tool_prompt_snippets: BTreeMap<String, String>,
    tool_prompt_guidelines: BTreeMap<String, Vec<String>>,
    lazy_tool_activators: Vec<LazyToolActivator>,
    eval_only_tool_names_override: Option<BTreeSet<String>>,
    withheld_eval_only_tool_names: BTreeSet<String>,
    published_eval_only_hint_names: BTreeSet<String>,
    requested_active_tool_names: Option<Vec<String>>,
    session_start_event: SessionStartEvent,
    auto_title_sessions: bool,
    uses_default_stream_function: bool,
    flag_values: BTreeMap<String, FlagValue>,
    session_fast_mode: bool,
    current_service_tier: Option<ServiceTier>,
    base_system_prompt: String,
    system_prompt_override: Option<String>,
    wake_sources: WakeSourceTracker,
    shown_high_reasoning_warning_keys: BTreeSet<String>,
    extension_mode: ExtensionMode,
    extension_ui_context: Option<Arc<dyn ExtensionUi>>,
    extension_abort_handler: Option<Arc<dyn Fn() + Send + Sync>>,
    extension_error_listener: Option<ExtensionErrorListener>,
    message_revision: u64,
    steering_messages: Vec<String>,
    follow_up_messages: Vec<String>,
    queued_input_order: Vec<QueuedInput>,
    next_queued_input_order: u64,
    post_compaction_deferred_steering_messages: Vec<AgentMessage>,
    post_compaction_deferred_follow_up_messages: Vec<AgentMessage>,
    had_cleared_queued_messages: bool,
}

fn name_set(names: Option<Vec<String>>) -> Option<BTreeSet<String>> {
    names.map(|names| names.into_iter().collect())
}

pub struct AgentSession {
    agent: Agent,
    session_manager: Mutex<SessionManager>,
    settings_manager: Mutex<SettingsManager>,
    model_registry: ModelRegistry,
    state: Mutex<AgentSessionState>,
    extension_runner: tokio::sync::Mutex<Option<ExtensionRunner>>,
    event_bus: EventBus,
    event_listeners: Arc<Mutex<Vec<(u64, AgentSessionEventListener)>>>,
    next_listener_id: Arc<AtomicU64>,
    session_logger: SessionLogger,
    fallback_now: Arc<dyn Fn() -> f64 + Send + Sync>,
    retry_random: Arc<dyn Fn() -> f64 + Send + Sync>,
}

/// Resolved provider auth for one request.
#[derive(Clone, Debug)]
pub struct RequestAuth {
    pub model: Model,
    pub api_key: Option<String>,
    pub headers: Option<BTreeMap<String, String>>,
    pub extra_body: Option<Map<String, Value>>,
    pub env: Option<ProviderEnv>,
}

/// Auth for a summarization stream; native stream functions may supply ambient credentials.
#[derive(Clone, Debug)]
pub struct SummarizationRequestAuth {
    pub model: Model,
    pub api_key: Option<String>,
    pub headers: Option<BTreeMap<String, String>>,
    pub env: Option<ProviderEnv>,
}

fn without_deleted_headers(headers: Option<ProviderHeaders>) -> Option<BTreeMap<String, String>> {
    headers.map(|headers| headers.into_iter().filter_map(|(key, value)| value.map(|value| (key, value))).collect())
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl AgentSession {
    /// Not yet ported from the constructor (tracked by the `agent-session.ts` ledger row): the
    /// settings source-selection subscription, `applyOverrides` for the `no-model-fallback` /
    /// `no-ask-user` flags, fallback-chain validation logging, the retry-fallback controller and
    /// probe scheduler, the agent subscription and tool hooks, and `_buildRuntime` (tool registry
    /// and system prompt). Each lands with the slice that owns it.
    pub fn new(config: AgentSessionConfig) -> Result<Self, MissingModelAccessError> {
        let model_registry = match (config.model_runtime, config.model_registry) {
            (Some(runtime), _) => ModelRegistry::new(runtime),
            (None, Some(registry)) => registry,
            (None, None) => return Err(MissingModelAccessError),
        };
        let agent_dir = config.agent_dir.clone().unwrap_or_else(crate::config::get_agent_dir);
        let session_logger_dir = agent_dir.clone();
        let agent = config.agent;
        let state = AgentSessionState {
            scoped_models: config.scoped_models,
            favorite_models: config.favorite_models,
            cwd: config.cwd,
            agent_dir,
            custom_tools: config.custom_tools,
            initial_active_tool_names: config.initial_active_tool_names,
            default_tool_names: name_set(config.default_tool_names),
            eval_only_tool_names: name_set(config.eval_only_tool_names.clone()),
            allowed_tool_names: name_set(config.allowed_tool_names),
            excluded_tool_names: name_set(config.excluded_tool_names),
            base_tools_override: config.base_tools_override,
            base_tool_definitions: BTreeMap::new(),
            tool_registry: BTreeMap::new(),
            tool_definitions: BTreeMap::new(),
            tool_prompt_snippets: BTreeMap::new(),
            tool_prompt_guidelines: BTreeMap::new(),
            lazy_tool_activators: Vec::new(),
            eval_only_tool_names_override: name_set(config.eval_only_tool_names),
            withheld_eval_only_tool_names: BTreeSet::new(),
            published_eval_only_hint_names: BTreeSet::new(),
            requested_active_tool_names: None,
            session_start_event: config.session_start_event.unwrap_or(SessionStartEvent {
                reason: SessionReason::Startup,
                initial_model_provenance: None,
                previous_session_file: None,
            }),
            auto_title_sessions: config.auto_title_sessions.unwrap_or(false),
            uses_default_stream_function: config.uses_default_stream_function.unwrap_or(true),
            flag_values: config.flag_values,
            session_fast_mode: false,
            current_service_tier: None,
            base_system_prompt: String::new(),
            system_prompt_override: None,
            wake_sources: WakeSourceTracker::default(),
            shown_high_reasoning_warning_keys: BTreeSet::new(),
            extension_mode: ExtensionMode::Print,
            extension_ui_context: None,
            extension_abort_handler: None,
            extension_error_listener: None,
            message_revision: 0,
            steering_messages: Vec::new(),
            follow_up_messages: Vec::new(),
            queued_input_order: Vec::new(),
            next_queued_input_order: 0,
            post_compaction_deferred_steering_messages: Vec::new(),
            post_compaction_deferred_follow_up_messages: Vec::new(),
            had_cleared_queued_messages: false,
        };
        let session = Self {
            agent,
            session_manager: Mutex::new(config.session_manager),
            settings_manager: Mutex::new(config.settings_manager),
            model_registry,
            state: Mutex::new(state),
            extension_runner: tokio::sync::Mutex::new(None),
            event_bus: EventBus::new(),
            event_listeners: Arc::new(Mutex::new(Vec::new())),
            next_listener_id: Arc::new(AtomicU64::new(0)),
            session_logger: SessionLogger::create(Some(&session_logger_dir), SessionLoggerOptions::default()),
            fallback_now: config
                .fallback_now
                .unwrap_or_else(|| Arc::new(|| maho_ai::utils::diagnostics::now_ms() as f64)),
            retry_random: config.retry_random.unwrap_or_else(|| Arc::new(rand_unit)),
        };

        let initial_model = session.agent.state().model;
        let scoped_tier = lock(&session.state)
            .scoped_models
            .iter()
            .find(|entry| models_are_equal(Some(&entry.model), Some(&initial_model)))
            .and_then(|entry| entry.service_tier);
        let tier = resolve_service_tier(&initial_model, scoped_tier);
        lock(&session.state).current_service_tier = tier;

        Ok(session)
    }

    fn state(&self) -> MutexGuard<'_, AgentSessionState> {
        lock(&self.state)
    }

    pub fn model_runtime(&self) -> &ModelRuntime {
        &self.model_registry.model_runtime
    }

    pub fn model_registry(&self) -> &ModelRegistry {
        &self.model_registry
    }

    /// Resolve the auth a provider request needs, refusing when none is configured.
    ///
    /// Adaptation: senpi inspects the thrown error's `cause` for the `authHeader requires a
    /// resolved API key` text; the Rust `ModelsError` keeps only a message, which is matched here.
    pub async fn get_required_request_auth(&self, model: &Model) -> Result<RequestAuth, String> {
        let result = match self.model_runtime().get_auth(&model.provider).await {
            Ok(result) => result,
            Err(error) => {
                if error.message.contains("authHeader requires a resolved API key") {
                    return Err(crate::auth_guidance::format_no_api_key_found_message(&model.provider));
                }
                return Err(error.message);
            }
        };
        if let Some(result) = result
            && (result.auth.api_key.is_some() || result.auth.headers.is_some())
        {
            let request_model = match &result.auth.base_url {
                Some(base_url) => {
                    let mut request_model = model.clone();
                    request_model.base_url = base_url.clone();
                    request_model
                }
                None => model.clone(),
            };
            return Ok(RequestAuth {
                model: request_model,
                api_key: result.auth.api_key,
                headers: without_deleted_headers(result.auth.headers),
                extra_body: self.model_runtime().get_compatibility_request_config(model).extra_body,
                env: result.env,
            });
        }
        if self.model_runtime().is_using_oauth(&model.provider) {
            return Err(format!(
                "Authentication failed for \"{}\". Credentials may have expired or network is unavailable. Run '/login {}' to re-authenticate.",
                model.provider, model.provider
            ));
        }
        Err(crate::auth_guidance::format_no_api_key_found_message(&model.provider))
    }

    /// Resolve optional auth for a summarization stream.
    ///
    /// Adaptations: senpi's ambient-credential refinement reads `agent.getApiKey` and
    /// `AuthResolution.source`; the Rust agent exposes no `getApiKey` getter and `AuthResolution`
    /// carries no `source`, so the stored resolution is used as-is.
    pub async fn get_summarization_request_auth(&self, model: &Model) -> Result<SummarizationRequestAuth, String> {
        if self.state().uses_default_stream_function {
            let auth = self.get_required_request_auth(model).await?;
            return Ok(SummarizationRequestAuth {
                model: auth.model,
                api_key: auth.api_key,
                headers: auth.headers,
                env: auth.env,
            });
        }
        let Ok(Some(result)) = self.model_runtime().get_auth(&model.provider).await else {
            return Ok(SummarizationRequestAuth { model: model.clone(), api_key: None, headers: None, env: None });
        };
        let request_model = match &result.auth.base_url {
            Some(base_url) => {
                let mut request_model = model.clone();
                request_model.base_url = base_url.clone();
                request_model
            }
            None => model.clone(),
        };
        Ok(SummarizationRequestAuth {
            model: request_model,
            api_key: result.auth.api_key,
            headers: without_deleted_headers(result.auth.headers),
            env: result.env,
        })
    }

    /// Compaction summarization auth, carrying the provider's compatibility `extraBody`.
    pub async fn get_compaction_request_auth(&self, model: &Model) -> Result<RequestAuth, String> {
        let auth = self.get_summarization_request_auth(model).await?;
        Ok(RequestAuth {
            model: auth.model,
            api_key: auth.api_key,
            headers: auth.headers,
            extra_body: self.model_runtime().get_compatibility_request_config(model).extra_body,
            env: auth.env,
        })
    }

    pub fn fallback_now(&self) -> f64 {
        (self.fallback_now)()
    }

    pub fn retry_random(&self) -> f64 {
        (self.retry_random)()
    }

    pub fn agent(&self) -> &Agent {
        &self.agent
    }

    /// Locked access to the session manager; the TS getter returns the live object.
    pub fn with_session_manager<T>(&self, f: impl FnOnce(&SessionManager) -> T) -> T {
        f(&lock(&self.session_manager))
    }

    pub fn with_session_manager_mut<T>(&self, f: impl FnOnce(&mut SessionManager) -> T) -> T {
        f(&mut lock(&self.session_manager))
    }

    pub fn with_settings_manager<T>(&self, f: impl FnOnce(&SettingsManager) -> T) -> T {
        f(&lock(&self.settings_manager))
    }

    pub fn with_settings_manager_mut<T>(&self, f: impl FnOnce(&mut SettingsManager) -> T) -> T {
        f(&mut lock(&self.settings_manager))
    }

    pub fn agent_dir(&self) -> String {
        self.state().agent_dir.clone()
    }

    pub fn cwd(&self) -> String {
        self.state().cwd.clone()
    }

    pub fn session_id(&self) -> String {
        self.with_session_manager(|manager| manager.session_id().to_owned())
    }

    pub fn session_file(&self) -> Option<String> {
        self.with_session_manager(|manager| manager.session_file().map(str::to_owned))
    }

    pub fn session_name(&self) -> Option<String> {
        self.with_session_manager(|manager| manager.session_name().map(str::to_owned))
    }

    /// A copy of the agent transcript; the TS getter hands out the live array.
    pub fn messages(&self) -> Vec<AgentMessage> {
        self.agent.state().messages().to_vec()
    }

    pub fn state_snapshot(&self) -> AgentState {
        self.agent.state()
    }

    pub fn model(&self) -> Model {
        self.agent.state().model
    }

    /// The agent carries `"off" | ThinkingLevel`; senpi's `ThinkingLevel` includes `"off"`.
    pub fn thinking_level(&self) -> ModelThinkingLevel {
        self.agent.state().thinking_level
    }

    pub fn thinking_selection(&self) -> Option<ThinkingSelection> {
        self.agent.state().thinking_selection
    }

    pub fn service_tier(&self) -> Option<ServiceTier> {
        self.state().current_service_tier
    }

    pub fn is_fast_mode_active(&self) -> bool {
        self.state().session_fast_mode
    }

    pub fn effective_service_tier(&self) -> Option<ServiceTier> {
        let state = self.state();
        resolve_effective_service_tier(state.current_service_tier, state.session_fast_mode)
    }

    pub fn set_session_fast_mode(&self, enabled: bool) {
        self.state().session_fast_mode = enabled;
    }

    pub fn is_streaming(&self) -> bool {
        self.agent.state().is_streaming
    }

    pub fn is_idle(&self) -> bool {
        let state = self.agent.state();
        !state.is_streaming && state.pending_tool_calls.is_empty()
    }

    pub fn system_prompt(&self) -> String {
        let state = self.state();
        state.system_prompt_override.clone().unwrap_or_else(|| state.base_system_prompt.clone())
    }

    pub fn scoped_models(&self) -> Vec<SessionModelEntry> {
        self.state().scoped_models.clone()
    }

    pub fn favorite_models(&self) -> Vec<SessionModelEntry> {
        self.state().favorite_models.clone()
    }

    pub fn activity_snapshot(&self) -> SessionActivitySnapshot {
        SessionActivitySnapshot {
            is_streaming: self.is_streaming(),
            has_active_wake_source: self.state().wake_sources.has_active(),
            ..SessionActivitySnapshot::default()
        }
    }

    pub fn is_session_busy(&self) -> bool {
        is_session_busy_snapshot(self.activity_snapshot())
    }

    pub fn message_revision(&self) -> u64 {
        self.state().message_revision
    }

    pub fn get_all_tools(&self) -> Vec<ToolInfo> {
        self.agent
            .state()
            .tools()
            .iter()
            .map(|tool| ToolInfo {
                name: tool.name().to_owned(),
                label: tool.label.clone(),
                description: tool.tool.description.clone(),
                parameters: tool.tool.parameters.clone(),
                prompt_guidelines: None,
                source_info: SourceInfo {
                    path: String::new(),
                    source: String::new(),
                    scope: SourceScope::Temporary,
                    origin: SourceOrigin::TopLevel,
                    base_dir: None,
                },
                exposure: ToolExposure::Direct,
                search_text: None,
                search_keywords: Vec::new(),
                search_group: None,
                allow_lazy_activation: false,
            })
            .collect()
    }

    pub fn get_active_tool_names(&self) -> Vec<String> {
        self.agent.state().tools().iter().map(|tool| tool.name().to_owned()).collect()
    }

    /// A runtime extension flag value (constructor input in TS).
    pub fn flag_value(&self, name: &str) -> Option<FlagValue> {
        self.state().flag_values.get(name).cloned()
    }

    pub fn get_tool_definition(&self, name: &str) -> Option<ToolDefinition> {
        self.state().tool_definitions.get(name).map(|entry| entry.definition.clone())
    }

    /// The tool a call named `requested` runs, by the same rule the agent loop applies (exact name,
    /// else the unique alias among callable tools), without activating anything.
    pub fn resolve_tool_call_name(&self, requested: &str) -> String {
        resolve_tool_name_alias(requested, self.callable_tool_names()).unwrap_or_else(|| requested.to_owned())
    }

    /// Active tool names plus search-exposed lazy tools; senpi also appends the tool-search catalog,
    /// which has no Rust counterpart (the service is a TS extension), so it contributes nothing.
    fn callable_tool_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.agent.state().tools().iter().map(|tool| tool.name().to_owned()).collect();
        for (name, entry) in &self.state().tool_definitions {
            let exposure = normalize_tool_exposure(&entry.definition, entry.source_info.clone());
            if exposure.exposure == ToolExposure::Search && exposure.allow_lazy_activation {
                names.push(name.clone());
            }
        }
        names
    }

    /// Resolve an executable tool from the full registry, independent of the active set.
    pub fn get_registered_tool(&self, name: &str) -> Option<AgentTool> {
        self.state().tool_registry.get(name).cloned()
    }

    fn is_eval_only_policy_armed(&self) -> bool {
        let state = self.state();
        state.eval_only_tool_names.is_some() && state.tool_registry.contains_key("eval")
    }

    /// Resolve fixed and declared eval-only tools, unless an SDK embedder supplied an override.
    pub fn resolve_eval_only_tool_names(&self) -> BTreeSet<String> {
        let state = self.state();
        if let Some(overrides) = &state.eval_only_tool_names_override {
            return overrides.clone();
        }
        let mut names: BTreeSet<String> = EVAL_ONLY_TOOL_NAMES.iter().map(|name| (*name).to_owned()).collect();
        for entry in state.tool_definitions.values() {
            if normalize_tool_exposure(&entry.definition, entry.source_info.clone()).exposure == ToolExposure::Eval {
                names.insert(entry.definition.name.clone());
            }
        }
        names
    }

    fn publish_eval_only_tool_hints(&self) {
        let armed = self.is_eval_only_policy_armed();
        let registered: Option<BTreeSet<String>> = armed.then(|| {
            let state = self.state();
            state
                .eval_only_tool_names
                .clone()
                .unwrap_or_default()
                .into_iter()
                .filter(|name| state.tool_registry.contains_key(name))
                .collect()
        });
        let mut hints = self.agent.removed_tool_hints();
        let published = self.state().published_eval_only_hint_names.clone();
        for name in published {
            if registered.as_ref().is_some_and(|registered| registered.contains(&name)) {
                continue;
            }
            hints.remove(&name);
        }
        self.state().published_eval_only_hint_names.clear();
        let Some(registered) = registered else {
            self.agent.set_removed_tool_hints(hints);
            return;
        };
        for name in registered {
            hints.insert(
                name.clone(),
                format!("Run {name} inside an eval cell via {}; hooks and permissions still apply.", eval_helper_call(&name)),
            );
            self.state().published_eval_only_hint_names.insert(name);
        }
        self.agent.set_removed_tool_hints(hints);
    }

    /// Lazily activate a registered inactive tool.
    ///
    /// Adaptation: senpi also asks the tool-search service to activate; that service is a TS
    /// extension with no Rust counterpart, so only the registered activators and the direct
    /// search-exposure promotion run.
    fn activate_lazy_tool(&self, tool_name: &str) -> bool {
        let Some(definition) = self.get_tool_definition(tool_name) else {
            return false;
        };
        let exposure = normalize_tool_exposure(
            &definition,
            self.state().tool_definitions.get(tool_name).map(|entry| entry.source_info.clone()).unwrap_or_else(empty_source_info),
        );
        if !exposure.allow_lazy_activation {
            return false;
        }
        if self.state().lazy_tool_activators.iter().any(|activate| activate(tool_name)) {
            return true;
        }
        if exposure.exposure == ToolExposure::Search && !self.get_active_tool_names().iter().any(|name| name == tool_name) {
            let mut names = self.get_active_tool_names();
            names.push(tool_name.to_owned());
            self.set_active_tools_by_name(names);
        }
        self.get_active_tool_names().iter().any(|name| name == tool_name)
    }

    /// Set active tools by name; only tools in the registry can be enabled.
    ///
    /// Not yet ported: the base system-prompt rebuild and the message-revision bump that follow a
    /// change (owned by the system-prompt slice).
    pub fn set_active_tools_by_name(&self, tool_names: Vec<String>) {
        let policy_armed = self.is_eval_only_policy_armed();
        self.publish_eval_only_tool_hints();
        let policy_names = self.state().eval_only_tool_names.clone();
        let mut withheld = self.state().withheld_eval_only_tool_names.clone();
        let filtered: Vec<String> = if policy_armed {
            if let Some(policy_names) = &policy_names {
                for name in &tool_names {
                    if policy_names.contains(name) {
                        withheld.insert(name.clone());
                    }
                }
                tool_names.iter().filter(|name| !policy_names.contains(*name)).cloned().collect()
            } else {
                tool_names.clone()
            }
        } else {
            withheld.clear();
            tool_names.clone()
        };
        let mut requested: Vec<String> = tool_names.clone();
        requested.extend(withheld.iter().cloned());
        requested.sort();
        requested.dedup();

        let mut tools = Vec::new();
        let mut valid_names = Vec::new();
        for name in filtered {
            if let Some(tool) = self.state().tool_registry.get(&name).cloned() {
                tools.push(tool);
                valid_names.push(name);
            }
        }
        self.agent.set_tools(tools);
        let mut state = self.state();
        state.withheld_eval_only_tool_names = withheld;
        state.requested_active_tool_names = Some(requested);
    }

    /// The active-tool selection as requested by callers, before eval-only filtering.
    pub fn requested_active_tool_names(&self) -> Option<Vec<String>> {
        self.state().requested_active_tool_names.clone()
    }

    /// Register a tool definition and its executable tool; used by the runtime build and by SDK
    /// custom tools.
    pub fn register_tool_definition(&self, definition: ToolDefinition, source_info: SourceInfo, tool: AgentTool) {
        let mut state = self.state();
        state.tool_registry.insert(definition.name.clone(), tool);
        state.base_tool_definitions.insert(definition.name.clone(), definition.clone());
        state.tool_definitions.insert(definition.name.clone(), ToolDefinitionEntry { definition, source_info });
    }

    /// Add a lazy-tool activator (extension or SDK supplied).
    pub fn add_lazy_tool_activator(&self, activator: LazyToolActivator) {
        self.state().lazy_tool_activators.push(activator);
    }

    /// Execute a tool by name through the same preparation and hook path the agent loop uses.
    ///
    /// Not yet ported: `_emitAfterToolCallHooks`'s image normalization, and the tool-search
    /// activation fallback (no Rust service).
    pub async fn execute_tool(
        &self,
        tool_name: &str,
        params: Value,
        options: ExecuteToolOptions,
    ) -> Result<AgentToolResult, ExecuteToolError> {
        let mut active_tools = self.get_active_tool_names();
        let mut tool = self.agent.state().tools().iter().find(|candidate| candidate.name() == tool_name).cloned();
        if tool.is_none() && self.is_eval_only_policy_armed() {
            let armed = self.state().eval_only_tool_names.clone().unwrap_or_default();
            if armed.contains(tool_name) {
                tool = self.state().tool_registry.get(tool_name).cloned();
            }
        }
        if tool.is_none()
            && options.activate_inactive_tool == Some(true)
            && self.state().tool_definitions.contains_key(tool_name)
            && self.activate_lazy_tool(tool_name)
        {
            active_tools = self.get_active_tool_names();
            tool = self.agent.state().tools().iter().find(|candidate| candidate.name() == tool_name).cloned();
        }
        let Some(tool) = tool else {
            let known = self.state().tool_definitions.contains_key(tool_name);
            let code = if known { "inactive_tool" } else { "unknown_tool" };
            let active_list = if active_tools.is_empty() { "(none)".to_owned() } else { active_tools.join(", ") };
            let message = if known {
                format!("Tool {tool_name} is registered but inactive. Active tools: {active_list}")
            } else {
                format!("Unknown tool {tool_name}. Active tools: {active_list}")
            };
            return Err(ExecuteToolError { code: code.to_owned(), tool_name: tool_name.to_owned(), message, active_tools });
        };
        let tool_call = maho_agent::types::AgentToolCall {
            id: format!("codemode-{}", uuid::Uuid::new_v4()),
            name: tool_name.to_owned(),
            arguments: params.as_object().cloned().unwrap_or_default(),
            ..Default::default()
        };
        let prepared = maho_agent::tool_arguments::prepare_agent_tool_call_arguments(&tool, &tool_call);
        if let Some(block) = self.preflight_tool_call(&prepared, Value::Object(prepared.arguments.clone())).await
            && block.block == Some(true)
        {
            return Err(ExecuteToolError {
                code: "blocked".to_owned(),
                tool_name: tool_name.to_owned(),
                message: block.reason.unwrap_or_else(|| "Tool execution was blocked".to_owned()),
                active_tools,
            });
        }
        let result = (tool.execute)(
            prepared.id.clone(),
            Value::Object(prepared.arguments.clone()),
            options.signal,
            None,
        )
        .await;
        Ok(result)
    }

    /// `preflightToolCall`: run the `tool_call` extension hook. The Rust agent exposes no
    /// `_agentEventQueue`, so the `waitForEventQueue` wait is not applied.
    pub async fn preflight_tool_call(
        &self,
        tool_call: &maho_agent::types::AgentToolCall,
        input: Value,
    ) -> Option<maho_ext_api::ToolCallEventResult> {
        let mut guard = self.extension_runner.lock().await;
        let runner = guard.as_mut()?;
        if !runner.has_handlers(maho_ext_api::EventKind::ToolCall) {
            return None;
        }
        let mut event = ToolCallEvent {
            tool_call_id: tool_call.id.clone(),
            tool_name: tool_call.name.clone(),
            input,
        };
        runner.emit_tool_call(&mut event).await.ok().flatten()
    }

    /// The extension runner currently bound to the session, if any.
    pub async fn extension_runner_bound(&self) -> bool {
        self.extension_runner.lock().await.is_some()
    }

    /// Bind the extension runner the tool hooks read at execution time.
    pub async fn set_extension_runner(&self, runner: ExtensionRunner) {
        *self.extension_runner.lock().await = Some(runner);
    }

    /// Subscribe to the internal event bus shared by this session's extensions.
    pub fn on_extension_event(&self, channel: &str, handler: EventHandler) -> EventSubscription {
        self.event_bus.on(channel, handler)
    }

    /// Publish on the internal event bus shared by this session's extensions.
    pub fn emit_extension_event(&self, channel: &str, data: &Value) {
        self.event_bus.emit(channel, data);
    }

    /// Append a transport-provided entry and publish it on the RPC event stream.
    pub fn append_session_entry(&self, entry: Value) {
        let entry_id = entry.get("id").and_then(Value::as_str).map(str::to_owned);
        self.with_session_manager_mut(|manager| manager.append_entry_raw(entry));
        let messages = self.with_session_manager(|manager| manager.build_context(None).messages);
        let agent_messages: Vec<AgentMessage> =
            messages.iter().filter_map(|message| serde_json::from_value(message.clone()).ok()).collect();
        self.agent.set_messages(agent_messages);
        if let Some(entry_id) = entry_id {
            self.emit_entry_appended(&entry_id);
        }
    }

    fn emit_entry_appended(&self, entry_id: &str) {
        if self.state().extension_mode != ExtensionMode::Rpc {
            return;
        }
        if let Some(entry) = self.with_session_manager(|manager| manager.entry(entry_id)) {
            self.emit(AgentSessionEvent::EntryAppended { entry: session_entry_from_value(entry) });
        }
    }

    /// Emit an event to all listeners.
    pub fn emit(&self, event: AgentSessionEvent) {
        self.log_session_event(&event);
        let listeners = self.event_listeners.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        for (_, listener) in listeners {
            listener(&event);
        }
    }

    /// Mirror stuck-prone lifecycle transitions into logs/session.log (content-free).
    ///
    /// Not yet ported: the `compaction_start`/`compaction_end` attempt bookkeeping, which lands
    /// with the compaction slice.
    fn log_session_event(&self, event: &AgentSessionEvent) {
        if let AgentSessionEvent::Agent(maho_agent::types::AgentEvent::MessageEnd { message }) = event
            && let Some(message) = message.as_assistant()
            && message.stop_reason == StopReason::Error
        {
            let kind = if maho_ai::utils::retry::is_provider_stream_stall_error(message) {
                "stall"
            } else if maho_ai::utils::retry::is_provider_timeout_error(message) {
                "timeout"
            } else {
                "error"
            };
            let mut data = Map::new();
            data.insert("kind".to_owned(), Value::String(kind.to_owned()));
            if let Some(error) = &message.error_message {
                data.insert("error".to_owned(), Value::String(error.clone()));
            }
            self.session_logger.warn("provider_error", Some(&data));
        }
    }

    /// Subscribe to session events. The returned handle unsubscribes when dropped.
    ///
    /// Not yet ported: the resume-compaction/resume-slice replays and the settings-source replay,
    /// which land with the compaction and settings-source slices.
    pub fn subscribe(&self, listener: AgentSessionEventListener) -> AgentSessionSubscription {
        let id = self.next_listener_id.fetch_add(1, Ordering::SeqCst);
        self.event_listeners.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((id, listener));
        AgentSessionSubscription { listeners: Arc::clone(&self.event_listeners), id }
    }

    /// Remove all listeners and release session-scoped resources.
    ///
    /// Not yet ported: aborting retry/compaction/branch-summary/session-title/bash, disposing tool
    /// contexts, disconnecting the agent subscription and the wake-source unsubscribe, and the
    /// session manager's own dispose; each lands with the slice that owns it.
    pub async fn dispose(&self) {
        self.agent.abort(None);
        {
            let mut guard = self.extension_runner.lock().await;
            if let Some(runner) = guard.as_mut() {
                runner.invalidate(
                    "This extension ctx is stale after session replacement or reload. Do not use a captured pi or command ctx after ctx.newSession(), ctx.fork(), ctx.switchSession(), or ctx.reload(). For newSession, fork, and switchSession, move post-replacement work into withSession and use the ctx passed to withSession. For reload, do not use the old ctx after await ctx.reload().",
                );
            }
        }
        self.event_listeners.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
        let _ = maho_ai::session_resources::cleanup_session_resources(Some(&self.session_id()));
    }

    /// Reserve a global order for input temporarily owned outside the native queues.
    pub fn reserve_queued_input_order(&self) -> u64 {
        let mut state = self.state();
        state.next_queued_input_order += 1;
        state.next_queued_input_order
    }

    #[allow(dead_code)] // consumed by the steer/follow-up queue entry points in the queue slice
    fn record_queued_input(&self, text: &str, mode: StreamingBehavior, enqueue_order: Option<u64>) {
        let mut state = self.state();
        let order = enqueue_order.unwrap_or_else(|| {
            state.next_queued_input_order += 1;
            state.next_queued_input_order
        });
        state.next_queued_input_order = state.next_queued_input_order.max(order);
        state.queued_input_order.push(QueuedInput { text: text.to_owned(), mode, enqueue_order: order });
    }

    #[allow(dead_code)] // consumed by the steer/follow-up delivery path in the queue slice
    fn remove_queued_input(&self, text: &str, mode: StreamingBehavior) {
        let mut state = self.state();
        if let Some(index) = state
            .queued_input_order
            .iter()
            .position(|message| message.mode == mode && message.text == text)
        {
            state.queued_input_order.remove(index);
        }
    }

    /// Clear all queued messages and return them. `ordered` preserves the recovery order across
    /// both native queue modes.
    pub fn clear_queue(&self, abort_will_follow: bool) -> ClearedQueue {
        let steering;
        let follow_up;
        let ordered;
        {
            let mut state = self.state();
            steering = std::mem::take(&mut state.steering_messages);
            follow_up = std::mem::take(&mut state.follow_up_messages);
            ordered = std::mem::take(&mut state.queued_input_order);
            state.post_compaction_deferred_steering_messages.clear();
            state.post_compaction_deferred_follow_up_messages.clear();
            if abort_will_follow && (!steering.is_empty() || !follow_up.is_empty()) {
                state.had_cleared_queued_messages = true;
            }
        }
        let mut ordered = ordered;
        ordered.sort_by_key(|input| input.enqueue_order);
        self.agent.clear_all_queues();
        self.emit_queue_update();
        ClearedQueue { steering, follow_up, ordered }
    }

    pub fn pending_message_count(&self) -> usize {
        let state = self.state();
        state.steering_messages.len() + state.follow_up_messages.len()
    }

    pub fn get_steering_messages(&self) -> Vec<String> {
        self.state().steering_messages.clone()
    }

    pub fn get_follow_up_messages(&self) -> Vec<String> {
        self.state().follow_up_messages.clone()
    }

    /// Update the global model narrowing.
    pub fn set_scoped_models(&self, scoped_models: Vec<SessionModelEntry>) {
        self.state().scoped_models = scoped_models;
    }

    /// Update the favorite models used for cycling.
    pub fn set_favorite_models(&self, favorite_models: Vec<SessionModelEntry>) {
        self.state().favorite_models = favorite_models;
    }

    /// Favorites narrowed by the scoped set and de-duplicated, resolved against the catalog.
    ///
    /// Adaptation: the Rust runtime exposes no availability snapshot, so the full catalog stands in
    /// for it (the TS `getAvailableSnapshot`).
    pub fn get_current_favorite_models(&self) -> Vec<SessionModelEntry> {
        let available = self.model_runtime().get_models(None);
        let available_by_id: BTreeMap<String, Model> = available
            .into_iter()
            .map(|model| (format!("{}/{}", model.provider, model.id), model))
            .collect();
        let state = self.state();
        let narrowed: Option<BTreeSet<String>> = if state.scoped_models.is_empty() {
            None
        } else {
            Some(state.scoped_models.iter().map(|scoped| format!("{}/{}", scoped.model.provider, scoped.model.id)).collect())
        };
        let mut seen = BTreeSet::new();
        let mut favorites = Vec::new();
        for favorite in &state.favorite_models {
            let id = format!("{}/{}", favorite.model.provider, favorite.model.id);
            if seen.contains(&id) {
                continue;
            }
            let Some(model) = available_by_id.get(&id) else {
                continue;
            };
            if narrowed.as_ref().is_some_and(|narrowed| !narrowed.contains(&id)) {
                continue;
            }
            seen.insert(id);
            favorites.push(SessionModelEntry {
                model: model.clone(),
                thinking_level: favorite.thinking_level,
                thinking_selection: favorite.thinking_selection.clone(),
                service_tier: favorite.service_tier,
            });
        }
        favorites
    }

    /// Surface a provider-level server-fallback abort before retry handling runs so the UI can
    /// explain the switch.
    ///
    /// Adaptation: `chainConfigured` is derived from the settings' fallback chains, because the
    /// session's retry-fallback controller is not ported (another todo's module).
    pub fn emit_server_fallback_aborted(&self, message: &maho_ai::types::AssistantMessage) {
        let Some(details) = message
            .diagnostics
            .as_ref()
            .and_then(|diagnostics| diagnostics.iter().find(|entry| entry.kind == maho_ai::utils::server_fallback_receipt::SERVER_FALLBACK_ABORTED_DIAGNOSTIC))
            .and_then(|entry| entry.details.clone())
        else {
            return;
        };
        let from = details
            .get("from")
            .and_then(Value::as_str)
            .map_or_else(|| message.model.clone(), str::to_owned);
        let to = details
            .get("to")
            .and_then(Value::as_str)
            .map_or_else(|| message.model.clone(), str::to_owned);
        self.emit(AgentSessionEvent::ServerFallbackAborted {
            from,
            to,
            chain_configured: self.has_configured_fallback_chain(),
        });
    }

    fn has_configured_fallback_chain(&self) -> bool {
        let settings = self.with_settings_manager(|manager| manager.get_value("retry").cloned());
        let chains = crate::retry_fallback::settings::resolve_retry_fallback_settings(settings.as_ref()).chains;
        crate::retry_fallback::chains::resolve_chain_key(&self.model(), Some(self.agent.state().thinking_level), &chains)
            .is_some()
    }

    fn emit_queue_update(&self) {
        let (steering, follow_up, mut ordered) = {
            let state = self.state();
            (state.steering_messages.clone(), state.follow_up_messages.clone(), state.queued_input_order.clone())
        };
        ordered.sort_by_key(|input| input.enqueue_order);
        let ordered = ordered
            .into_iter()
            .map(|input| maho_ext_api::QueuedInput {
                text: input.text,
                mode: input.mode,
                enqueue_order: input.enqueue_order,
            })
            .collect();
        self.emit(AgentSessionEvent::QueueUpdate { steering, follow_up, ordered });
    }

    pub fn steering_mode(&self) -> maho_agent::types::QueueMode {
        self.agent.steering_mode()
    }

    pub fn follow_up_mode(&self) -> maho_agent::types::QueueMode {
        self.agent.follow_up_mode()
    }

    /// Set steering message mode; saves to settings and emits `session_settings_changed`.
    pub fn set_steering_mode(&self, mode: maho_agent::types::QueueMode) {
        self.agent.set_steering_mode(mode);
        self.persist_queue_mode("steeringMode", mode);
        self.emit_session_settings_changed();
    }

    /// Set follow-up message mode; saves to settings and emits `session_settings_changed`.
    pub fn set_follow_up_mode(&self, mode: maho_agent::types::QueueMode) {
        self.agent.set_follow_up_mode(mode);
        self.persist_queue_mode("followUpMode", mode);
        self.emit_session_settings_changed();
    }

    fn persist_queue_mode(&self, key: &str, mode: maho_agent::types::QueueMode) {
        let mut settings = crate::settings_manager::Settings::new();
        settings.insert(key.to_owned(), Value::String(queue_mode_str(mode).to_owned()));
        self.with_settings_manager_mut(|manager| {
            let _ = manager.set(crate::settings_manager::SettingsScope::Global, &settings);
        });
    }

    /// Apply the persisted steering/follow-up modes to the agent.
    pub fn sync_queue_modes_from_settings(&self) {
        let steering = self.with_settings_manager(|manager| manager.get_string("steeringMode"));
        let follow_up = self.with_settings_manager(|manager| manager.get_string("followUpMode"));
        if let Some(mode) = steering.as_deref().and_then(queue_mode_from_str) {
            self.agent.set_steering_mode(mode);
        }
        if let Some(mode) = follow_up.as_deref().and_then(queue_mode_from_str) {
            self.agent.set_follow_up_mode(mode);
        }
    }

    fn emit_session_settings_changed(&self) {
        self.emit(AgentSessionEvent::SessionSettingsChanged {
            steering_mode: queue_mode_str(self.agent.steering_mode()).to_owned(),
            follow_up_mode: queue_mode_str(self.agent.follow_up_mode()).to_owned(),
            auto_compaction_enabled: self.auto_compaction_enabled(),
        });
    }

    /// `autoCompactionEnabled`: the session override when set, else the settings value.
    /// `autoCompactionEnabled`: the session override when set, else the settings value.
    ///
    /// Not yet ported: `_autoCompactionSessionOverride` (owned by the compaction slice); this reads
    /// the settings value only.
    pub fn auto_compaction_enabled(&self) -> bool {
        self.with_settings_manager(|manager| {
            manager
                .get_value("compaction")
                .and_then(|value| value.get("enabled"))
                .and_then(Value::as_bool)
                .unwrap_or(true)
        })
    }

    /// Set the thinking level, clamping to the model's available levels.
    ///
    /// Adaptations: the Rust agent exposes no thinkingSelection setter, so the selection is not
    /// propagated into agent state; and the settings manager has no thinking-level accessors, so
    /// `updateGlobalDefault` does not persist (both live in other todos' modules).
    pub fn set_thinking_level(&self, level: ModelThinkingLevel) {
        self.apply_thinking_level(level, true);
    }

    /// Set the thinking level for this session without changing the global default.
    pub fn set_session_thinking_level(&self, level: ModelThinkingLevel) {
        self.apply_thinking_level(level, false);
    }

    fn apply_thinking_level(&self, level: ModelThinkingLevel, update_global_default: bool) {
        let available = self.get_available_thinking_levels();
        let effective = if available.contains(&level) { level } else { clamp_thinking_level(level, &available) };
        let previous = self.agent.state().thinking_level;
        let changing = effective != previous;

        self.agent.set_thinking_level(effective);

        if changing {
            self.with_session_manager_mut(|manager| {
                manager.append_thinking_level_change(effective.as_str(), None);
            });
            if let Some(level) = thinking_level_from_model_level(effective) {
                self.emit(AgentSessionEvent::ThinkingLevelChanged { level });
            }
            self.emit_high_reasoning_warning_if_needed();
        }
        let _ = update_global_default;
    }

    fn emit_high_reasoning_warning_if_needed(&self) {
        let model = self.model();
        let level = self.agent.state().thinking_level;
        if !crate::high_reasoning_warning::should_warn_high_reasoning(&model.id, level) {
            return;
        }
        let key = format!("{}/{}", model.provider, model.id);
        if !self.state().shown_high_reasoning_warning_keys.insert(key) {
            return;
        }
        self.emit(AgentSessionEvent::HighReasoningWarning {
            model_id: model.id.clone(),
            provider: model.provider.clone(),
            thinking_level: thinking_level_from_model_level(level).unwrap_or(ThinkingLevel::Minimal),
        });
    }

    /// Cycle to the next thinking level; `None` when the model does not support thinking.
    pub fn cycle_thinking_level(&self) -> Option<ModelThinkingLevel> {
        if !self.supports_thinking() {
            return None;
        }
        let levels = self.get_available_thinking_levels();
        if levels.is_empty() {
            return None;
        }
        let current = self.agent.state().thinking_level;
        let index = levels.iter().position(|level| *level == current).unwrap_or(levels.len() - 1);
        let next = levels[(index + 1) % levels.len()];
        self.set_thinking_level(next);
        Some(next)
    }

    /// Available thinking levels for the current model.
    pub fn get_available_thinking_levels(&self) -> Vec<ModelThinkingLevel> {
        crate::thinking_levels::get_supported_thinking_levels(&self.model())
    }

    pub fn supports_xhigh_thinking(&self) -> bool {
        crate::thinking_levels::supports_xhigh(&self.model())
    }

    pub fn supports_max_thinking(&self) -> bool {
        crate::thinking_levels::supports_max(&self.model())
    }

    pub fn supports_thinking(&self) -> bool {
        self.model().reasoning
    }

    /// The thinking level a model switch would land on, from an explicit level, the model's
    /// remembered level, the configured default, or `DEFAULT_THINKING_LEVEL`.
    ///
    /// Adaptations: the settings manager has no per-model or default thinking-level accessors, so
    /// the remembered and configured-default steps are skipped (other todos' modules).
    pub fn get_thinking_for_model_switch(
        &self,
        model: &Model,
        explicit_level: Option<ModelThinkingLevel>,
    ) -> ModelThinkingLevel {
        let requested = explicit_level.unwrap_or(ModelThinkingLevel::Medium);
        let available = crate::thinking_levels::get_supported_thinking_levels(model);
        if available.contains(&requested) { requested } else { clamp_thinking_level(requested, &available) }
    }
}

/// Thinking levels including the native max tier.
const THINKING_LEVELS_WITH_MAX: [ModelThinkingLevel; 7] = [
    ModelThinkingLevel::Off,
    ModelThinkingLevel::Minimal,
    ModelThinkingLevel::Low,
    ModelThinkingLevel::Medium,
    ModelThinkingLevel::High,
    ModelThinkingLevel::Xhigh,
    ModelThinkingLevel::Max,
];

/// `"off" | ThinkingLevel` collapsed to the non-off union, or `None` for `Off`.
fn thinking_level_from_model_level(level: ModelThinkingLevel) -> Option<ThinkingLevel> {
    match level {
        ModelThinkingLevel::Off => None,
        ModelThinkingLevel::Minimal => Some(ThinkingLevel::Minimal),
        ModelThinkingLevel::Low => Some(ThinkingLevel::Low),
        ModelThinkingLevel::Medium => Some(ThinkingLevel::Medium),
        ModelThinkingLevel::High => Some(ThinkingLevel::High),
        ModelThinkingLevel::Xhigh => Some(ThinkingLevel::Xhigh),
        ModelThinkingLevel::Max => Some(ThinkingLevel::Max),
    }
}

fn clamp_thinking_level(level: ModelThinkingLevel, available: &[ModelThinkingLevel]) -> ModelThinkingLevel {
    let Some(requested) = THINKING_LEVELS_WITH_MAX.iter().position(|candidate| *candidate == level) else {
        return available.first().copied().unwrap_or(ModelThinkingLevel::Off);
    };
    for candidate in &THINKING_LEVELS_WITH_MAX[requested..] {
        if available.contains(candidate) {
            return *candidate;
        }
    }
    for candidate in THINKING_LEVELS_WITH_MAX[..requested].iter().rev() {
        if available.contains(candidate) {
            return *candidate;
        }
    }
    available.first().copied().unwrap_or(ModelThinkingLevel::Off)
}

fn queue_mode_str(mode: maho_agent::types::QueueMode) -> &'static str {
    match mode {
        maho_agent::types::QueueMode::All => "all",
        maho_agent::types::QueueMode::OneAtATime => "one-at-a-time",
    }
}

fn queue_mode_from_str(value: &str) -> Option<maho_agent::types::QueueMode> {
    match value {
        "all" => Some(maho_agent::types::QueueMode::All),
        "one-at-a-time" => Some(maho_agent::types::QueueMode::OneAtATime),
        _ => None,
    }
}

fn session_entry_from_value(entry: Value) -> maho_ext_api::SessionEntry {
    maho_ext_api::SessionEntry {
        id: entry.get("id").and_then(Value::as_str).unwrap_or_default().to_owned(),
        parent_id: entry.get("parentId").and_then(Value::as_str).map(str::to_owned),
        timestamp: entry.get("timestamp").and_then(Value::as_str).unwrap_or_default().to_owned(),
        kind: entry.get("type").and_then(Value::as_str).unwrap_or_default().to_owned(),
        data: entry,
    }
}

fn empty_source_info() -> SourceInfo {
    SourceInfo {
        path: String::new(),
        source: String::new(),
        scope: SourceScope::Temporary,
        origin: SourceOrigin::TopLevel,
        base_dir: None,
    }
}

fn resolve_service_tier(model: &Model, scoped: Option<ServiceTier>) -> Option<ServiceTier> {
    scoped.or_else(|| model.service_tier.map(service_tier_from_preference))
}

fn service_tier_from_preference(preference: ServiceTierPreference) -> ServiceTier {
    match preference {
        ServiceTierPreference::Auto => ServiceTier::Auto,
        ServiceTierPreference::Flex => ServiceTier::Flex,
        ServiceTierPreference::Priority => ServiceTier::Priority,
    }
}

fn resolve_effective_service_tier(tier: Option<ServiceTier>, fast_mode: bool) -> Option<ServiceTier> {
    if fast_mode { Some(ServiceTier::Priority) } else { tier }
}

fn rand_unit() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |duration| duration.subsec_nanos());
    f64::from(nanos % 1_000_000) / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eval_helper_calls_use_the_argument_name_the_tool_takes() {
        assert_eq!(eval_helper_call("bash"), "tool.bash({ command: \"...\" })");
        assert_eq!(eval_helper_call("grep"), "tool.grep({ pattern: \"...\", path: \"...\" })");
        assert_eq!(eval_helper_call("workflow"), "tool.workflow({ action: \"...\" })");
        assert_eq!(
            eval_helper_call("monitor"),
            "tool.monitor({ description: \"...\", command: \"...\", filter: \"...\" })"
        );
        assert_eq!(eval_helper_call("read"), "tool.read({ ... })");
    }

    #[test]
    fn watchdog_message_notes_the_stream_start_guard() {
        assert!(provider_retry_watchdog_abort_message(Some(30_000), None).contains("stream-start guard disabled"));
        assert!(provider_retry_watchdog_abort_message(Some(30_000), Some(5_000)).contains("stream-start guard: 5000ms"));
    }

    #[test]
    fn every_rejection_cause_has_a_message() {
        for cause in [
            CompactionRejectionCause::WouldOverflow,
            CompactionRejectionCause::CancelledByExtension,
            CompactionRejectionCause::ExternalOwner,
            CompactionRejectionCause::CircuitBreaker,
            CompactionRejectionCause::PerTurnCap,
            CompactionRejectionCause::StaleRevision,
        ] {
            assert!(describe_compaction_rejection(cause).starts_with("Compaction rejected"));
        }
    }

    #[test]
    fn fast_mode_promotes_the_effective_tier() {
        assert_eq!(resolve_effective_service_tier(None, false), None);
        assert_eq!(resolve_effective_service_tier(Some(ServiceTier::Flex), false), Some(ServiceTier::Flex));
        assert_eq!(resolve_effective_service_tier(None, true), Some(ServiceTier::Priority));
    }

    #[test]
    fn a_scoped_entry_tier_wins_over_the_model_preference() {
        let mut model = test_model();
        model.service_tier = Some(ServiceTierPreference::Flex);
        assert_eq!(resolve_service_tier(&model, Some(ServiceTier::Priority)), Some(ServiceTier::Priority));
        assert_eq!(resolve_service_tier(&model, None), Some(ServiceTier::Flex));
    }

    #[test]
    fn a_null_header_value_is_dropped() {
        let headers: ProviderHeaders = [
            ("Authorization".to_owned(), Some("Bearer x".to_owned())),
            ("X-Deleted".to_owned(), None),
        ]
        .into_iter()
        .collect();
        let filtered = without_deleted_headers(Some(headers)).expect("headers");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered.get("Authorization").map(String::as_str), Some("Bearer x"));
    }

    fn stub_agent() -> Agent {
        let stream_fn: maho_agent::types::StreamFn =
            Arc::new(|_, _, _| maho_ai::types::AssistantMessageEventStream::assistant());
        Agent::new(maho_agent::AgentOptions { stream_fn: Some(stream_fn), ..Default::default() })
    }

    fn test_session() -> AgentSession {
        test_session_with_stream_function(true)
    }

    fn test_session_with_stream_function(uses_default_stream_function: bool) -> AgentSession {
        let runtime = ModelRuntime::create_sync(crate::model_runtime::CreateModelRuntimeOptions {
            providers: Some(Vec::new()),
            ..Default::default()
        });
        AgentSession::new(AgentSessionConfig {
            agent: stub_agent(),
            session_manager: SessionManager::in_memory("/tmp", None, None),
            settings_manager: SettingsManager::from_storage(
                Box::new(crate::settings_manager::InMemorySettingsStorage::default()),
                false,
            ),
            cwd: "/tmp".to_owned(),
            agent_dir: Some("/tmp/maho-agent".to_owned()),
            fallback_now: None,
            retry_random: None,
            scoped_models: Vec::new(),
            favorite_models: Vec::new(),
            flag_values: BTreeMap::new(),
            custom_tools: Vec::new(),
            model_runtime: Some(runtime),
            model_registry: None,
            uses_default_stream_function: Some(uses_default_stream_function),
            initial_active_tool_names: None,
            default_tool_names: None,
            eval_only_tool_names: None,
            allowed_tool_names: None,
            excluded_tool_names: None,
            base_tools_override: None,
            session_start_event: None,
            auto_title_sessions: None,
        })
        .expect("session")
    }

    #[tokio::test]
    async fn an_unconfigured_provider_refuses_the_request() {
        let session = test_session();
        let error = session.get_required_request_auth(&test_model()).await.expect_err("refused");
        assert!(error.contains("No API key found for faux"), "{error}");
    }

    #[tokio::test]
    async fn a_native_stream_function_falls_back_to_the_bare_model() {
        let session = test_session_with_stream_function(false);
        let auth = session.get_summarization_request_auth(&test_model()).await.expect("auth");
        assert_eq!(auth.model.id, "faux-1");
        assert!(auth.api_key.is_none());
    }

    #[tokio::test]
    async fn the_default_stream_function_requires_request_auth() {
        let session = test_session();
        let error = session.get_summarization_request_auth(&test_model()).await.expect_err("refused");
        assert!(error.contains("No API key found for faux"), "{error}");
    }

    #[test]
    fn a_session_without_model_access_is_refused() {
        let result = AgentSession::new(AgentSessionConfig {
            agent: stub_agent(),
            session_manager: SessionManager::in_memory("/tmp", None, None),
            settings_manager: SettingsManager::from_storage(
                Box::new(crate::settings_manager::InMemorySettingsStorage::default()),
                false,
            ),
            cwd: "/tmp".to_owned(),
            agent_dir: None,
            fallback_now: None,
            retry_random: None,
            scoped_models: Vec::new(),
            favorite_models: Vec::new(),
            flag_values: BTreeMap::new(),
            custom_tools: Vec::new(),
            model_runtime: None,
            model_registry: None,
            uses_default_stream_function: None,
            initial_active_tool_names: None,
            default_tool_names: None,
            eval_only_tool_names: None,
            allowed_tool_names: None,
            excluded_tool_names: None,
            base_tools_override: None,
            session_start_event: None,
            auto_title_sessions: None,
        });
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn listeners_receive_events_until_they_unsubscribe() {
        let session = test_session();
        let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&seen);
        let subscription = session.subscribe(Arc::new(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        }));
        session.emit(AgentSessionEvent::AgentIdle);
        assert_eq!(seen.load(Ordering::SeqCst), 1);
        drop(subscription);
        session.emit(AgentSessionEvent::AgentIdle);
        assert_eq!(seen.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn dispose_clears_the_listeners() {
        let session = test_session();
        let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&seen);
        let _subscription = session.subscribe(Arc::new(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        }));
        session.dispose().await;
        session.emit(AgentSessionEvent::AgentIdle);
        assert_eq!(seen.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn appending_a_session_entry_stores_it() {
        let session = test_session();
        session.append_session_entry(serde_json::json!({
            "id": "entry-1", "type": "message", "timestamp": "2026-01-01T00:00:00.000Z",
            "message": { "role": "user", "content": [{ "type": "text", "text": "hi" }], "timestamp": 0 }
        }));
        assert!(session.with_session_manager(|manager| manager.entry("entry-1")).is_some());
    }

    #[test]
    fn reserving_queued_input_order_is_monotonic() {
        let session = test_session();
        assert_eq!(session.reserve_queued_input_order(), 1);
        assert_eq!(session.reserve_queued_input_order(), 2);
    }

    #[test]
    fn clear_queue_returns_messages_in_recovery_order() {
        let session = test_session();
        session.record_queued_input("b", StreamingBehavior::FollowUp, Some(2));
        session.record_queued_input("a", StreamingBehavior::Steer, Some(1));
        session.state().steering_messages.push("a".to_owned());
        session.state().follow_up_messages.push("b".to_owned());
        let cleared = session.clear_queue(false);
        let ordered: Vec<&str> = cleared.ordered.iter().map(|input| input.text.as_str()).collect();
        assert_eq!(ordered, vec!["a", "b"]);
        assert_eq!(cleared.steering, vec!["a".to_owned()]);
        assert_eq!(cleared.follow_up, vec!["b".to_owned()]);
        assert_eq!(session.pending_message_count(), 0);
    }

    #[test]
    fn setting_a_queue_mode_persists_and_emits() {
        let session = test_session();
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let _subscription = session.subscribe(Arc::new(move |event| {
            sink.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(event.clone());
        }));
        session.set_steering_mode(maho_agent::types::QueueMode::All);
        assert_eq!(session.steering_mode(), maho_agent::types::QueueMode::All);
        let emitted = events.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        assert!(emitted.iter().any(|event| matches!(
            event,
            AgentSessionEvent::SessionSettingsChanged { steering_mode, .. } if steering_mode == "all"
        )));
        assert_eq!(session.with_settings_manager(|manager| manager.get_string("steeringMode")), Some("all".to_owned()));
    }

    #[test]
    fn clamping_prefers_the_next_higher_available_level() {
        let available = [ModelThinkingLevel::Off, ModelThinkingLevel::Low, ModelThinkingLevel::Max];
        assert_eq!(clamp_thinking_level(ModelThinkingLevel::Medium, &available), ModelThinkingLevel::Max);
        assert_eq!(clamp_thinking_level(ModelThinkingLevel::Minimal, &available), ModelThinkingLevel::Low);
        assert_eq!(clamp_thinking_level(ModelThinkingLevel::Off, &available), ModelThinkingLevel::Off);
        assert_eq!(clamp_thinking_level(ModelThinkingLevel::Max, &[ModelThinkingLevel::Off]), ModelThinkingLevel::Off);
    }

    #[test]
    fn only_non_off_levels_reach_the_thinking_level_event() {
        assert_eq!(thinking_level_from_model_level(ModelThinkingLevel::Off), None);
        assert_eq!(thinking_level_from_model_level(ModelThinkingLevel::High), Some(ThinkingLevel::High));
    }

    #[test]
    fn a_non_reasoning_model_supports_no_thinking() {
        let session = test_session();
        assert!(!session.supports_thinking());
        assert_eq!(session.cycle_thinking_level(), None);
        assert_eq!(session.get_available_thinking_levels(), vec![ModelThinkingLevel::Off]);
    }

    #[test]
    fn a_model_switch_clamps_the_default_to_the_supported_levels() {
        let session = test_session();
        assert_eq!(session.get_thinking_for_model_switch(&test_model(), None), ModelThinkingLevel::Off);
        assert_eq!(
            session.get_thinking_for_model_switch(&test_model(), Some(ModelThinkingLevel::Xhigh)),
            ModelThinkingLevel::Off
        );
    }

    #[test]
    fn favorites_are_narrowed_by_the_scoped_set() {
        let session = test_session();
        let model = test_model();
        session.set_favorite_models(vec![SessionModelEntry {
            model: model.clone(),
            thinking_level: None,
            thinking_selection: None,
            service_tier: None,
        }]);
        session.set_scoped_models(vec![SessionModelEntry {
            model: model.clone(),
            thinking_level: None,
            thinking_selection: None,
            service_tier: None,
        }]);
        assert_eq!(session.favorite_models().len(), 1);
    }

    #[test]
    fn a_favorite_outside_the_catalog_is_dropped() {
        let session = test_session();
        session.set_favorite_models(vec![SessionModelEntry {
            model: test_model(),
            thinking_level: None,
            thinking_selection: None,
            service_tier: None,
        }]);
        assert!(session.get_current_favorite_models().is_empty());
    }

    #[test]
    fn a_message_without_the_fallback_receipt_emits_nothing() {
        let session = test_session();
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let _subscription = session.subscribe(Arc::new(move |event| {
            sink.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(event.clone());
        }));
        let message: maho_ai::types::AssistantMessage = serde_json::from_value(serde_json::json!({
            "content": [], "api": "faux", "provider": "faux", "model": "faux-1",
            "usage": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 } },
            "stopReason": "error", "timestamp": 0
        }))
        .expect("assistant");
        session.emit_server_fallback_aborted(&message);
        assert!(events.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_empty());
    }

    fn test_tool(name: &str) -> AgentTool {
        let tool = maho_ai::types::Tool {
            name: name.to_owned(),
            description: format!("{name} tool"),
            parameters: serde_json::json!({ "type": "object" }),
            freeform: None,
            constrained_sampling: None,
        };
        AgentTool {
            label: name.to_owned(),
            prepare_arguments: None,
            execute: Arc::new(|_, _, _, _| Box::pin(async { AgentToolResult::text("ok") })),
            replay: None,
            execution_mode: None,
            tool,
        }
    }

    fn test_definition(name: &str) -> ToolDefinition {
        ToolDefinition::new(
            name,
            &format!("{name} tool"),
            serde_json::json!({ "type": "object" }),
            Arc::new(|_| Box::pin(async { Ok(maho_tools::definition::ToolResult::text("ok")) })),
        )
    }

    #[test]
    fn an_unresolvable_tool_name_is_returned_unchanged() {
        let session = test_session();
        assert_eq!(session.resolve_tool_call_name("missing"), "missing");
    }

    #[test]
    fn registered_tools_are_reachable_by_name() {
        let session = test_session();
        session.register_tool_definition(test_definition("read"), empty_source_info(), test_tool("read"));
        assert!(session.get_registered_tool("read").is_some());
        assert!(session.get_tool_definition("read").is_some());
        assert_eq!(session.resolve_tool_call_name("read"), "read");
    }

    #[test]
    fn unknown_names_are_ignored_when_setting_active_tools() {
        let session = test_session();
        session.register_tool_definition(test_definition("read"), empty_source_info(), test_tool("read"));
        session.set_active_tools_by_name(vec!["read".to_owned(), "nope".to_owned()]);
        assert_eq!(session.get_active_tool_names(), vec!["read".to_owned()]);
    }

    #[test]
    fn armed_eval_only_tools_are_withheld_and_restorable() {
        let session = test_session_with_eval_only(vec!["workflow".to_owned()]);
        session.register_tool_definition(test_definition("eval"), empty_source_info(), test_tool("eval"));
        session.register_tool_definition(test_definition("workflow"), empty_source_info(), test_tool("workflow"));
        session.register_tool_definition(test_definition("read"), empty_source_info(), test_tool("read"));
        session.set_active_tools_by_name(vec!["workflow".to_owned(), "read".to_owned()]);
        assert_eq!(session.get_active_tool_names(), vec!["read".to_owned()]);
        assert!(session.requested_active_tool_names().unwrap().contains(&"workflow".to_owned()));
        assert!(session.agent().removed_tool_hints().contains_key("workflow"));
    }

    fn test_session_with_eval_only(names: Vec<String>) -> AgentSession {
        let runtime = ModelRuntime::create_sync(crate::model_runtime::CreateModelRuntimeOptions {
            providers: Some(Vec::new()),
            ..Default::default()
        });
        AgentSession::new(AgentSessionConfig {
            agent: stub_agent(),
            session_manager: SessionManager::in_memory("/tmp", None, None),
            settings_manager: SettingsManager::from_storage(
                Box::new(crate::settings_manager::InMemorySettingsStorage::default()),
                false,
            ),
            cwd: "/tmp".to_owned(),
            agent_dir: None,
            fallback_now: None,
            retry_random: None,
            scoped_models: Vec::new(),
            favorite_models: Vec::new(),
            flag_values: BTreeMap::new(),
            custom_tools: Vec::new(),
            model_runtime: Some(runtime),
            model_registry: None,
            uses_default_stream_function: None,
            initial_active_tool_names: None,
            default_tool_names: None,
            eval_only_tool_names: Some(names),
            allowed_tool_names: None,
            excluded_tool_names: None,
            base_tools_override: None,
            session_start_event: None,
            auto_title_sessions: None,
        })
        .expect("session")
    }

    fn test_model() -> Model {
        serde_json::from_value(serde_json::json!({
            "id": "faux-1", "name": "faux-1", "api": "faux", "provider": "faux",
            "baseUrl": "", "reasoning": false, "input": [], "contextWindow": 128000, "maxTokens": 4096,
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }
        }))
        .expect("model")
    }
}
