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

tokio::task_local! {
    static EXTENSION_EVENT_SIGNAL: maho_ext_api::AbortSignal;
    static EXTENSION_COMPACTION_PREPARATION: maho_ext_api::CompactionPreparationDetails;
}
struct ExtensionEventSignalSubscription {
    signal: maho_ai::utils::abort::AbortSignal,
    listener: maho_ai::utils::abort::ListenerId,
}
impl ExtensionEventSignalSubscription {
    fn new(signal: maho_ai::utils::abort::AbortSignal) -> (Self, maho_ext_api::AbortSignal) {
        let extension_signal = maho_ext_api::AbortSignal::default();
        let cancellation = extension_signal.clone();
        let listener = signal.add_abort_listener(move |_| cancellation.abort());
        (Self { signal, listener }, extension_signal)
    }
}
impl Drop for ExtensionEventSignalSubscription {
    fn drop(&mut self) { self.signal.remove_abort_listener(self.listener); }
}

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

#[derive(Clone, Debug)]
pub struct PendingModelSwitch {
    pub model: Model,
    pub budget: maho_ext_api::ModelBudget,
    pub persist_default: bool,
    pub notice: String,
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
    pending_model_switch: Option<PendingModelSwitch>,
    compaction_abort_controller: Option<maho_ai::utils::abort::AbortController>,
    prompt_templates: Vec<crate::prompt_templates::PromptTemplate>,
    extension_commands: Vec<maho_ext_api::SlashCommandInfo>,
    extension_command_catalog: Option<Arc<dyn Fn() -> Vec<maho_ext_api::SlashCommandInfo> + Send + Sync>>,
    extension_event_sender: Option<tokio::sync::mpsc::UnboundedSender<maho_ext_api::ExtensionEvent>>,
    extension_tool_context: Option<(maho_ext_api::ExtensionRuntime, maho_ext_host::wrapper::ToolContextFactory)>,
    extension_tool_backups: BTreeMap<String, Option<(ToolDefinitionEntry, AgentTool)>>,
    skills: Vec<crate::skills::Skill>,
    bash_abort_signals: BTreeMap<String, maho_ext_api::AbortSignal>,
    pending_bash_messages: Vec<maho_agent::harness::messages::BashExecutionMessage>,
    pending_next_turn_messages: Vec<maho_agent::harness::messages::CustomMessage>,
    pending_custom_messages: Vec<maho_agent::harness::messages::CustomMessage>,
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
    work_barrier: Arc<crate::session_work_barrier::SessionWorkBarrier>,
}

struct SessionFallbackDeps(std::sync::Weak<AgentSessionInner>);

struct SessionExtensionActions(std::sync::Weak<AgentSessionInner>);

struct ExtensionSessionManagerView {
    session: std::sync::Weak<AgentSessionInner>,
    id: String,
    file: Option<std::path::PathBuf>,
}
struct ExtensionModelRegistryView(crate::model_registry::ModelRegistry);
impl maho_ext_api::ModelRegistry for ExtensionModelRegistryView {
    fn get_all(&self) -> Vec<Model> { self.0.get_all() }
    fn get_available(&self) -> Vec<Model> { self.0.get_available() }
    fn find(&self, provider: &str, id: &str) -> Option<Model> { self.0.find(provider, id) }
    fn has_configured_auth(&self, model: &Model) -> bool { self.0.has_configured_auth(model) }
    fn get_api_key_for_provider<'a>(&'a self, provider: &'a str) -> maho_ext_api::ExtensionFuture<'a, Option<String>> {
        Box::pin(async move { Ok(self.0.get_api_key_for_provider(provider).await) })
    }
}
impl maho_ext_api::ToolSessionManager for ExtensionSessionManagerView {
    fn session_id(&self) -> &str { &self.id }
    fn session_file(&self) -> Option<&std::path::Path> { self.file.as_deref() }
}
impl maho_ext_api::SessionManager for ExtensionSessionManagerView {
    fn get_entries(&self) -> Vec<maho_ext_api::SessionEntry> {
        self.session.upgrade().map_or_else(Vec::new, |inner| AgentSession { inner }.with_session_manager(|manager|
            manager.entries().into_iter().map(session_entry_from_value).collect()))
    }
    fn get_branch(&self) -> Vec<maho_ext_api::SessionEntry> {
        self.session.upgrade().map_or_else(Vec::new, |inner| AgentSession { inner }.with_session_manager(|manager|
            manager.branch(manager.leaf_id()).into_iter().map(session_entry_from_value).collect()))
    }
    fn get_leaf_id(&self) -> Option<String> {
        self.session.upgrade().and_then(|inner| AgentSession { inner }.with_session_manager(|manager| manager.leaf_id().map(str::to_owned)))
    }
    fn get_session_name(&self) -> Option<String> { self.session.upgrade().and_then(|inner| AgentSession { inner }.session_name()) }
}

impl SessionExtensionActions {
    fn session(&self) -> Result<AgentSession, maho_ext_api::ExtensionFailure> {
        self.0.upgrade().map(|inner| AgentSession { inner }).ok_or_else(|| maho_ext_api::ExtensionFailure::new("Session disposed"))
    }

    fn update_retry(&self, update: impl FnOnce(&mut Map<String, Value>)) -> Result<(), maho_ext_api::ExtensionFailure> {
        let session = self.session()?;
        let mut retry = session.with_settings_manager(|manager| manager.get_value("retry").and_then(Value::as_object).cloned()).unwrap_or_default();
        update(&mut retry);
        session.with_settings_manager_mut(|manager| manager.set(crate::settings_manager::SettingsScope::Global,
            &Map::from_iter([("retry".to_owned(), Value::Object(retry))]))).map_err(maho_ext_api::ExtensionFailure::new)
    }
}

impl maho_ext_api::ExtensionSessionSettings for SessionExtensionActions {
    fn get_retry_fallback_settings(&self) -> maho_ext_api::RetryFallbackSettings {
        let raw = self.session().ok().and_then(|session| session.with_settings_manager(|manager| manager.get_value("retry").cloned()));
        let settings = crate::retry_fallback::settings::resolve_retry_fallback_settings(raw.as_ref());
        maho_ext_api::RetryFallbackSettings { model_fallback: settings.model_fallback, chains: settings.chains.into_iter().collect(),
            revert_policy: match settings.revert_policy { crate::retry_fallback::settings::FallbackRevertPolicy::Never => maho_ext_api::FallbackRevertPolicy::Never,
                crate::retry_fallback::settings::FallbackRevertPolicy::CooldownExpiry => maho_ext_api::FallbackRevertPolicy::CooldownExpiry } }
    }
    fn set_fallback_chain<'a>(&'a self, key: &'a str, entries: &'a [String]) -> maho_ext_api::ExtensionFuture<'a, ()> {
        Box::pin(async move { self.update_retry(|retry| {
            let mut chains = retry.get("fallbackChains").and_then(Value::as_object).cloned().unwrap_or_default();
            chains.insert(key.to_owned(), Value::Array(entries.iter().cloned().map(Value::String).collect())); retry.insert("fallbackChains".to_owned(), Value::Object(chains));
        }) })
    }
    fn remove_fallback_chain<'a>(&'a self, key: &'a str) -> maho_ext_api::ExtensionFuture<'a, ()> { Box::pin(async move {
        self.update_retry(|retry| { if let Some(chains) = retry.get_mut("fallbackChains").and_then(Value::as_object_mut) { chains.remove(key); } })
    }) }
    fn set_model_fallback_enabled(&self, enabled: bool) -> maho_ext_api::ExtensionFuture<'_, ()> { Box::pin(async move {
        self.update_retry(|retry| { retry.insert("modelFallback".to_owned(), Value::Bool(enabled)); })
    }) }
    fn set_fallback_revert_policy(&self, policy: maho_ext_api::FallbackRevertPolicy) -> maho_ext_api::ExtensionFuture<'_, ()> { Box::pin(async move {
        self.update_retry(|retry| { retry.insert("fallbackRevertPolicy".to_owned(), Value::String(match policy {
            maho_ext_api::FallbackRevertPolicy::Never => "never", maho_ext_api::FallbackRevertPolicy::CooldownExpiry => "cooldown-expiry",
        }.to_owned())); })
    }) }
    fn reload(&self) -> maho_ext_api::ExtensionFuture<'_, ()> { Box::pin(async move { self.session()?.with_settings_manager_mut(|manager| manager.reload()); Ok(()) }) }
    fn get_fallback_status(&self) -> Option<maho_ext_api::RetryFallbackStatus> {
        let session = self.session().ok()?;
        let controller = session.retry_fallback.try_lock().ok()?;
        let state = controller.as_ref()?.state.as_ref()?;
        Some(maho_ext_api::RetryFallbackStatus { active: true, current_model: Some(format!("{}/{}",session.model().provider,session.model().id)),
            original_selector: Some(state.original_selector.clone()), pinned: state.pinned })
    }
}

impl maho_ext_api::ExtensionContextActions for SessionExtensionActions {
    fn assert_active(&self) -> Result<(), maho_ext_api::ExtensionFailure> { self.session().map(|_| ()) }
    fn get_model(&self) -> Option<Model> { self.session().ok().map(|session| session.model()) }
    fn get_service_tier(&self) -> Option<ServiceTier> { self.session().ok().and_then(|session| session.service_tier()) }
    fn get_effective_service_tier(&self) -> Option<ServiceTier> { self.session().ok().and_then(|session| session.effective_service_tier()) }
    fn get_scoped_models(&self) -> Vec<maho_ext_api::ScopedModel> { self.session().map_or_else(|_| Vec::new(), |session| session.scoped_models().into_iter().map(|model|
        maho_ext_api::ScopedModel { model: model.model, thinking_level: model.thinking_level, service_tier: model.service_tier }).collect()) }
    fn get_agent_dir(&self) -> std::path::PathBuf { self.session().map_or_else(|_| Default::default(), |session| session.agent_dir().into()) }
    fn is_idle(&self) -> bool { self.session().is_ok_and(|session| !session.is_streaming() && !session.work_barrier.has_active_work()) }
    fn is_project_trusted(&self) -> bool { self.session().is_ok_and(|session| session.with_settings_manager(|manager| manager.is_project_trusted())) }
    fn get_signal(&self) -> Option<maho_ext_api::AbortSignal> { EXTENSION_EVENT_SIGNAL.try_with(Clone::clone).ok() }
    fn get_compaction_preparation(&self) -> Option<maho_ext_api::CompactionPreparationDetails> { EXTENSION_COMPACTION_PREPARATION.try_with(Clone::clone).ok() }
    fn abort(&self, source: Option<maho_ext_api::AbortSource>) {
        if let Ok(session) = self.session() {
            if source.unwrap_or(maho_ext_api::AbortSource::User) == maho_ext_api::AbortSource::User {
                session.state().user_aborted = true;
                session.agent.suppress_queued_message_drain();
            }
            session.agent.abort(None);
            session.abort_retry();
            session.abort_compaction();
        }
    }
    fn has_pending_messages(&self) -> bool { self.session().is_ok_and(|session| session.pending_message_count() > 0) }
    fn request_reload(&self) -> maho_ext_api::ExtensionFuture<'_, ()> { Box::pin(async move { self.session()?.reload().await.map(|_| ()).map_err(maho_ext_api::ExtensionFailure::new) }) }
    fn is_compacting(&self) -> bool { self.session().is_ok_and(|session| session.is_compacting()) }
    fn check_reload_veto(&self) -> maho_ext_api::ExtensionFuture<'_, maho_ext_api::ReloadVetoDecision> { Box::pin(async move {
        let result = self.session()?.session_before(maho_ext_api::ExtensionEvent::SessionBeforeReload).await.map_err(maho_ext_api::ExtensionFailure::new)?;
        Ok(maho_ext_api::ReloadVetoDecision { cancelled: result.cancel == Some(true), reason: result.reason })
    }) }
    fn shutdown(&self) { if let Ok(session) = self.session() { tokio::spawn(async move { session.dispose().await; }); } }
    fn get_context_usage(&self) -> Option<maho_ext_api::ContextUsage> { self.session().ok()?.get_context_usage().map(|usage|
        maho_ext_api::ContextUsage { tokens: usage.tokens, context_window: usage.context_window, percent: usage.percent }) }
    fn get_compaction_settings(&self) -> maho_ext_api::CompactionSettings {
        let session = self.session().unwrap_or_else(|error| std::panic::panic_any(error));
        let model = session.model();
        let raw = session.with_settings_manager(|manager| manager.get_value("compaction").cloned());
        let settings = raw.map(serde_json::from_value::<crate::compaction_settings_access::CompactionSettings>)
            .transpose().unwrap_or_else(|error| std::panic::panic_any(maho_ext_api::ExtensionFailure::new(error.to_string())));
        let resolved = crate::compaction_settings_resolver::resolve_compaction_settings(settings.as_ref(), Some(
            crate::compaction_settings_access::CompactionModelSelector { provider: &model.provider, id: &model.id },
        )).unwrap_or_else(|error| std::panic::panic_any(maho_ext_api::ExtensionFailure::new(error)));
        maho_ext_api::CompactionSettings { enabled: resolved.enabled, reserve_tokens: resolved.reserve_tokens as u64,
            keep_recent_tokens: resolved.keep_recent_tokens as u64 }
    }
    fn get_prompt_cache_safe_wait_seconds(&self) -> Option<f64> { self.session().ok()?.with_settings_manager(|manager| manager.get_number("promptCacheSafeWaitSeconds")) }
    fn get_prompt_cache_goal_backstop_max_seconds(&self) -> f64 {
        self.session().ok().and_then(|session| session.with_settings_manager(|manager|
            manager.get_value("promptCache").and_then(|value| value.get("goalBackstopMaxSeconds")).and_then(Value::as_f64))).unwrap_or(270.0)
    }
    fn get_prompt_cache_keep_alive_settings(&self) -> maho_ext_api::PromptCacheKeepAliveSettings {
        let configured = self.session().ok().and_then(|session| session.with_settings_manager(|manager|
            manager.get_value("promptCache").and_then(|value| value.get("keepAlive")).cloned()));
        maho_ext_api::PromptCacheKeepAliveSettings {
            enabled: configured.as_ref().and_then(|value| value.get("enabled")).and_then(Value::as_bool).unwrap_or(false),
            max_requests_per_session: configured.as_ref().and_then(|value| value.get("maxRequestsPerSession")).and_then(Value::as_u64).unwrap_or(3),
            max_cost_usd_per_session: configured.as_ref().and_then(|value| value.get("maxCostUsdPerSession")).and_then(Value::as_f64).unwrap_or(0.05),
            margin_seconds: configured.as_ref().and_then(|value| value.get("marginSeconds")).and_then(Value::as_f64).unwrap_or(60.0),
        }
    }
    fn get_look_at_settings(&self) -> maho_ext_api::LookAtSettings {
        let configured = self.session().ok().and_then(|session| session.with_settings_manager(|manager| manager.get_value("lookAt").cloned()));
        maho_ext_api::LookAtSettings {
            enabled: configured.as_ref().and_then(|value| value.get("enabled")).and_then(Value::as_bool).unwrap_or(true),
            models: configured.as_ref().and_then(|value| value.get("models")).and_then(Value::as_array)
                .map(|models| models.iter().filter_map(Value::as_str).map(str::to_owned).collect()),
        }
    }
    fn get_ask_user_settings(&self) -> maho_ext_api::AskUserSettings {
        let configured = self.session().ok().and_then(|session| session.with_settings_manager(|manager| manager.get_value("askUser").cloned()));
        maho_ext_api::AskUserSettings {
            enabled: configured.as_ref().and_then(|value| value.get("enabled")).and_then(Value::as_bool).unwrap_or(true),
            timeout_minutes: configured.as_ref().and_then(|value| value.get("timeoutMinutes")).and_then(Value::as_f64)
                .filter(|value| value.is_finite()).map(|value| value.floor().clamp(1.0, 120.0)).unwrap_or(30.0),
        }
    }
    fn get_image_settings(&self) -> maho_ext_api::ImageSettings {
        let configured = self.session().ok().and_then(|session| session.with_settings_manager(|manager| manager.get_value("images").cloned()));
        maho_ext_api::ImageSettings {
            auto_resize: configured.as_ref().and_then(|value| value.get("autoResize")).and_then(Value::as_bool).unwrap_or(true),
            block_images: configured.as_ref().and_then(|value| value.get("blockImages")).and_then(Value::as_bool).unwrap_or(false),
        }
    }
    fn session_settings(&self) -> &dyn maho_ext_api::ExtensionSessionSettings { self }
    fn compact(&self, options: maho_ext_api::CompactOptions) { if let Ok(session) = self.session() { tokio::spawn(async move {
        match session.compact(options.custom_instructions.as_deref()).await {
            Ok(result) => if let Some(callback) = options.on_complete { callback(maho_ext_api::CompactionResult { summary: result.summary,
                first_kept_entry_id: result.first_kept_entry_id, tokens_before: result.tokens_before as u64, details: result.details }); },
            Err(error) => if let Some(callback) = options.on_error { callback(maho_ext_api::ExtensionFailure::new(error)); },
        }
    }); } }
    fn prepare_provider_request(&self, messages: Vec<AgentMessage>) -> maho_ext_api::ExtensionFuture<'_, maho_ext_api::ProviderRequestPreparation> {
        Box::pin(async move { let session = self.session()?;
            let runner = session.extension_runner.lock().await.clone();
            if let Some(runner) = runner { return runner.prepare_provider_request(messages, None).await; }
            Ok(maho_ext_api::ProviderRequestPreparation { messages,
                transform_payload: Arc::new(|payload| Box::pin(async move { Ok(payload) })),
                transform_headers: Arc::new(|headers| Box::pin(async move { Ok(headers) })),
            })
        })
    }
    fn begin_compaction(&self, options: maho_ext_api::BeginCompactionOptions) -> Option<maho_ext_api::AbortSignal> {
        let session = self.session().ok()?;
        if session.is_compacting() { return None; }
        let controller = maho_ai::utils::abort::AbortController::new();
        let cancellation = controller.signal();
        let signal = maho_ext_api::AbortSignal::default();
        let observed = signal.clone();
        session.state().compaction_abort_controller = Some(controller);
        session.emit(AgentSessionEvent::CompactionStart { reason: options.reason, request_id: None });
        tokio::spawn(async move { cancellation.cancelled().await; observed.abort(); });
        Some(signal)
    }
    fn update_compaction(&self, options: maho_ext_api::UpdateCompactionOptions) { if let Ok(session) = self.session() {
        session.emit(AgentSessionEvent::CompactionProgress { reason: options.reason, delta: options.delta, text: options.text });
    } }
    fn end_compaction(&self, options: maho_ext_api::EndCompactionOptions) { if let Ok(session) = self.session() {
        if let Some(controller) = session.state().compaction_abort_controller.take() { controller.abort(None); }
        session.emit(AgentSessionEvent::CompactionEnd { reason: options.reason, result: None, aborted: options.aborted.unwrap_or(false),
            will_retry: false, request_id: None, accepted: None, rejection_cause: None, error_message: options.error_message });
    } }
    fn get_message_revision(&self) -> u64 { self.session().map_or(0, |session| session.message_revision()) }
    fn apply_compaction(&self, result: maho_ext_api::CompactionResult, options: maho_ext_api::ApplyCompactionOptions) -> maho_ext_api::ExtensionFuture<'_, maho_ext_api::ApplyCompactionResult> {
        Box::pin(async move { let session = self.session()?;
            if options.expected_revision.is_some_and(|revision| revision != session.message_revision()) { return Ok(maho_ext_api::ApplyCompactionResult::Stale); }
            if let Some(anchor) = options.expected_warm_anchor {
                let anchor = crate::compaction::warm_anchor::WarmAnchorSnapshot {
                    first_kept_entry_id: anchor.first_kept_entry_id, prefix_entry_ids: anchor.prefix_entry_ids,
                    latest_compaction_entry_id: anchor.latest_compaction_entry_id,
                };
                if !session.with_session_manager(|manager| crate::compaction::warm_anchor::is_warm_summary_anchor_valid(&anchor, &manager.branch(None))) {
                    return Ok(maho_ext_api::ApplyCompactionResult::Stale);
                }
            }
            if options.signal.as_ref().is_some_and(maho_ext_api::AbortSignal::is_aborted) { return Ok(maho_ext_api::ApplyCompactionResult::Rejected); }
            session.apply_compaction(&crate::compaction::compaction::CompactionResult { summary: result.summary, first_kept_entry_id: result.first_kept_entry_id,
                tokens_before: result.tokens_before as i64, estimated_tokens_after: None, usage: None, details: result.details }).map_err(maho_ext_api::ExtensionFailure::new)?;
            Ok(maho_ext_api::ApplyCompactionResult::Applied)
        })
    }
    fn get_system_prompt(&self) -> String { self.session().map_or_else(|_| String::new(), |session| session.system_prompt()) }
    fn get_system_prompt_options(&self) -> maho_ext_api::BuildSystemPromptOptions {
        self.session().map_or_else(|_| Default::default(), |session| session.extension_system_prompt_options())
    }
    fn get_loaded_hook_sources(&self) -> maho_ext_api::LoadedHookSources {
        let session = self.session().ok(); let cwd = session.as_ref().map_or_else(std::path::PathBuf::new, |session| session.cwd().into());
        maho_ext_api::LoadedHookSources { global_hooks_path: cwd.join("hooks.json"), project_hooks_path: cwd.join(".maho/hooks.json"), agent_dir: cwd.clone(), cwd,
            global_settings_hooks: None, project_settings_hooks: None, global_hook_source_paths: Vec::new(), project_hook_source_paths: Vec::new(),
            pre_session_hook_source_paths: Vec::new(), runtime_hook_source_paths: Vec::new() }
    }
    fn kernel_tools(&self) -> Option<&dyn maho_ext_api::ExtensionKernelTools> { None }
    fn get_thinking_level(&self) -> Option<ThinkingLevel> {
        self.session().ok().and_then(|session| thinking_level_from_model_level(session.thinking_level()))
    }
}

impl maho_ext_api::ExtensionActions for SessionExtensionActions {
    fn send_message(&self, message: maho_ext_api::CustomMessage, options: maho_ext_api::SendMessageOptions) -> Result<(), maho_ext_api::ExtensionFailure> {
        let session = self.session()?;
        let custom: maho_agent::harness::messages::CustomMessage = serde_json::from_value(serde_json::json!({
            "role":"custom","customType":message.custom_type,"content":message.content,"display":message.display,
            "details":message.details,"timestamp":maho_ai::utils::diagnostics::now_ms(),
        })).map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?;
        let agent_message = AgentMessage::Custom(maho_agent::types::CustomAgentMessage::Custom(custom.clone()));
        if options.deliver_as == Some(maho_ext_api::DeliverAs::NextTurn) {
            session.state().pending_next_turn_messages.push(custom);
        } else if session.is_streaming() && !options.trigger_turn {
            session.state().pending_custom_messages.push(custom);
        } else if session.is_streaming() {
            match options.deliver_as { Some(maho_ext_api::DeliverAs::FollowUp) => session.agent.follow_up(agent_message), _ => session.agent.steer(agent_message) }
        } else {
            session.append_extension_custom_message(custom).map_err(maho_ext_api::ExtensionFailure::new)?;
            if options.trigger_turn { tokio::spawn(async move { session.continue_session().await }); }
        }
        Ok(())
    }
    fn send_user_message(&self, content: maho_ext_api::UserMessageContent, options: maho_ext_api::SendUserMessageOptions) -> Result<(), maho_ext_api::ExtensionFailure> {
        let session = self.session()?;
        let (text, images) = match content {
            maho_ext_api::UserMessageContent::Text(text) => (text, Vec::new()),
            maho_ext_api::UserMessageContent::Blocks(blocks) => {
                let mut text = Vec::new(); let mut images = Vec::new();
                for block in blocks { match block {
                    maho_ext_api::ToolContent::Text { text: part, .. } => text.push(part),
                    maho_ext_api::ToolContent::Image { data, mime_type } => images.push(ImageContent { data, mime_type }),
                }} (text.join("\n"), images)
            }
        };
        tokio::spawn(async move { session.prompt(&text, PromptOptions { images: Some(images), source: Some(InputSource::Extension),
            streaming_behavior: options.deliver_as, expand_prompt_templates: Some(options.expand_prompt_templates), ..Default::default() }).await });
        Ok(())
    }
    fn append_entry(&self, custom_type: &str, data: Option<Value>) -> Result<(), maho_ext_api::ExtensionFailure> {
        let session = self.session()?;
        let entry = session.with_session_manager_mut(|manager| manager.append_custom(custom_type, data));
        session.emit(AgentSessionEvent::EntryAppended { entry: session_entry_from_value(entry) });
        Ok(())
    }
    fn get_all_tools(&self) -> Result<Vec<maho_ext_api::ToolInfo>, maho_ext_api::ExtensionFailure> {
        Ok(self.session()?.get_all_tools().into_iter().map(|tool| maho_ext_api::ToolInfo {
            name: tool.name, label: tool.label, description: tool.description,
            parameters: serde_json::to_value(tool.parameters).unwrap_or(Value::Null), source_info: tool.source_info,
            prompt_guidelines: tool.prompt_guidelines, exposure: tool.exposure, search_text: tool.search_text,
            search_keywords: tool.search_keywords, search_group: tool.search_group, allow_lazy_activation: tool.allow_lazy_activation,
        }).collect())
    }
}

impl maho_ext_api::ExtensionSessionActions for SessionExtensionActions {
    fn set_session_name(&self, name: &str) -> Result<(), maho_ext_api::ExtensionFailure> { self.session()?.set_session_name(name); Ok(()) }
    fn get_session_name(&self) -> Result<Option<String>, maho_ext_api::ExtensionFailure> { Ok(self.session()?.session_name()) }
    fn set_label(&self, id: &str, label: Option<&str>) -> Result<(), maho_ext_api::ExtensionFailure> {
        self.session()?.with_session_manager_mut(|manager| manager.append_label(id, label)); Ok(())
    }
    fn execute_tool<'a>(&'a self, name: &'a str, params: Value, options: maho_ext_api::ExecuteToolOptions) -> maho_ext_api::ExecuteToolFuture<'a> {
        Box::pin(async move { let session = self.session().map_err(|error| maho_ext_api::ExecuteToolError {
            code: maho_ext_api::ExecuteToolErrorCode::Blocked, tool_name: name.to_owned(), message: error.message, active_tools: Vec::new(),
        })?;
            session.execute_tool_with_updates(name, params, ExecuteToolOptions { signal: options.signal, activate_inactive_tool: options.activate_inactive_tool }, options.on_update).await
                .map_err(|error| maho_ext_api::ExecuteToolError { code: match error.code.as_str() {
                    "unknown_tool" => maho_ext_api::ExecuteToolErrorCode::UnknownTool, "inactive_tool" => maho_ext_api::ExecuteToolErrorCode::InactiveTool,
                    "invalid_params" => maho_ext_api::ExecuteToolErrorCode::InvalidParams, _ => maho_ext_api::ExecuteToolErrorCode::Blocked,
                }, tool_name: error.tool_name, message: error.message, active_tools: error.active_tools })
        })
    }
    fn get_active_tools(&self) -> Result<Vec<String>, maho_ext_api::ExtensionFailure> { Ok(self.session()?.get_active_tool_names()) }
    fn set_active_tools(&self, names: Vec<String>) -> Result<(), maho_ext_api::ExtensionFailure> { self.session()?.set_active_tools_by_name(names); Ok(()) }
    fn refresh_tools(&self) -> Result<(), maho_ext_api::ExtensionFailure> { self.session()?.publish_eval_only_tool_hints(); Ok(()) }
    fn install_registered_tool(&self, registered: maho_ext_api::RegisteredTool) -> Result<(), maho_ext_api::ExtensionFailure> {
        let session = self.session()?;
        let binding = session.state().extension_tool_context.clone();
        if let Some((runtime, factory)) = binding {
            let definition = registered.definition.clone();
            let source = registered.source_info.clone();
            let tool = maho_ext_host::wrapper::wrap_registered_tool(registered, runtime, factory);
            let mut active = session.get_active_tool_names();
            if normalize_tool_exposure(&definition, source.clone()).exposure != ToolExposure::Search && !active.contains(&definition.name) { active.push(definition.name.clone()); }
            session.register_extension_tool(definition, source, tool);
            session.set_active_tools_by_name(active);
        }
        self.refresh_tools()
    }
    fn register_removed_tool_hint(&self, name: &str, hint: &str) -> Result<(), maho_ext_api::ExtensionFailure> {
        let session = self.session()?; let mut hints = session.agent.removed_tool_hints(); hints.insert(name.to_owned(), hint.to_owned()); session.agent.set_removed_tool_hints(hints); Ok(())
    }
    fn register_lazy_tool_activator(&self, activator: maho_ext_api::LazyToolActivator) -> Result<(), maho_ext_api::ExtensionFailure> { self.session()?.add_lazy_tool_activator(activator); Ok(()) }
    fn get_commands(&self) -> Result<Vec<maho_ext_api::SlashCommandInfo>, maho_ext_api::ExtensionFailure> { Ok(self.session()?.get_commands()) }
    fn set_model(&self, model: Model) -> maho_ext_api::ExtensionFuture<'_, bool> { Box::pin(async move {
        let session = self.session()?;
        if !session.model_registry().has_configured_auth(&model) { return Ok(false); }
        session.set_model(model).await.map(|_| true).map_err(maho_ext_api::ExtensionFailure::new)
    }) }
    fn get_thinking_level(&self) -> Result<ThinkingLevel, maho_ext_api::ExtensionFailure> { Ok(thinking_level_from_model_level(self.session()?.thinking_level()).unwrap_or(ThinkingLevel::Minimal)) }
    fn set_thinking_level(&self, level: ThinkingLevel) -> Result<(), maho_ext_api::ExtensionFailure> { self.session()?.set_thinking_level(extension_thinking_level(level)); Ok(()) }
    fn set_session_model(&self, model: Model) -> maho_ext_api::ExtensionFuture<'_, bool> { Box::pin(async move {
        let session = self.session()?;
        if !session.model_registry().has_configured_auth(&model) { return Ok(false); }
        session.set_session_model(model).await.map(|_| true).map_err(maho_ext_api::ExtensionFailure::new)
    }) }
    fn set_session_thinking_level(&self, level: ThinkingLevel) -> Result<(), maho_ext_api::ExtensionFailure> { self.session()?.set_session_thinking_level(extension_thinking_level(level)); Ok(()) }
    fn set_session_fast_mode(&self, enabled: bool) -> Result<(), maho_ext_api::ExtensionFailure> { self.session()?.set_session_fast_mode(enabled); Ok(()) }
    fn exec<'a>(&'a self, command: &'a str, args: &'a [String], cwd: &'a std::path::Path, options: maho_ext_api::ExecOptions) -> maho_ext_api::ExtensionFuture<'a, maho_ext_api::ExecResult> {
        Box::pin(async move {
            self.session()?;
            Ok(maho_ext_host::exec::exec_command(command, args, cwd, options).await)
        })
    }
}

impl maho_ext_api::ExtensionCommandContextActions for SessionExtensionActions {
    fn wait_for_idle(&self) -> maho_ext_api::ExtensionFuture<'_, ()> {
        Box::pin(async move { self.session()?.wait_for_idle().await; Ok(()) })
    }
    fn new_session(&self, options: maho_ext_api::NewSessionOptions) -> maho_ext_api::ExtensionFuture<'_, maho_ext_api::SessionNavigationResult> {
        Box::pin(async move {
            let session = self.session()?;
            let changed = session.new_session(Some(crate::session_manager::NewSessionOptions {
                parent_session: options.parent_session, ..Default::default()
            })).await.map_err(maho_ext_api::ExtensionFailure::new)?;
            if changed { session.run_replacement_callbacks(options.setup, options.with_session).await?; }
            Ok(maho_ext_api::SessionNavigationResult { cancelled: !changed })
        })
    }
    fn fork<'a>(&'a self, entry_id: &'a str, options: maho_ext_api::ForkOptions) -> maho_ext_api::ExtensionFuture<'a, maho_ext_api::SessionNavigationResult> {
        Box::pin(async move {
            let session = self.session()?;
            let result = session.fork(entry_id, options.position == Some(maho_ext_api::ForkPosition::At)).await.map_err(maho_ext_api::ExtensionFailure::new)?;
            if !result.cancelled { session.run_replacement_callbacks(None, options.with_session).await?; }
            Ok(maho_ext_api::SessionNavigationResult { cancelled: result.cancelled })
        })
    }
    fn navigate_tree<'a>(&'a self, target_id: &'a str, options: maho_ext_api::ExtensionTreeNavigationOptions) -> maho_ext_api::ExtensionFuture<'a, maho_ext_api::SessionNavigationResult> {
        Box::pin(async move {
            let result = self.session()?.navigate_tree(target_id, TreeNavigationOptions {
                summarize: options.summarize, custom_instructions: options.custom_instructions,
                replace_instructions: options.replace_instructions, label: options.label, expected_leaf_id: options.expected_leaf_id, ..Default::default()
            }).await.map_err(maho_ext_api::ExtensionFailure::new)?;
            Ok(maho_ext_api::SessionNavigationResult { cancelled: result.cancelled })
        })
    }
    fn edit_assistant_message<'a>(&'a self, entry_id: &'a str, text: &'a str, options: maho_ext_api::EditMessageOptions) -> maho_ext_api::ExtensionFuture<'a, maho_ext_api::EditMessageResult> {
        Box::pin(async move {
            let result = self.session()?.edit_assistant_message(entry_id, text, TreeNavigationOptions {
                summarize: options.summarize, custom_instructions: options.custom_instructions, expected_leaf_id: options.expected_leaf_id, ..Default::default()
            }).await.map_err(maho_ext_api::ExtensionFailure::new)?;
            Ok(maho_ext_api::EditMessageResult { cancelled: result.cancelled, unchanged: result.unchanged, entry_id: result.entry_id })
        })
    }
    fn edit_user_message<'a>(&'a self, entry_id: &'a str, text: &'a str, options: maho_ext_api::EditMessageOptions) -> maho_ext_api::ExtensionFuture<'a, maho_ext_api::EditMessageResult> {
        Box::pin(async move {
            let result = self.session()?.edit_user_message(entry_id, text, TreeNavigationOptions {
                summarize: options.summarize, custom_instructions: options.custom_instructions, expected_leaf_id: options.expected_leaf_id, ..Default::default()
            }).await.map_err(maho_ext_api::ExtensionFailure::new)?;
            Ok(maho_ext_api::EditMessageResult { cancelled: result.cancelled, unchanged: result.unchanged, entry_id: result.entry_id })
        })
    }
    fn switch_session<'a>(&'a self, path: &'a str, options: maho_ext_api::SwitchSessionOptions) -> maho_ext_api::ExtensionFuture<'a, maho_ext_api::SessionNavigationResult> {
        Box::pin(async move {
            let session = self.session()?;
            let changed = session.switch_session(path).await.map_err(maho_ext_api::ExtensionFailure::new)?;
            if changed { session.run_replacement_callbacks(None, options.with_session).await?; }
            Ok(maho_ext_api::SessionNavigationResult { cancelled: !changed })
        })
    }
    fn reload(&self) -> maho_ext_api::ExtensionFuture<'_, ()> {
        Box::pin(async move { self.session()?.reload().await.map(|_| ()).map_err(maho_ext_api::ExtensionFailure::new) })
    }
}

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
            pending_model_switch: None,
            compaction_abort_controller: None,
            prompt_templates: Vec::new(),
            extension_commands: Vec::new(),
            extension_command_catalog: None,
            extension_event_sender: None,
            extension_tool_context: None,
            extension_tool_backups: BTreeMap::new(),
            skills: Vec::new(),
            bash_abort_signals: BTreeMap::new(),
            pending_bash_messages: Vec::new(),
            pending_next_turn_messages: Vec::new(),
            pending_custom_messages: Vec::new(),
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
            work_barrier: Arc::new(crate::session_work_barrier::SessionWorkBarrier::new()),
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
        event: maho_agent::types::AgentEvent,
        signal: maho_ai::utils::abort::AbortSignal,
    ) {
        let (_subscription, extension_signal) = ExtensionEventSignalSubscription::new(signal);
        EXTENSION_EVENT_SIGNAL.scope(extension_signal, self.process_agent_event_inner(event)).await;
    }

    async fn process_agent_event_inner(&self, mut event: maho_agent::types::AgentEvent) {
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
            let pending = std::mem::take(&mut self.state().pending_custom_messages);
            for message in pending {
                if let Err(error_message) = self.append_extension_custom_message(message) { self.emit(AgentSessionEvent::ContinuationError { error_message }); }
            }
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
        if text.starts_with('/') && self.try_execute_extension_command(text).await? { return Ok(PromptDisposition::Handled); }
        if self.is_streaming() {
            let mode = options.streaming_behavior.ok_or_else(||
                "Agent is already processing a prompt. Use steer() or followUp() to queue messages, or wait for completion.".to_owned())?;
            self.queue_user_input(text, options.images, mode, QueuedInputOptions { source: options.source, ..Default::default() }).await?;
            return Ok(PromptDisposition::Queued);
        }
        let _admission = self.prompt_admission.lock().await;
        let _work = self.work_barrier.begin();
        self.state().user_aborted = false;
        let Some((text, images)) = self.run_input_handlers(text, options.images, options.source, None).await? else {
            return Ok(PromptDisposition::Handled);
        };
        let text = self.expand_input(&text, options.expand_prompt_templates.unwrap_or(true))?;
        if self.auto_compaction_enabled() && self.pending_model_switch().is_none() {
            let model = self.model();
            let raw = self.with_settings_manager(|manager| manager.get_value("compaction").cloned());
            let settings = raw.map(serde_json::from_value::<crate::compaction_settings_access::CompactionSettings>)
                .transpose().map_err(|error| error.to_string())?;
            let resolved = crate::compaction_settings_resolver::resolve_compaction_settings(settings.as_ref(), Some(
                crate::compaction_settings_access::CompactionModelSelector { provider: &model.provider, id: &model.id },
            ))?;
            if self.get_context_usage().and_then(|usage| usage.tokens).is_some_and(|tokens|
                tokens.saturating_add(text.len().div_ceil(4) as u64) > model.context_window.saturating_sub(resolved.reserve_tokens as u64)) {
                self.compact_for_model(None, &model, "pre-prompt").await?;
            }
        }
        if let Some(pending) = self.pending_model_switch() {
            self.compact_for_model(None, &pending.model, "manual").await?;
            self.set_model_internal(pending.model, pending.persist_default, maho_ext_api::ModelSelectSource::Set, false).await?;
        }
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
        let base_system_prompt = self.state().base_system_prompt.clone();
        let before = {
            let mut runner = self.extension_runner.lock().await;
            match runner.as_mut() { Some(runner) => runner.emit_before_agent_start(maho_ext_api::BeforeAgentStartEvent {
                prompt: text.clone(), images: images.clone(), system_prompt: base_system_prompt,
                system_prompt_options: self.extension_system_prompt_options(),
            }).await.map_err(|error| error.to_string())?, None => None }
        };
        if let Some(before) = before {
            if let Some(prompt) = before.system_prompt { self.agent.set_system_prompt(prompt); }
            for message in before.messages { maho_ext_api::ExtensionActions::send_message(
                &SessionExtensionActions(Arc::downgrade(&self.inner)), message, Default::default(),
            ).map_err(|error| error.to_string())?; }
        } else { self.agent.set_system_prompt(self.state().base_system_prompt.clone()); }
        self.flush_pending_next_turn_messages()?;
        self.agent.prompt(maho_agent::agent::AgentPromptInput::Message(make_user_message(&text, images))).await;
        self.finish_provider_turn().await?;
        self.flush_pending_bash_messages();
        if self.state().auto_title_sessions { self.generate_session_title_if_needed(&text).await; }
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
        let text = self.expand_input(&text, true)?;
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
        self.abort_compaction();
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

    pub fn is_compacting(&self) -> bool { self.state().compaction_abort_controller.is_some() }

    pub fn abort_compaction(&self) {
        if let Some(controller) = self.state().compaction_abort_controller.as_ref() { controller.abort(None); }
    }

    pub async fn compact(&self, instructions: Option<&str>) -> Result<crate::compaction::compaction::CompactionResult, String> {
        self.agent.abort(None);
        self.abort_retry();
        self.agent.wait_for_idle().await;
        self.compact_for_model(instructions, &self.model(), "manual").await
    }

    async fn compact_for_model(&self, instructions: Option<&str>, budget_model: &Model, reason: &str)
        -> Result<crate::compaction::compaction::CompactionResult, String>
    {
        use crate::compaction::compaction::{CompactionResult, prepare_compaction};
        let raw = self.with_settings_manager(|manager| manager.get_value("compaction").cloned());
        let configured = raw.map(serde_json::from_value::<crate::compaction_settings_access::CompactionSettings>)
            .transpose().map_err(|error| error.to_string())?;
        let resolved = crate::compaction_settings_resolver::resolve_compaction_settings(configured.as_ref(), Some(
            crate::compaction_settings_access::CompactionModelSelector { provider: &budget_model.provider, id: &budget_model.id },
        ))?;
        let entries = self.with_session_manager(|manager| manager.branch(None));
        let preparation = prepare_compaction(&entries, &crate::compaction::settings::CompactionSettings {
            enabled: resolved.enabled, reserve_tokens: resolved.reserve_tokens, keep_recent_tokens: resolved.keep_recent_tokens,
            ..crate::compaction::settings::default_compaction_settings()
        }, false, false).ok_or("Nothing to compact")?;
        let request_id = uuid::Uuid::new_v4().to_string();
        let controller = maho_ai::utils::abort::AbortController::new();
        let signal = controller.signal();
        let extension_signal = maho_ext_api::AbortSignal::default();
        self.state().compaction_abort_controller = Some(controller);
        let compact_reason = if reason == "manual" { maho_ext_api::CompactionReason::Manual }
            else if reason == "overflow" { maho_ext_api::CompactionReason::Overflow }
            else if reason == "pre-prompt" { maho_ext_api::CompactionReason::PrePrompt }
            else { maho_ext_api::CompactionReason::Threshold };
        self.emit(AgentSessionEvent::CompactionStart { reason: compact_reason, request_id: Some(request_id.clone()) });
        let revision = self.message_revision();
        let execution = async {
            let before = {
                let mut runner = self.extension_runner.lock().await;
                if let Some(runner) = runner.as_mut() {
                    let event = maho_ext_api::SessionBeforeCompactEvent {
                        reason: compact_reason, will_retry: reason != "manual", request_id: request_id.clone(),
                        preparation: maho_ext_api::CompactionPreparation {
                            settings: maho_ext_api::CompactionSettings { enabled: resolved.enabled, reserve_tokens: resolved.reserve_tokens as u64,
                                keep_recent_tokens: resolved.keep_recent_tokens as u64 },
                            messages_to_summarize: preparation.messages_to_summarize.iter().cloned().map(serde_json::from_value)
                                .collect::<Result<_, _>>().map_err(|error| error.to_string())?,
                            turn_prefix_messages: preparation.turn_prefix_messages.iter().cloned().map(serde_json::from_value)
                                .collect::<Result<_, _>>().map_err(|error| error.to_string())?,
                            tokens_before: preparation.tokens_before as u64, first_kept_entry_id: preparation.first_kept_entry_id.clone(),
                            previous_summary: preparation.previous_summary.clone(),
                        }, branch_entries: entries.iter().cloned().map(session_entry_from_value).collect(),
                        custom_instructions: instructions.map(str::to_owned), signal: extension_signal.clone(),
                    };
                    let details = maho_ext_api::CompactionPreparationDetails {
                        preparation: event.preparation.clone(),
                        source_messages: Some(preparation.source_messages.iter().cloned().map(serde_json::from_value)
                            .collect::<Result<_, _>>().map_err(|error| error.to_string())?),
                        turn_prefix_source_messages: Some(preparation.turn_prefix_source_messages.iter().cloned().map(serde_json::from_value)
                            .collect::<Result<_, _>>().map_err(|error| error.to_string())?),
                        is_split_turn: preparation.is_split_turn,
                        file_ops: maho_ext_api::CompactionFileOperations {
                            read: preparation.file_ops.read.iter().cloned().collect(),
                            written: preparation.file_ops.written.iter().cloned().collect(),
                            edited: preparation.file_ops.edited.iter().cloned().collect(),
                        },
                    };
                    EXTENSION_COMPACTION_PREPARATION.scope(details, runner.emit(maho_ext_api::ExtensionEvent::SessionBeforeCompact(event)))
                        .await.map_err(|error| error.to_string())?
                } else { maho_ext_api::EventResult::None }
            };
            let (result, from_extension) = match before {
                maho_ext_api::EventResult::SessionBefore(maho_ext_api::SessionBeforeEventResult { cancel: Some(true), .. }) => return Err("Compaction cancelled".to_owned()),
                maho_ext_api::EventResult::SessionBefore(maho_ext_api::SessionBeforeEventResult { compaction: Some(result), .. }) => (CompactionResult {
                    summary: result.summary, first_kept_entry_id: result.first_kept_entry_id, tokens_before: result.tokens_before as i64,
                    details: result.details, usage: None, estimated_tokens_after: None,
                }, true),
                _ => {
                    let model = self.model();
                    let auth = self.get_summarization_request_auth(&model).await?;
                    let mut transcript = preparation.previous_summary.clone().unwrap_or_default();
                    for message in preparation.messages_to_summarize.iter().chain(&preparation.turn_prefix_messages) {
                        transcript.push('\n'); transcript.push_str(&serde_json::to_string(message).map_err(|error| error.to_string())?);
                    }
                    let prompt = format!("{}\n\n{}\n\n{}", crate::compaction::compaction::update_summarization_prompt(),
                        instructions.unwrap_or_default(), transcript);
                    let AgentMessage::Llm(user) = make_user_message(&prompt, None) else { return Err("Invalid summary prompt".to_owned()); };
                    let context = maho_ai::types::Context { system_prompt: Some("Summarize the conversation without continuing it.".to_owned()),
                        messages: vec![user], tools: None };
                    let response = self.model_runtime().complete(&auth.model, &context, Some(maho_ai::types::StreamOptions {
                        request: maho_ai::types::ProviderRequestOptions { signal: Some(signal.clone()), api_key: auth.api_key,
                            headers: auth.headers.map(|headers| headers.into_iter().map(|(key, value)| (key, Some(value))).collect()), env: auth.env,
                            ..Default::default() },
                        ..Default::default()
                    })).await.map_err(|error| error.to_string())?;
                    if let Some(error) = crate::compaction::compaction::get_summarization_failure(&response, "Compaction") { return Err(error); }
                    let summary = maho_ai::utils::text::content_text(&response.content, "");
                    if summary.trim().is_empty() { return Err("Compaction produced an empty summary".to_owned()); }
                    (CompactionResult { summary, first_kept_entry_id: preparation.first_kept_entry_id.clone(),
                        tokens_before: preparation.tokens_before, details: None, usage: Some(response.usage), estimated_tokens_after: None }, false)
                }
            };
            signal.throw_if_aborted().map_err(|error| error.to_string())?;
            if self.message_revision() != revision { return Err("Conversation changed during compaction".to_owned()); }
            let entry = self.apply_compaction(&result)?;
            self.dispatch_extension_event(maho_ext_api::ExtensionEvent::SessionCompact(maho_ext_api::SessionCompactEvent::Accepted {
                reason: compact_reason, request_id: request_id.clone(), compaction_entry: session_entry_from_value(entry),
                from_extension, will_retry: reason != "manual",
            })).await;
            Ok(result)
        }.await;
        self.state().compaction_abort_controller = None;
        match &execution {
            Ok(result) => {
                let value = maho_ext_api::CompactionResult { summary: result.summary.clone(), first_kept_entry_id: result.first_kept_entry_id.clone(),
                    tokens_before: result.tokens_before as u64, details: result.details.clone() };
                self.emit(AgentSessionEvent::CompactionEnd { reason: compact_reason, request_id: Some(request_id), aborted: false,
                    result: Some(value), rejection_cause: None, error_message: None, accepted: Some(true), will_retry: reason != "manual" });
            }
            Err(error) => self.emit(AgentSessionEvent::CompactionEnd { reason: compact_reason, request_id: Some(request_id),
                aborted: signal.aborted(), result: None, rejection_cause: None, error_message: Some(error.clone()), accepted: Some(false), will_retry: false }),
        }
        execution
    }

    pub fn apply_compaction(&self, result: &crate::compaction::compaction::CompactionResult) -> Result<Value, String> {
        let branch = self.with_session_manager(|manager| manager.branch(None));
        if !branch.iter().any(|entry| entry.get("id").and_then(Value::as_str) == Some(result.first_kept_entry_id.as_str())) {
            return Err("Compaction first kept entry is not on the current branch".to_owned());
        }
        let usage = result.usage.as_ref().map(serde_json::to_value).transpose().map_err(|error| error.to_string())?;
        let entry = self.with_session_manager_mut(|manager| manager.append_compaction(
            &result.summary, &result.first_kept_entry_id, result.tokens_before, result.details.clone(), usage, None,
        ));
        let context = self.with_session_manager(|manager| manager.build_context(manager.leaf_id()));
        self.agent.set_messages(context.messages.into_iter().map(session_message_from_value).collect::<Result<_, _>>()
            .map_err(|error| error.to_string())?);
        let mut state = self.state();
        state.message_revision += 1;
        Ok(entry)
    }

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
        let mut overflow_compacted = false;
        loop {
            self.agent.wait_for_idle().await;
            let Some(message) = self.messages().iter().rev().find_map(AgentMessage::as_assistant).cloned() else { return Ok(()); };
            if self.auto_compaction_enabled() && !self.state().user_aborted && !overflow_compacted
                && maho_ai::utils::overflow::is_context_overflow(&message, Some(self.model().context_window))
            {
                overflow_compacted = true;
                let execution = self.compact_for_model(None, &self.model(), "overflow").await;
                execution?;
                self.agent.continue_run(maho_agent::agent::AgentContinuationOptions { defer_queued_messages: Some(true), ..Default::default() }).await;
                continue;
            }
            if !self.will_retry(Some(&message)).await {
                if self.auto_compaction_enabled() && !self.state().user_aborted
                    && !matches!(message.stop_reason, StopReason::Error | StopReason::Aborted)
                {
                    let model = self.model();
                    let threshold = model.context_window.saturating_sub(16_384);
                    if self.get_context_usage().and_then(|usage| usage.tokens).is_some_and(|tokens| tokens > threshold) {
                        self.compact_for_model(None, &model, "threshold").await?;
                    }
                }
                return Ok(());
            }
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

    pub fn pending_model_switch(&self) -> Option<PendingModelSwitch> { self.state().pending_model_switch.clone() }

    fn model_budget(&self, model: &Model, live_context_tokens: u64, speculation: bool)
        -> Result<(maho_ext_api::ModelBudget, bool), String>
    {
        let raw = self.with_settings_manager(|manager| manager.get_value("compaction").cloned());
        let settings = raw.map(serde_json::from_value::<crate::compaction_settings_access::CompactionSettings>)
            .transpose().map_err(|error| error.to_string())?;
        let settings = crate::compaction_settings_resolver::resolve_compaction_settings(settings.as_ref(), Some(
            crate::compaction_settings_access::CompactionModelSelector { provider: &model.provider, id: &model.id },
        ))?;
        let window = model.context_window;
        let ratio = match window {
            0..=16_000 => 0.45, 16_001..=32_000 => 0.5, 32_001..=64_000 => 0.55,
            64_001..=128_000 => 0.6, 128_001..=512_000 => 0.7, _ => 0.8,
        };
        let prompt_tokens = u64::try_from(self.system_prompt().len().div_ceil(4)).map_err(|error| error.to_string())?;
        let tools = self.agent.state().tools().iter().map(|tool| serde_json::to_string(&tool.tool)
            .map(|text| u64::try_from(text.len().div_ceil(4)).unwrap_or(u64::MAX))).collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?.into_iter().sum::<u64>();
        let (margin, profile) = if model.provider == "anthropic" || model.id.contains("claude") { (16_384, "anthropic") }
            else if model.provider == "openai" || ["gpt-5", "o1", "o3", "o4"].iter().any(|family| model.id.contains(family)) { (16_384, "openai-reasoning") }
            else if model.provider == "google" || model.id.contains("gemini") { (12_288, "google") }
            else if model.provider == "deepseek" || model.id.contains("deepseek") { (12_288, "deepseek") }
            else { (8_192, "default") };
        let reserve = if settings.enabled {
            let configured = u64::try_from(settings.reserve_tokens).map_err(|error| error.to_string())?;
            if settings.reserve_scaling_enabled { configured.max((window / 25).min(49_152)) } else { configured }
        } else { 0 };
        let lead = if speculation && settings.enabled && settings.speculative_enabled {
            settings.speculative_lead_tokens.unwrap_or((window as f64 * ratio * 0.125).floor()).clamp(8_192.0, 32_768.0) as u64
        } else { 0 };
        let overhead = prompt_tokens.saturating_add(tools).saturating_add(model.max_tokens.min(window / 2))
            .saturating_add(reserve).saturating_add(lead).saturating_add(margin);
        let required_tokens = overhead.saturating_add(live_context_tokens);
        let keep = u64::try_from(settings.keep_recent_tokens).map_err(|error| error.to_string())?;
        let keep = if window > 409_600 && keep >= 10_000 { keep.max((window / 20).min(60_000)) } else { keep };
        let keep = keep.min(((window as f64 * (1.0 - ratio - 0.05)).floor() as u64).max(1_024));
        Ok((maho_ext_api::ModelBudget {
            context_window: window, live_context_tokens, required_tokens,
            shortfall_tokens: required_tokens.saturating_sub(window), safety_margin_profile: Some(profile.to_owned()),
        }, settings.enabled && overhead.saturating_add(keep) <= window))
    }

    pub fn assert_model_usable(&self, model: &Model, live_context_tokens: u64) -> Result<(), String> {
        if model.context_window == 0 { return Ok(()); }
        let (budget, _) = self.model_budget(model, live_context_tokens, live_context_tokens == 0)?;
        if budget.shortfall_tokens == 0 { Ok(()) } else { Err(format!(
            "Model \"{}/{}\" cannot {}: context window {} tokens is {} tokens short of the {}-token requirement.",
            model.provider, model.id, if live_context_tokens == 0 { "start" } else { "switch" },
            budget.context_window, budget.shortfall_tokens, budget.required_tokens,
        )) }
    }

    pub async fn set_model(&self, model: Model) -> Result<Option<SystemPromptChangeEvent>, String> {
        self.set_model_internal(model, true, maho_ext_api::ModelSelectSource::Set, true).await
    }

    pub async fn set_session_model(&self, model: Model) -> Result<Option<SystemPromptChangeEvent>, String> {
        self.set_model_internal(model, false, maho_ext_api::ModelSelectSource::Set, true).await
    }

    async fn set_model_internal(&self, model: Model, persist_default: bool, source: maho_ext_api::ModelSelectSource,
        allow_deferral: bool) -> Result<Option<SystemPromptChangeEvent>, String>
    {
        let previous = self.model();
        let live = if model.context_window < previous.context_window {
            self.get_context_usage().and_then(|usage| usage.tokens).unwrap_or(0)
        } else { 0 };
        let (budget, repairable) = self.model_budget(&model, live, self.messages().is_empty())?;
        let admission = if model.context_window > 0 && budget.shortfall_tokens > 0 && !repairable {
            self.assert_model_usable(&model, live)
        } else { Ok(()) };
        if let Err(detail) = admission {
            self.emit(AgentSessionEvent::ModelChangeRejected { model, reason: "context-budget".to_owned(), detail: detail.clone(), budget: Some(budget) });
            return Err(detail);
        }
        if self.state().uses_default_stream_function { self.get_required_request_auth(&model).await?; }
        if allow_deferral && budget.shortfall_tokens > 0 && repairable {
            let notice = format!("{} needs {} fewer tokens than this conversation holds. It is compacted on your next message, and the switch applies after that.", model.id, budget.shortfall_tokens);
            self.state().pending_model_switch = Some(PendingModelSwitch { model: model.clone(), budget: budget.clone(), persist_default, notice: notice.clone() });
            self.emit(AgentSessionEvent::ModelChangePending { model, budget, notice });
            return Ok(None);
        }
        let old_prompt = self.system_prompt();
        let old_thinking = self.thinking_level();
        let old_tier = self.service_tier();
        self.agent.set_model(model.clone());
        self.agent.set_thinking_level(self.get_thinking_for_model_switch(&model, None));
        let result = {
            let mut runner = self.extension_runner.lock().await;
            match runner.as_mut() {
                Some(runner) => runner.emit_model_select(maho_ext_api::ModelSelectEvent {
                    model: model.clone(), previous_model: Some(previous.clone()), source,
                    system_prompt: old_prompt.clone(), system_prompt_options: self.extension_system_prompt_options(),
                }).await.map_err(|error| error.to_string()),
                None => Ok(None),
            }
        };
        let change = match result {
            Ok(result) => {
                let prompt = result.as_ref().and_then(|result| result.system_prompt.clone())
                    .unwrap_or_else(|| Some(old_prompt.clone())).unwrap_or_else(|| self.state().base_system_prompt.clone());
                self.agent.set_system_prompt(prompt.clone());
                let admission = self.assert_model_usable(&model, live);
                if let Err(error) = admission {
                    self.agent.set_model(previous); self.agent.set_system_prompt(old_prompt); self.agent.set_thinking_level(old_thinking);
                    return Err(error);
                }
                (prompt != old_prompt).then(|| SystemPromptChangeEvent {
                    system_prompt: prompt, previous_system_prompt: old_prompt,
                    system_prompt_name: result.and_then(|result| result.system_prompt_name), model: model.clone(), previous_model: Some(previous.clone()),
                })
            }
            Err(error) => {
                self.agent.set_model(previous); self.agent.set_thinking_level(old_thinking); self.agent.set_system_prompt(old_prompt);
                return Err(error);
            }
        };
        self.with_session_manager_mut(|manager| manager.append_model_change(&model.provider, &model.id, None, None));
        if persist_default {
            self.with_settings_manager_mut(|manager| manager.set(crate::settings_manager::SettingsScope::Global,
                &Map::from_iter([("defaultProvider".to_owned(), Value::String(model.provider.clone())), ("defaultModel".to_owned(), Value::String(model.id.clone()))])))?;
        }
        self.state().pending_model_switch = None;
        self.state().current_service_tier = resolve_service_tier(&model, self.scoped_models().iter().find(|entry|
            models_are_equal(Some(&entry.model), Some(&model))).and_then(|entry| entry.service_tier));
        self.emit(AgentSessionEvent::ModelChanged { model: model.clone(), thinking_level: thinking_level_from_model_level(self.thinking_level()).unwrap_or(ThinkingLevel::Minimal), source });
        if old_tier != self.service_tier() { self.emit(AgentSessionEvent::ServiceTierChanged { tier: self.service_tier(), fast_mode: self.is_fast_mode_active() }); }
        if let Some(change) = &change { self.emit(AgentSessionEvent::SystemPromptChange {
            system_prompt: change.system_prompt.clone(), previous_system_prompt: change.previous_system_prompt.clone(),
            system_prompt_name: change.system_prompt_name.clone(), model, previous_model: Some(previous),
        }); }
        Ok(change)
    }

    pub async fn cycle_model(&self, forward: bool) -> Result<Option<ModelCycleResult>, String> {
        let models = self.get_current_favorite_models();
        if models.len() <= 1 { return Ok(None); }
        let current = self.model();
        let index = models.iter().position(|entry| models_are_equal(Some(&entry.model), Some(&current)));
        let mut skipped_models = Vec::new();
        for offset in 1..=models.len() {
            let next = match (index, forward) {
                (Some(index), true) => (index + offset) % models.len(),
                (Some(index), false) => (index + models.len() - offset % models.len()) % models.len(),
                (None, true) => offset - 1, (None, false) => models.len() - offset,
            };
            let model = &models[next].model;
            if models_are_equal(Some(model), Some(&current)) { continue; }
            let live = self.get_context_usage().and_then(|usage| usage.tokens).unwrap_or(0);
            let (budget, repairable) = self.model_budget(model, live, false)?;
            if budget.shortfall_tokens > 0 && !repairable {
                self.emit(AgentSessionEvent::ModelChangeSkipped { model: model.clone(), budget, direction: if forward { "forward" } else { "backward" }.to_owned() });
                skipped_models.push(model.clone()); continue;
            }
            let system_prompt_change = self.set_model_internal(model.clone(), true, maho_ext_api::ModelSelectSource::Cycle, true).await?;
            return Ok(Some(ModelCycleResult { model: self.model(), thinking_level: thinking_level_from_model_level(self.thinking_level()).unwrap_or(ThinkingLevel::Minimal),
                is_scoped: true, skipped_models, system_prompt_change }));
        }
        Ok(None)
    }

    async fn session_before(&self, event: maho_ext_api::ExtensionEvent) -> Result<maho_ext_api::SessionBeforeEventResult, String> {
        let mut runner = self.extension_runner.lock().await;
        match runner.as_mut() {
            Some(runner) => match runner.emit(event).await.map_err(|error| error.to_string())? {
                maho_ext_api::EventResult::SessionBefore(result) => Ok(result), _ => Ok(Default::default()),
            },
            None => Ok(Default::default()),
        }
    }

    fn rebuild_session_context(&self) -> Result<(), String> {
        let context = self.with_session_manager(|manager| manager.build_context(manager.leaf_id()));
        let messages = context.messages.into_iter().map(session_message_from_value).collect::<Result<_, _>>()
            .map_err(|error| error.to_string())?;
        if let Some((provider, id)) = context.model
            && let Some(model) = self.model_registry.find(&provider, &id)
        { self.agent.set_model(model); }
        if let Some(level) = ModelThinkingLevel::ALL.into_iter().find(|candidate| candidate.as_str() == context.thinking_level) {
            self.set_session_thinking_level(level);
        }
        self.agent.set_messages(messages);
        self.state().message_revision += 1;
        Ok(())
    }

    async fn finish_session_replacement(&self, reason: maho_ext_api::SessionReason, previous: Option<String>) -> Result<(), String> {
        self.clear_queue(false);
        self.state().pending_model_switch = None;
        self.state().retry_attempt = 0;
        self.reset_hint_tier_state();
        self.rebuild_session_context()?;
        self.renew_extension_runtime(reason).await?;
        self.dispatch_extension_event(maho_ext_api::ExtensionEvent::SessionStart(maho_ext_api::SessionStartEvent {
            reason, initial_model_provenance: None, previous_session_file: previous,
        })).await;
        Ok(())
    }

    pub async fn new_session(&self, options: Option<crate::session_manager::NewSessionOptions>) -> Result<bool, String> {
        if self.session_before(maho_ext_api::ExtensionEvent::SessionBeforeSwitch {
            reason: maho_ext_api::SessionReason::New, target_session_file: None,
        }).await?.cancel == Some(true) { return Ok(false); }
        self.abort().await;
        let previous = self.session_file();
        self.emit_session_shutdown(maho_ext_api::SessionReason::New).await;
        self.with_session_manager_mut(|manager| manager.new_session(options));
        self.finish_session_replacement(maho_ext_api::SessionReason::New, previous).await?;
        Ok(true)
    }

    pub async fn switch_session(&self, path: &str) -> Result<bool, String> {
        let entries = crate::session_manager::load_entries_from_file(path);
        if entries.first().and_then(|entry| entry.get("type")).and_then(Value::as_str) != Some("session") {
            return Err(format!("Invalid session file: {path}"));
        }
        if self.session_before(maho_ext_api::ExtensionEvent::SessionBeforeSwitch {
            reason: maho_ext_api::SessionReason::Resume, target_session_file: Some(path.to_owned()),
        }).await?.cancel == Some(true) { return Ok(false); }
        self.abort().await;
        let previous = self.session_file();
        self.emit_session_shutdown(maho_ext_api::SessionReason::Resume).await;
        self.with_session_manager_mut(|manager| manager.set_session_file(path, None));
        self.finish_session_replacement(maho_ext_api::SessionReason::Resume, previous).await?;
        Ok(true)
    }

    pub async fn fork(&self, entry_id: &str, include_entry: bool) -> Result<AssistantEditResult, String> {
        let entry = self.with_session_manager(|manager| manager.entry(entry_id)).ok_or_else(|| format!("Entry {entry_id} not found"))?;
        let position = if include_entry { maho_ext_api::ForkPosition::At } else { maho_ext_api::ForkPosition::Before };
        if self.session_before(maho_ext_api::ExtensionEvent::SessionBeforeFork { entry_id: entry_id.to_owned(), position }).await?.cancel == Some(true) {
            return Ok(AssistantEditResult { cancelled: true, ..Default::default() });
        }
        self.abort().await;
        let leaf = if include_entry { Some(entry_id) } else { entry.get("parentId").and_then(Value::as_str) };
        let entries = self.with_session_manager(|manager| if leaf.is_some() { manager.branch(leaf) } else { Vec::new() });
        let previous = self.session_file();
        self.emit_session_shutdown(maho_ext_api::SessionReason::Fork).await;
        self.with_session_manager_mut(|manager| {
            manager.new_session(Some(crate::session_manager::NewSessionOptions { parent_session: previous.clone(), ..Default::default() }));
            for entry in entries { manager.append_entry_raw(entry); }
        });
        self.finish_session_replacement(maho_ext_api::SessionReason::Fork, previous).await?;
        Ok(AssistantEditResult { editor_text: (!include_entry).then(|| entry.get("message").and_then(|message|
            serde_json::from_value::<AgentMessage>(message.clone()).ok()).map(|message| user_message_text(&message))).flatten(), ..Default::default() })
    }

    pub async fn navigate_tree(&self, target_id: &str, options: TreeNavigationOptions) -> Result<AssistantEditResult, String> {
        self.navigate_tree_internal(target_id, options, None).await
    }

    pub async fn edit_assistant_message(&self, entry_id: &str, text: &str, options: TreeNavigationOptions) -> Result<AssistantEditResult, String> {
        let entry = self.with_session_manager(|manager| manager.entry(entry_id)).ok_or_else(|| format!("Entry {entry_id} not found"))?;
        let message: maho_ai::types::AssistantMessage = serde_json::from_value(entry["message"].clone()).map_err(|error| error.to_string())?;
        let replacement = crate::edited_assistant_message::build_edited_assistant_message(&message, text).map_err(|error| error.to_string())?;
        if entry["message"] == serde_json::to_value(&replacement).map_err(|error| error.to_string())? {
            return Ok(AssistantEditResult { unchanged: Some(true), ..Default::default() });
        }
        self.navigate_tree_internal(entry_id, options, Some(AgentMessage::Llm(maho_ai::types::Message::Assistant(Box::new(replacement))))).await
    }

    pub async fn edit_user_message(&self, entry_id: &str, text: &str, options: TreeNavigationOptions) -> Result<UserEditResult, String> {
        let entry = self.with_session_manager(|manager| manager.entry(entry_id)).ok_or_else(|| format!("Entry {entry_id} not found"))?;
        let message: maho_ai::types::UserMessage = serde_json::from_value(entry["message"].clone()).map_err(|error| error.to_string())?;
        let replacement = crate::edited_user_message::build_edited_user_message(&message, text).map_err(|error| error.to_string())?;
        self.navigate_tree_internal(entry_id, options, Some(AgentMessage::Llm(maho_ai::types::Message::User(replacement)))).await
    }

    async fn navigate_tree_internal(&self, target_id: &str, options: TreeNavigationOptions, replacement: Option<AgentMessage>) -> Result<AssistantEditResult, String> {
        if self.is_streaming() { return Err("Cannot navigate the session tree while streaming".to_owned()); }
        if self.is_compacting() { return Err("Cannot navigate the session tree while compacting".to_owned()); }
        let entry = self.with_session_manager(|manager| manager.entry(target_id)).ok_or_else(|| format!("Entry {target_id} not found"))?;
        let old_leaf = self.with_session_manager(|manager| manager.leaf_id().map(str::to_owned));
        if options.expected_leaf_id.is_some() && options.expected_leaf_id != old_leaf { return Err("Session leaf changed before edit".to_owned()); }
        let signal = maho_ext_api::AbortSignal::default();
        let before = self.session_before(maho_ext_api::ExtensionEvent::SessionBeforeTree {
            preparation: maho_ext_api::TreePreparation { target_id: target_id.to_owned(), old_leaf_id: old_leaf.clone(), common_ancestor_id: None,
                entries_to_summarize: Vec::new(), user_wants_summary: options.summarize.unwrap_or(false), custom_instructions: options.custom_instructions.clone(),
                replace_instructions: options.replace_instructions, label: options.label.clone() }, signal,
        }).await?;
        if before.cancel == Some(true) { return Ok(AssistantEditResult { cancelled: true, ..Default::default() }); }
        let mut editor_text = None;
        let leaf = if replacement.is_some() { entry.get("parentId").and_then(Value::as_str).map(str::to_owned) }
            else if options.intent == Some(TreeNavigationIntent::Resume) { Some(target_id.to_owned()) }
            else if entry.get("message").and_then(|message| message.get("role")).and_then(Value::as_str) == Some("user") {
                editor_text = Some(user_message_text(&session_message_from_value(entry["message"].clone()).map_err(|error| error.to_string())?));
                entry.get("parentId").and_then(Value::as_str).map(str::to_owned)
            } else { Some(target_id.to_owned()) };
        self.with_session_manager_mut(|manager| manager.set_leaf(leaf.as_deref()));
        let replacement_entry = replacement.map(|message| serde_json::to_value(message).map(|message|
            self.with_session_manager_mut(|manager| manager.append_message(message)))).transpose().map_err(|error| error.to_string())?;
        let summary_entry = before.summary.and_then(|summary| summary.get("summary").and_then(Value::as_str).map(str::to_owned))
            .map(|summary| self.with_session_manager_mut(|manager| manager.append_branch_summary(&summary, old_leaf.as_deref().unwrap_or_default(), None, None, None)));
        if let Some(label) = options.label.as_deref() { self.with_session_manager_mut(|manager| manager.append_label(target_id, Some(label))); }
        self.rebuild_session_context()?;
        self.dispatch_extension_event(maho_ext_api::ExtensionEvent::SessionTree { new_leaf_id: self.with_session_manager(|manager| manager.leaf_id().map(str::to_owned)),
            old_leaf_id: old_leaf, summary_entry: summary_entry.clone().map(session_entry_from_value), from_extension: Some(summary_entry.is_some()) }).await;
        Ok(AssistantEditResult { editor_text, summary_entry, entry_id: replacement_entry.as_ref().and_then(|entry| entry.get("id")).and_then(Value::as_str).map(str::to_owned), ..Default::default() })
    }

    pub fn set_prompt_resources(&self, templates: Vec<crate::prompt_templates::PromptTemplate>, skills: Vec<crate::skills::Skill>) {
        let mut state = self.state(); state.prompt_templates = templates; state.skills = skills;
    }

    pub fn prompt_templates(&self) -> Vec<crate::prompt_templates::PromptTemplate> { self.state().prompt_templates.clone() }

    pub async fn generate_session_title_if_needed(&self, prompt: &str) {
        if self.session_name().is_some() || crate::session_title_generator::should_skip_session_title(prompt) { return; }
        let session_id = self.session_id();
        let generation = async {
            let model = self.model();
            let auth = self.get_summarization_request_auth(&model).await?;
            let AgentMessage::Llm(user) = make_user_message(prompt, None) else { return Err("Invalid title prompt".to_owned()); };
            let response = self.model_runtime().complete(&auth.model, &maho_ai::types::Context {
                system_prompt: Some("Generate a short session title. Return only <title>title</title>; use <title>none</title> when no task is stated.".to_owned()),
                messages: vec![user], tools: None,
            }, Some(maho_ai::types::StreamOptions {
                request: maho_ai::types::ProviderRequestOptions { api_key: auth.api_key,
                    headers: auth.headers.map(|headers| headers.into_iter().map(|(key,value)| (key,Some(value))).collect()),
                    stream_kind: Some(maho_ai::types::StreamKind::Auxiliary), env: auth.env, ..Default::default() },
                max_tokens: Some(128), session_id: Some(session_id.clone()), ..Default::default()
            })).await.map_err(|error| error.to_string())?;
            if let Some(error) = crate::session_title_generator::title_error_message(&response) { return Err(error); }
            Ok::<_, String>(crate::session_title_generator::parse_session_title(&response))
        }.await;
        match generation {
            Ok(Some(title)) if self.session_id() == session_id && self.session_name().is_none() => self.set_session_name(&title),
            Ok(_) => {}, Err(error) => self.emit(AgentSessionEvent::ContinuationError { error_message: error }),
        }
    }

    pub async fn execute_bash(&self, command: &str, on_chunk: Option<maho_tools::bash_executor::BashChunkCallback>,
        exclude_from_context: bool, id: Option<String>, operations: Option<Arc<dyn maho_tools::bash::BashOperations>>)
        -> Result<maho_tools::bash_executor::BashResult, String>
    {
        let signal = maho_ext_api::AbortSignal::default();
        let key = id.clone().unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        self.state().bash_abort_signals.insert(key.clone(), signal.clone());
        let prefix = self.with_settings_manager(|manager| manager.get_string("shellCommandPrefix"));
        let shell = self.with_settings_manager(|manager| manager.get_string("shellPath"));
        let local = maho_tools::bash::create_local_bash_operations(shell.as_deref());
        let resolved = prefix.map_or_else(|| command.to_owned(), |prefix| format!("{prefix}\n{command}"));
        let session = self.clone();
        let cwd = self.cwd();
        let result = maho_tools::bash_executor::execute_bash_with_operations(&resolved, std::path::Path::new(&cwd),
            operations.as_deref().unwrap_or(local.as_ref()), maho_tools::bash_executor::BashExecutorOptions {
                signal, on_chunk: Some(Arc::new(move |chunk| {
                    if let Some(callback) = &on_chunk { callback(chunk)?; }
                    session.emit(AgentSessionEvent::BashExecutionUpdate { id: id.clone(), delta: chunk.to_owned() });
                    Ok(())
                })), on_chunk_async: None,
            }).await.map_err(|error| error.to_string());
        self.state().bash_abort_signals.remove(&key);
        if let Ok(result) = &result { self.record_bash_result(command, result, exclude_from_context); }
        result
    }

    pub fn record_bash_result(&self, command: &str, result: &maho_tools::bash_executor::BashResult, exclude_from_context: bool) {
        let message = maho_agent::harness::messages::BashExecutionMessage {
            role: "bashExecution".to_owned(), command: command.to_owned(), output: result.output.clone(), exit_code: result.exit_code.map(i64::from),
            cancelled: result.cancelled, truncated: result.truncated, full_output_path: result.full_output_path.as_ref().map(|path| path.to_string_lossy().into_owned()),
            timestamp: maho_ai::utils::diagnostics::now_ms(), exclude_from_context: Some(exclude_from_context),
        };
        self.state().pending_bash_messages.push(message);
        if !self.is_streaming() { self.flush_pending_bash_messages(); }
    }

    pub fn flush_pending_bash_messages(&self) {
        let pending = std::mem::take(&mut self.state().pending_bash_messages);
        let mut messages = self.messages();
        for message in pending {
            match serde_json::to_value(&message) {
                Ok(value) => { self.with_session_manager_mut(|manager| manager.append_message(value)); self.state().message_revision += 1; }
                Err(error) => self.emit(AgentSessionEvent::ContinuationError { error_message: error.to_string() }),
            }
            messages.push(AgentMessage::Custom(maho_agent::types::CustomAgentMessage::BashExecution(message)));
        }
        self.agent.set_messages(messages);
    }

    pub fn is_bash_running(&self) -> bool { !self.state().bash_abort_signals.is_empty() }
    pub fn has_pending_bash_messages(&self) -> bool { !self.state().pending_bash_messages.is_empty() }
    pub fn abort_bash(&self) { for signal in self.state().bash_abort_signals.values() { signal.abort(); } }
    pub async fn cleanup_bash_output(&self, path: &std::path::Path) -> Result<(), String> {
        match tokio::fs::remove_file(path).await { Ok(()) => Ok(()), Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()), Err(error) => Err(error.to_string()) }
    }

    pub fn rebuild_system_prompt(&self) {
        let skills = self.state().skills.clone();
        let active = self.get_active_tool_names();
        let base = crate::system_prompt::build_system_prompt(&crate::system_prompt::BuildSystemPromptOptions {
            cwd: self.cwd(), selected_tools: Some(active), skills: Some(skills), ..Default::default()
        });
        self.state().base_system_prompt = base.clone();
        self.agent.set_system_prompt(base);
    }

    fn extension_system_prompt_options(&self) -> maho_ext_api::BuildSystemPromptOptions {
        maho_ext_api::BuildSystemPromptOptions {
            cwd: self.cwd().into(), tools: self.get_active_tool_names(),
            skills: self.state().skills.iter().map(|skill| maho_ext_api::Skill {
                name: skill.name.clone(), description: skill.description.clone(), file_path: skill.file_path.clone(),
                base_dir: skill.base_dir.clone(), disable_model_invocation: skill.disable_model_invocation,
                source_info: maho_ext_api::SourceInfo {
                    path: skill.source_info.path.clone(), source: skill.source_info.source.clone(), base_dir: skill.source_info.base_dir.clone(),
                    scope: match skill.source_info.scope {
                        crate::source_info::SourceScope::User => maho_ext_api::SourceScope::User,
                        crate::source_info::SourceScope::Project => maho_ext_api::SourceScope::Project,
                        crate::source_info::SourceScope::Temporary => maho_ext_api::SourceScope::Temporary,
                        crate::source_info::SourceScope::System => maho_ext_api::SourceScope::System,
                    },
                    origin: match skill.source_info.origin {
                        crate::source_info::SourceOrigin::Package => maho_ext_api::SourceOrigin::Package,
                        crate::source_info::SourceOrigin::TopLevel => maho_ext_api::SourceOrigin::TopLevel,
                    },
                },
            }).collect(), ..Default::default()
        }
    }

    pub async fn reload(&self) -> Result<bool, String> {
        if self.session_before(maho_ext_api::ExtensionEvent::SessionBeforeReload).await?.cancel == Some(true) { return Ok(false); }
        self.abort().await;
        self.emit_session_shutdown(maho_ext_api::SessionReason::Reload).await;
        self.with_settings_manager_mut(|manager| manager.reload());
        let templates = crate::prompt_templates::load_prompt_templates(&crate::prompt_templates::LoadPromptTemplatesOptions {
            cwd: self.cwd(), agent_dir: self.agent_dir(), include_defaults: true, ..Default::default()
        });
        let skills = crate::skills::load_skills(&crate::skills::LoadSkillsOptions {
            cwd: self.cwd(), agent_dir: self.agent_dir(), include_defaults: true, ..Default::default()
        });
        self.set_prompt_resources(templates, skills.skills);
        self.rebuild_system_prompt();
        self.publish_eval_only_tool_hints();
        self.renew_extension_runtime(maho_ext_api::SessionReason::Reload).await?;
        self.dispatch_extension_event(maho_ext_api::ExtensionEvent::SessionStart(maho_ext_api::SessionStartEvent {
            reason: maho_ext_api::SessionReason::Reload, initial_model_provenance: None, previous_session_file: self.session_file(),
        })).await;
        Ok(true)
    }

    fn expand_input(&self, text: &str, templates: bool) -> Result<String, String> {
        let skills = self.state().skills.clone();
        let tokens = crate::skill_invocation::parse_skill_invocation_tokens(text, &crate::skill_invocation::ParseSkillInvocationOptions {
            known_skill_names: Some(skills.iter().map(|skill| skill.name.clone()).collect()),
        });
        let mut blocks = Vec::new();
        let mut removed = Vec::new();
        let mut metadata = Vec::new();
        let mut seen = BTreeSet::new();
        for token in tokens {
            if let Some(skill) = skills.iter().find(|skill| skill.name == token.name) {
                removed.push(token.clone());
                if !seen.insert(skill.name.clone()) { continue; }
                let source = match std::fs::read_to_string(&skill.file_path) { Ok(source) => source, Err(error) => {
                    self.emit(AgentSessionEvent::ContinuationError { error_message: error.to_string() }); return Ok(text.to_owned());
                }};
                let parsed = crate::frontmatter::parse_frontmatter(&source).map_err(|error| error.to_string())?;
                blocks.push(crate::skill_invocation::SkillInvocationPromptSkill {
                    name: skill.name.clone(), file_path: skill.file_path.clone(), base_dir: skill.base_dir.clone(), body: parsed.body.trim().to_owned(),
                });
                metadata.push(maho_ext_api::SkillInvocation { name: skill.name.clone(), path: skill.file_path.clone(), syntax: match token.syntax {
                    crate::skill_invocation::SkillInvocationSyntax::Slash => "slash", crate::skill_invocation::SkillInvocationSyntax::Dollar => "dollar",
                }.to_owned() });
            }
        }
        let expanded = if blocks.is_empty() { text.to_owned() } else {
            self.emit(AgentSessionEvent::SkillInvocation { skills: metadata });
            crate::skill_invocation::format_skill_invocation_prompt(&blocks, Some(&crate::skill_invocation::remove_skill_invocation_tokens(text, &removed)))
        };
        if !templates { return Ok(expanded); }
        let expansion = crate::prompt_templates::expand_prompt_template_with_metadata(&expanded, &self.prompt_templates());
        if let Some(template) = expansion.template { self.emit(AgentSessionEvent::CommandInvocation { command: serde_json::json!({
            "name":template.name,"source":"prompt","syntax":"slash","path":template.file_path,
        }) }); }
        Ok(expansion.text)
    }

    async fn try_execute_extension_command(&self, text: &str) -> Result<bool, String> {
        let command_text = text.strip_prefix('/').unwrap_or(text);
        let (name, args) = command_text.split_once(' ').unwrap_or((command_text, ""));
        let runner = self.extension_runner.lock().await.clone();
        let Some(runner) = runner else { return Ok(false); };
        if runner.get_command(name).is_none() { return Ok(false); }
        self.emit(AgentSessionEvent::CommandInvocation { command: serde_json::json!({"name":name,"source":"extension","syntax":"slash"}) });
        let context = runner.create_command_context(Arc::new(SessionExtensionActions(Arc::downgrade(&self.inner)))).map_err(|error| error.to_string())?;
        runner.invoke_command(name, args, &context).await.map_err(|error| error.to_string())?;
        Ok(true)
    }

    async fn run_replacement_callbacks(&self, setup: Option<maho_ext_api::SessionSetup>, callback: Option<maho_ext_api::WithSession>) -> Result<(), maho_ext_api::ExtensionFailure> {
        if setup.is_none() && callback.is_none() { return Ok(()); }
        let mut runner = self.extension_runner.lock().await.clone().ok_or_else(|| maho_ext_api::ExtensionFailure::new("Extension runner is not bound"))?;
        let actions = Arc::new(SessionExtensionActions(Arc::downgrade(&self.inner)));
        let mut base = runner.create_context()?;
        base.session_manager = Arc::new(ExtensionSessionManagerView {
            session: Arc::downgrade(&self.inner), id: self.session_id(), file: self.session_file().map(Into::into),
        });
        runner.bind_core(actions.clone(), base);
        runner.bind_context_actions(actions.clone())?;
        let context = runner.create_command_context(actions.clone())?;
        if let Some(setup) = setup { setup(context.session_manager.as_ref()).await?; self.rebuild_session_context().map_err(maho_ext_api::ExtensionFailure::new)?; }
        if let Some(callback) = callback {
            callback(&maho_ext_api::ReplacedSessionContext { context, message_actions: actions }).await?;
        }
        Ok(())
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
        let state = self.state();
        let mut tools: BTreeMap<String, ToolInfo> = state.tool_definitions.iter().map(|(name, entry)| {
            (name.clone(), normalize_tool_exposure(&entry.definition, entry.source_info.clone()))
        }).collect();
        for tool in self.agent
            .state()
            .tools()
            .iter()
        {
            tools.entry(tool.name().to_owned()).or_insert_with(|| ToolInfo {
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
            });
        }
        tools.into_values().collect()
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

    fn register_extension_tool(&self, definition: ToolDefinition, source_info: SourceInfo, tool: AgentTool) {
        {
            let mut state = self.state();
            if !state.extension_tool_backups.contains_key(&definition.name) {
                let previous = state.tool_definitions.get(&definition.name).cloned()
                    .zip(state.tool_registry.get(&definition.name).cloned());
                state.extension_tool_backups.insert(definition.name.clone(), previous);
            }
        }
        self.register_tool_definition(definition, source_info, tool);
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
        self.execute_tool_with_updates(tool_name, params, options, None).await
    }

    async fn execute_tool_with_updates(
        &self,
        tool_name: &str,
        params: Value,
        options: ExecuteToolOptions,
        on_update: Option<maho_agent::types::AgentToolUpdateCallback>,
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
            on_update,
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

    async fn renew_extension_runtime(&self, reason: maho_ext_api::SessionReason) -> Result<(), String> {
        let old = self.extension_runner.lock().await.clone();
        let Some(mut old) = old else { return Ok(()); };
        let next = old.recreate().await.map_err(|error| error.message)?;
        let paths: BTreeSet<_> = next.extensions.iter().map(|extension| extension.identity.resolved_path.clone()).collect();
        let removed: Vec<_> = old.extensions.iter().filter(|extension| !paths.contains(&extension.identity.resolved_path))
            .map(|extension| extension.identity.clone()).collect();
        if !removed.is_empty() {
            let _ = old.emit(maho_ext_api::ExtensionEvent::SessionExtensionsRemoved { reason, removed }).await;
        }
        old.invalidate("This extension ctx is stale after session replacement or reload.");
        let mut active = self.get_active_tool_names();
        {
            let mut state = self.state();
            for (name, previous) in std::mem::take(&mut state.extension_tool_backups) {
                if let Some((entry, tool)) = previous {
                    state.base_tool_definitions.insert(name.clone(), entry.definition.clone());
                    state.tool_definitions.insert(name.clone(), entry);
                    state.tool_registry.insert(name, tool);
                } else {
                    state.base_tool_definitions.remove(&name);
                    state.tool_definitions.remove(&name);
                    state.tool_registry.remove(&name);
                    active.retain(|active| active != &name);
                }
            }
        }
        self.set_active_tools_by_name(active);
        self.set_extension_runner(next).await;
        Ok(())
    }

    /// Bind the extension runner the tool hooks read at execution time.
    pub async fn set_extension_runner(&self, mut runner: ExtensionRunner) {
        let weak = Arc::downgrade(&self.inner);
        runner.set_shutdown_budget_resolver(Arc::new(move || {
            let Some(inner) = weak.upgrade() else { return (2_000, 10_000); };
            AgentSession { inner }.with_settings_manager(|manager| {
                let parse = |key: &str, default| match manager.get_value(key) {
                    None => Some(default), Some(value) => crate::http_dispatcher::parse_http_idle_timeout_ms(value),
                };
                match (parse("sessionShutdownHandlerWarnMs", 2_000), parse("sessionShutdownHandlerTimeoutMs", 10_000)) {
                    (Some(warn), Some(timeout)) => (warn, timeout),
                    _ => (2_000, 10_000),
                }
            })
        }));
        if let Ok(mut context) = runner.create_context() {
            context.model_registry = Arc::new(ExtensionModelRegistryView(self.model_registry().clone()));
            let weak = Arc::downgrade(&self.inner);
            context.wait_for_idle_fn = Arc::new(move || {
                let weak = weak.clone();
                Box::pin(async move {
                    let inner = weak.upgrade().unwrap_or_else(|| std::panic::panic_any(maho_ext_api::ExtensionFailure::new("Agent session has been disposed")));
                    AgentSession { inner }.wait_for_idle().await;
                })
            });
            context.session_manager = Arc::new(ExtensionSessionManagerView {
                session: Arc::downgrade(&self.inner), id: self.session_id(), file: self.session_file().map(Into::into),
            });
            runner.bind_core(Arc::new(SessionExtensionActions(Arc::downgrade(&self.inner))), context);
        }
        if let Err(error) = runner.bind_session_actions(Arc::new(SessionExtensionActions(Arc::downgrade(&self.inner)))) {
            self.emit(AgentSessionEvent::ContinuationError { error_message: error.message });
        }
        if let Err(error) = runner.bind_context_actions(Arc::new(SessionExtensionActions(Arc::downgrade(&self.inner)))) {
            self.emit(AgentSessionEvent::ContinuationError { error_message: error.message });
        }
        if let Err(error) = runner.bind_providers(Arc::new(crate::agent_session_runtime::ExtensionModelRuntimeActions(Mutex::new(self.model_runtime().clone())))) {
            self.emit(AgentSessionEvent::ContinuationError { error_message: error.message });
        }
        if let Ok(context) = runner.create_context() {
            let (events, mut receiver) = tokio::sync::mpsc::unbounded_channel();
            self.state().extension_event_sender = Some(events.clone());
            let weak = Arc::downgrade(&self.inner);
            let ui = maho_ext_host::ui::LifecycleUi::new(context.ui, runner.runtime.clone(), Arc::new(move |event| { let _ = events.send(event); }));
            if let Err(error) = runner.bind_ui(Arc::new(ui)) { self.emit(AgentSessionEvent::ContinuationError { error_message: error.message }); }
            tokio::spawn(async move {
                while let Some(event) = receiver.recv().await {
                    let Some(inner) = weak.upgrade() else { break; };
                    AgentSession { inner }.dispatch_extension_event(event).await;
                }
            });
        }
        let tool_runner = runner.clone();
        let context_factory: maho_ext_host::wrapper::ToolContextFactory = Arc::new(move || tool_runner.create_context());
        self.state().extension_tool_context = Some((runner.runtime.clone(), context_factory.clone()));
        let mut active = self.get_active_tool_names();
        for registered in runner.get_all_registered_tools() {
            let definition = registered.definition.clone();
            let source = registered.source_info.clone();
            let tool = maho_ext_host::wrapper::wrap_registered_tool(registered, runner.runtime.clone(), context_factory.clone());
            if normalize_tool_exposure(&definition, source.clone()).exposure != ToolExposure::Search && !active.contains(&definition.name) { active.push(definition.name.clone()); }
            self.register_extension_tool(definition, source, tool);
        }
        self.set_active_tools_by_name(active);
        self.state().extension_commands = runner.get_registered_commands().into_iter().map(|command| maho_ext_api::SlashCommandInfo {
            name: command.invocation_name, description: command.command.description,
            argument_hint: command.command.argument_hint, source_info: Some(command.command.source_info),
        }).collect();
        let command_runner = runner.clone();
        self.state().extension_command_catalog = Some(Arc::new(move || command_runner.get_registered_commands().into_iter().map(|command| maho_ext_api::SlashCommandInfo {
            name: command.invocation_name, description: command.command.description,
            argument_hint: command.command.argument_hint, source_info: Some(command.command.source_info),
        }).collect()));
        *self.extension_runner.lock().await = Some(runner);
        let weak = Arc::downgrade(&self.inner);
        self.agent.set_transform_context(Some(Arc::new(move |messages, _signal| {
            let weak = weak.clone(); Box::pin(async move {
                let Some(inner) = weak.upgrade() else { return messages; };
                let session = AgentSession { inner };
                let mut runner = session.extension_runner.lock().await;
                match runner.as_mut() {
                    Some(runner) => match runner.emit_context(&messages, None).await {
                        Ok(messages) => messages, Err(error) => { session.emit(AgentSessionEvent::ContinuationError { error_message: error.message }); messages }
                    }, None => messages,
                }
            })
        })));
    }

    pub fn get_commands(&self) -> Vec<maho_ext_api::SlashCommandInfo> {
        let mut commands = {
            let state = self.state();
            state.extension_command_catalog.as_ref().map_or_else(|| state.extension_commands.clone(), |catalog| catalog())
        };
        let source_info = |source: crate::source_info::SourceInfo| maho_ext_api::SourceInfo {
            path: source.path, source: source.source, base_dir: source.base_dir,
            scope: match source.scope {
                crate::source_info::SourceScope::User => maho_ext_api::SourceScope::User,
                crate::source_info::SourceScope::Project => maho_ext_api::SourceScope::Project,
                crate::source_info::SourceScope::Temporary => maho_ext_api::SourceScope::Temporary,
                crate::source_info::SourceScope::System => maho_ext_api::SourceScope::System,
            },
            origin: match source.origin {
                crate::source_info::SourceOrigin::Package => maho_ext_api::SourceOrigin::Package,
                crate::source_info::SourceOrigin::TopLevel => maho_ext_api::SourceOrigin::TopLevel,
            },
        };
        commands.extend(self.prompt_templates().into_iter().map(|template| maho_ext_api::SlashCommandInfo {
            name: template.name, description: Some(template.description), argument_hint: template.argument_hint,
            source_info: Some(source_info(template.source_info)),
        }));
        commands.extend(self.state().skills.clone().into_iter().map(|skill| maho_ext_api::SlashCommandInfo {
            name: format!("skill:{}", skill.name), description: Some(skill.description), argument_hint: None,
            source_info: Some(source_info(skill.source_info)),
        }));
        commands
    }

    pub async fn continue_session(&self) -> Result<(), String> {
        let _admission = self.prompt_admission.lock().await;
        let _work = self.work_barrier.begin();
        self.state().user_aborted = false;
        self.agent.continue_run(Default::default()).await;
        self.finish_provider_turn().await?;
        self.flush_pending_bash_messages();
        self.dispatch_extension_event(maho_ext_api::ExtensionEvent::AgentSettled).await;
        self.emit(AgentSessionEvent::AgentSettled);
        self.emit(AgentSessionEvent::AgentIdle);
        Ok(())
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
        self.state().extension_event_sender.take();
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

    fn flush_pending_next_turn_messages(&self) -> Result<(), String> {
        let pending = std::mem::take(&mut self.state().pending_next_turn_messages);
        for message in pending { self.append_extension_custom_message(message)?; }
        Ok(())
    }

    fn append_extension_custom_message(&self, message: maho_agent::harness::messages::CustomMessage) -> Result<(), String> {
        let content = serde_json::to_value(&message.content).map_err(|error| error.to_string())?;
        let entry = self.with_session_manager_mut(|manager| manager.append_custom_message(&message.custom_type, content, message.display, message.details.clone()));
        if let Some(id) = entry.get("id").and_then(Value::as_str) { self.emit_entry_appended(id); }
        let message = AgentMessage::Custom(maho_agent::types::CustomAgentMessage::Custom(message));
        let mut messages = self.messages(); messages.push(message.clone()); self.agent.set_messages(messages);
        self.state().message_revision += 1;
        self.emit(AgentSessionEvent::Agent(maho_agent::types::AgentEvent::MessageStart { message: message.clone() }));
        self.emit(AgentSessionEvent::Agent(maho_agent::types::AgentEvent::MessageEnd { message }));
        Ok(())
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
        let sender = self.state().extension_event_sender.clone();
        if let Some(sender) = sender {
            let _ = sender.send(maho_ext_api::ExtensionEvent::SessionInfoChanged { name: self.session_name() });
        }
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
        self.work_barrier.wait_for_settled(|| self.agent.wait_for_idle()).await;
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
        if update_global_default {
            let model = self.model();
            let key = format!("{}/{}", model.provider, model.id);
            let mut levels = self.with_settings_manager(|manager| manager.get_value("modelThinkingLevels")
                .and_then(Value::as_object).cloned()).unwrap_or_default();
            levels.insert(key, Value::String(effective.as_str().to_owned()));
            if let Err(error) = self.with_settings_manager_mut(|manager| manager.set(crate::settings_manager::SettingsScope::Global,
                &Map::from_iter([("modelThinkingLevels".to_owned(), Value::Object(levels))]))) {
                self.emit(AgentSessionEvent::ContinuationError { error_message: error });
            }
        }
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
    pub fn get_thinking_for_model_switch(
        &self,
        model: &Model,
        explicit_level: Option<ModelThinkingLevel>,
    ) -> ModelThinkingLevel {
        let remembered = self.with_settings_manager(|manager| manager.get_value("modelThinkingLevels")
            .and_then(|levels| levels.get(format!("{}/{}", model.provider, model.id))).and_then(Value::as_str)
            .and_then(|level| ModelThinkingLevel::ALL.into_iter().find(|candidate| candidate.as_str() == level)));
        let default = self.with_settings_manager(|manager| manager.get_string("defaultThinkingLevel"))
            .and_then(|level| ModelThinkingLevel::ALL.into_iter().find(|candidate| candidate.as_str() == level));
        let requested = explicit_level.or(remembered).or(default).unwrap_or(ModelThinkingLevel::Medium);
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

pub(crate) fn session_message_from_value(mut message: Value) -> Result<AgentMessage, serde_json::Error> {
    if let Some(timestamp) = message.get("timestamp").and_then(Value::as_str) {
        let millis = chrono::DateTime::parse_from_rfc3339(timestamp).map(|time| time.timestamp_millis()).unwrap_or(0);
        message["timestamp"] = Value::from(millis);
    }
    use maho_agent::types::CustomAgentMessage;
    let custom = match message.get("role").and_then(Value::as_str) {
        Some("compactionSummary") => Some(CustomAgentMessage::CompactionSummary(serde_json::from_value(message.clone())?)),
        Some("branchSummary") => Some(CustomAgentMessage::BranchSummary(serde_json::from_value(message.clone())?)),
        Some("bashExecution") => Some(CustomAgentMessage::BashExecution(serde_json::from_value(message.clone())?)),
        Some("custom") => Some(CustomAgentMessage::Custom(serde_json::from_value(message.clone())?)),
        _ => None,
    };
    match custom { Some(custom) => Ok(AgentMessage::Custom(custom)), None => serde_json::from_value(message) }
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

fn extension_thinking_level(level: ThinkingLevel) -> ModelThinkingLevel {
    match level { ThinkingLevel::Minimal => ModelThinkingLevel::Minimal, ThinkingLevel::Low => ModelThinkingLevel::Low,
        ThinkingLevel::Medium => ModelThinkingLevel::Medium, ThinkingLevel::High => ModelThinkingLevel::High,
        ThinkingLevel::Xhigh => ModelThinkingLevel::Xhigh, ThinkingLevel::Max => ModelThinkingLevel::Max }
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
    async fn extension_tool_execution_forwards_partial_updates() {
        use maho_ext_api::ExtensionSessionActions;
        let session = test_session();
        let tool = AgentTool {
            label: "updates".to_owned(), prepare_arguments: None, replay: None, execution_mode: None,
            tool: maho_ai::types::Tool { name: "updates".to_owned(), description: "updates".to_owned(),
                parameters: serde_json::json!({"type":"object","properties":{}}), freeform: None, constrained_sampling: None },
            execute: Arc::new(|_, _, _, update| Box::pin(async move {
                update.expect("update callback")(AgentToolResult::text("partial"));
                AgentToolResult::text("complete")
            })),
        };
        session.agent.set_tools(vec![tool]);
        let updates = Arc::new(Mutex::new(Vec::new()));
        let observed = updates.clone();
        let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
        let result = actions.execute_tool("updates", serde_json::json!({}), maho_ext_api::ExecuteToolOptions {
            on_update: Some(Arc::new(move |result| observed.lock().unwrap().push(result))), ..Default::default()
        }).await.unwrap();
        assert_eq!(result, AgentToolResult::text("complete"));
        assert_eq!(*updates.lock().unwrap(), vec![AgentToolResult::text("partial")]);
    }

    #[test]
    fn extension_settings_read_live_configured_values() {
        use maho_ext_api::ExtensionContextActions;
        let session = test_session();
        session.agent.set_model(test_model());
        let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
        assert_eq!(actions.get_prompt_cache_goal_backstop_max_seconds(), 270.0);
        session.with_settings_manager_mut(|manager| manager.set(crate::settings_manager::SettingsScope::Global,
            &serde_json::json!({
                "lookAt":{"enabled":false,"models":["faux/faux-1"]},
                "askUser":{"enabled":false,"timeoutMinutes":200},
                "images":{"autoResize":false,"blockImages":true},
                "compaction":{"reserveTokens":100,"keepRecentTokens":200,"modelOverrides":{"faux/faux-1":{"reserveTokens":300,"keepRecentTokens":400}}},
                "promptCache":{"goalBackstopMaxSeconds":99,"keepAlive":{"enabled":true,"maxRequestsPerSession":7,"maxCostUsdPerSession":0.3,"marginSeconds":11}}
            }).as_object().unwrap().clone())).unwrap();
        assert_eq!(actions.get_look_at_settings(), maho_ext_api::LookAtSettings { enabled: false, models: Some(vec!["faux/faux-1".to_owned()]) });
        assert_eq!(actions.get_ask_user_settings(), maho_ext_api::AskUserSettings { enabled: false, timeout_minutes: 120.0 });
        assert_eq!(actions.get_image_settings(), maho_ext_api::ImageSettings { auto_resize: false, block_images: true });
        assert_eq!(actions.get_compaction_settings(), maho_ext_api::CompactionSettings { enabled: true, reserve_tokens: 300, keep_recent_tokens: 400 });
        assert_eq!(actions.get_prompt_cache_goal_backstop_max_seconds(), 99.0);
        assert_eq!(actions.get_prompt_cache_keep_alive_settings(), maho_ext_api::PromptCacheKeepAliveSettings {
            enabled: true, max_requests_per_session: 7, max_cost_usd_per_session: 0.3, margin_seconds: 11.0,
        });
    }

    #[tokio::test]
    async fn an_unconfigured_provider_refuses_the_request() {
        let session = test_session();
        let error = session.get_required_request_auth(&test_model()).await.expect_err("refused");
        assert!(error.contains("No API key found for faux"), "{error}");
    }

    #[tokio::test]
    async fn extension_fallback_chain_updates_roundtrip_through_real_settings() {
        use maho_ext_api::ExtensionSessionSettings;
        let session = test_session();
        let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
        actions.set_fallback_chain("faux-1", &["faux/faux-2".into()]).await.unwrap();
        assert_eq!(actions.get_retry_fallback_settings().chains["faux-1"], ["faux/faux-2"]);
        actions.remove_fallback_chain("faux-1").await.unwrap();
        assert!(!actions.get_retry_fallback_settings().chains.contains_key("faux-1"));
    }

    #[tokio::test]
    async fn extension_model_selection_returns_false_without_configured_auth() {
        use maho_ext_api::ExtensionSessionActions;
        let session = test_session();
        let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
        assert!(!actions.set_model(test_model()).await.unwrap());
        assert!(!actions.set_session_model(test_model()).await.unwrap());
        let registry = ExtensionModelRegistryView(session.model_registry().clone());
        assert_eq!(maho_ext_api::ModelRegistry::get_all(&registry), session.model_registry().get_all());
        assert_eq!(maho_ext_api::ModelRegistry::find(&registry, "faux", "faux-1"), session.model_registry().find("faux", "faux-1"));
        assert!(maho_ext_api::ModelRegistry::get_api_key_for_provider(&registry, "faux").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn extension_exec_honors_cancel_and_preserves_process_results() {
        use maho_ext_api::ExtensionSessionActions;
        let session = test_session();
        let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
        let signal = maho_ext_api::AbortSignal::default();
        signal.abort();
        let killed = actions.exec("sh", &["-c".to_owned(), "exit 99".to_owned()], std::path::Path::new("/tmp"),
            maho_ext_api::ExecOptions { signal: Some(signal), ..Default::default() }).await.unwrap();
        assert!(killed.killed);
        let result = actions.exec("sh", &["-c".to_owned(), "printf out; printf err >&2; exit 3".to_owned()],
            std::path::Path::new("/tmp"), Default::default()).await.unwrap();
        assert_eq!((result.stdout.as_str(), result.stderr.as_str(), result.code, result.killed), ("out", "err", 3, false));
        let missing = actions.exec("/definitely/missing", &[], std::path::Path::new("/tmp"), Default::default()).await.unwrap();
        assert_eq!(missing.code, 1);
        assert!(!missing.stderr.is_empty());
    }

    #[tokio::test]
    async fn extension_command_new_session_reaches_real_session_manager() {
        use maho_ext_api::ExtensionCommandContextActions;
        let session = test_session();
        let original = session.session_id();
        let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
        actions.wait_for_idle().await.unwrap();
        let result = actions.new_session(maho_ext_api::NewSessionOptions::default()).await.unwrap();
        assert!(!result.cancelled);
        assert_ne!(session.session_id(), original);
        assert!(session.messages().is_empty());
    }

    struct ReplacementTestUi;
    impl maho_ext_api::ExtensionUi for ReplacementTestUi {
        fn select<'a>(&'a self, _: &'a str, _: &'a [String], _: maho_ext_api::ExtensionUiDialogOptions) -> maho_ext_api::UiFuture<'a, Option<String>> { Box::pin(async { None }) }
        fn confirm<'a>(&'a self, _: &'a str, _: &'a str, _: maho_ext_api::ExtensionUiDialogOptions) -> maho_ext_api::UiFuture<'a, bool> { Box::pin(async { false }) }
        fn input<'a>(&'a self, _: &'a str, _: Option<&'a str>, _: maho_ext_api::ExtensionUiDialogOptions) -> maho_ext_api::UiFuture<'a, Option<String>> { Box::pin(async { None }) }
        fn notify(&self, _: &str, _: maho_ext_api::NotificationType) {}
        fn set_status(&self, _: &str, _: Option<&str>) {}
        fn set_widget(&self, _: &str, _: Option<maho_ext_api::WidgetContent>, _: maho_ext_api::ExtensionWidgetOptions) {}
        fn set_header(&self, _: Option<maho_ext_api::ComponentFactory>) {}
        fn set_footer(&self, _: Option<maho_ext_api::ComponentFactory>) {}
        fn set_title(&self, _: &str) {}
        fn paste_to_editor(&self, _: &str) {}
        fn set_editor_text(&self, _: &str) {}
        fn get_editor_text(&self) -> String { String::new() }
        fn custom(&self, _: maho_ext_api::ComponentFactory, _: maho_ext_api::CustomUiOptions) -> maho_ext_api::ExtensionFuture<'_, Value> { Box::pin(async { Err("UI unavailable".into()) }) }
        fn theme(&self) -> maho_ext_api::Theme { Default::default() }
    }
    fn replacement_test_context(session: &AgentSession) -> maho_ext_api::ExtensionContext {
        maho_ext_api::ExtensionContext {
            ui: Arc::new(ReplacementTestUi), mode: ExtensionMode::Print, has_ui: false, cwd: session.cwd().into(), agent_dir: session.agent_dir().into(),
            session_manager: Arc::new(ExtensionSessionManagerView { session: Arc::downgrade(&session.inner), id: session.session_id(), file: None }),
            model_registry: Arc::new(ExtensionModelRegistryView(session.model_registry().clone())), model: None, thinking_level: None,
            service_tier: None, effective_service_tier: None, scoped_models: Vec::new(), goal_store_file: None,
            loaded_extension_paths: Vec::new(), signal: None, steering_signal: None, is_idle_fn: Arc::new(|| true),
            wait_for_idle_fn: Arc::new(|| Box::pin(async {})), is_project_trusted_fn: Arc::new(|| true), is_compacting_fn: Arc::new(|| false),
            get_system_prompt_fn: Arc::new(String::new), get_system_prompt_options_fn: Arc::new(Default::default), registered_mcp_servers: Vec::new(), update_tool_hook_status: None,
        }
    }

    #[tokio::test]
    async fn replacement_reloads_factories_invalidates_old_handles_and_removes_old_tools() {
        let session = test_session();
        session.register_tool_definition(test_definition("base"), empty_source_info(), test_tool("base"));
        let runtimes = Arc::new(Mutex::new(Vec::new()));
        let captured = runtimes.clone();
        let factory = maho_ext_host::loader::NativeAsyncExtensionFactory {
            path: "replacement".into(), source_info: empty_source_info(),
            factory: Arc::new(move |api| {
                let first = lock(&captured).is_empty();
                lock(&captured).push(api.runtime.clone());
                if first { api.register_tool(test_definition("old-only")); }
                api.register_command("generation", Some(lock(&captured).len().to_string()), None, Arc::new(|_, _| Box::pin(async { Ok(()) })));
                Box::pin(async { Ok(()) })
            }),
        };
        let runner = ExtensionRunner::from_async_factories(vec![factory], replacement_test_context(&session), Default::default()).await.unwrap();
        session.set_extension_runner(runner).await;
        let old = session.extension_runner.lock().await.as_ref().unwrap().create_context().unwrap();
        assert!(session.get_registered_tool("old-only").is_some());
        assert!(session.new_session(None).await.unwrap());
        assert!(lock(&runtimes)[0].assert_active().is_err());
        assert!(old.actions().is_err());
        assert!(session.get_registered_tool("old-only").is_none());
        assert!(session.get_registered_tool("base").is_some());
        let current = session.extension_runner.lock().await.as_ref().unwrap().create_context().unwrap();
        assert_eq!(current.session_manager.session_id(), session.session_id());
        current.ui.notify("new runtime", maho_ext_api::NotificationType::Info);
        assert!(session.reload().await.unwrap());
        assert!(current.actions().is_err());
        assert_eq!(lock(&runtimes).len(), 3);
        assert_eq!(session.get_commands().iter().find(|command| command.name == "generation").unwrap().description.as_deref(), Some("3"));
    }

    #[test]
    fn extension_next_turn_message_is_retained_until_next_admission() {
        use maho_ext_api::ExtensionActions;
        let session = test_session();
        session.state().extension_mode = ExtensionMode::Rpc;
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = events.clone();
        let _subscription = session.subscribe(Arc::new(move |event| lock(&captured).push(event.clone())));
        let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
        actions.send_message(maho_ext_api::CustomMessage {
            custom_type: "notice".into(), content: vec![maho_ext_api::ToolContent::text("later")], display: true, details: None,
        }, maho_ext_api::SendMessageOptions { trigger_turn: false, deliver_as: Some(maho_ext_api::DeliverAs::NextTurn) }).unwrap();
        assert!(session.messages().is_empty());
        assert_eq!(session.state().pending_next_turn_messages.len(), 1);
        assert_eq!(session.pending_message_count(), 0);
        session.flush_pending_next_turn_messages().unwrap();
        assert_eq!(session.messages()[0].role(), "custom");
        assert_eq!(session.pending_message_count(), 0);
        assert_eq!(session.with_session_manager(|manager| manager.entries())[0]["type"], "custom_message");
        assert!(matches!(&lock(&events)[0], AgentSessionEvent::EntryAppended { .. }));
    }

    #[tokio::test]
    async fn extension_user_message_text_blocks_are_separated_by_newlines() {
        use maho_ext_api::ExtensionActions;
        let session = test_session();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let _subscription = session.subscribe(Arc::new(move |event| {
            if let AgentSessionEvent::Agent(maho_agent::types::AgentEvent::MessageStart { message }) = event
                && message.role() == "user" { let _ = sender.send(user_message_text(message)); }
        }));
        SessionExtensionActions(Arc::downgrade(&session.inner)).send_user_message(
            maho_ext_api::UserMessageContent::Blocks(vec![maho_ext_api::ToolContent::text("first"), maho_ext_api::ToolContent::text("second")]),
            Default::default(),
        ).unwrap();
        let text = tokio::time::timeout(std::time::Duration::from_secs(5), receiver.recv()).await.unwrap().unwrap();
        assert_eq!(text, "first\nsecond");
    }

    #[test]
    fn extension_append_entry_publishes_saved_entry_without_changing_messages() {
        use maho_ext_api::ExtensionActions;
        let session = test_session();
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = events.clone();
        let _subscription = session.subscribe(Arc::new(move |event| lock(&captured).push(event.clone())));
        SessionExtensionActions(Arc::downgrade(&session.inner)).append_entry("state", Some(serde_json::json!({"value":1}))).unwrap();
        assert!(session.messages().is_empty());
        let events = lock(&events);
        let AgentSessionEvent::EntryAppended { entry } = &events[0] else { panic!("saved entry event") };
        assert_eq!(entry.kind, "custom");
        assert_eq!(entry.data["data"]["value"], 1);
    }

    #[test]
    fn session_name_change_queues_extension_metadata_event() {
        let session = test_session();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        session.state().extension_event_sender = Some(sender);
        session.set_session_name("renamed");
        assert!(matches!(receiver.try_recv().unwrap(), maho_ext_api::ExtensionEvent::SessionInfoChanged { name: Some(name) } if name == "renamed"));
    }

    #[test]
    fn extension_abort_defaults_to_user_without_marking_system_cancellation_as_user() {
        use maho_ext_api::ExtensionContextActions;
        let session = test_session();
        let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
        actions.abort(Some(maho_ext_api::AbortSource::System));
        assert!(!session.state().user_aborted);
        actions.abort(None);
        assert!(session.state().user_aborted);
    }

    #[tokio::test]
    async fn event_context_signal_observes_cancellation_only_during_invocation() {
        use maho_ext_api::ExtensionContextActions;
        let session = test_session();
        let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
        let controller = maho_ai::utils::abort::AbortController::new();
        let (subscription, signal) = ExtensionEventSignalSubscription::new(controller.signal());
        EXTENSION_EVENT_SIGNAL.scope(signal, async {
            let captured = actions.get_signal().unwrap();
            assert!(!captured.is_aborted());
            controller.abort(None);
            assert!(captured.is_aborted());
        }).await;
        drop(subscription);
        assert!(actions.get_signal().is_none());
    }

    #[tokio::test]
    async fn compaction_preparation_details_are_scoped_to_the_hook() {
        use maho_ext_api::ExtensionContextActions;
        let session = test_session();
        let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
        let details = maho_ext_api::CompactionPreparationDetails {
            preparation: maho_ext_api::CompactionPreparation {
                settings: actions.get_compaction_settings(), messages_to_summarize: Vec::new(), turn_prefix_messages: Vec::new(),
                tokens_before: 1, first_kept_entry_id: "kept".into(), previous_summary: None,
            },
            source_messages: Some(vec![make_user_message("prefix", None)]),
            turn_prefix_source_messages: Some(vec![make_user_message("turn prefix", None)]),
            is_split_turn: true,
            file_ops: maho_ext_api::CompactionFileOperations { read: vec!["file".into()], ..Default::default() },
        };
        EXTENSION_COMPACTION_PREPARATION.scope(details.clone(), async {
            assert_eq!(actions.get_compaction_preparation(), Some(details));
        }).await;
        assert!(actions.get_compaction_preparation().is_none());
    }

    #[tokio::test]
    async fn extension_compaction_rejects_stale_warm_anchor_before_writing() {
        use maho_ext_api::ExtensionContextActions;
        let session = test_session();
        let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
        let result = actions.apply_compaction(maho_ext_api::CompactionResult {
            summary: "summary".into(), first_kept_entry_id: "missing".into(), tokens_before: 100, details: None,
        }, maho_ext_api::ApplyCompactionOptions {
            reason: maho_ext_api::CompactionReason::Extension, expected_revision: None, signal: None,
            expected_warm_anchor: Some(maho_ext_api::WarmAnchorSnapshot {
                first_kept_entry_id: "missing".into(), prefix_entry_ids: vec!["old".into()], latest_compaction_entry_id: None,
            }),
        }).await.unwrap();
        assert_eq!(result, maho_ext_api::ApplyCompactionResult::Stale);
        assert!(session.with_session_manager(|manager| manager.entries()).is_empty());
    }

    #[tokio::test]
    async fn extension_compaction_signal_observes_real_session_cancellation() {
        use maho_ext_api::ExtensionContextActions;
        let session = test_session();
        let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
        let signal = actions.begin_compaction(maho_ext_api::BeginCompactionOptions { reason: maho_ext_api::CompactionReason::Extension }).unwrap();
        session.abort_compaction();
        tokio::time::timeout(std::time::Duration::from_secs(1), signal.cancelled()).await.unwrap();
        assert!(signal.is_aborted());
        actions.end_compaction(maho_ext_api::EndCompactionOptions {
            reason: maho_ext_api::CompactionReason::Extension, signal: Some(signal), aborted: Some(true), error_message: None,
        });
        assert!(!session.is_compacting());
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
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        session.state().extension_event_sender = Some(sender);
        let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&seen);
        let _subscription = session.subscribe(Arc::new(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        }));
        session.dispose().await;
        session.emit(AgentSessionEvent::AgentIdle);
        assert_eq!(seen.load(Ordering::SeqCst), 0);
        assert!(receiver.recv().await.is_none());
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
    fn late_extension_tool_registration_installs_into_live_agent() {
        use maho_ext_api::ExtensionSessionActions;
        let session = test_session();
        let runtime = maho_ext_api::ExtensionRuntime::default();
        let factory: maho_ext_host::wrapper::ToolContextFactory = Arc::new(|| Err(maho_ext_api::ExtensionFailure::new("unused context")));
        session.state().extension_tool_context = Some((runtime, factory));
        SessionExtensionActions(Arc::downgrade(&session.inner)).install_registered_tool(maho_ext_api::RegisteredTool {
            definition: test_definition("late"), source_info: empty_source_info(),
        }).unwrap();
        assert!(session.get_registered_tool("late").is_some());
        assert!(session.get_active_tool_names().contains(&"late".to_owned()));
    }

    #[test]
    fn unknown_names_are_ignored_when_setting_active_tools() {
        let session = test_session();
        session.register_tool_definition(test_definition("read"), empty_source_info(), test_tool("read"));
        session.set_active_tools_by_name(vec!["read".to_owned(), "nope".to_owned()]);
        assert_eq!(session.get_active_tool_names(), vec!["read".to_owned()]);
    }

    #[test]
    fn all_tools_includes_inactive_definitions_and_preserves_labels() {
        use maho_ext_api::ExtensionActions;
        let session = test_session();
        let mut definition = test_definition("read");
        definition.label = "Read files".into();
        definition.search_keywords = Some(vec!["files".into()]);
        session.register_tool_definition(definition, empty_source_info(), test_tool("read"));
        assert!(session.get_active_tool_names().is_empty());
        let tools = SessionExtensionActions(Arc::downgrade(&session.inner)).get_all_tools().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].label, "Read files");
        assert_eq!(tools[0].search_keywords, ["files"]);
    }

    #[test]
    fn late_search_tool_registration_does_not_expose_it_as_direct() {
        use maho_ext_api::ExtensionSessionActions;
        let session = test_session();
        session.state().extension_tool_context = Some((maho_ext_api::ExtensionRuntime::default(), Arc::new(|| Err("unused context".into()))));
        let mut definition = test_definition("lookup");
        definition.exposure = Some(ToolExposure::Search);
        SessionExtensionActions(Arc::downgrade(&session.inner)).install_registered_tool(maho_ext_api::RegisteredTool {
            definition, source_info: empty_source_info(),
        }).unwrap();
        assert!(session.get_active_tool_names().is_empty());
        assert_eq!(session.get_all_tools()[0].exposure, ToolExposure::Search);
        assert!(session.get_registered_tool("lookup").is_some());
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
    async fn nontriggering_custom_messages_flush_at_turn_end() {
        let session = test_session();
        session.state().pending_custom_messages.push(maho_agent::harness::messages::CustomMessage {
            role: "custom".into(), custom_type: "aside".into(),
            content: maho_agent::harness::messages::CustomMessageContent::Text("after turn".into()),
            display: true, details: None, timestamp: 0,
        });
        session.process_agent_event(maho_agent::types::AgentEvent::TurnEnd {
            message: make_user_message("turn", None), tool_results: Vec::new(),
        }, maho_ai::utils::abort::AbortController::new().signal()).await;
        assert!(session.state().pending_custom_messages.is_empty());
        assert_eq!(session.messages()[0].role(), "custom");
        assert_eq!(session.with_session_manager(|manager| manager.entries()).len(), 1);
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

    #[tokio::test]
    async fn model_switch_persists_only_after_admission() {
        let session = test_session_with_stream_function(false);
        let mut model = test_model();
        model.id = "second".to_owned();
        session.set_session_model(model).await.expect("switch");
        assert_eq!(session.model().id, "second");
        assert_eq!(session.with_session_manager(|manager| manager.entries()[0]["type"].clone()), "model_change");
        assert!(session.with_settings_manager(|manager| manager.get_string("defaultModel")).is_none());
    }

    #[tokio::test]
    async fn impossible_model_switch_leaves_active_model_and_history_unchanged() {
        let session = test_session_with_stream_function(false);
        session.agent.set_model(test_model());
        let mut model = test_model();
        model.id = "tiny".to_owned();
        model.context_window = 4_096;
        assert!(session.set_model(model).await.is_err());
        assert_eq!(session.model().id, "faux-1");
        assert!(session.with_session_manager(|manager| manager.entries()).is_empty());
    }

    #[tokio::test]
    async fn oversized_transcript_holds_repairable_switch_without_durable_change() {
        let session = test_session_with_stream_function(false);
        session.agent.set_model(test_model());
        session.agent.set_messages(vec![make_user_message(&"x".repeat(200_000), None)]);
        let mut model = test_model();
        model.id = "smaller".to_owned();
        model.context_window = 64_000;
        session.set_model(model).await.expect("held switch");
        assert_eq!(session.model().id, "faux-1");
        assert_eq!(session.pending_model_switch().expect("pending").model.id, "smaller");
        assert!(session.with_session_manager(|manager| manager.entries()).is_empty());
    }

    #[test]
    fn compaction_application_rebuilds_only_summary_and_retained_suffix() {
        let session = test_session();
        session.with_session_manager_mut(|manager| manager.append_message(serde_json::json!({"role":"user","content":"old","timestamp":0})));
        let retained = session.with_session_manager_mut(|manager| manager.append_message(serde_json::json!({"role":"user","content":"recent","timestamp":1})));
        session.apply_compaction(&crate::compaction::compaction::CompactionResult {
            summary: "summary".to_owned(), first_kept_entry_id: retained["id"].as_str().expect("id").to_owned(),
            tokens_before: 100, estimated_tokens_after: None, usage: None, details: None,
        }).expect("apply");
        assert_eq!(session.messages().len(), 2);
        assert_eq!(user_message_text(&session.messages()[1]), "recent");
        assert_eq!(session.with_session_manager(|manager| manager.entries().last().expect("entry")["type"].clone()), "compaction");
    }

    #[test]
    fn invalid_compaction_retention_does_not_mutate_history() {
        let session = test_session();
        assert!(session.apply_compaction(&crate::compaction::compaction::CompactionResult {
            summary: "summary".to_owned(), first_kept_entry_id: "missing".to_owned(), tokens_before: 100,
            estimated_tokens_after: None, usage: None, details: None,
        }).is_err());
        assert!(session.with_session_manager(|manager| manager.entries()).is_empty());
    }

    #[tokio::test]
    async fn editing_user_message_preserves_old_branch_and_rejects_stale_leaf() {
        let session = test_session();
        let entry = session.with_session_manager_mut(|manager| manager.append_message(serde_json::json!({"role":"user","content":"old","timestamp":0})));
        let id = entry["id"].as_str().expect("id");
        assert!(session.edit_user_message(id, "changed", TreeNavigationOptions {
            expected_leaf_id: Some("stale".to_owned()), ..Default::default()
        }).await.is_err());
        let result = session.edit_user_message(id, "changed", Default::default()).await.expect("edit");
        assert!(result.entry_id.is_some());
        assert_eq!(session.with_session_manager(|manager| manager.entries().len()), 2);
        assert_eq!(user_message_text(&session.messages()[0]), "changed");
        session.navigate_tree(id, TreeNavigationOptions { intent: Some(TreeNavigationIntent::Resume), ..Default::default() }).await.expect("resume");
        assert_eq!(user_message_text(&session.messages()[0]), "old");
    }

    #[tokio::test]
    async fn new_session_clears_context_and_changes_identity() {
        let session = test_session();
        let id = session.session_id();
        session.agent.set_messages(vec![make_user_message("old", None)]);
        assert!(session.new_session(None).await.expect("new"));
        assert_ne!(session.session_id(), id);
        assert!(session.messages().is_empty());
    }

    #[test]
    fn prompt_expansion_uses_template_arguments_and_can_be_disabled() {
        let session = test_session();
        session.set_prompt_resources(vec![crate::prompt_templates::PromptTemplate {
            name: "review".to_owned(), description: String::new(), argument_hint: None, content: "review $1".to_owned(),
            source_info: crate::source_info::create_synthetic_source_info("/tmp/review.md", crate::source_info::SyntheticSourceInfoOptions::default()),
            file_path: "/tmp/review.md".to_owned(),
        }], Vec::new());
        assert_eq!(session.expand_input("/review file", true).expect("expand"), "review file");
        assert_eq!(session.expand_input("/review file", false).expect("raw"), "/review file");
        session.state().extension_commands.push(maho_ext_api::SlashCommandInfo {
            name: "extension".into(), description: Some("extension command".into()), argument_hint: Some("target".into()), source_info: None,
        });
        session.state().skills.push(crate::skills::Skill {
            name: "guide".into(), description: "guide skill".into(), file_path: "/tmp/guide/SKILL.md".into(), base_dir: "/tmp/guide".into(),
            source_info: crate::source_info::create_synthetic_source_info("/tmp/guide/SKILL.md", crate::source_info::SyntheticSourceInfoOptions::default()),
            disable_model_invocation: true,
        });
        let commands = session.get_commands();
        assert_eq!(commands.iter().map(|command| command.name.as_str()).collect::<Vec<_>>(), ["extension", "review", "skill:guide"]);
        assert_eq!(commands[0].argument_hint.as_deref(), Some("target"));
        assert_eq!(commands[1].source_info.as_ref().unwrap().path, "/tmp/review.md");
        assert_eq!(commands[2].source_info.as_ref().unwrap().path, "/tmp/guide/SKILL.md");
        let live = Arc::new(std::sync::Mutex::new(Vec::new()));
        let catalog = live.clone();
        session.state().extension_command_catalog = Some(Arc::new(move || catalog.lock().unwrap().clone()));
        live.lock().unwrap().push(maho_ext_api::SlashCommandInfo { name: "late".into(), description: None, argument_hint: None, source_info: None });
        assert_eq!(session.get_commands().iter().map(|command| command.name.as_str()).collect::<Vec<_>>(), ["late", "review", "skill:guide"]);
    }

    #[tokio::test]
    async fn local_bash_records_native_custom_context_and_history() {
        let session = test_session();
        let result = session.execute_bash("printf native", None, true, None, None).await.expect("bash");
        assert_eq!(result.output, "native");
        assert_eq!(result.exit_code, Some(0));
        assert!(!session.is_bash_running());
        assert!(!session.has_pending_bash_messages());
        assert_eq!(session.messages()[0].role(), "bashExecution");
        let entry = session.with_session_manager(|manager| manager.entries()[0].clone());
        assert_eq!(entry["message"]["command"], "printf native");
        assert_eq!(entry["message"]["excludeFromContext"], true);
    }
}
