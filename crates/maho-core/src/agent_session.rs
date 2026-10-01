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
    auto_compaction_session_override: Option<bool>,
    turn_index: u64,
    message_replacements: Vec<(AgentMessage, AgentMessage)>,
    retry_attempt: u32,
    retry_abort_controller: Option<maho_ai::utils::abort::AbortController>,
    user_aborted: bool,
    probe_phase: crate::retry_fallback::hint_policy::ProbePhase,
    hint_deadline_ms: Option<f64>,
    cumulative_hinted_wait_ms: f64,
}

fn name_set(names: Option<Vec<String>>) -> Option<BTreeSet<String>> {
    names.map(|names| names.into_iter().collect())
}

#[derive(Clone)]
pub struct AgentSession {
    inner: Arc<AgentSessionInner>,
}

#[doc(hidden)]
pub struct AgentSessionInner {
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
    agent_subscription: Mutex<Option<maho_agent::agent::AgentSubscription>>,
    prompt_admission: tokio::sync::Mutex<()>,
    retry_fallback: tokio::sync::Mutex<Option<crate::retry_fallback::controller::RetryFallbackController<SessionFallbackDeps>>>,
}

struct SessionFallbackDeps(std::sync::Weak<AgentSessionInner>);

impl crate::retry_fallback::controller::RetryFallbackDeps for SessionFallbackDeps {
    fn settings(&self) -> crate::retry_fallback::settings::ResolvedRetryFallbackSettings {
        let settings = self.0.upgrade().map(|inner| AgentSession { inner })
            .and_then(|session| session.with_settings_manager(|manager| manager.get_value("retry").cloned()));
        crate::retry_fallback::settings::resolve_retry_fallback_settings(settings.as_ref())
    }
    fn models(&self) -> Vec<Model> {
        self.0.upgrade().map_or_else(Vec::new, |inner| inner.model_registry.get_all())
    }
    fn current(&self) -> Option<(Model, Option<ModelThinkingLevel>)> {
        self.0.upgrade().map(|inner| {
            let state = inner.agent.state();
            (state.model, Some(state.thinking_level))
        })
    }
    fn is_auth_available(&self, provider: &str) -> bool {
        self.0.upgrade().is_some_and(|inner| inner.model_registry.get_all().iter()
            .any(|model| model.provider == provider && inner.model_registry.has_configured_auth(model)))
    }
    fn is_using_oauth(&self, model: &Model) -> bool {
        self.0.upgrade().is_some_and(|inner| inner.model_registry.is_using_oauth(model))
    }
    fn is_fallback_eligible(&self, model: &Model) -> bool {
        self.0.upgrade().is_some_and(|inner| inner.model_registry.is_fallback_eligible(model))
    }
    fn switch_model<'a>(&'a mut self, model: Model, thinking: ModelThinkingLevel, revert: bool)
        -> maho_ai::types::BoxFuture<'a, Result<(), String>>
    {
        Box::pin(async move {
            let inner = self.0.upgrade().ok_or("Session disposed")?;
            let session = AgentSession { inner };
            let previous = session.model();
            session.agent.set_model(model.clone());
            session.set_session_thinking_level(thinking);
            session.with_session_manager_mut(|manager| manager.append_model_change(
                &model.provider, &model.id, Some(if revert { "fallback-revert" } else { "fallback" }),
                Some((&previous.provider, &previous.id)),
            ));
            Ok(())
        })
    }
    fn emit(&mut self, event: crate::retry_fallback::controller::FallbackEvent) {
        if let Some(inner) = self.0.upgrade() {
            let session = AgentSession { inner };
            use crate::retry_fallback::controller::{FallbackEvent, FallbackReason};
            session.emit(match event {
                FallbackEvent::Applied { from, to, chain_key, reason } => AgentSessionEvent::RetryFallbackApplied {
                    from, to, chain_key, reason: match reason {
                        FallbackReason::Transient => "transient", FallbackReason::Refusal => "refusal",
                        FallbackReason::HardError => "hard-error", FallbackReason::Billing => "billing",
                    }.to_owned(),
                },
                FallbackEvent::Reverted { from, to } => AgentSessionEvent::RetryFallbackReverted { from, to },
            });
        }
    }
}

impl std::ops::Deref for AgentSession {
    type Target = AgentSessionInner;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
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
            auto_compaction_session_override: None,
            turn_index: 0,
            message_replacements: Vec::new(),
            retry_attempt: 0,
            retry_abort_controller: None,
            user_aborted: false,
            probe_phase: crate::retry_fallback::hint_policy::ProbePhase::Idle,
            hint_deadline_ms: None,
            cumulative_hinted_wait_ms: 0.0,
        };
        let session = Self { inner: Arc::new(AgentSessionInner {
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
            agent_subscription: Mutex::new(None),
            prompt_admission: tokio::sync::Mutex::new(()),
            retry_fallback: tokio::sync::Mutex::new(None),
        }) };

        let initial_model = session.agent.state().model;
        let scoped_tier = lock(&session.state)
            .scoped_models
            .iter()
            .find(|entry| models_are_equal(Some(&entry.model), Some(&initial_model)))
            .and_then(|entry| entry.service_tier);
        let tier = resolve_service_tier(&initial_model, scoped_tier);
        lock(&session.state).current_service_tier = tier;

        let weak = Arc::downgrade(&session.inner);
        let subscription = session.agent.subscribe(Arc::new(move |event, signal| {
            let weak = weak.clone();
            Box::pin(async move {
                if let Some(inner) = weak.upgrade() {
                    let session = AgentSession { inner };
                    session.process_agent_event(event, signal).await;
                }
            })
        }));
        *lock(&session.agent_subscription) = Some(subscription);

        let now = session.fallback_now.clone();
        let random = session.retry_random.clone();
        let cooldowns = crate::retry_fallback::cooldown::SelectorCooldowns::new(move || now(), move || random())
            .unwrap_or_else(|error| panic!("Invalid pinned cooldown pattern: {error}"));
        *session.retry_fallback.try_lock().map_err(|_| MissingModelAccessError)? = Some(
            crate::retry_fallback::controller::RetryFallbackController::new(
                SessionFallbackDeps(Arc::downgrade(&session.inner)), cooldowns,
            ),
        );

        Ok(session)
    }

    fn state(&self) -> MutexGuard<'_, AgentSessionState> {
        lock(&self.state)
    }

    async fn process_agent_event(
        &self,
        mut event: maho_agent::types::AgentEvent,
        _signal: maho_ai::utils::abort::AbortSignal,
    ) {
        use maho_agent::types::AgentEvent;
        {
            let mut state = self.state();
            match &mut event {
                AgentEvent::AgentStart => {
                    state.turn_index = 0;
                    state.message_replacements.clear();
                }
                AgentEvent::TurnEnd { message, .. } => {
                    for (original, replacement) in &state.message_replacements {
                        if message == original { *message = replacement.clone(); }
                    }
                }
                AgentEvent::AgentEnd { messages } => {
                    for message in messages {
                        for (original, replacement) in &state.message_replacements {
                            if message == original { *message = replacement.clone(); }
                        }
                    }
                }
                AgentEvent::TurnStart | AgentEvent::MessageStart { .. } |
                AgentEvent::MessageUpdate { .. } | AgentEvent::MessageEnd { .. } |
                AgentEvent::ToolExecutionStart { .. } | AgentEvent::ToolExecutionUpdate { .. } |
                AgentEvent::ToolExecutionEnd { .. } => {}
            }
        }
        if let AgentEvent::MessageEnd { message } = &event
            && let Some(assistant) = message.as_assistant()
        {
            self.emit_server_fallback_aborted(assistant);
        }
        if let AgentEvent::MessageStart { message } = &event
            && message.role() == "user"
        {
            if let Some(controller) = self.retry_fallback.lock().await.as_mut() {
                controller.reset_turn();
            }
            let text = user_message_text(message);
            let removed = {
                let mut state = self.state();
                if let Some(index) = state.steering_messages.iter().position(|queued| queued == &text) {
                    state.steering_messages.remove(index);
                    Some(StreamingBehavior::Steer)
                } else if let Some(index) = state.follow_up_messages.iter().position(|queued| queued == &text) {
                    state.follow_up_messages.remove(index);
                    Some(StreamingBehavior::FollowUp)
                } else {
                    None
                }
            };
            if let Some(mode) = removed {
                self.remove_queued_input(&text, mode);
                self.emit_queue_update();
            }
        }
        let will_retry = if let AgentEvent::AgentEnd { messages } = &event {
            self.will_retry(messages.iter().rev().find_map(AgentMessage::as_assistant)).await
        } else { false };
        if will_retry { self.agent.suppress_queued_message_drain(); }
        let extension_event = match &event {
            AgentEvent::AgentStart => maho_ext_api::ExtensionEvent::AgentStart,
            AgentEvent::AgentEnd { messages } => maho_ext_api::ExtensionEvent::AgentEnd {
                messages: messages.clone(), aborted: Some(self.state().user_aborted), will_retry: Some(will_retry), abort_source: None,
            },
            AgentEvent::TurnStart => maho_ext_api::ExtensionEvent::TurnStart {
                turn_index: self.state().turn_index, timestamp: maho_ai::utils::diagnostics::now_ms() as u64,
            },
            AgentEvent::TurnEnd { message, tool_results } => maho_ext_api::ExtensionEvent::TurnEnd {
                turn_index: self.state().turn_index, message: message.clone(), tool_results: tool_results.clone(),
            },
            AgentEvent::MessageStart { message } => maho_ext_api::ExtensionEvent::MessageStart { message: message.clone() },
            AgentEvent::MessageUpdate { message, assistant_message_event } => maho_ext_api::ExtensionEvent::MessageUpdate {
                message: message.clone(),
                assistant_message_event: serde_json::to_value(assistant_message_event).unwrap_or(Value::Null),
            },
            AgentEvent::MessageEnd { message } => maho_ext_api::ExtensionEvent::MessageEnd { message: message.clone() },
            AgentEvent::ToolExecutionStart { tool_call_id, tool_name, args } => maho_ext_api::ExtensionEvent::ToolExecutionStart {
                tool_call_id: tool_call_id.clone(), tool_name: tool_name.clone(), args: args.clone(),
            },
            AgentEvent::ToolExecutionUpdate { tool_call_id, tool_name, args, partial_result } => maho_ext_api::ExtensionEvent::ToolExecutionUpdate {
                tool_call_id: tool_call_id.clone(), tool_name: tool_name.clone(), args: args.clone(), partial_result: partial_result.clone(),
            },
            AgentEvent::ToolExecutionEnd { tool_call_id, tool_name, result, is_error } => maho_ext_api::ExtensionEvent::ToolExecutionEnd {
                tool_call_id: tool_call_id.clone(), tool_name: tool_name.clone(), result: result.clone(), is_error: *is_error,
            },
        };
        if let AgentEvent::MessageEnd { message } = &mut event {
            let replacement = {
                let mut runner = self.extension_runner.lock().await;
                match runner.as_mut() {
                    Some(runner) => runner.emit_message_end(message.clone()).await,
                    None => Ok(None),
                }
            };
            match replacement {
                Ok(Some(replacement)) => {
                    self.state().message_replacements.push((message.clone(), replacement.clone()));
                    let mut messages = self.messages();
                    if let Some(original) = messages.iter_mut().rev().find(|original| *original == message) {
                        *original = replacement.clone();
                        self.agent.set_messages(messages);
                    }
                    *message = replacement;
                }
                Ok(None) => {}
                Err(error) => self.emit(AgentSessionEvent::ContinuationError { error_message: error.to_string() }),
            }
        } else {
            self.dispatch_extension_event(extension_event).await;
        }
        if matches!(event, AgentEvent::TurnEnd { .. }) {
            self.state().turn_index += 1;
        }
        self.emit(AgentSessionEvent::Agent(event.clone()));
        if let AgentEvent::MessageEnd { message } = &event {
            if let AgentMessage::Custom(maho_agent::types::CustomAgentMessage::Custom(custom)) = message {
                match serde_json::to_value(&custom.content) {
                    Ok(content) => {
                        self.with_session_manager_mut(|manager| manager.append_custom_message(
                            &custom.custom_type, content, custom.display, custom.details.clone(),
                        ));
                        self.state().message_revision += 1;
                    }
                    Err(error) => self.emit(AgentSessionEvent::ContinuationError { error_message: error.to_string() }),
                }
                return;
            }
            if !matches!(message.role(), "user" | "assistant" | "toolResult") {
                return;
            }
            match serde_json::to_value(message) {
                Ok(message) => {
                    let entry = self.with_session_manager_mut(|manager| manager.append_message(message));
                    if let Some(id) = entry.get("id").and_then(Value::as_str) {
                        self.emit_entry_appended(id);
                    }
                    self.state().message_revision += 1;
                }
                Err(error) => self.emit(AgentSessionEvent::ContinuationError { error_message: error.to_string() }),
            }
            if let Some(assistant) = message.as_assistant()
                && assistant.error_message.is_none()
                && !matches!(assistant.stop_reason, StopReason::Error | StopReason::Aborted)
                && !maho_ai::utils::stop_details::is_classifier_refusal(assistant)
            {
                let attempt = self.state().retry_attempt;
                if attempt > 0 {
                    if let Some(fallback) = self.retry_fallback.lock().await.as_ref().and_then(|controller| controller.state.clone()) {
                        self.emit(AgentSessionEvent::RetryFallbackSucceeded {
                            model: format!("{}/{}", self.model().provider, self.model().id), chain_key: fallback.chain_key,
                        });
                    }
                    self.state().retry_attempt = 0;
                    self.reset_hint_tier_state();
                    self.emit(AgentSessionEvent::AutoRetryEnd { success: true, attempt, final_error: None });
                }
            }
        }
    }

    async fn dispatch_extension_event(&self, event: maho_ext_api::ExtensionEvent) {
        let result = {
            let mut guard = self.extension_runner.lock().await;
            match guard.as_mut() {
                Some(runner) => runner.emit(event).await.map(|_| ()),
                None => Ok(()),
            }
        };
        if let Err(error) = result {
            self.emit(AgentSessionEvent::ContinuationError { error_message: error.to_string() });
        }
    }

    /// Start a prompt, or explicitly queue input when a provider turn is active.
    pub async fn prompt(&self, text: &str, options: PromptOptions) -> Result<PromptDisposition, String> {
        if options.signal.as_ref().is_some_and(|signal| signal.aborted()) {
            return Err("Prompt cancelled".to_owned());
        }
        if self.is_streaming() {
            let mode = options.streaming_behavior.ok_or_else(||
                "Agent is already processing a prompt. Use steer() or followUp() to queue messages, or wait for completion.".to_owned())?;
            self.queue_user_input(text, options.images, mode, QueuedInputOptions { source: options.source, ..Default::default() }).await?;
            return Ok(PromptDisposition::Queued);
        }
        let _admission = self.prompt_admission.lock().await;
        self.state().user_aborted = false;
        let Some((text, images)) = self.run_input_handlers(text, options.images, options.source, None).await? else {
            return Ok(PromptDisposition::Handled);
        };
        if let Some(level) = options.thinking_level {
            self.set_session_thinking_level(match level {
                ThinkingLevel::Minimal => ModelThinkingLevel::Minimal,
                ThinkingLevel::Low => ModelThinkingLevel::Low,
                ThinkingLevel::Medium => ModelThinkingLevel::Medium,
                ThinkingLevel::High => ModelThinkingLevel::High,
                ThinkingLevel::Xhigh => ModelThinkingLevel::Xhigh,
                ThinkingLevel::Max => ModelThinkingLevel::Max,
            });
        }
        self.agent.prompt(maho_agent::agent::AgentPromptInput::Message(make_user_message(&text, images))).await;
        self.finish_provider_turn().await?;
        self.dispatch_extension_event(maho_ext_api::ExtensionEvent::AgentSettled).await;
        self.emit(AgentSessionEvent::AgentSettled);
        self.emit(AgentSessionEvent::AgentIdle);
        Ok(PromptDisposition::Started)
    }

    async fn run_input_handlers(
        &self, text: &str, images: Option<Vec<ImageContent>>, source: Option<InputSource>,
        streaming_behavior: Option<StreamingBehavior>,
    ) -> Result<Option<(String, Option<Vec<ImageContent>>)>, String> {
        let mut runner = self.extension_runner.lock().await;
        let Some(runner) = runner.as_mut() else { return Ok(Some((text.to_owned(), images))); };
        let result = runner.emit_input(maho_ext_api::InputEvent {
            input_id: format!("{}:{}", self.session_id(), self.reserve_queued_input_order()),
            text: text.to_owned(), images: images.clone(), source: source.unwrap_or(InputSource::Interactive), streaming_behavior,
        }).await.map_err(|error| error.to_string())?;
        match result {
            maho_ext_api::InputEventResult::Continue => Ok(Some((text.to_owned(), images))),
            maho_ext_api::InputEventResult::Transform { text, images } => Ok(Some((text, images))),
            maho_ext_api::InputEventResult::Handled => Ok(None),
        }
    }

    async fn queue_user_input(
        &self, text: &str, images: Option<Vec<ImageContent>>, mode: StreamingBehavior, options: QueuedInputOptions,
    ) -> Result<(), String> {
        let Some((text, images)) = self.run_input_handlers(text, images, options.source, self.is_streaming().then_some(mode)).await? else {
            return Ok(());
        };
        self.record_queued_input(&text, mode, options.enqueue_order);
        let message = make_user_message(&text, images);
        match mode {
            StreamingBehavior::Steer => {
                self.state().steering_messages.push(text);
                self.agent.steer(message);
            }
            StreamingBehavior::FollowUp => {
                self.state().follow_up_messages.push(text);
                self.agent.follow_up(message);
            }
        }
        self.emit_queue_update();
        Ok(())
    }

    pub async fn steer(&self, text: &str, images: Option<Vec<ImageContent>>, options: QueuedInputOptions) -> Result<(), String> {
        self.queue_user_input(text, images, StreamingBehavior::Steer, options).await
    }

    pub async fn follow_up(&self, text: &str, images: Option<Vec<ImageContent>>, options: QueuedInputOptions) -> Result<(), String> {
        self.queue_user_input(text, images, StreamingBehavior::FollowUp, options).await
    }

    pub async fn abort(&self) {
        self.state().user_aborted = true;
        self.abort_retry();
        let pending = !self.is_streaming() && (self.pending_message_count() > 0 || self.state().had_cleared_queued_messages);
        self.state().had_cleared_queued_messages = false;
        self.agent.suppress_queued_message_drain();
        self.agent.abort(None);
        self.agent.wait_for_idle().await;
        if pending {
            self.dispatch_extension_event(maho_ext_api::ExtensionEvent::SessionAbort).await;
            self.emit(AgentSessionEvent::SessionAbort);
        }
    }

    pub fn is_retrying(&self) -> bool { self.state().retry_attempt > 0 }

    pub fn retry_attempt(&self) -> u32 { self.state().retry_attempt }

    pub fn abort_retry(&self) {
        if let Some(controller) = self.state().retry_abort_controller.as_ref() { controller.abort(None); }
    }

    pub fn auto_retry_enabled(&self) -> bool {
        self.with_settings_manager(|manager| manager.get_value("retry")
            .and_then(|settings| settings.get("enabled")).and_then(Value::as_bool).unwrap_or(true))
    }

    fn reset_hint_tier_state(&self) {
        let mut state = self.state();
        state.probe_phase = crate::retry_fallback::hint_policy::ProbePhase::Idle;
        state.hint_deadline_ms = None;
        state.cumulative_hinted_wait_ms = 0.0;
    }

    pub fn fallback_validation_warnings(&self) -> Vec<String> {
        let settings = self.with_settings_manager(|manager| manager.get_value("retry").cloned());
        crate::retry_fallback::validate::validate_fallback_chains(
            settings.as_ref().and_then(|settings| settings.get("fallbackChains")), &self.model_registry.get_all(),
        )
    }

    pub fn set_auto_retry_enabled(&self, enabled: bool) -> Result<(), String> {
        let mut retry = self.with_settings_manager(|manager| manager.get_value("retry").cloned())
            .and_then(|retry| retry.as_object().cloned()).unwrap_or_default();
        retry.insert("enabled".to_owned(), Value::Bool(enabled));
        self.with_settings_manager_mut(|manager| manager.set(crate::settings_manager::SettingsScope::Global,
            &Map::from_iter([("retry".to_owned(), Value::Object(retry))])))
    }

    pub async fn wait_for_retry(&self) {
        let _admission = self.prompt_admission.lock().await;
    }

    async fn will_retry(&self, message: Option<&maho_ai::types::AssistantMessage>) -> bool {
        let Some(message) = message else { return false; };
        if self.state().user_aborted || !self.with_settings_manager(|manager| manager.get_value("retry")
            .and_then(|settings| settings.get("enabled")).and_then(Value::as_bool).unwrap_or(true)) { return false; }
        if message.error_message.as_deref().is_some_and(|error| error.starts_with(
            maho_ai::utils::provider_failure_description::TURN_RETRY_SUPPRESSION_PREFIX))
            || maho_ai::utils::overflow::is_context_overflow(message, Some(self.model().context_window)) { return false; }
        if maho_ai::utils::retry::is_retryable_assistant_error(message)
            || maho_ai::utils::retry::is_provider_timeout_error(message)
            || maho_ai::utils::stop_details::is_classifier_refusal(message) { return true; }
        message.stop_reason == StopReason::Error
            && !message.content.iter().any(|content| matches!(content, maho_ai::types::ContentBlock::ToolCall(_)))
            && self.retry_fallback.lock().await.as_mut().is_some_and(|controller| controller.can_try_fallback())
    }

    async fn finish_provider_turn(&self) -> Result<(), String> {
        use crate::retry_fallback::controller::FallbackReason;
        use maho_ai::utils::retry_hint::parse_retry_after_ms_marker;
        loop {
            self.agent.wait_for_idle().await;
            let Some(message) = self.messages().iter().rev().find_map(AgentMessage::as_assistant).cloned() else { return Ok(()); };
            if !self.will_retry(Some(&message)).await { return Ok(()); }
            let settings = self.with_settings_manager(|manager| manager.get_value("retry").cloned()).unwrap_or(Value::Null);
            let max_attempts = settings.get("maxRetries").and_then(Value::as_u64).and_then(|value| u32::try_from(value).ok()).unwrap_or(5);
            let base_delay = settings.get("baseDelayMs").and_then(Value::as_u64).unwrap_or(2_000);
            let cap = settings.get("maxAgentDelayMs").and_then(Value::as_u64).unwrap_or(60_000);
            let error = message.error_message.clone().unwrap_or_else(|| "Unknown error".to_owned());
            let hint = parse_retry_after_ms_marker(&error);
            let refusal = maho_ai::utils::stop_details::is_classifier_refusal(&message);
            let transient = maho_ai::utils::retry::is_retryable_assistant_error(&message)
                || maho_ai::utils::retry::is_provider_timeout_error(&message);
            let attempt = self.state().retry_attempt.saturating_add(1);
            let rate_limited = ["rate limit", "rate_limit", "429", "too many requests", "resource_exhausted"]
                .iter().any(|marker| error.to_lowercase().contains(marker));
            let hint_settings = crate::retry_fallback::settings::resolve_hint_policy_settings(Some(&settings));
            let tier = crate::retry_fallback::hint_policy::classify_rate_limited_wait(hint.map(|hint| hint as f64), hint_settings);
            let mut hint_delay = None;
            if rate_limited && tier == crate::retry_fallback::hint_policy::HintTier::Tier1InTurn {
                let mut state = self.state();
                let result = crate::retry_fallback::hint_policy::next_in_turn_delay_ms(
                    crate::retry_fallback::hint_policy::InTurnState {
                        probe_phase: state.probe_phase, hint_deadline_ms: state.hint_deadline_ms,
                        attempt, cumulative_hinted_wait_ms: state.cumulative_hinted_wait_ms,
                    }, hint.map(|hint| hint as f64), base_delay as f64, hint_settings.hinted_wait_cap_ms, self.fallback_now(),
                );
                state.probe_phase = result.probe_phase;
                state.hint_deadline_ms = result.hint_deadline_ms;
                state.cumulative_hinted_wait_ms = result.cumulative_hinted_wait_ms;
                if !result.demote_to_probe_back { hint_delay = Some(result.delay_ms as u64); }
            }
            let needs_fallback = refusal || !transient || attempt > max_attempts ||
                if rate_limited { hint_delay.is_none() } else { hint.is_some_and(|hint| hint > cap) };
            let mut switched = false;
            if needs_fallback {
                let reason = if refusal { FallbackReason::Refusal } else if transient { FallbackReason::Transient }
                    else if crate::retry_fallback::billing::is_billing_error_message(Some(&error)) { FallbackReason::Billing }
                    else { FallbackReason::HardError };
                let mut controller = self.retry_fallback.lock().await;
                if let Some(controller) = controller.as_mut() {
                    switched = controller.try_fallback(reason, crate::retry_fallback::cooldown::SelectorFailure {
                        error_message: Some(&error), retry_after_ms: hint.map(|hint| hint as f64),
                    }).await?;
                    if !switched && let Some(chain_key) = controller.exhausted_chain_key.clone() {
                        self.emit(AgentSessionEvent::RetryFallbackExhausted { chain_key, last_error: error.clone() });
                    }
                }
                if !switched && rate_limited && attempt <= max_attempts && !refusal {
                    match crate::retry_fallback::hint_policy::degrade_without_fallback(
                        tier, hint.map(|hint| hint as f64), attempt, base_delay as f64, hint_settings.hinted_wait_cap_ms,
                    ) {
                        crate::retry_fallback::hint_policy::DegradedRateLimitAction::InTurn { delay_ms } => hint_delay = Some(delay_ms as u64),
                        crate::retry_fallback::hint_policy::DegradedRateLimitAction::Fail { .. } => {}
                    }
                }
                if !switched && hint_delay.is_none() {
                    let attempt = self.state().retry_attempt;
                    self.state().retry_attempt = 0;
                    self.reset_hint_tier_state();
                    self.emit(AgentSessionEvent::AutoRetryEnd { success: false, attempt, final_error: Some(error) });
                    return Ok(());
                }
            }
            let attempt = if switched { 1 } else { attempt };
            self.state().retry_attempt = attempt;
            let delay_ms = if switched { 0 } else { hint_delay.or(hint).unwrap_or_else(|| base_delay.saturating_mul(
                2_u64.saturating_pow(attempt.saturating_sub(1)))).min(cap) };
            let abort = maho_ai::utils::abort::AbortController::new();
            self.state().retry_abort_controller = Some(abort.clone());
            self.emit(AgentSessionEvent::AutoRetryStart { attempt, max_attempts, delay_ms, error_message: error });
            let mut messages = self.messages();
            if messages.last().is_some_and(|message| message.role() == "assistant") {
                messages.pop();
                self.agent.set_messages(messages);
                self.state().message_revision += 1;
            }
            let signal = abort.signal();
            tokio::select! {
                _ = signal.cancelled() => {
                    self.state().retry_abort_controller = None;
                    self.state().retry_attempt = 0;
                    self.reset_hint_tier_state();
                    self.emit(AgentSessionEvent::AutoRetryEnd { success: false, attempt, final_error: Some("Retry cancelled".to_owned()) });
                    return Ok(());
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(delay_ms)) => {}
            }
            self.state().retry_abort_controller = None;
            let policy = crate::retry_fallback::settings::resolve_retry_fallback_settings(Some(&settings)).revert_policy;
            if let Some(controller) = self.retry_fallback.lock().await.as_mut() { controller.maybe_restore_primary(policy).await?; }
            let plan = crate::provider_timeout_retry::create_provider_timeout_retry_plan(&message,
                crate::provider_timeout_retry::ProviderTimeoutRetryPlanInput {
                    stream_retry_timeout_ms: settings.get("provider").and_then(|provider| provider.get("streamRetryTimeoutMs"))
                        .and_then(Value::as_u64).or(Some(30_000)),
                    timeout_ms: self.agent.timeout_ms(), stream_start_timeout_ms: self.agent.stream_start_timeout_ms(),
                });
            let continuation = self.agent.continue_run(plan.options);
            if let Some(timeout) = plan.watchdog_timeout_ms {
                tokio::pin!(continuation);
                tokio::select! {
                    _ = &mut continuation => {}
                    _ = tokio::time::sleep(std::time::Duration::from_millis(timeout)) => {
                        self.agent.abort(Some(maho_ai::utils::abort::AbortReason::new("AbortError",
                            provider_retry_watchdog_abort_message(Some(timeout), self.agent.stream_start_timeout_ms()))));
                        continuation.await;
                    }
                }
            } else { continuation.await; }
        }
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

    pub async fn bind_extensions(&self, bindings: ExtensionBindings) {
        {
            let mut state = self.state();
            if let Some(ui) = bindings.ui_context { state.extension_ui_context = Some(ui); }
            if let Some(mode) = bindings.mode { state.extension_mode = mode; }
            if let Some(handler) = bindings.abort_handler { state.extension_abort_handler = Some(handler); }
            if let Some(listener) = bindings.on_error { state.extension_error_listener = Some(listener); }
        }
        let event = self.state().session_start_event.clone();
        self.dispatch_extension_event(maho_ext_api::ExtensionEvent::SessionStart(event)).await;
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
        lock(&self.agent_subscription).take();
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

    /// Set the session display name and publish the change.
    ///
    /// Adaptation: the extension `session_info_changed` emit is not replicated because the runner's
    /// raw-event emit needs an `ExtensionEvent` variant for it; the session event is emitted.
    pub fn set_session_name(&self, name: &str) {
        self.with_session_manager_mut(|manager| {
            manager.append_session_info(Some(name));
        });
        self.emit(AgentSessionEvent::SessionInfoChanged { name: self.session_name() });
    }

    /// User messages available for forking, in session order.
    pub fn get_user_messages_for_forking(&self) -> Vec<(String, String)> {
        let entries = self.with_session_manager(|manager| manager.entries());
        let mut result = Vec::new();
        for entry in entries {
            if entry.get("type").and_then(Value::as_str) != Some("message") {
                continue;
            }
            let Some(message) = entry.get("message") else {
                continue;
            };
            if message.get("role").and_then(Value::as_str) != Some("user") {
                continue;
            }
            let text = message
                .get("content")
                .and_then(|content| serde_json::from_value::<Vec<maho_ai::types::ContentBlock>>(content.clone()).ok())
                .map_or_else(String::new, |content| maho_ai::utils::text::content_text(&content, ""));
            if !text.is_empty() {
                let entry_id = entry.get("id").and_then(Value::as_str).unwrap_or_default().to_owned();
                result.push((entry_id, text));
            }
        }
        result
    }

    /// Session statistics, aggregated over every session entry (including compacted-away history).
    pub fn get_session_stats(&self) -> SessionStats {
        let entries = self.with_session_manager(|manager| manager.entries());
        let mut user_messages = 0;
        let mut assistant_messages = 0;
        let mut tool_results = 0;
        let mut total_messages = 0;
        let mut tool_calls = 0;
        let mut totals = crate::usage_totals::create_usage_totals();
        for entry in &entries {
            let kind = entry.get("type").and_then(Value::as_str).unwrap_or_default();
            if (kind == "branch_summary" || kind == "compaction")
                && let Some(usage) = entry.get("usage").and_then(|usage| serde_json::from_value(usage.clone()).ok())
            {
                crate::usage_totals::add_usage_to_totals(&mut totals, &usage);
            }
            if kind != "message" {
                continue;
            }
            total_messages += 1;
            let Some(message) = entry.get("message") else {
                continue;
            };
            match message.get("role").and_then(Value::as_str) {
                Some("user") => user_messages += 1,
                Some("toolResult") => {
                    tool_results += 1;
                    if let Some(usage) = message.get("usage").and_then(|usage| serde_json::from_value(usage.clone()).ok()) {
                        crate::usage_totals::add_usage_to_totals(&mut totals, &usage);
                    }
                }
                Some("assistant") => {
                    assistant_messages += 1;
                    if let Some(content) = message.get("content").and_then(Value::as_array) {
                        tool_calls += content
                            .iter()
                            .filter(|block| block.get("type").and_then(Value::as_str) == Some("toolCall"))
                            .count();
                    }
                    if let Some(usage) = message.get("usage").and_then(|usage| serde_json::from_value(usage.clone()).ok()) {
                        crate::usage_totals::add_usage_to_totals(&mut totals, &usage);
                    }
                }
                _ => {}
            }
        }
        SessionStats {
            session_file: self.session_file(),
            session_id: self.session_id(),
            user_messages,
            assistant_messages,
            tool_calls,
            tool_results,
            total_messages,
            tokens: SessionStatsTokens {
                input: totals.input as u64,
                output: totals.output as u64,
                cache_read: totals.cache_read as u64,
                cache_write: totals.cache_write as u64,
                total: (totals.input + totals.output + totals.cache_read + totals.cache_write) as u64,
            },
            cost: totals.cost,
            context_usage: self.get_context_usage(),
        }
    }

    /// Context usage for the live model, or `None` when the model has no context window.
    pub fn get_context_usage(&self) -> Option<ContextUsage> {
        let context_window = self.model().context_window;
        if context_window == 0 {
            return None;
        }
        let messages: Vec<Value> = self
            .messages()
            .iter()
            .filter_map(|message| serde_json::to_value(message).ok())
            .collect();
        let messages = crate::messages::filter_context_excluded_messages(messages);
        let branch = self.with_session_manager(|manager| manager.branch(None));
        if let Some(latest) = crate::session_manager::get_latest_compaction_entry(&branch) {
            let compaction_index = branch.iter().rposition(|entry| entry == &latest).unwrap_or(0);
            let has_post_compaction_usage = branch[compaction_index + 1..].iter().rev().any(|entry| {
                entry.get("type").and_then(Value::as_str) == Some("message")
                    && entry.get("message").and_then(|message| message.get("role")).and_then(Value::as_str)
                        == Some("assistant")
                    && entry
                        .get("message")
                        .and_then(|message| serde_json::from_value::<maho_ai::types::AssistantMessage>(message.clone()).ok())
                        .is_some_and(|assistant| {
                            !matches!(
                                assistant.stop_reason,
                                maho_ai::types::StopReason::Aborted | maho_ai::types::StopReason::Error
                            ) && crate::compaction::calculate_context_tokens(&assistant.usage) > 0
                        })
            });
            if !has_post_compaction_usage {
                let tokens: u64 = messages.iter().map(crate::compaction::estimate_tokens).sum();
                return Some(ContextUsage {
                    tokens: Some(tokens),
                    context_window,
                    percent: Some((tokens as f64 / context_window as f64) * 100.0),
                });
            }
        }
        let estimate = crate::compaction::estimate_context_tokens(&messages);
        Some(ContextUsage {
            tokens: Some(estimate.tokens),
            context_window,
            percent: Some((estimate.tokens as f64 / context_window as f64) * 100.0),
        })
    }

    /// Export the current branch to a linear JSONL session file; returns the path written.
    pub fn export_to_jsonl(&self, output_path: Option<&str>) -> Result<String, std::io::Error> {
        let file_path = output_path.map_or_else(
            || format!("session-{}.jsonl", chrono::Utc::now().to_rfc3339().replace([':', '.'], "-")),
            str::to_owned,
        );
        if let Some(parent) = std::path::Path::new(&file_path).parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let header = serde_json::json!({
            "type": "session",
            "version": crate::session_manager::CURRENT_SESSION_VERSION,
            "id": self.session_id(),
            "timestamp": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "cwd": self.with_session_manager(|manager| manager.cwd().to_owned()),
        });
        let branch = self.with_session_manager(|manager| manager.branch(None));
        let mut lines = vec![header.to_string()];
        let mut previous: Option<String> = None;
        for entry in branch {
            let mut linear = entry.clone();
            if let Some(object) = linear.as_object_mut() {
                object.insert("parentId".to_owned(), previous.clone().map_or(Value::Null, Value::String));
            }
            lines.push(linear.to_string());
            previous = entry.get("id").and_then(Value::as_str).map(str::to_owned);
        }
        std::fs::write(&file_path, format!("{}\n", lines.join("\n")))?;
        Ok(file_path)
    }

    /// Text content of the last assistant message, for `/copy`.
    pub fn get_last_assistant_text(&self) -> Option<String> {
        let messages = self.messages();
        let last = messages.iter().rev().find_map(|message| {
            let assistant = message.as_assistant()?;
            if assistant.stop_reason == maho_ai::types::StopReason::Aborted && assistant.content.is_empty() {
                return None;
            }
            Some(assistant)
        })?;
        let mut text = String::new();
        for content in &last.content {
            if let maho_ai::types::ContentBlock::Text(block) = content {
                text.push_str(&block.text);
            }
        }
        let trimmed = text.trim();
        if trimmed.is_empty() { None } else { Some(trimmed.to_owned()) }
    }

    /// Whether extensions registered handlers for an event kind.
    pub async fn has_extension_handlers(&self, kind: maho_ext_api::EventKind) -> bool {
        self.extension_runner
            .lock()
            .await
            .as_ref()
            .is_some_and(|runner| runner.has_handlers(kind))
    }

    /// Resolve once the current run and all awaited event listeners have finished.
    pub async fn wait_for_idle(&self) {
        self.agent.wait_for_idle().await;
    }

    /// Emit a `session_shutdown` event through the bound extension runner.
    pub async fn emit_session_shutdown(&self, reason: maho_ext_api::SessionReason) {
        let mut guard = self.extension_runner.lock().await;
        if let Some(runner) = guard.as_mut() {
            let event = maho_ext_api::ExtensionEvent::SessionShutdown(maho_ext_api::SessionShutdownEvent {
                reason,
                target_session_file: None,
                signal: None,
            });
            let _ = runner.emit(event).await;
        }
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
        if let Some(enabled) = self.state().auto_compaction_session_override {
            return enabled;
        }
        self.with_settings_manager(|manager| {
            manager
                .get_value("compaction")
                .and_then(|value| value.get("enabled"))
                .and_then(Value::as_bool)
                .unwrap_or(true)
        })
    }

    pub fn set_auto_compaction_enabled(&self, enabled: bool) {
        self.state().auto_compaction_session_override = Some(enabled);
        self.emit_session_settings_changed();
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

fn make_user_message(text: &str, images: Option<Vec<ImageContent>>) -> AgentMessage {
    let mut content = vec![maho_ai::types::ContentBlock::Text(maho_ai::types::TextContent {
        text: text.to_owned(), ..Default::default()
    })];
    content.extend(images.unwrap_or_default().into_iter().map(maho_ai::types::ContentBlock::Image));
    maho_ai::types::Message::User(maho_ai::types::UserMessage {
        content: maho_ai::types::UserContent::Blocks(content),
        timestamp: maho_ai::utils::diagnostics::now_ms(),
    }).into()
}

fn user_message_text(message: &AgentMessage) -> String {
    match message.try_as_llm() {
        Some(maho_ai::types::Message::User(user)) => match &user.content {
            maho_ai::types::UserContent::Text(text) => text.clone(),
            maho_ai::types::UserContent::Blocks(content) => maho_ai::utils::text::content_text(content, ""),
        },
        _ => String::new(),
    }
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
    fn auto_compaction_override_emits_settings_without_changing_persisted_defaults() {
        let session = test_session();
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        let _subscription = session.subscribe(Arc::new(move |event| {
            lock(&sink).push(event.clone());
        }));
        session.set_auto_compaction_enabled(false);
        assert!(!session.auto_compaction_enabled());
        assert!(session.with_settings_manager(|manager| manager.get_value("compaction").is_none()));
        assert!(matches!(lock(&events).last(), Some(AgentSessionEvent::SessionSettingsChanged {
            auto_compaction_enabled: false, ..
        })));
    }

    #[tokio::test]
    async fn queued_user_start_updates_queue_before_message_event() {
        let session = test_session();
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        let _subscription = session.subscribe(Arc::new(move |event| {
            lock(&sink).push(event.clone());
        }));
        session.steer("queued", None, QueuedInputOptions::default()).await.expect("steer");
        assert_eq!(session.get_steering_messages(), vec!["queued"]);
        session.process_agent_event(maho_agent::types::AgentEvent::MessageStart {
            message: make_user_message("queued", None),
        }, maho_ai::utils::abort::AbortController::new().signal()).await;
        assert_eq!(session.pending_message_count(), 0);
        let events = lock(&events);
        assert!(matches!(&events[1], AgentSessionEvent::QueueUpdate { steering, .. } if steering.is_empty()));
        assert!(matches!(&events[2], AgentSessionEvent::Agent(maho_agent::types::AgentEvent::MessageStart { .. })));
    }

    #[tokio::test]
    async fn aborting_pending_input_emits_session_abort_once() {
        let session = test_session();
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        let _subscription = session.subscribe(Arc::new(move |event| { lock(&sink).push(event.clone()); }));
        session.follow_up("later", None, QueuedInputOptions::default()).await.expect("follow up");
        session.clear_queue(true);
        session.abort().await;
        session.abort().await;
        assert_eq!(lock(&events).iter().filter(|event| matches!(event, AgentSessionEvent::SessionAbort)).count(), 1);
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

    #[test]
    fn session_stats_aggregate_over_entries() {
        let session = test_session();
        session.append_session_entry(serde_json::json!({
            "id": "u1", "type": "message", "timestamp": "2026-01-01T00:00:00.000Z",
            "message": { "role": "user", "content": [{ "type": "text", "text": "hi" }], "timestamp": 0 }
        }));
        session.append_session_entry(serde_json::json!({
            "id": "a1", "type": "message", "timestamp": "2026-01-01T00:00:01.000Z",
            "message": { "role": "assistant", "content": [{ "type": "text", "text": "hello" }], "api": "faux",
                "provider": "faux", "model": "faux-1", "stopReason": "stop", "timestamp": 1,
                "usage": { "input": 10, "output": 5, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 15,
                    "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0.5 } } }
        }));
        let stats = session.get_session_stats();
        assert_eq!(stats.user_messages, 1);
        assert_eq!(stats.assistant_messages, 1);
        assert_eq!(stats.total_messages, 2);
        assert_eq!(stats.tokens.input, 10);
        assert_eq!(stats.tokens.output, 5);
        assert_eq!(stats.tokens.total, 15);
        assert_eq!(stats.cost, 0.5);
    }

    #[test]
    fn forking_lists_user_messages_in_order() {
        let session = test_session();
        session.append_session_entry(serde_json::json!({
            "id": "u1", "type": "message", "timestamp": "2026-01-01T00:00:00.000Z",
            "message": { "role": "user", "content": [{ "type": "text", "text": "first" }], "timestamp": 0 }
        }));
        session.append_session_entry(serde_json::json!({
            "id": "u2", "type": "message", "timestamp": "2026-01-01T00:00:01.000Z",
            "message": { "role": "user", "content": [{ "type": "text", "text": "second" }], "timestamp": 1 }
        }));
        assert_eq!(
            session.get_user_messages_for_forking(),
            vec![("u1".to_owned(), "first".to_owned()), ("u2".to_owned(), "second".to_owned())]
        );
    }

    #[test]
    fn a_model_with_a_context_window_reports_usage() {
        let session = test_session();
        assert!(session.get_context_usage().is_none(), "the default agent model has no context window");
        session.agent().set_model(test_model());
        let usage = session.get_context_usage().expect("usage");
        assert_eq!(usage.context_window, 128_000);
        assert!(usage.tokens.is_some());
    }

    #[test]
    fn exporting_linearizes_the_branch_parent_ids() {
        let dir = tempfile::tempdir().expect("tempdir");
        let session = test_session();
        session.append_session_entry(serde_json::json!({
            "id": "u1", "type": "message", "timestamp": "2026-01-01T00:00:00.000Z",
            "message": { "role": "user", "content": [{ "type": "text", "text": "hi" }], "timestamp": 0 }
        }));
        let path = dir.path().join("out.jsonl");
        let written = session.export_to_jsonl(Some(path.to_str().expect("path"))).expect("export");
        let content = std::fs::read_to_string(&written).expect("read");
        let mut lines = content.lines();
        let header: Value = serde_json::from_str(lines.next().expect("header")).expect("header json");
        assert_eq!(header.get("type").and_then(Value::as_str), Some("session"));
        let first: Value = serde_json::from_str(lines.next().expect("entry")).expect("entry json");
        assert!(first.get("parentId").expect("parentId").is_null());
    }

    #[test]
    fn the_session_name_change_is_published() {
        let session = test_session();
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let _subscription = session.subscribe(Arc::new(move |event| {
            sink.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(event.clone());
        }));
        session.set_session_name("lane 21");
        assert_eq!(session.session_name(), Some("lane 21".to_owned()));
        assert!(events.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter().any(|event| matches!(
            event,
            AgentSessionEvent::SessionInfoChanged { name } if name.as_deref() == Some("lane 21")
        )));
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

    #[tokio::test]
    async fn custom_message_end_persists_custom_entry() {
        // Given a session and an extension custom message.
        let session = test_session();
        let message = AgentMessage::Custom(maho_agent::types::CustomAgentMessage::Custom(
            maho_agent::harness::messages::CustomMessage {
                role: "custom".to_owned(), custom_type: "notice".to_owned(),
                content: maho_agent::harness::messages::CustomMessageContent::Text("payload".to_owned()),
                display: true, details: Some(serde_json::json!({"origin": "extension"})), timestamp: 0,
            },
        ));
        // When the finalized message passes through session persistence.
        session.process_agent_event(
            maho_agent::types::AgentEvent::MessageEnd { message },
            maho_ai::utils::abort::AbortController::new().signal(),
        ).await;
        // Then it is stored as a custom entry, not an LLM message.
        let entries = session.with_session_manager(|manager| manager.entries());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["type"], "custom_message");
        assert_eq!(entries[0]["customType"], "notice");
        assert_eq!(entries[0]["content"], "payload");
        assert_eq!(entries[0]["details"]["origin"], "extension");
        assert_eq!(session.message_revision(), 1);
    }

    #[tokio::test]
    async fn replacement_is_retained_in_later_turn_events() {
        // Given an earlier message_end replacement and a later turn_end carrying the original.
        let session = test_session();
        let original = make_user_message("original", None);
        let replacement = make_user_message("replacement", None);
        session.state().message_replacements.push((original.clone(), replacement.clone()));
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = events.clone();
        let _subscription = session.subscribe(Arc::new(move |event| lock(&captured).push(event.clone())));
        // When the turn ends.
        session.process_agent_event(
            maho_agent::types::AgentEvent::TurnEnd { message: original, tool_results: Vec::new() },
            maho_ai::utils::abort::AbortController::new().signal(),
        ).await;
        // Then listeners receive the replacement, and the next turn advances.
        assert!(matches!(&lock(&events)[0], AgentSessionEvent::Agent(
            maho_agent::types::AgentEvent::TurnEnd { message, .. }
        ) if message == &replacement));
        assert_eq!(session.state().turn_index, 1);
    }

    #[tokio::test]
    async fn agent_start_resets_turn_index() {
        // Given a session whose previous run ended on a later turn.
        let session = test_session();
        session.state().turn_index = 4;
        // When a new run starts.
        session.process_agent_event(
            maho_agent::types::AgentEvent::AgentStart,
            maho_ai::utils::abort::AbortController::new().signal(),
        ).await;
        // Then its first extension turn starts at zero.
        assert_eq!(session.state().turn_index, 0);
    }

    fn retry_session(responses: Vec<maho_ai::types::AssistantMessage>, max_retries: u32) -> AgentSession {
        use maho_ai::providers::faux::{faux_provider, faux_streams, RegisterFauxProviderOptions};
        let provider = faux_provider(RegisterFauxProviderOptions {
            tokens_per_second: Some(0.0), ..Default::default()
        });
        provider.set_responses(responses.into_iter().map(Into::into).collect());
        let streams = faux_streams(provider.core.clone());
        let session = test_session_with_stream_function(false);
        session.agent.set_model(provider.get_model(Some("faux-1")).expect("model"));
        session.agent.set_stream_function(Arc::new(move |model, context, options| {
            streams.stream_simple(model, context, options.map(|options| options.simple))
        }));
        session.with_settings_manager_mut(|manager| manager.set(crate::settings_manager::SettingsScope::Global, &serde_json::from_value(
            serde_json::json!({"retry":{"enabled":true,"maxRetries":max_retries,"baseDelayMs":0,"modelFallback":false}})
        ).expect("settings")).expect("save settings"));
        session
    }

    #[tokio::test]
    async fn prompt_retries_transient_failure_and_preserves_durable_history() {
        let session = retry_session(vec![
            maho_ai::providers::faux::faux_assistant_message("", maho_ai::providers::faux::FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Error), error_message: Some("overloaded_error".to_owned()), ..Default::default()
            }),
            maho_ai::providers::faux::faux_assistant_message("recovered", Default::default()),
        ], 2);
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = events.clone();
        let _subscription = session.subscribe(Arc::new(move |event| lock(&captured).push(event.clone())));
        tokio::time::timeout(std::time::Duration::from_secs(5), session.prompt("input", Default::default()))
            .await.expect("bounded prompt").expect("prompt");
        assert_eq!(session.get_last_assistant_text().as_deref(), Some("recovered"));
        assert!(!session.is_retrying());
        assert_eq!(session.messages().len(), 2);
        assert_eq!(session.with_session_manager(|manager| manager.entries().len()), 3);
        assert!(lock(&events).iter().any(|event| matches!(event, AgentSessionEvent::AutoRetryEnd { success: true, attempt: 1, .. })));
    }

    #[tokio::test]
    async fn transient_failures_exhaust_bounded_retry_budget() {
        let failed = maho_ai::providers::faux::faux_assistant_message("", maho_ai::providers::faux::FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error), error_message: Some("overloaded_error".to_owned()), ..Default::default()
        });
        let session = retry_session(vec![failed.clone(), failed.clone(), failed], 2);
        tokio::time::timeout(std::time::Duration::from_secs(5), session.prompt("input", Default::default()))
            .await.expect("bounded prompt").expect("prompt");
        assert!(!session.is_retrying());
        assert_eq!(session.with_session_manager(|manager| manager.entries().len()), 4);
        assert_eq!(session.messages().len(), 2);
    }

    #[tokio::test]
    async fn abort_retry_cancels_backoff_before_another_request() {
        let failed = maho_ai::providers::faux::faux_assistant_message("", maho_ai::providers::faux::FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error), error_message: Some("overloaded_error".to_owned()), ..Default::default()
        });
        let session = retry_session(vec![failed], 2);
        let cancelling = session.clone();
        let _subscription = session.subscribe(Arc::new(move |event| {
            if matches!(event, AgentSessionEvent::AutoRetryStart { .. }) { cancelling.abort_retry(); }
        }));
        tokio::time::timeout(std::time::Duration::from_secs(5), session.prompt("input", Default::default()))
            .await.expect("bounded prompt").expect("prompt");
        assert!(!session.is_retrying());
        assert_eq!(session.with_session_manager(|manager| manager.entries().len()), 2);
    }
}
