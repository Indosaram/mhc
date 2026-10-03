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
#[path = "monitor_invocation.rs"]
mod monitor_invocation;

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
    pub prompt_admitted: Option<Arc<dyn Fn(PromptDisposition) + Send + Sync>>,
    pub session_title_prompt: Option<SessionTitlePrompt>,
}

#[derive(Clone)]
pub enum SessionTitlePrompt {
    Text(String),
    Disabled,
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
    custom_system_prompt_source: Option<String>,
    append_system_prompt_sources: Vec<String>,
    context_files_enabled: bool,
    system_prompt_override: Option<String>,
    wake_sources: WakeSourceTracker,
    shown_high_reasoning_warning_keys: BTreeSet<String>,
    extension_mode: ExtensionMode,
    extension_ui_context: Option<Arc<dyn ExtensionUi>>,
    extension_abort_handler: Option<Arc<dyn Fn() + Send + Sync>>,
    extension_error_listener: Option<ExtensionErrorListener>,
    message_revision: u64,
    assistant_generation: u64,
    last_persisted_assistant: Option<(String, u64)>,
    post_compaction_assistant_generation: Option<u64>,
    post_compaction_usage_exempt_entries: BTreeSet<String>,
    messages_awaiting_persistence: Vec<(uuid::Uuid, AgentMessage)>,
    steering_messages: Vec<String>,
    follow_up_messages: Vec<String>,
    queued_input_order: Vec<QueuedInput>,
    next_queued_input_order: u64,
    post_compaction_deferred_steering_messages: Vec<AgentMessage>,
    post_compaction_deferred_follow_up_messages: Vec<AgentMessage>,
    prompt_start_pending: bool,
    had_cleared_queued_messages: bool,
    auto_compaction_session_override: Option<bool>,
    turn_index: u64,
    message_replacements: Vec<(AgentMessage, AgentMessage)>,
    retry_attempt: u32,
    retry_abort_controller: Option<maho_ai::utils::abort::AbortController>,
    user_aborted: bool,
    abort_source: Option<maho_ext_api::AbortSource>,
    abort_provenance: crate::agent_abort_provenance::AgentAbortProvenance,
    probe_phase: crate::retry_fallback::hint_policy::ProbePhase,
    hint_deadline_ms: Option<f64>,
    cumulative_hinted_wait_ms: f64,
    pending_model_switch: Option<PendingModelSwitch>,
    compaction_abort_controller: Option<crate::compaction::lifecycle::CompactionAbortController>,
    pending_compaction_admission: Option<Arc<PendingCompactionAdmission>>,
    compaction_lifecycle: crate::compaction::lifecycle::CompactionLifecycleCoordinator,
    delegated_compaction_key: Option<(String, String)>,
    prompt_templates: Vec<crate::prompt_templates::PromptTemplate>,
    extension_commands: Vec<maho_ext_api::SlashCommandInfo>,
    extension_command_catalog: Option<Arc<dyn Fn() -> Vec<maho_ext_api::SlashCommandInfo> + Send + Sync>>,
    extension_event_sender: Option<tokio::sync::mpsc::UnboundedSender<maho_ext_api::ExtensionEvent>>,
    extension_tool_context: Option<(maho_ext_api::ExtensionRuntime, maho_ext_host::wrapper::ToolContextFactory)>,
    extension_tool_backups: BTreeMap<String, Option<(ToolDefinitionEntry, AgentTool)>>,
    extension_lazy_activators: Vec<LazyToolActivator>,
    extension_hint_backups: BTreeMap<String, Option<String>>,
    skills: Vec<crate::skills::Skill>,
    discovered_resources: maho_ext_api::DiscoveredResources,
    global_hook_source_paths: Vec<std::path::PathBuf>,
    project_hook_source_paths: Vec<std::path::PathBuf>,
    pre_session_hook_source_paths: Vec<std::path::PathBuf>,
    loaded_hook_sources: Option<maho_ext_api::LoadedHookSources>,
    bash_abort_signals: BTreeMap<String, maho_ext_api::AbortSignal>,
    pending_bash_messages: Vec<maho_agent::harness::messages::BashExecutionMessage>,
    pending_next_turn_messages: Vec<AgentMessage>,
    pending_custom_messages: Vec<maho_agent::harness::messages::CustomMessage>,
    extension_event_signal: Option<maho_ext_api::AbortSignal>,
    compaction_extension_signal: Option<maho_ext_api::AbortSignal>,
    branch_summary_abort_controller: Option<maho_ai::utils::abort::AbortController>,
    session_title_abort_controller: Option<maho_ai::utils::abort::AbortController>,
    disposed: bool,
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
    settings_manager: Arc<Mutex<SettingsManager>>,
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
    binding_readiness: Mutex<Option<BindingPromptReadiness>>,
    retry_fallback: tokio::sync::Mutex<Option<crate::retry_fallback::controller::RetryFallbackController<SessionFallbackDeps>>>,
    work_barrier: Arc<crate::session_work_barrier::SessionWorkBarrier>,
    wake_source_subscription: Mutex<Option<maho_ext_api::BusSubscription>>,
    settings_source_subscription: Mutex<Option<crate::settings_manager::SettingsSourceSubscription>>,
    settled_delivery: Mutex<crate::agent_settled_delivery::AgentSettledDelivery>,
    user_abort_generation: AtomicU64,
    monitor_generation: AtomicU64,
    settlement_epoch: AtomicU64,
    probe_scheduler: Mutex<crate::retry_fallback::probe_scheduler::ProbeBackScheduler>,
    probe_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

struct SessionFallbackDeps(std::sync::Weak<AgentSessionInner>);

struct SessionExtensionActions(std::sync::Weak<AgentSessionInner>);

struct ExtensionSessionManagerView {
    session: std::sync::Weak<AgentSessionInner>,
    id: String,
    file: Option<std::path::PathBuf>,
}
pub(crate) struct ExtensionModelRegistryView {
    registry: crate::model_registry::ModelRegistry,
    agent_dir: String,
    events: maho_ext_api::EventBus,
    session: std::sync::Weak<AgentSessionInner>,
    generation: u64,
}
impl ExtensionModelRegistryView {
    pub(crate) fn new(session: &AgentSession, events: maho_ext_api::EventBus) -> Self {
        Self { registry: session.model_registry().clone(), agent_dir: session.agent_dir().to_owned(), events,
            session: Arc::downgrade(&session.inner), generation: session.monitor_generation.load(Ordering::SeqCst) }
    }
    fn assert_account_active(&self) -> Result<(), maho_ext_api::ExtensionFailure> {
        let inner = self.session.upgrade().ok_or_else(|| maho_ext_api::ExtensionFailure::new("Agent session has been disposed"))?;
        if lock(&inner.state).disposed || inner.monitor_generation.load(Ordering::SeqCst) != self.generation {
            return Err(maho_ext_api::ExtensionFailure::new("Account registry context is stale"));
        }
        Ok(())
    }
    fn accounts_changed(&self, provider: &str) {
        self.events.emit("provider-accounts-changed", &serde_json::json!({"type":"accounts_changed","provider":provider}));
    }
}
impl maho_ext_api::ModelRegistry for ExtensionModelRegistryView {
    fn get_all(&self) -> Vec<Model> { self.registry.get_all() }
    fn get_available(&self) -> Vec<Model> { self.registry.get_available() }
    fn find(&self, provider: &str, id: &str) -> Option<Model> { self.registry.find(provider, id) }
    fn has_configured_auth(&self, model: &Model) -> bool { self.registry.has_configured_auth(model) }
    fn get_api_key_for_provider<'a>(&'a self, provider: &'a str) -> maho_ext_api::ExtensionFuture<'a, Option<String>> {
        Box::pin(async move { Ok(self.registry.get_api_key_for_provider(provider).await) })
    }
    fn get_provider_auth<'a>(&'a self, provider: &'a str) -> maho_ext_api::ExtensionFuture<'a, Option<maho_ai::models::AuthResolution>> {
        Box::pin(async move { self.registry.model_runtime.get_auth(provider).await.map_err(|error| maho_ext_api::ExtensionFailure::new(error.message)) })
    }
    fn get_stored_credential_type(&self, provider: &str) -> Result<Option<maho_ai::auth::types::CredentialType>, maho_ext_api::ExtensionFailure> {
        Ok(self.registry.auth_storage.get(provider).as_ref().and_then(crate::auth_storage::credential_kind).map(|kind| match kind {
            crate::auth_storage::CredentialKind::ApiKey => maho_ai::auth::types::CredentialType::ApiKey,
            crate::auth_storage::CredentialKind::Oauth => maho_ai::auth::types::CredentialType::OAuth,
        }))
    }
    fn stream_simple(&self, model: &Model, context: &maho_ai::types::Context, options: Option<maho_ai::types::SimpleStreamOptions>) -> Result<maho_ai::utils::event_stream::AssistantMessageEventStream, maho_ext_api::ExtensionFailure> {
        Ok(self.registry.stream_simple(model, context, options))
    }
    fn get_api_key_and_headers<'a>(&'a self, model: &'a Model) -> maho_ext_api::ExtensionFuture<'a, maho_ext_api::ResolvedRequestAuth> {
        Box::pin(async move {
            match self.registry.get_api_key_and_headers(model).await {
                crate::model_registry::ResolvedRequestAuth::Resolved { auth, compatibility, env } => Ok(maho_ext_api::ResolvedRequestAuth {
                    auth, extra_body: compatibility.extra_body, upstream_model_id: compatibility.upstream_model_id,
                    service_tier: compatibility.service_tier, env,
                }),
                crate::model_registry::ResolvedRequestAuth::Failed { error } => Err(maho_ext_api::ExtensionFailure::new(error)),
            }
        })
    }
    fn get_credential_accounts<'a>(&'a self, provider: &'a str) -> maho_ext_api::ExtensionFuture<'a, Vec<maho_ext_api::CredentialAccountSummary>> {
        Box::pin(async move {
            self.assert_account_active()?;
            let accounts = self.registry.get_credential_accounts(provider, &self.agent_dir).await.map_err(maho_ext_api::ExtensionFailure::new)?;
            self.assert_account_active()?;
            Ok(accounts)
        })
    }
    fn pin_credential_account<'a>(&'a self, provider: &'a str, name: Option<&'a str>) -> maho_ext_api::ExtensionFuture<'a, ()> {
        Box::pin(async move {
            self.assert_account_active()?;
            self.registry.pin_credential_account_guarded(provider, name, &self.agent_dir,
                &|| self.assert_account_active().map_err(|error| error.message)).await.map_err(maho_ext_api::ExtensionFailure::new)?;
            self.assert_account_active()?;
            self.accounts_changed(provider); Ok(())
        })
    }
    fn remove_credential_account<'a>(&'a self, provider: &'a str, name: &'a str) -> maho_ext_api::ExtensionFuture<'a, ()> {
        Box::pin(async move {
            self.assert_account_active()?;
            self.registry.remove_credential_account_guarded(provider, name, &self.agent_dir,
                &|| self.assert_account_active().map_err(|error| error.message)).await.map_err(maho_ext_api::ExtensionFailure::new)?;
            self.assert_account_active()?;
            self.accounts_changed(provider); Ok(())
        })
    }
    fn rename_credential_account<'a>(&'a self, provider: &'a str, name: &'a str, display_name: Option<&'a str>) -> maho_ext_api::ExtensionFuture<'a, ()> {
        Box::pin(async move {
            self.assert_account_active()?;
            self.registry.rename_credential_account_guarded(provider, name, display_name,
                &|| self.assert_account_active().map_err(|error| error.message)).await.map_err(maho_ext_api::ExtensionFailure::new)?;
            self.assert_account_active()?;
            self.accounts_changed(provider); Ok(())
        })
    }
}
impl maho_ext_api::ToolSessionManager for ExtensionSessionManagerView {
    fn session_id(&self) -> &str { &self.id }
    fn session_file(&self) -> Option<&std::path::Path> { self.file.as_deref() }
}
impl maho_ext_api::SessionManager for ExtensionSessionManagerView {
    fn get_session_dir(&self) -> Option<std::path::PathBuf> {
        self.session.upgrade().map(|inner| AgentSession { inner }.with_session_manager(|manager| manager.session_dir().into()))
    }
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

struct AbortSignalBridge(tokio::task::JoinHandle<()>);

type BindingPromptReadiness = Arc<Mutex<Vec<tokio::sync::oneshot::Receiver<()>>>>;

struct ExtensionBindingReadiness<'a> {
    session: &'a AgentSession,
    pending: BindingPromptReadiness,
}

impl Drop for ExtensionBindingReadiness<'_> {
    fn drop(&mut self) {
        let mut active = lock(&self.session.binding_readiness);
        if active.as_ref().is_some_and(|pending| Arc::ptr_eq(pending, &self.pending)) {
            *active = None;
        }
    }
}

impl Drop for AbortSignalBridge {
    fn drop(&mut self) { self.0.abort(); }
}

struct PendingCompactionAdmission {
    controller: crate::compaction::lifecycle::CompactionAbortController,
    completed: std::sync::atomic::AtomicBool,
}

struct PromptStartGuard<'a>(&'a AgentSession);

struct MessagePersistenceGuard<'a> {
    session: &'a AgentSession,
    id: uuid::Uuid,
}

impl Drop for MessagePersistenceGuard<'_> {
    fn drop(&mut self) {
        self.session.state().messages_awaiting_persistence.retain(|(id, _)| *id != self.id);
    }
}

impl Drop for PromptStartGuard<'_> {
    fn drop(&mut self) { self.0.state().prompt_start_pending = false; }
}

struct PendingCompactionAdmissionGuard<'a> {
    session: &'a AgentSession,
    controller: crate::compaction::lifecycle::CompactionAbortController,
}

impl Drop for PendingCompactionAdmissionGuard<'_> {
    fn drop(&mut self) {
        let mut state = self.session.state();
        if state.pending_compaction_admission.as_ref().is_some_and(|current| current.controller.same(&self.controller)) {
            self.controller.abort();
            state.pending_compaction_admission = None;
        }
    }
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
    fn set_approved_monitor_parent(&self, id: &str, input: &Value, parent: &std::path::Path) -> Result<(), maho_ext_api::ExtensionFailure> {
        let session = self.session()?;
        monitor_invocation::CURRENT.try_with(|invocation| {
            if invocation.session_identity != Arc::as_ptr(&session.inner) as usize
                || invocation.generation != session.monitor_generation.load(Ordering::SeqCst) || session.state().disposed {
                return Err("Monitor invocation belongs to another session".to_owned());
            }
            invocation.attach(id, input, std::path::Path::new(&session.cwd()), parent.to_path_buf())
        }).map_err(|_| maho_ext_api::ExtensionFailure::new("No active monitor invocation"))?
            .map_err(maho_ext_api::ExtensionFailure::new)
    }
    fn take_approved_monitor_parent(&self, id: &str, input: &Value) -> Result<Option<std::path::PathBuf>, maho_ext_api::ExtensionFailure> {
        let session = self.session()?;
        monitor_invocation::CURRENT.try_with(|invocation| {
            if invocation.session_identity != Arc::as_ptr(&session.inner) as usize
                || invocation.generation != session.monitor_generation.load(Ordering::SeqCst) || session.state().disposed {
                return Err("Monitor invocation belongs to another session".to_owned());
            }
            invocation.take(id, input, std::path::Path::new(&session.cwd()))
        }).map_err(|_| maho_ext_api::ExtensionFailure::new("No active monitor invocation"))?
            .map_err(maho_ext_api::ExtensionFailure::new)
    }
    fn get_model(&self) -> Option<Model> { self.session().ok().map(|session| session.model()) }
    fn get_service_tier(&self) -> Option<ServiceTier> { self.session().ok().and_then(|session| session.service_tier()) }
    fn get_effective_service_tier(&self) -> Option<ServiceTier> { self.session().ok().and_then(|session| session.effective_service_tier()) }
    fn get_scoped_models(&self) -> Vec<maho_ext_api::ScopedModel> { self.session().map_or_else(|_| Vec::new(), |session| session.scoped_models().into_iter().map(|model|
        maho_ext_api::ScopedModel { model: model.model, thinking_level: model.thinking_level, service_tier: model.service_tier }).collect()) }
    fn get_agent_dir(&self) -> std::path::PathBuf { self.session().map_or_else(|_| Default::default(), |session| session.agent_dir().into()) }
    fn is_idle(&self) -> bool { self.session().is_ok_and(|session| !session.is_streaming() && !session.work_barrier.has_active_work()) }
    fn is_project_trusted(&self) -> bool { self.session().is_ok_and(|session| session.with_settings_manager(|manager| manager.is_project_trusted())) }
    fn get_signal(&self) -> Option<maho_ext_api::AbortSignal> { EXTENSION_EVENT_SIGNAL.try_with(Clone::clone).ok().or_else(|| self.session().ok()?.state().extension_event_signal.clone()) }
    fn get_compaction_preparation(&self) -> Option<maho_ext_api::CompactionPreparationDetails> { EXTENSION_COMPACTION_PREPARATION.try_with(Clone::clone).ok() }
    fn abort(&self, source: Option<maho_ext_api::AbortSource>) { if let Ok(session) = self.session() {
        if source == Some(maho_ext_api::AbortSource::System) {
            session.state().abort_source = source;
            let streaming = session.is_streaming();
            let joined = session.state().abort_provenance.join(maho_ext_api::AbortSource::System, streaming);
            if joined.abort_current_agent { session.agent.abort(None); }
            session.abort_retry(); session.abort_compaction();
        } else if let Some(pending) = session.begin_user_abort()
            && let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move { session.finish_user_abort(pending).await; });
        }
    } }
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
        let resolved = self.get_resolved_compaction_settings().expect("live compaction settings");
        maho_ext_api::CompactionSettings { enabled: resolved.enabled, reserve_tokens: resolved.reserve_tokens,
            keep_recent_tokens: resolved.keep_recent_tokens }
    }
    fn get_resolved_compaction_settings(&self) -> Option<maho_ext_api::ResolvedCompactionSettings> {
        let session = self.session().unwrap_or_else(|error| std::panic::panic_any(error));
        let model = session.model();
        let raw = session.with_settings_manager(|manager| manager.get_value("compaction").cloned());
        let settings = raw.map(serde_json::from_value::<crate::compaction_settings_access::CompactionSettings>)
            .transpose().unwrap_or_else(|error| std::panic::panic_any(maho_ext_api::ExtensionFailure::new(error.to_string())));
        let resolved = crate::compaction_settings_resolver::resolve_compaction_settings(settings.as_ref(), Some(
            crate::compaction_settings_access::CompactionModelSelector { provider: &model.provider, id: &model.id },
        )).unwrap_or_else(|error| std::panic::panic_any(maho_ext_api::ExtensionFailure::new(error)));
        Some(maho_ext_api::ResolvedCompactionSettings {
            enabled: session.auto_compaction_enabled(),
            reserve_tokens: u64::try_from(resolved.reserve_tokens).unwrap_or_else(|error|
                std::panic::panic_any(maho_ext_api::ExtensionFailure::new(error.to_string()))),
            keep_recent_tokens: u64::try_from(resolved.keep_recent_tokens).unwrap_or_else(|error|
                std::panic::panic_any(maho_ext_api::ExtensionFailure::new(error.to_string()))),
            speculative_enabled: resolved.speculative_enabled,
            speculative_fraction: resolved.speculative_fraction,
            speculative_cooldown_ms: resolved.speculative_cooldown_ms,
            restoration_enabled: resolved.restoration_enabled,
            restoration_max_items: resolved.restoration_max_items,
            restoration_max_tokens_per_item: resolved.restoration_max_tokens_per_item,
            restoration_max_total_tokens: resolved.restoration_max_total_tokens,
            restoration_context_ratio: resolved.restoration_context_ratio,
            idle_compaction_enabled: resolved.idle_compaction_enabled,
            grace_band_enabled: resolved.grace_band_enabled,
            tool_admission_enabled: resolved.tool_admission_enabled,
            reminder_enabled: resolved.reminder_enabled,
            reserve_scaling_enabled: resolved.reserve_scaling_enabled,
            speculative_lead_tokens: resolved.speculative_lead_tokens,
            summarization_max_duration_ms: resolved.summarization_max_duration_ms,
        })
    }
    fn get_prompt_cache_safe_wait_seconds(&self) -> Option<f64> {
        let session = self.session().ok()?;
        let configured = session.with_settings_manager(|manager| manager.get_value("promptCache").cloned());
        crate::prompt_cache_budget::resolve_prompt_cache_safe_wait_seconds(
            Some(&session.model()), configured.as_ref().and_then(|value| value.get("cacheAwareTimeouts")).and_then(Value::as_bool).unwrap_or(true),
            configured.as_ref().and_then(|value| value.get("safetyBufferSeconds")).and_then(Value::as_f64),
            &std::env::vars().collect(),
        ).map(|value| value as f64)
    }
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
        let signal = maho_ext_api::AbortSignal::default();
        let model = session.model();
        let request_id = uuid::Uuid::new_v4().to_string();
        {
            let mut state = session.state();
            if state.compaction_abort_controller.is_some() { return None; }
            let controller = crate::compaction::lifecycle::CompactionAbortController::new();
            let revision = state.message_revision as i64;
            state.compaction_lifecycle.begin(crate::compaction::lifecycle::BeginCompactionOperation {
                operation_id: request_id.clone(), stage: crate::compaction::lifecycle::CompactionStage::Feedback,
                reason: format!("{:?}", options.reason), model: Some(crate::compaction::lifecycle::CompactionModelRef {
                    provider: model.provider, id: model.id,
                }), started_revision: revision,
            }, controller.clone());
            state.compaction_abort_controller = Some(controller);
            state.compaction_extension_signal = Some(signal.clone());
        }
        session.emit(AgentSessionEvent::CompactionStart { reason: options.reason, request_id: Some(request_id) });
        Some(signal)
    }
    fn update_compaction(&self, options: maho_ext_api::UpdateCompactionOptions) { if let Ok(session) = self.session() {
        if !session.is_compacting() || options.signal.as_ref().is_some_and(maho_ext_api::AbortSignal::is_aborted) { return; }
        session.emit(AgentSessionEvent::CompactionProgress { reason: options.reason, delta: options.delta, text: options.text });
    } }
    fn end_compaction(&self, options: maho_ext_api::EndCompactionOptions) { if let Ok(session) = self.session() {
        let lifecycle = session.compaction_state();
        let Some(operation) = lifecycle.operation() else { return; };
        if lifecycle.status() != "running" || operation.stage != crate::compaction::lifecycle::CompactionStage::Feedback { return; }
        let aborted = options.aborted.unwrap_or_else(|| options.signal.as_ref().is_some_and(maho_ext_api::AbortSignal::is_aborted));
        let revision = session.message_revision() as i64;
        session.state().compaction_lifecycle.finish(&crate::compaction::lifecycle::FinishCompactionOperation {
            operation_id: operation.operation_id.clone(), status: if aborted { crate::compaction::lifecycle::CompactionFinishStatus::Aborted }
                else { crate::compaction::lifecycle::CompactionFinishStatus::Failed }, ended_revision: revision,
            rejection_cause: None, error_message: options.error_message.clone(),
        });
        session.state().compaction_abort_controller = None;
        session.state().compaction_extension_signal = None;
        session.emit(AgentSessionEvent::CompactionEnd { reason: options.reason, result: None, aborted,
            will_retry: false, request_id: Some(operation.operation_id.clone()), accepted: None, rejection_cause: None, error_message: options.error_message });
    } }
    fn get_message_revision(&self) -> u64 { self.session().map_or(0, |session| session.message_revision()) }
    fn apply_compaction(&self, result: maho_ext_api::CompactionResult, options: maho_ext_api::ApplyCompactionOptions) -> maho_ext_api::ExtensionFuture<'_, maho_ext_api::ApplyCompactionResult> {
        Box::pin(async move { let session = self.session()?;
            if options.expected_revision.is_some_and(|revision| revision != session.message_revision()) { return Ok(maho_ext_api::ApplyCompactionResult::Stale); }
            if options.signal.as_ref().is_some_and(maho_ext_api::AbortSignal::is_aborted) { return Ok(maho_ext_api::ApplyCompactionResult::Rejected); }
            if let Some(anchor) = options.expected_warm_anchor {
                let snapshot = crate::compaction::warm_anchor::WarmAnchorSnapshot {
                    first_kept_entry_id: anchor.first_kept_entry_id, prefix_entry_ids: anchor.prefix_entry_ids,
                    latest_compaction_entry_id: anchor.latest_compaction_entry_id,
                };
                if !session.with_session_manager(|manager| crate::compaction::warm_anchor::is_warm_summary_anchor_valid(&snapshot, &manager.branch(manager.leaf_id().or(Some(""))))) {
                    return Ok(maho_ext_api::ApplyCompactionResult::Stale);
                }
            }
            let Ok(_admission) = session.prompt_admission.try_lock() else { return Ok(maho_ext_api::ApplyCompactionResult::Rejected); };
            if session.is_streaming() { return Ok(maho_ext_api::ApplyCompactionResult::Rejected); }
            let _work = session.work_barrier.begin();
            let model = session.model();
            let (request_id, owns_controller) = {
                let mut state = session.state();
                let owns = state.compaction_abort_controller.is_none();
                let controller = state.compaction_abort_controller.clone().unwrap_or_default();
                let revision = state.message_revision as i64;
                let request_id = state.compaction_lifecycle.begin(crate::compaction::lifecycle::BeginCompactionOperation {
                    operation_id: uuid::Uuid::new_v4().to_string(), stage: crate::compaction::lifecycle::CompactionStage::Execution,
                    reason: format!("{:?}", options.reason), model: Some(crate::compaction::lifecycle::CompactionModelRef {
                        provider: model.provider, id: model.id,
                    }), started_revision: revision,
                }, controller.clone());
                state.compaction_abort_controller = Some(controller);
                (request_id, owns)
            };
            if owns_controller { session.emit(AgentSessionEvent::CompactionStart { reason: options.reason, request_id: Some(request_id.clone()) }); }
            let applied = session.apply_compaction_internal(&crate::compaction::compaction::CompactionResult { summary: result.summary.clone(), first_kept_entry_id: result.first_kept_entry_id.clone(),
                tokens_before: result.tokens_before as i64, estimated_tokens_after: None, usage: None, details: result.details.clone() }, Some(true));
            let revision = session.message_revision() as i64;
            session.state().compaction_lifecycle.finish(&crate::compaction::lifecycle::FinishCompactionOperation {
                operation_id: request_id.clone(), status: if applied.is_ok() { crate::compaction::lifecycle::CompactionFinishStatus::Completed }
                    else { crate::compaction::lifecycle::CompactionFinishStatus::Failed },
                ended_revision: revision, rejection_cause: None, error_message: applied.as_ref().err().cloned(),
            });
            session.state().compaction_abort_controller = None;
            session.state().compaction_extension_signal = None;
            session.emit(AgentSessionEvent::CompactionEnd { reason: options.reason, request_id: Some(request_id),
                result: applied.is_ok().then_some(result), aborted: false, will_retry: false,
                accepted: Some(applied.is_ok()), rejection_cause: None,
                error_message: applied.as_ref().err().map(|error| format!("Compaction failed: {error}")),
            });
            if applied.is_err() {
                return Ok(maho_ext_api::ApplyCompactionResult::Rejected);
            }
            session.state().delegated_compaction_key = None;
            Ok(maho_ext_api::ApplyCompactionResult::Applied)
        })
    }
    fn get_system_prompt(&self) -> String { self.session().map_or_else(|_| String::new(), |session| session.system_prompt()) }
    fn get_system_prompt_options(&self) -> maho_ext_api::BuildSystemPromptOptions {
        let Ok(session) = self.session() else { return Default::default(); };
        let skills = session.state().skills.iter().map(|skill| maho_ext_api::Skill {
            name: skill.name.clone(), description: skill.description.clone(), file_path: skill.file_path.clone(),
            base_dir: skill.base_dir.clone(), source_info: maho_ext_api::SourceInfo {
                path: skill.source_info.path.clone(), source: skill.source_info.source.clone(),
                scope: match skill.source_info.scope {
                    crate::source_info::SourceScope::User => maho_ext_api::SourceScope::User,
                    crate::source_info::SourceScope::Project => maho_ext_api::SourceScope::Project,
                    crate::source_info::SourceScope::Temporary => maho_ext_api::SourceScope::Temporary,
                    crate::source_info::SourceScope::System => maho_ext_api::SourceScope::System,
                },
                origin: match skill.source_info.origin {
                    crate::source_info::SourceOrigin::Package => maho_ext_api::SourceOrigin::Package,
                    crate::source_info::SourceOrigin::TopLevel => maho_ext_api::SourceOrigin::TopLevel,
                }, base_dir: skill.source_info.base_dir.clone(),
            },
            disable_model_invocation: skill.disable_model_invocation,
        }).collect();
        let (custom, append) = session.system_prompt_sources();
        let append: Vec<_> = append.iter().filter_map(|source| crate::resource_loader::resolve_prompt_input(Some(source), "append system prompt")).collect();
        maho_ext_api::BuildSystemPromptOptions { cwd: session.cwd().into(), tools: session.get_active_tool_names(), skills,
            custom_prompt: crate::resource_loader::resolve_prompt_input(custom.as_deref(), "system prompt"),
            append_system_prompt: (!append.is_empty()).then(|| append.join("\n\n")),
            context_files: session.project_context_files().into_iter()
                .map(|file| maho_ext_api::ContextFile { path: file.path, content: file.content }).collect(),
        }
    }
    fn get_loaded_hook_sources(&self) -> maho_ext_api::LoadedHookSources {
        if let Some(session) = self.session().ok() {
            let cwd = session.cwd();
            let state = session.state();
            if let Some(mut sources) = state.loaded_hook_sources.clone() {
                sources.cwd = cwd.into();
                sources.project_hooks_path = sources.cwd.join(crate::config::config_dir_name()).join("hooks.json");
                for entry in &state.discovered_resources.hook_paths {
                    let path = std::path::PathBuf::from(&entry.path);
                    if !sources.runtime_hook_source_paths.contains(&path) { sources.runtime_hook_source_paths.push(path); }
                }
                return sources;
            }
        }
        let session = self.session().ok(); let cwd = session.as_ref().map_or_else(std::path::PathBuf::new, |session| session.cwd().into());
        let dir = session.as_ref().map_or_else(std::path::PathBuf::new, |session| session.agent_dir().into());
        let (global_settings_hooks, project_settings_hooks) = session.as_ref().map_or((None, None), |session|
            session.with_settings_manager(|manager| (manager.get_global().get("hooks").cloned(),
                manager.is_project_trusted().then(|| manager.get_project().get("hooks").cloned()).flatten())));
        maho_ext_api::LoadedHookSources { global_hooks_path: dir.join("hooks.json"), project_hooks_path: cwd.join(crate::config::config_dir_name()).join("hooks.json"), cwd, agent_dir: dir,
            global_settings_hooks, project_settings_hooks,
            global_hook_source_paths: session.as_ref().map(|session| session.state().global_hook_source_paths.clone()).unwrap_or_default(),
            project_hook_source_paths: session.as_ref().map(|session| session.state().project_hook_source_paths.clone()).unwrap_or_default(),
            pre_session_hook_source_paths: session.as_ref().map(|session| session.state().pre_session_hook_source_paths.clone()).unwrap_or_default(),
            runtime_hook_source_paths: session.as_ref().map(|session|
                session.state().discovered_resources.hook_paths.iter().map(|entry| std::path::PathBuf::from(&entry.path)).collect()).unwrap_or_default() }
    }
    fn kernel_tools(&self) -> Option<&dyn maho_ext_api::ExtensionKernelTools> { None }
    fn get_thinking_level(&self) -> Option<ThinkingLevel> {
        self.session().ok().and_then(|session| thinking_level_from_model_level(session.thinking_level()))
    }
}

impl maho_ext_api::ExtensionActions for SessionExtensionActions {
    fn send_message(&self, message: maho_ext_api::CustomMessage, options: maho_ext_api::SendMessageOptions) -> Result<(), maho_ext_api::ExtensionFailure> {
        let session = self.session()?;
        let deferred_session = session.clone();
        let deferred_message = message.clone();
        let deferred_options = options.clone();
        let deferred_generation = session.monitor_generation.load(Ordering::SeqCst);
        let deferred_abort_generation = session.user_abort_generation.load(Ordering::SeqCst);
        let deferred = if options.trigger_turn {
            lock(&session.settled_delivery).defer_trigger_turn(move |claim| {
                let guard = deferred_session.work_barrier.begin();
                tokio::spawn(async move {
                    if deferred_session.state().disposed || deferred_session.monitor_generation.load(Ordering::SeqCst) != deferred_generation
                        || deferred_session.user_abort_generation.load(Ordering::SeqCst) != deferred_abort_generation {
                        claim.resolve(crate::agent_settled_delivery::DeferredTurnDisposition::FinishedWithoutStart);
                        return;
                    }
                    claim.resolve(crate::agent_settled_delivery::DeferredTurnDisposition::Delegated);
                    if let Err(error) = maho_ext_api::ExtensionActions::send_message(
                        &SessionExtensionActions(Arc::downgrade(&deferred_session.inner)), deferred_message, deferred_options,
                    ) { deferred_session.emit(AgentSessionEvent::ContinuationError { error_message: error.message }); }
                    drop(guard);
                });
            })
        } else {
            lock(&session.settled_delivery).defer(Box::new(move || {
                if deferred_session.state().disposed || deferred_session.monitor_generation.load(Ordering::SeqCst) != deferred_generation
                    || deferred_session.user_abort_generation.load(Ordering::SeqCst) != deferred_abort_generation { return; }
                if let Err(error) = maho_ext_api::ExtensionActions::send_message(
                    &SessionExtensionActions(Arc::downgrade(&deferred_session.inner)), deferred_message, deferred_options,
                ) { deferred_session.emit(AgentSessionEvent::ContinuationError { error_message: error.message }); }
            }))
        };
        if deferred { return Ok(()); }
        let custom: maho_agent::harness::messages::CustomMessage = serde_json::from_value(serde_json::json!({
            "role":"custom","customType":message.custom_type,"content":message.content,"display":message.display,
            "details":message.details,"timestamp":maho_ai::utils::diagnostics::now_ms(),
        })).map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?;
        let agent_message = AgentMessage::Custom(maho_agent::types::CustomAgentMessage::Custom(custom.clone()));
        if options.deliver_as == Some(maho_ext_api::DeliverAs::NextTurn) {
            session.state().pending_next_turn_messages.push(agent_message);
            return Ok(());
        }
        if (session.is_streaming() || session.is_compacting()) && options.trigger_turn {
            match options.deliver_as { Some(maho_ext_api::DeliverAs::FollowUp) => session.agent.follow_up(agent_message), _ => session.agent.steer(agent_message) }
        } else if session.is_streaming() {
            session.state().pending_custom_messages.push(custom);
        } else {
            session.append_extension_custom_message(custom).map_err(maho_ext_api::ExtensionFailure::new)?;
            if options.trigger_turn {
                let guard = session.work_barrier.begin();
                let generation = session.user_abort_generation.load(Ordering::SeqCst);
                let runtime_generation = session.monitor_generation.load(Ordering::SeqCst);
                tokio::spawn(async move {
                    if session.state().disposed || session.monitor_generation.load(Ordering::SeqCst) != runtime_generation {
                        drop(guard);
                        return;
                    }
                    if let Err(error) = session.continue_session_internal(Some(generation)).await { session.emit(AgentSessionEvent::ContinuationError { error_message: error }); }
                    drop(guard);
                });
            }
        }
        Ok(())
    }
    fn send_user_message(&self, content: maho_ext_api::UserMessageContent, options: maho_ext_api::SendUserMessageOptions) -> Result<(), maho_ext_api::ExtensionFailure> {
        let session = self.session()?;
        let deferred_session = session.clone();
        let deferred_content = content.clone();
        let deferred_options = options.clone();
        let deferred_generation = session.monitor_generation.load(Ordering::SeqCst);
        let deferred_abort_generation = session.user_abort_generation.load(Ordering::SeqCst);
        if lock(&session.settled_delivery).defer_trigger_turn(move |claim| {
            let guard = deferred_session.work_barrier.begin();
            tokio::spawn(async move {
                if deferred_session.state().disposed || deferred_session.monitor_generation.load(Ordering::SeqCst) != deferred_generation
                    || deferred_session.user_abort_generation.load(Ordering::SeqCst) != deferred_abort_generation {
                    claim.resolve(crate::agent_settled_delivery::DeferredTurnDisposition::FinishedWithoutStart);
                    return;
                }
                claim.resolve(crate::agent_settled_delivery::DeferredTurnDisposition::Delegated);
                if let Err(error) = maho_ext_api::ExtensionActions::send_user_message(
                    &SessionExtensionActions(Arc::downgrade(&deferred_session.inner)), deferred_content, deferred_options,
                ) { deferred_session.emit(AgentSessionEvent::ContinuationError { error_message: error.message }); }
                drop(guard);
            });
        }) { return Ok(()); }
        let readiness = lock(&session.binding_readiness).as_ref().map(|pending| {
            let (ready, receiver) = tokio::sync::oneshot::channel();
            lock(pending).push(receiver);
            ready
        });
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
        let guard = session.work_barrier.begin();
        tokio::spawn(async move {
            let mut readiness = readiness;
            if session.state().disposed || session.monitor_generation.load(Ordering::SeqCst) != deferred_generation
                || session.user_abort_generation.load(Ordering::SeqCst) != deferred_abort_generation {
                drop(readiness);
                drop(guard);
                return;
            }
            let mut disposition_reported = false;
            if let Err(error) = session.prompt_with_readiness(&text, PromptOptions { images: Some(images.clone()), source: Some(InputSource::Extension),
                streaming_behavior: options.deliver_as, expand_prompt_templates: Some(options.expand_prompt_templates), ..Default::default() }, &mut readiness, &mut disposition_reported).await {
                if !disposition_reported && !session.state().disposed
                    && session.monitor_generation.load(Ordering::SeqCst) == deferred_generation
                    && session.user_abort_generation.load(Ordering::SeqCst) == deferred_abort_generation {
                    let mode = options.deliver_as.unwrap_or(StreamingBehavior::FollowUp);
                    session.queue_expanded_input(text, Some(images), mode, None);
                }
                session.emit(AgentSessionEvent::ContinuationError { error_message: error });
            }
            drop(readiness);
            drop(guard);
        });
        Ok(())
    }
    fn append_entry(&self, custom_type: &str, data: Option<Value>) -> Result<(), maho_ext_api::ExtensionFailure> {
        let session = self.session()?;
        let entry = session.with_session_manager_mut(|manager| manager.append_custom(custom_type, data));
        session.emit(AgentSessionEvent::EntryAppended { entry: session_entry_from_value(entry) });
        Ok(())
    }
    fn get_all_tools(&self) -> Result<Vec<maho_ext_api::ToolInfo>, maho_ext_api::ExtensionFailure> {
        Ok(self.session()?.get_all_tools())
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
            session.execute_tool_with_updates(name, params, ExecuteToolOptions { signal: options.signal, activate_inactive_tool: options.activate_inactive_tool }, options.on_update, None).await
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
        let session = self.session()?;
        let mut hints = session.agent.removed_tool_hints();
        session.state().extension_hint_backups.entry(name.to_owned()).or_insert_with(|| hints.get(name).cloned());
        hints.insert(name.to_owned(), hint.to_owned());
        session.agent.set_removed_tool_hints(hints); Ok(())
    }
    fn register_lazy_tool_activator(&self, activator: maho_ext_api::LazyToolActivator) -> Result<(), maho_ext_api::ExtensionFailure> {
        let session = self.session()?;
        session.state().extension_lazy_activators.push(activator.clone());
        session.add_lazy_tool_activator(activator); Ok(())
    }
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
                replace_instructions: options.replace_instructions, label: options.label,
                expected_leaf_id: options.expected_leaf_id, intent: None,
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

#[cfg(test)]
struct SessionContextManager {
    session: AgentSession,
    id: String,
    file: Option<std::path::PathBuf>,
    actions: SessionExtensionActions,
}

#[cfg(test)]
impl SessionContextManager {
    fn new(session: &AgentSession) -> Self {
        Self { session: session.clone(), id: session.session_id(), file: session.session_file().map(Into::into),
            actions: SessionExtensionActions(Arc::downgrade(&session.inner)) }
    }
}

#[cfg(test)]
impl maho_ext_api::ToolSessionManager for SessionContextManager {
    fn session_id(&self) -> &str { &self.id }
    fn session_file(&self) -> Option<&std::path::Path> { self.file.as_deref() }
}

#[cfg(test)]
impl maho_ext_api::SessionManager for SessionContextManager {
    fn get_entries(&self) -> Vec<maho_ext_api::SessionEntry> {
        self.session.with_session_manager(|manager| manager.entries()).into_iter().map(session_entry_from_value).collect()
    }
    fn get_branch(&self) -> Vec<maho_ext_api::SessionEntry> {
        self.session.with_session_manager(|manager| manager.branch(manager.leaf_id().or(Some("")))).into_iter().map(session_entry_from_value).collect()
    }
    fn get_leaf_id(&self) -> Option<String> { self.session.with_session_manager(|manager| manager.leaf_id().map(str::to_owned)) }
    fn get_session_name(&self) -> Option<String> { self.session.session_name() }
    fn extension_context_actions(&self) -> Option<&dyn maho_ext_api::ExtensionContextActions> { Some(&self.actions) }
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
            session.switch_model_with_thinking(model, false, if revert {
                maho_ext_api::ModelSelectSource::FallbackRevert
            } else { maho_ext_api::ModelSelectSource::Fallback }, false, Some(thinking)).await?;
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
    pub fn new(mut config: AgentSessionConfig) -> Result<Self, MissingModelAccessError> {
        if config.flag_values.get("no-model-fallback").is_some_and(|value| matches!(value, FlagValue::Boolean(true)))
            || std::env::var("NO_FALLBACK").as_deref() == Ok("1")
        { config.settings_manager.apply_overrides(&Map::from_iter([("retry".to_owned(), serde_json::json!({"modelFallback":false}))])); }
        if config.flag_values.get("no-ask-user").is_some_and(|value| matches!(value, FlagValue::Boolean(true))) {
            config.settings_manager.apply_overrides(&Map::from_iter([("askUser".to_owned(), serde_json::json!({"enabled":false}))]));
        }
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
            custom_system_prompt_source: None,
            append_system_prompt_sources: Vec::new(),
            context_files_enabled: true,
            system_prompt_override: None,
            wake_sources: WakeSourceTracker::default(),
            shown_high_reasoning_warning_keys: BTreeSet::new(),
            extension_mode: ExtensionMode::Print,
            extension_ui_context: None,
            extension_abort_handler: None,
            extension_error_listener: None,
            message_revision: 0,
            assistant_generation: 0,
            last_persisted_assistant: None,
            post_compaction_assistant_generation: None,
            post_compaction_usage_exempt_entries: BTreeSet::new(),
            messages_awaiting_persistence: Vec::new(),
            steering_messages: Vec::new(),
            follow_up_messages: Vec::new(),
            queued_input_order: Vec::new(),
            next_queued_input_order: 0,
            post_compaction_deferred_steering_messages: Vec::new(),
            post_compaction_deferred_follow_up_messages: Vec::new(),
            prompt_start_pending: false,
            had_cleared_queued_messages: false,
            auto_compaction_session_override: None,
            turn_index: 0,
            message_replacements: Vec::new(),
            retry_attempt: 0,
            retry_abort_controller: None,
            user_aborted: false,
            abort_source: None,
            abort_provenance: Default::default(),
            probe_phase: crate::retry_fallback::hint_policy::ProbePhase::Idle,
            hint_deadline_ms: None,
            cumulative_hinted_wait_ms: 0.0,
            pending_model_switch: None,
            compaction_abort_controller: None,
            pending_compaction_admission: None,
            compaction_lifecycle: Default::default(),
            delegated_compaction_key: None,
            prompt_templates: Vec::new(),
            extension_commands: Vec::new(),
            extension_command_catalog: None,
            extension_event_sender: None,
            extension_tool_context: None,
            extension_tool_backups: BTreeMap::new(),
            extension_lazy_activators: Vec::new(),
            extension_hint_backups: BTreeMap::new(),
            skills: Vec::new(),
            discovered_resources: maho_ext_api::DiscoveredResources::default(),
            global_hook_source_paths: Vec::new(), project_hook_source_paths: Vec::new(), pre_session_hook_source_paths: Vec::new(),
            loaded_hook_sources: None,
            bash_abort_signals: BTreeMap::new(),
            pending_bash_messages: Vec::new(),
            pending_next_turn_messages: Vec::new(),
            pending_custom_messages: Vec::new(),
            extension_event_signal: None,
            compaction_extension_signal: None,
            branch_summary_abort_controller: None,
            session_title_abort_controller: None,
            disposed: false,
        };
        let session = Self { inner: Arc::new(AgentSessionInner {
            agent,
            session_manager: Mutex::new(config.session_manager),
            settings_manager: Arc::new(Mutex::new(config.settings_manager)),
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
            binding_readiness: Mutex::new(None),
            retry_fallback: tokio::sync::Mutex::new(None),
            work_barrier: Arc::new(crate::session_work_barrier::SessionWorkBarrier::new()),
            wake_source_subscription: Mutex::new(None),
            settings_source_subscription: Mutex::new(None),
            settled_delivery: Mutex::new(crate::agent_settled_delivery::AgentSettledDelivery::new()),
            user_abort_generation: AtomicU64::new(0),
            monitor_generation: AtomicU64::new(0),
            settlement_epoch: AtomicU64::new(0),
            probe_scheduler: Mutex::new(crate::retry_fallback::probe_scheduler::ProbeBackScheduler::default()),
            probe_task: Mutex::new(None),
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

        let weak = Arc::downgrade(&session.inner);
        let subscription = session.with_settings_manager(|manager| manager.subscribe_to_source_selection(Arc::new(move |source| {
            if let Some(inner) = weak.upgrade() {
                AgentSession { inner }.emit(AgentSessionEvent::SettingsSourceSelected { selection: serde_json::json!({
                    "path": source.path, "scope": source.scope.as_str(),
                    "format": match source.format { crate::settings_manager::SettingsFormat::Jsonc => "jsonc", crate::settings_manager::SettingsFormat::Json => "json" },
                    "reason": match source.reason { crate::settings_manager::SettingsSourceReason::ExplicitJsonc => "explicit-jsonc", crate::settings_manager::SettingsSourceReason::JsonOnly => "json-only" },
                }) });
            }
        })));
        *lock(&session.settings_source_subscription) = Some(subscription);

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
        let _persistence = if let AgentEvent::MessageEnd { message } = &event {
            let id = uuid::Uuid::new_v4();
            self.state().messages_awaiting_persistence.push((id, message.clone()));
            Some(MessagePersistenceGuard { session: self, id })
        } else { None };
        {
            let mut state = self.state();
            match &mut event {
                AgentEvent::AgentStart => {
                    state.turn_index = 0;
                    state.message_replacements.clear();
                    state.abort_source = None;
                    state.abort_provenance = Default::default();
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
                AgentEvent::MessageStart { message } => {
                    if message.as_assistant().is_some() { state.assistant_generation += 1; }
                }
                AgentEvent::TurnStart |
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
        let end_boundary = if let AgentEvent::AgentEnd { messages } = &event {
            let aborted = messages.iter().rev().find_map(AgentMessage::as_assistant)
                .is_some_and(|message| message.stop_reason == StopReason::Aborted);
            Some(self.state().abort_provenance.begin_agent_end(messages.clone(), will_retry, aborted))
        } else { None };
        let extension_event = match &event {
            AgentEvent::AgentStart => maho_ext_api::ExtensionEvent::AgentStart,
            AgentEvent::AgentEnd { messages } => maho_ext_api::ExtensionEvent::AgentEnd {
                messages: messages.clone(), aborted: end_boundary.as_ref().map(|boundary| boundary.aborted),
                will_retry: Some(will_retry), abort_source: end_boundary.as_ref().and_then(|boundary| boundary.abort_source),
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
                    if let Some(guard) = &_persistence
                        && let Some((_, pending)) = self.state().messages_awaiting_persistence.iter_mut().find(|(id, _)| *id == guard.id)
                    { *pending = replacement.clone(); }
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
        if let Some(boundary) = &end_boundary {
            self.state().abort_provenance.end_agent_end(boundary);
            self.emit_late_user_abort().await;
        }
        if matches!(event, AgentEvent::TurnEnd { .. }) {
            self.state().turn_index += 1;
            let pending = std::mem::take(&mut self.state().pending_custom_messages);
            for message in pending {
                if let Err(error_message) = self.append_extension_custom_message(message) { self.emit(AgentSessionEvent::ContinuationError { error_message }); }
            }
        }
        self.emit(AgentSessionEvent::Agent(event.clone()));
        if end_boundary.is_some() { self.emit_late_user_abort().await; }
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
                        if entry["message"]["role"] == "assistant" {
                            let mut state = self.state();
                            state.last_persisted_assistant = Some((id.to_owned(), state.assistant_generation));
                        }
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
        let runner = self.extension_runner.lock().await.clone();
        let result = match runner {
            Some(mut runner) => runner.emit(event).await.map(|_| ()),
            None => Ok(()),
        };
        if let Err(error) = result {
            self.emit(AgentSessionEvent::ContinuationError { error_message: error.to_string() });
        }
    }

    /// Start a prompt, or explicitly queue input when a provider turn is active.
    pub async fn prompt(&self, text: &str, options: PromptOptions) -> Result<PromptDisposition, String> {
        let admitted = options.prompt_admitted.clone();
        let mut reported = false;
        let result = Box::pin(self.prompt_with_readiness(text, options, &mut None, &mut reported)).await;
        if !reported && let Ok(disposition) = &result && let Some(admitted) = admitted {
            admitted(*disposition);
        }
        result
    }

    async fn prompt_with_readiness(&self, text: &str, options: PromptOptions, readiness: &mut Option<tokio::sync::oneshot::Sender<()>>, disposition_reported: &mut bool) -> Result<PromptDisposition, String> {
        if options.signal.as_ref().is_some_and(|signal| signal.aborted()) {
            return Err("Prompt cancelled".to_owned());
        }
        if text.starts_with('/') && self.try_execute_extension_command(text).await? { return Ok(PromptDisposition::Handled); }
        let (pending_compaction, compaction_generation) = if options.source == Some(InputSource::Extension) {
            let state = self.state();
            (state.pending_compaction_admission.clone(),
                (state.compaction_lifecycle.state().status() == "running"
                    && state.compaction_lifecycle.state().operation().is_some_and(|operation|
                        operation.stage == crate::compaction::lifecycle::CompactionStage::Execution && operation.reason == "manual"))
                    .then(|| state.compaction_lifecycle.state().generation()))
        } else { (None, None) };
        let waits_for_manual_compaction = pending_compaction.is_some() || compaction_generation.is_some();
        let streaming = self.is_streaming();
        let _prompt_start = {
            let mut state = self.state();
            if !streaming && !state.prompt_start_pending && options.streaming_behavior.is_none() {
                state.prompt_start_pending = true;
                Some(PromptStartGuard(self))
            } else { None }
        };
        let queues_behind_pending_prompt = self.state().prompt_start_pending && options.streaming_behavior.is_some();
        if !waits_for_manual_compaction && (self.is_streaming() || self.is_compacting() || queues_behind_pending_prompt) {
            if options.thinking_level.is_some() { return Err("Cannot set thinkingLevel on a queued prompt; set it after the current turn completes.".to_owned()); }
            let mode = options.streaming_behavior.ok_or_else(||
                "Agent is already processing a prompt. Use steer() or followUp() to queue messages, or wait for completion.".to_owned())?;
            self.queue_user_input(text, options.images, mode, QueuedInputOptions { source: options.source, ..Default::default() }).await?;
            return Ok(PromptDisposition::Queued);
        }
        let _admission = if let Some(signal) = &options.signal {
            tokio::select! {
                biased;
                _ = signal.cancelled() => return Err("Prompt cancelled".to_owned()),
                admission = self.prompt_admission.lock() => admission,
            }
        } else { self.prompt_admission.lock().await };
        let weak = Arc::downgrade(&self.inner);
        let _prompt_signal_bridge = options.signal.map(|signal| AbortSignalBridge(tokio::spawn(async move {
            signal.cancelled().await;
            if let Some(inner) = weak.upgrade() { AgentSession { inner }.abort().await; }
        })));
        let _work = self.work_barrier.begin();
        self.state().user_aborted = false;
        let Some((text, images, input_id)) = self.run_input_handlers(text, options.images, options.source, None).await? else {
            return Ok(PromptDisposition::Handled);
        };
        let text = self.expand_input(&text, options.expand_prompt_templates.unwrap_or(true))?;
        let compaction_failed = {
            let state = self.state();
            let lifecycle = state.compaction_lifecycle.state();
            pending_compaction.as_ref().is_some_and(|pending| !pending.completed.load(Ordering::SeqCst))
                || (compaction_generation.is_some_and(|generation| generation == lifecycle.generation())
                    && matches!(lifecycle.status(), "failed" | "aborted"))
        };
        if compaction_failed {
            let mode = options.streaming_behavior.unwrap_or(StreamingBehavior::Steer);
            self.queue_expanded_input(text, images, mode, None);
            self.dispatch_extension_event(maho_ext_api::ExtensionEvent::InputDisposition {
                input_id, disposition: maho_ext_api::InputDisposition::Queued,
            }).await;
            *disposition_reported = true;
            drop(readiness.take());
            if let Some(admitted) = &options.prompt_admitted { admitted(PromptDisposition::Queued); }
            return Ok(PromptDisposition::Queued);
        }
        if self.auto_compaction_enabled() && !self.is_compaction_delegated() && self.pending_model_switch().is_none() {
            let model = self.model();
            let raw = self.with_settings_manager(|manager| manager.get_value("compaction").cloned());
            let settings = raw.map(serde_json::from_value::<crate::compaction_settings_access::CompactionSettings>)
                .transpose().map_err(|error| error.to_string())?;
            let resolved = crate::compaction_settings_resolver::resolve_compaction_settings(settings.as_ref(), Some(
                crate::compaction_settings_access::CompactionModelSelector { provider: &model.provider, id: &model.id },
            ))?;
            if self.get_context_usage().and_then(|usage| usage.tokens).is_some_and(|tokens|
                tokens.saturating_add(text.len().div_ceil(4) as u64) > model.context_window.saturating_sub(
                    if resolved.reserve_scaling_enabled { crate::compaction::compaction::resolve_reserve_tokens(
                        model.context_window as f64, resolved.reserve_tokens as f64,
                    ) as u64 } else { resolved.reserve_tokens as u64 })) {
                let result = self.compact_for_model(None, &model, "pre-prompt").await;
                if !self.is_compaction_delegated() { result?; }
            }
        }
        if let Some(pending) = self.pending_model_switch() {
            self.compact_for_model(None, &pending.model, "pre-prompt").await?;
            let measured = self.with_session_manager(|manager| manager.build_context(manager.leaf_id())).messages.iter()
                .map(crate::compaction::compaction::estimate_tokens).sum();
            let admitted = self.reduce_for_switch_target(&pending.model, measured)?;
            self.state().pending_model_switch = None;
            self.assert_model_usable(&pending.model, admitted)?;
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
        let consumed = std::mem::take(&mut self.state().pending_next_turn_messages);
        let before = {
            let mut runner = self.extension_runner.lock().await;
            match runner.as_mut() { Some(runner) => runner.emit_before_agent_start(maho_ext_api::BeforeAgentStartEvent {
                prompt: text.clone(), images: images.clone(), system_prompt: base_system_prompt,
                system_prompt_options: self.extension_system_prompt_options(),
            }).await.map_err(|error| error.to_string()), None => Ok(None) }
        };
        let before = match before {
            Ok(before) => before,
            Err(error) => {
                self.state().pending_next_turn_messages.splice(0..0, consumed);
                self.dispatch_extension_event(maho_ext_api::ExtensionEvent::InputDisposition {
                    input_id, disposition: maho_ext_api::InputDisposition::Rejected,
                }).await;
                return Err(error);
            }
        };
        let mut messages = vec![make_user_message(&text, images)];
        messages.extend(consumed.clone());
        if let Some(before) = before {
            self.state().system_prompt_override = before.system_prompt.clone();
            self.agent.set_system_prompt(before.system_prompt.unwrap_or_else(|| self.state().base_system_prompt.clone()));
            for message in before.messages {
                messages.push(session_message_from_value(serde_json::json!({"role":"custom", "customType":message.custom_type,
                    "content":message.content, "display":message.display, "details":message.details,
                    "timestamp":maho_ai::utils::diagnostics::now_ms()})).map_err(|error| error.to_string())?);
            }
        } else {
            self.state().system_prompt_override = None;
            self.agent.set_system_prompt(self.state().base_system_prompt.clone());
        }
        if let Err(error) = self.enforce_final_provider_admission(&messages).await {
            self.state().pending_next_turn_messages.splice(0..0, consumed);
            self.dispatch_extension_event(maho_ext_api::ExtensionEvent::InputDisposition {
                input_id, disposition: maho_ext_api::InputDisposition::Rejected,
            }).await;
            return Err(error);
        }
        self.dispatch_extension_event(maho_ext_api::ExtensionEvent::InputDisposition {
            input_id, disposition: maho_ext_api::InputDisposition::Started,
        }).await;
        *disposition_reported = true;
        drop(readiness.take());
        if let Some(admitted) = &options.prompt_admitted { admitted(PromptDisposition::Started); }
        self.agent.prompt(maho_agent::agent::AgentPromptInput::Messages(messages)).await;
        self.finish_provider_turn().await?;
        self.flush_pending_bash_messages();
        let title_prompt = match options.session_title_prompt {
            Some(SessionTitlePrompt::Disabled) => None,
            Some(SessionTitlePrompt::Text(prompt)) => Some(prompt),
            None => Some(text),
        };
        if self.state().auto_title_sessions && let Some(text) = title_prompt {
            let session = self.clone();
            tokio::spawn(async move { session.generate_session_title_if_needed(&text).await; });
        }
        drop(_work);
        drop(_admission);
        self.emit_agent_settled().await;
        Ok(PromptDisposition::Started)
    }

    async fn enforce_final_provider_admission(&self, additions: &[AgentMessage]) -> Result<(), String> {
        let pending_queued_messages = || {
            let state = self.state();
            state.steering_messages.iter().chain(&state.follow_up_messages)
                .map(|text| make_user_message(text, None)).collect::<Vec<_>>()
        };
        if !additions.iter().any(|message| message.role() == "custom") && pending_queued_messages().is_empty() { return Ok(()); }
        let model = self.model();
        let raw = self.with_settings_manager(|manager| manager.get_value("compaction").cloned());
        let settings = raw.map(serde_json::from_value::<crate::compaction_settings_access::CompactionSettings>)
            .transpose().map_err(|error| error.to_string())?;
        let resolved = crate::compaction_settings_resolver::resolve_compaction_settings(settings.as_ref(), Some(
            crate::compaction_settings_access::CompactionModelSelector { provider: &model.provider, id: &model.id },
        ))?;
        if !self.auto_compaction_enabled() || self.is_compaction_delegated() { return Ok(()); }
        let reserve = if resolved.reserve_scaling_enabled {
            crate::compaction::compaction::resolve_reserve_tokens(model.context_window as f64, resolved.reserve_tokens as f64) as u64
        } else { resolved.reserve_tokens as u64 };
        let oversized = || -> Result<bool, String> {
            let messages = self.messages().into_iter().chain(additions.iter().cloned()).chain(pending_queued_messages())
                .map(|message| session_message_to_value(&message)).collect::<Result<Vec<_>, _>>().map_err(|error| error.to_string())?;
            let messages = crate::messages::filter_context_excluded_messages(messages);
            let estimate = crate::compaction::estimate_context_tokens(&messages);
            let compacted = self.with_session_manager(|manager|
                crate::session_manager::get_latest_compaction_entry(&manager.branch(manager.leaf_id())).is_some());
            let tokens = if compacted { messages.iter().map(crate::compaction::estimate_tokens).sum() } else { estimate.tokens };
            Ok(tokens > model.context_window.saturating_sub(reserve))
        };
        if !oversized()? { return Ok(()); }
        if !self.messages().iter().any(|message| message.role() == "assistant") {
            return Err("Compaction required before provider request".to_owned());
        }
        let result = self.compact_for_model(None, &model, "pre-prompt").await;
        if self.is_compaction_delegated() { return Ok(()); }
        result?;
        if oversized()? { return Err("Compaction required before provider request".to_owned()); }
        Ok(())
    }

    async fn run_input_handlers(
        &self, text: &str, images: Option<Vec<ImageContent>>, source: Option<InputSource>,
        streaming_behavior: Option<StreamingBehavior>,
    ) -> Result<Option<(String, Option<Vec<ImageContent>>, String)>, String> {
        let input_id = format!("{}:{}", self.session_id(), self.reserve_queued_input_order());
        let mut runner = self.extension_runner.lock().await;
        let Some(runner) = runner.as_mut() else { return Ok(Some((text.to_owned(), images, input_id))); };
        let result = runner.emit_input(maho_ext_api::InputEvent {
            input_id: input_id.clone(),
            text: text.to_owned(), images: images.clone(), source: source.unwrap_or(InputSource::Interactive), streaming_behavior,
        }).await.map_err(|error| error.to_string())?;
        match result {
            maho_ext_api::InputEventResult::Continue => Ok(Some((text.to_owned(), images, input_id))),
            maho_ext_api::InputEventResult::Transform { text, images } => Ok(Some((text, images, input_id))),
            maho_ext_api::InputEventResult::Handled => {
                runner.emit(maho_ext_api::ExtensionEvent::InputDisposition {
                    input_id, disposition: maho_ext_api::InputDisposition::Handled,
                }).await.map_err(|error| error.to_string())?;
                Ok(None)
            }
        }
    }

    async fn queue_user_input(
        &self, text: &str, images: Option<Vec<ImageContent>>, mode: StreamingBehavior, options: QueuedInputOptions,
    ) -> Result<(), String> {
        if let Some(command_text) = text.strip_prefix('/') {
            let name = command_text.split_once(' ').map_or(command_text, |(name, _)| name);
            let runner = self.extension_runner.lock().await;
            if runner.as_ref().is_some_and(|runner| runner.get_command(name).is_some()) {
                return Err(format!("Extension command \"/{name}\" cannot be queued. Use prompt() or execute the command when not streaming."));
            }
        }
        let Some((text, images, input_id)) = self.run_input_handlers(text, images, options.source, self.is_streaming().then_some(mode)).await? else {
            return Ok(());
        };
        let text = self.expand_input(&text, true)?;
        self.queue_expanded_input(text, images, mode, options.enqueue_order);
        self.dispatch_extension_event(maho_ext_api::ExtensionEvent::InputDisposition {
            input_id, disposition: maho_ext_api::InputDisposition::Queued,
        }).await;
        Ok(())
    }

    fn queue_expanded_input(&self, text: String, images: Option<Vec<ImageContent>>, mode: StreamingBehavior, enqueue_order: Option<u64>) {
        self.record_queued_input(&text, mode, enqueue_order);
        let message = make_user_message(&text, images);
        let defer = {
            let state = self.state();
            state.prompt_start_pending && state.post_compaction_assistant_generation.is_some()
        };
        match mode {
            StreamingBehavior::Steer => {
                self.state().steering_messages.push(text);
                if defer { self.state().post_compaction_deferred_steering_messages.push(message); }
                else { self.agent.steer(message); }
            }
            StreamingBehavior::FollowUp => {
                self.state().follow_up_messages.push(text);
                if defer { self.state().post_compaction_deferred_follow_up_messages.push(message); }
                else { self.agent.follow_up(message); }
            }
        }
        self.emit_queue_update();
    }

    pub async fn steer(&self, text: &str, images: Option<Vec<ImageContent>>, options: QueuedInputOptions) -> Result<(), String> {
        self.queue_user_input(text, images, StreamingBehavior::Steer, options).await
    }

    pub async fn follow_up(&self, text: &str, images: Option<Vec<ImageContent>>, options: QueuedInputOptions) -> Result<(), String> {
        self.queue_user_input(text, images, StreamingBehavior::FollowUp, options).await
    }

    pub async fn abort(&self) {
        if let Some(pending) = self.begin_user_abort() { self.finish_user_abort(pending).await; }
    }

    fn begin_user_abort(&self) -> Option<bool> {
        self.user_abort_generation.fetch_add(1, Ordering::SeqCst);
        lock(&self.settled_delivery).cancel();
        self.state().user_aborted = true;
        self.state().abort_source = Some(maho_ext_api::AbortSource::User);
        if let Some(signal) = self.state().extension_event_signal.as_ref() { signal.abort(); }
        self.abort_retry();
        self.abort_compaction();
        self.abort_branch_summary();
        let streaming = self.is_streaming();
        let joined = self.state().abort_provenance.join(maho_ext_api::AbortSource::User, streaming);
        if !joined.abort_current_agent && joined.user_owned { return None; }
        let pending = !self.is_streaming() && (self.pending_message_count() > 0 || self.state().had_cleared_queued_messages);
        self.state().had_cleared_queued_messages = false;
        self.agent.suppress_queued_message_drain();
        self.agent.abort(None);
        Some(pending)
    }

    async fn finish_user_abort(&self, pending: bool) {
        self.agent.wait_for_idle().await;
        if pending {
            self.dispatch_extension_event(maho_ext_api::ExtensionEvent::SessionAbort).await;
            self.emit(AgentSessionEvent::SessionAbort);
        }
    }

    pub fn is_retrying(&self) -> bool { self.state().retry_attempt > 0 }

    pub fn is_compacting(&self) -> bool {
        let state = self.state();
        state.compaction_lifecycle.state().status() == "running" || state.compaction_abort_controller.is_some()
            || state.pending_compaction_admission.is_some() || state.branch_summary_abort_controller.is_some()
    }

    fn is_compaction_delegated(&self) -> bool {
        let model = self.model();
        self.state().delegated_compaction_key.as_ref().is_some_and(|(provider, id)| *provider == model.provider && *id == model.id)
    }

    pub fn compaction_state(&self) -> crate::compaction::lifecycle::CompactionLifecycleState {
        self.state().compaction_lifecycle.state().clone()
    }

    pub fn abort_compaction(&self) {
        if let Some(pending) = self.state().pending_compaction_admission.as_ref() { pending.controller.abort(); }
        if let Some(controller) = self.state().compaction_abort_controller.as_ref() { controller.abort(); }
        if let Some(signal) = self.state().compaction_extension_signal.as_ref() { signal.abort(); }
    }

    pub async fn compact(&self, instructions: Option<&str>) -> Result<crate::compaction::compaction::CompactionResult, String> {
        let pending = crate::compaction::lifecycle::CompactionAbortController::new();
        let pending_outcome = Arc::new(PendingCompactionAdmission {
            controller: pending.clone(), completed: std::sync::atomic::AtomicBool::new(false),
        });
        let _work = self.work_barrier.begin();
        {
            let mut state = self.state();
            if let Some(prior) = state.pending_compaction_admission.replace(pending_outcome.clone()) { prior.controller.abort(); }
            if let Some(prior) = &state.compaction_abort_controller { prior.abort(); }
            if let Some(signal) = &state.compaction_extension_signal { signal.abort(); }
        }
        let pending_guard = PendingCompactionAdmissionGuard { session: self, controller: pending.clone() };
        self.agent.abort(None);
        self.abort_retry();
        let signal = pending.signal();
        let admission = tokio::select! {
            biased;
            _ = signal.cancelled() => None,
            admission = async {
                self.agent.wait_for_idle().await;
                self.prompt_admission.lock().await
            } => Some(admission),
        };
        {
            let mut state = self.state();
            if state.pending_compaction_admission.as_ref().is_some_and(|current| current.controller.same(&pending)) {
                state.pending_compaction_admission = None;
            }
        }
        drop(pending_guard);
        let _admission = admission;
        let result = if pending.aborted() { Err("Compaction cancelled".to_owned()) }
            else { self.compact_for_model(instructions, &self.model(), "manual").await };
        pending_outcome.completed.store(result.is_ok(), Ordering::SeqCst);
        if result.is_ok() && self.agent.has_queued_messages() {
            let session = self.clone();
            let guard = self.work_barrier.begin();
            let generation = self.user_abort_generation.load(Ordering::SeqCst);
            tokio::spawn(async move {
                if let Err(error) = session.continue_session_internal(Some(generation)).await {
                    session.emit(AgentSessionEvent::ContinuationError { error_message: error });
                }
                drop(guard);
            });
        }
        result
    }

    async fn compact_for_model(&self, instructions: Option<&str>, budget_model: &Model, reason: &str)
        -> Result<crate::compaction::compaction::CompactionResult, String>
    {
        self.compact_for_model_with_retry(instructions, budget_model, reason, reason != "manual").await
    }

    async fn compact_for_model_with_retry(&self, instructions: Option<&str>, budget_model: &Model, reason: &str, will_retry: bool)
        -> Result<crate::compaction::compaction::CompactionResult, String>
    {
        use crate::compaction::compaction::{CompactionResult, prepare_compaction};
        let raw = self.with_settings_manager(|manager| manager.get_value("compaction").cloned());
        let configured = raw.map(serde_json::from_value::<crate::compaction_settings_access::CompactionSettings>)
            .transpose().map_err(|error| error.to_string())?;
        let mut resolved = crate::compaction_settings_resolver::resolve_compaction_settings(configured.as_ref(), Some(
            crate::compaction_settings_access::CompactionModelSelector { provider: &budget_model.provider, id: &budget_model.id },
        ))?;
        if self.pending_model_switch().is_some_and(|pending| models_are_equal(Some(&pending.model), Some(budget_model))) {
            let window = budget_model.context_window;
            let ratio = match window {
                0..=16_000 => 0.45, 16_001..=32_000 => 0.5, 32_001..=64_000 => 0.55,
                64_001..=128_000 => 0.6, 128_001..=512_000 => 0.7, _ => 0.8,
            };
            let keep = if window > 409_600 && resolved.keep_recent_tokens >= 10_000 {
                resolved.keep_recent_tokens.max((window / 20).min(60_000) as i64)
            } else { resolved.keep_recent_tokens };
            resolved.keep_recent_tokens = keep.min(((window as f64 * (1.0 - ratio - 0.05)).floor() as i64).max(1_024)).max(1);
        }
        let entries = self.with_session_manager(|manager| manager.branch(manager.leaf_id().or(Some(""))));
        let preparation = prepare_compaction(&entries, &crate::compaction::settings::CompactionSettings {
            enabled: resolved.enabled, reserve_tokens: resolved.reserve_tokens, keep_recent_tokens: resolved.keep_recent_tokens,
            ..crate::compaction::settings::default_compaction_settings()
        }, reason == "overflow", false);
        let request_id = uuid::Uuid::new_v4().to_string();
        let Some(preparation) = preparation else {
            let error = if entries.last().is_some_and(|entry| entry["type"] == "compaction") {
                "Already compacted"
            } else { "Nothing to compact (session too small)" };
            let compact_reason = if reason == "manual" { maho_ext_api::CompactionReason::Manual }
                else if reason == "overflow" { maho_ext_api::CompactionReason::Overflow }
                else if reason == "pre-prompt" { maho_ext_api::CompactionReason::PrePrompt }
                else { maho_ext_api::CompactionReason::Threshold };
            self.emit(AgentSessionEvent::CompactionStart { reason: compact_reason, request_id: Some(request_id.clone()) });
            self.emit(AgentSessionEvent::CompactionEnd { reason: compact_reason, request_id: Some(request_id),
                aborted: false, result: None, rejection_cause: None,
                error_message: Some(format!("Compaction failed: {error}")), accepted: Some(false), will_retry: false });
            self.dispatch_extension_event(maho_ext_api::ExtensionEvent::SessionCompactFailed {
                reason: compact_reason, error_message: Some(format!("Compaction failed: {error}")),
                aborted: false, will_retry: false, from_extension: false,
            }).await;
            return Err(error.to_owned());
        };
        let controller = crate::compaction::lifecycle::CompactionAbortController::new();
        let signal = controller.signal();
        let extension_signal = maho_ext_api::AbortSignal::default();
        self.state().compaction_extension_signal = Some(extension_signal.clone());
        self.state().compaction_abort_controller = Some(controller.clone());
        let compact_reason = if reason == "manual" { maho_ext_api::CompactionReason::Manual }
            else if reason == "overflow" { maho_ext_api::CompactionReason::Overflow }
            else if reason == "pre-prompt" { maho_ext_api::CompactionReason::PrePrompt }
            else { maho_ext_api::CompactionReason::Threshold };
        self.emit(AgentSessionEvent::CompactionStart { reason: compact_reason, request_id: Some(request_id.clone()) });
        let revision = self.message_revision();
        let request_id = self.state().compaction_lifecycle.begin(crate::compaction::lifecycle::BeginCompactionOperation {
            operation_id: request_id, stage: crate::compaction::lifecycle::CompactionStage::Execution,
            reason: reason.to_owned(), model: Some(crate::compaction::lifecycle::CompactionModelRef {
                provider: budget_model.provider.clone(), id: budget_model.id.clone(),
            }), started_revision: revision as i64,
        }, controller.clone());
        let _work = self.work_barrier.begin();
        let mut rejection = None;
        let mut rejection_aborted = false;
        let mut rejection_reason = None;
        let mut accepted_entry = None;
        let execution = async {
            let messages_before_extension = self.messages();
            let before = {
                let mut runner = self.extension_runner.lock().await;
                if let Some(runner) = runner.as_mut() {
                    let event = maho_ext_api::SessionBeforeCompactEvent {
                        reason: compact_reason, will_retry, request_id: request_id.clone(),
                        preparation: maho_ext_api::CompactionPreparation {
                            settings: maho_ext_api::CompactionSettings { enabled: resolved.enabled, reserve_tokens: resolved.reserve_tokens as u64,
                                keep_recent_tokens: resolved.keep_recent_tokens as u64 },
                            messages_to_summarize: preparation.messages_to_summarize.iter().cloned().map(session_message_from_value)
                                .collect::<Result<_, _>>().map_err(|error| error.to_string())?,
                            turn_prefix_messages: preparation.turn_prefix_messages.iter().cloned().map(session_message_from_value)
                                .collect::<Result<_, _>>().map_err(|error| error.to_string())?,
                            tokens_before: preparation.tokens_before as u64, first_kept_entry_id: preparation.first_kept_entry_id.clone(),
                            previous_summary: preparation.previous_summary.clone(),
                        }, branch_entries: entries.iter().cloned().map(session_entry_from_value).collect(),
                        custom_instructions: instructions.map(str::to_owned), signal: extension_signal.clone(),
                    };
                    let details = maho_ext_api::CompactionPreparationDetails {
                        preparation: event.preparation.clone(),
                        source_messages: Some(preparation.source_messages.iter().cloned().map(session_message_from_value)
                            .collect::<Result<_, _>>().map_err(|error| error.to_string())?),
                        turn_prefix_source_messages: Some(preparation.turn_prefix_source_messages.iter().cloned().map(session_message_from_value)
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
            let messages_after_extension = self.messages();
            let diagnostic_messages = if messages_after_extension.starts_with(&messages_before_extension) {
                messages_after_extension[messages_before_extension.len()..].iter().filter(|message| {
                    let AgentMessage::Custom(maho_agent::types::CustomAgentMessage::Custom(custom)) = message else { return false; };
                    custom.custom_type == "senpi.hook" && custom.details.as_ref().is_some_and(|details|
                        details["event"] == "PreCompact" && details["compactionRequestId"] == request_id)
                }).cloned().collect::<Vec<_>>()
            } else { Vec::new() };
            let (mut result, from_extension) = match before {
                maho_ext_api::EventResult::SessionBefore(maho_ext_api::SessionBeforeEventResult { cancel: Some(true), rejection_cause, reason, .. }) => {
                    rejection = Some(rejection_cause.unwrap_or(maho_ext_api::CompactionRejectionCause::CancelledByExtension));
                    rejection_aborted = true;
                    rejection_reason = reason.map(|reason| reason.trim().to_owned()).filter(|reason| !reason.is_empty());
                    return Err(rejection_reason.as_ref().map_or_else(|| describe_compaction_rejection(rejection.expect("cause")).to_owned(),
                        |reason| format!("Compaction rejected: {reason}")));
                }
                maho_ext_api::EventResult::SessionBefore(maho_ext_api::SessionBeforeEventResult { compaction: Some(result), .. }) => (CompactionResult {
                    summary: result.summary, first_kept_entry_id: result.first_kept_entry_id, tokens_before: result.tokens_before as i64,
                    details: result.details, usage: None, estimated_tokens_after: None,
                }, true),
                _ => {
                    let model = self.model();
                    let override_model = self.with_settings_manager(|manager| manager.get_value("compaction")
                        .and_then(|settings| settings.get("model")).and_then(Value::as_str).map(str::to_owned));
                    let model = override_model.as_deref().and_then(|selector| selector.split_once('/'))
                        .filter(|(provider, id)| !provider.is_empty() && !id.is_empty())
                        .and_then(|(provider, id)| self.model_runtime().get_model(provider, id)).unwrap_or(model);
                    signal.throw_if_aborted().map_err(|error| error.to_string())?;
                    if !self.state().compaction_lifecycle.is_current(&request_id, &controller) {
                        return Err("Compaction cancelled".to_owned());
                    }
                    let auth = self.get_summarization_request_auth(&model).await?;
                    let split = preparation.is_split_turn && !preparation.turn_prefix_messages.is_empty();
                    let messages = &preparation.messages_to_summarize;
                    let mut prompt = format!("<conversation>\n{}\n</conversation>\n\n",
                        crate::compaction::utils::serialize_conversation(messages));
                    if let Some(previous) = &preparation.previous_summary {
                        prompt.push_str(&format!("<previous-summary>\n{previous}\n</previous-summary>\n\n"));
                        prompt.push_str(&crate::compaction::compaction::update_summarization_prompt());
                    } else { prompt.push_str(crate::compaction::compaction::SUMMARIZATION_PROMPT); }
                    if let Some(instructions) = instructions.filter(|instructions| !instructions.is_empty()) {
                        prompt.push_str(&format!("\n\nAdditional focus: {instructions}"));
                    }
                    let AgentMessage::Llm(user) = make_user_message(&prompt, None) else { return Err("Invalid summary prompt".to_owned()); };
                    let context = maho_ai::types::Context { system_prompt: Some(crate::compaction::utils::SUMMARIZATION_SYSTEM_PROMPT.to_owned()),
                        messages: vec![user], tools: None };
                    let mut summary = "No prior history.".to_owned();
                    let mut summary_usage = None;
                    if !split || !messages.is_empty() {
                    let response = self.complete_summary_stream(&auth.model, &context, Some(maho_ai::types::StreamOptions {
                        request: maho_ai::types::ProviderRequestOptions { signal: Some(signal.clone()), api_key: auth.api_key.clone(),
                            headers: auth.headers.clone().map(|headers| headers.into_iter().map(|(key, value)| (key, Some(value))).collect()), env: auth.env.clone(),
                            ..Default::default() },
                        max_tokens: Some(((resolved.reserve_tokens as f64 * 0.8).floor() as u64).min(
                            if auth.model.max_tokens > 0 { auth.model.max_tokens } else { u64::MAX })),
                        session_id: Some(self.session_id()), ..Default::default()
                    })).await.map_err(|error| error.to_string())?;
                    if let Some(error) = crate::compaction::compaction::get_summarization_failure(&response, "Compaction") { return Err(error); }
                    if response.content.iter().any(|block| matches!(block, maho_ai::types::ContentBlock::ToolCall(_))) {
                        return Err("Summarization attempted to call a tool".to_owned());
                    }
                    summary = maho_ai::utils::text::content_text(&response.content, "");
                    if summary.trim().is_empty() { return Err("Compaction produced an empty summary".to_owned()); }
                    summary_usage = Some(response.usage);
                    }
                    if split {
                        let prompt = format!("<conversation>\n{}\n</conversation>\n\nThis is the PREFIX of a turn that was too large to keep. The SUFFIX (recent work) is retained.\n\nSummarize the prefix to provide context for the retained suffix:\n\n## Original Request\n[What did the user ask for in this turn?]\n\n## Early Progress\n- [Key decisions and work done in the prefix]\n\n## Context for Suffix\n- [Information needed to understand the retained recent work]\n\nBe concise. Focus on what's needed to understand the kept suffix.",
                            crate::compaction::utils::serialize_conversation(&preparation.turn_prefix_messages));
                        let AgentMessage::Llm(user) = make_user_message(&prompt, None) else { return Err("Invalid summary prompt".to_owned()); };
                        let response = self.complete_summary_stream(&auth.model, &maho_ai::types::Context {
                            system_prompt: Some(crate::compaction::utils::SUMMARIZATION_SYSTEM_PROMPT.to_owned()), messages: vec![user], tools: None,
                        }, Some(maho_ai::types::StreamOptions {
                            request: maho_ai::types::ProviderRequestOptions { signal: Some(signal.clone()), api_key: auth.api_key,
                                headers: auth.headers.map(|headers| headers.into_iter().map(|(key,value)| (key,Some(value))).collect()), env: auth.env,
                                ..Default::default() },
                            max_tokens: Some(((resolved.reserve_tokens as f64 * 0.5).floor() as u64).min(
                                if auth.model.max_tokens > 0 { auth.model.max_tokens } else { u64::MAX })),
                            session_id: Some(self.session_id()), ..Default::default()
                        })).await.map_err(|error| error.to_string())?;
                        if let Some(error) = crate::compaction::compaction::get_summarization_failure(&response, "Turn prefix summarization") { return Err(error); }
                        if response.content.iter().any(|block| matches!(block, maho_ai::types::ContentBlock::ToolCall(_))) {
                            return Err("Turn prefix summarization attempted to call a tool".to_owned());
                        }
                        summary.push_str("\n\n---\n\n**Turn Context (split turn):**\n\n");
                        summary.push_str(&maho_ai::utils::text::content_text(&response.content, ""));
                        summary_usage = Some(summary_usage.map_or_else(|| response.usage, |usage|
                            crate::compaction::compaction::combine_usage(&usage, &response.usage)));
                    }
                    let (read_files, modified_files) = crate::compaction::utils::compute_file_lists(&preparation.file_ops);
                    summary.push_str(&crate::compaction::utils::format_file_operations(&read_files, &modified_files));
                    (CompactionResult { summary, first_kept_entry_id: preparation.first_kept_entry_id.clone(),
                        tokens_before: preparation.tokens_before, details: Some(serde_json::json!({"readFiles":read_files,"modifiedFiles":modified_files})),
                        usage: summary_usage, estimated_tokens_after: None }, false)
                }
            };
            signal.throw_if_aborted().map_err(|error| error.to_string())?;
            if !self.state().compaction_lifecycle.is_current(&request_id, &controller) {
                return Err("Compaction cancelled".to_owned());
            }
            let current_messages = self.messages();
            if self.message_revision() != revision + diagnostic_messages.len() as u64
                || !current_messages.starts_with(&messages_before_extension)
                || current_messages[messages_before_extension.len()..] != diagnostic_messages
            {
                rejection = Some(maho_ext_api::CompactionRejectionCause::StaleRevision);
                return Err("Conversation changed during compaction".to_owned());
            }
            let entry = self.apply_compaction_internal(&result, Some(from_extension)).map_err(|error| {
                if error == "Compaction rejected: summary-would-overflow" { rejection = Some(maho_ext_api::CompactionRejectionCause::WouldOverflow); }
                error
            })?;
            result.estimated_tokens_after = Some(self.with_session_manager(|manager| manager.build_context(manager.leaf_id()))
                .messages.iter().map(crate::compaction::compaction::estimate_tokens).sum::<u64>() as i64);
            let mut messages = self.messages();
            if !diagnostic_messages.is_empty() {
                messages.retain(|message| {
                    let AgentMessage::Custom(maho_agent::types::CustomAgentMessage::Custom(custom)) = message else { return true; };
                    !(custom.custom_type == "senpi.hook" && custom.details.as_ref().is_some_and(|details|
                        details["event"] == "PreCompact" && details["compactionRequestId"] == request_id))
                });
                messages.extend(diagnostic_messages);
            }
            self.agent.set_messages(messages);
            accepted_entry = Some((entry, from_extension));
            Ok(result)
        }.await;
        let ended_revision = self.message_revision() as i64;
        let owns_terminal = self.state().compaction_lifecycle.finish(&crate::compaction::lifecycle::FinishCompactionOperation {
            operation_id: request_id.clone(), status: if signal.aborted() {
                crate::compaction::lifecycle::CompactionFinishStatus::Aborted
            } else if execution.is_ok() { crate::compaction::lifecycle::CompactionFinishStatus::Completed }
            else { crate::compaction::lifecycle::CompactionFinishStatus::Failed },
            ended_revision, rejection_cause: rejection.map(|cause| match cause {
                CompactionRejectionCause::CancelledByExtension => "cancelled-by-extension",
                CompactionRejectionCause::ExternalOwner => "external-owner",
                CompactionRejectionCause::WouldOverflow => "would-overflow",
                CompactionRejectionCause::CircuitBreaker => "circuit-breaker",
                CompactionRejectionCause::PerTurnCap => "per-turn-cap",
                CompactionRejectionCause::StaleRevision => "stale-revision",
            }.to_owned()), error_message: execution.as_ref().err().cloned(),
        });
        if !owns_terminal { return execution; }
        self.state().compaction_abort_controller = None;
        self.state().compaction_extension_signal = None;
        match &execution {
            Ok(result) => {
                self.state().delegated_compaction_key = None;
                let value = maho_ext_api::CompactionResult { summary: result.summary.clone(), first_kept_entry_id: result.first_kept_entry_id.clone(),
                    tokens_before: result.tokens_before as u64, details: result.details.clone() };
                self.emit(AgentSessionEvent::CompactionEnd { reason: compact_reason, request_id: Some(request_id.clone()), aborted: false,
                    result: Some(value), rejection_cause: None, error_message: None, accepted: Some(true), will_retry });
                if let Some((entry, from_extension)) = accepted_entry {
                    self.dispatch_extension_event(maho_ext_api::ExtensionEvent::SessionCompact(maho_ext_api::SessionCompactEvent::Accepted {
                        reason: compact_reason, request_id, compaction_entry: session_entry_from_value(entry),
                        from_extension, will_retry,
                    })).await;
                }
                let policy = self.with_settings_manager(|manager| crate::retry_fallback::settings::resolve_retry_fallback_settings(
                    manager.get_value("retry")).revert_policy);
                let mut guard = self.retry_fallback.lock().await;
                if let Some(controller) = guard.as_mut() {
                    let released = controller.notify_compaction_applied();
                    if released && !self.is_streaming() { controller.maybe_restore_primary(policy).await?; }
                }
            }
            Err(error) => {
                let error_message = if let Some(cause) = rejection {
                    if rejection_aborted && rejection_reason.is_none() { None }
                    else { Some(rejection_reason.as_ref().map_or_else(|| describe_compaction_rejection(cause).to_owned(),
                        |reason| format!("Compaction rejected: {reason}"))) }
                } else { (!signal.aborted()).then(|| format!("Compaction failed: {error}")) };
                self.emit(AgentSessionEvent::CompactionEnd { reason: compact_reason, request_id: Some(request_id.clone()),
                    aborted: signal.aborted() || rejection_aborted,
                    result: None, rejection_cause: rejection, error_message: error_message.clone(), accepted: Some(false), will_retry: false });
                if let Some(rejection_cause) = rejection {
                    if rejection_cause == maho_ext_api::CompactionRejectionCause::ExternalOwner {
                        let model = self.model();
                        self.state().delegated_compaction_key = Some((model.provider, model.id));
                    }
                    self.dispatch_extension_event(maho_ext_api::ExtensionEvent::SessionCompact(maho_ext_api::SessionCompactEvent::Rejected {
                        reason: compact_reason, request_id, rejection_cause,
                    })).await;
                } else {
                    self.dispatch_extension_event(maho_ext_api::ExtensionEvent::SessionCompactFailed {
                        reason: compact_reason, error_message, aborted: signal.aborted(), will_retry: false, from_extension: false,
                    }).await;
                }
            }
        }
        execution
    }

    pub fn apply_compaction(&self, result: &crate::compaction::compaction::CompactionResult) -> Result<Value, String> {
        self.apply_compaction_internal(result, None)
    }

    fn apply_compaction_internal(&self, result: &crate::compaction::compaction::CompactionResult, from_hook: Option<bool>) -> Result<Value, String> {
        let mut branch = self.with_session_manager(|manager| manager.branch(manager.leaf_id().or(Some(""))));
        if !branch.iter().any(|entry| entry.get("id").and_then(Value::as_str) == Some(result.first_kept_entry_id.as_str())) {
            return Err("Compaction first kept entry is not on the current branch".to_owned());
        }
        let model = self.model();
        let parent = branch.last().and_then(|entry| entry.get("id")).cloned().unwrap_or(Value::Null);
        let simulated_id = format!("simulated-{}", uuid::Uuid::new_v4());
        branch.push(serde_json::json!({"type":"compaction","id":simulated_id,"parentId":parent,
            "timestamp":"1970-01-01T00:00:00.000Z","summary":result.summary,
            "firstKeptEntryId":result.first_kept_entry_id,"tokensBefore":result.tokens_before,"details":result.details}));
        let context = crate::session_manager::build_session_context(&branch, Some(&simulated_id));
        let tokens = context.messages.iter().map(crate::compaction::compaction::estimate_tokens).sum::<u64>() as i64;
        let raw = self.with_settings_manager(|manager| manager.get_value("compaction").cloned());
        let settings = raw.map(serde_json::from_value::<crate::compaction_settings_access::CompactionSettings>)
            .transpose().map_err(|error| error.to_string())?;
        let settings = crate::compaction_settings_resolver::resolve_compaction_settings(settings.as_ref(), Some(
            crate::compaction_settings_access::CompactionModelSelector { provider: &model.provider, id: &model.id },
        ))?;
        let reserve = if settings.reserve_scaling_enabled {
            crate::compaction::compaction::resolve_reserve_tokens(model.context_window as f64, settings.reserve_tokens as f64) as i64
        } else { settings.reserve_tokens };
        if tokens > (model.context_window as i64).saturating_sub(reserve) {
            return Err("Compaction rejected: summary-would-overflow".to_owned());
        }
        let usage = result.usage.as_ref().map(serde_json::to_value).transpose().map_err(|error| error.to_string())?;
        let entry = self.with_session_manager_mut(|manager| manager.append_compaction(
            &result.summary, &result.first_kept_entry_id, result.tokens_before, result.details.clone(), usage, from_hook,
        ));
        let context = self.with_session_manager(|manager| manager.build_context(manager.leaf_id()));
        let mut messages = context.messages.into_iter().map(session_message_from_value).collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        messages.extend(self.state().messages_awaiting_persistence.iter().map(|(_, message)| message.clone()));
        self.agent.set_messages(messages);
        let mut state = self.state();
        state.message_revision += 1;
        state.post_compaction_assistant_generation = Some(state.assistant_generation);
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

    fn is_subscription_same_model_remint_error(&self, message: &maho_ai::types::AssistantMessage) -> bool {
        self.model().provider == "anthropic-subscription" && message.error_message.as_deref().is_some_and(|error|
            error.contains("Lock file is already being held") || error == "invalid_request")
    }

    async fn will_retry(&self, message: Option<&maho_ai::types::AssistantMessage>) -> bool {
        let Some(message) = message else { return false; };
        if !self.resolve_retry_profile().turn.enabled { return false; }
        if self.state().user_aborted || !self.with_settings_manager(|manager| manager.get_value("retry")
            .and_then(|settings| settings.get("enabled")).and_then(Value::as_bool).unwrap_or(true)) { return false; }
        if message.error_message.as_deref().is_some_and(|error| error.starts_with(
            maho_ai::utils::provider_failure_description::TURN_RETRY_SUPPRESSION_PREFIX))
            || maho_ai::utils::overflow::is_context_overflow(message, Some(self.model().context_window)) { return false; }
        if maho_ai::utils::retry::is_retryable_assistant_error(message)
            || maho_ai::utils::retry::is_provider_timeout_error(message)
            || maho_ai::utils::overflow::is_cursor_zero_token_resource_exhausted(
                &maho_ai::utils::overflow::CursorExhaustionProbe::from_message(message))
            || maho_ai::utils::overflow::is_cursor_quota_resource_exhausted(
                &maho_ai::utils::overflow::CursorExhaustionProbe::from_message(message), self.model().context_window)
            || self.is_subscription_same_model_remint_error(message)
            || maho_ai::utils::stop_details::is_classifier_refusal(message) { return true; }
        message.stop_reason == StopReason::Error
            && !(self.model().provider == "anthropic-subscription"
                && message.error_message.as_deref() == Some(maho_ai::auth::resolve::provider_not_configured_message("anthropic-subscription").as_str()))
            && !message.content.iter().any(|content| matches!(content, maho_ai::types::ContentBlock::ToolCall(_)))
            && self.retry_fallback.lock().await.as_mut().is_some_and(|controller| controller.can_try_fallback())
    }

    fn retire_failed_retry_assistant(&self, failed: &maho_ai::types::AssistantMessage) -> Result<(), String> {
        let mut messages = self.messages();
        if messages.last().and_then(AgentMessage::as_assistant) != Some(failed) { return Ok(()); }
        let failed_value = serde_json::to_value(messages.last().expect("assistant tail")).map_err(|error| error.to_string())?;
        self.with_session_manager_mut(|manager| {
            let branch = manager.branch(manager.leaf_id().or(Some("")));
            if let Some(entry) = branch.last().filter(|entry| entry.get("message") == Some(&failed_value)) {
                manager.set_leaf(entry.get("parentId").and_then(Value::as_str));
            }
        });
        messages.pop();
        self.agent.set_messages(messages);
        self.state().message_revision += 1;
        Ok(())
    }

    fn flush_post_compaction_deferred_messages(&self) -> bool {
        let (steering, follow_up) = {
            let mut state = self.state();
            (std::mem::take(&mut state.post_compaction_deferred_steering_messages),
                std::mem::take(&mut state.post_compaction_deferred_follow_up_messages))
        };
        let has_deferred = !steering.is_empty() || !follow_up.is_empty();
        for message in steering { self.agent.steer(message); }
        for message in follow_up { self.agent.follow_up(message); }
        has_deferred
    }

    fn consume_post_compaction_usage_exemption(&self) -> bool {
        let latest_entry = self.with_session_manager(|manager| manager.branch(manager.leaf_id()).iter().rev()
            .find(|entry| entry["type"] == "message" && entry["message"]["role"] == "assistant")
            .and_then(|entry| entry["id"].as_str()).map(str::to_owned));
        let mut state = self.state();
        latest_entry.is_some_and(|id| {
            if state.post_compaction_usage_exempt_entries.contains(&id) { return true; }
            let is_new = state.last_persisted_assistant.as_ref().is_some_and(|(entry, generation)|
                entry == &id && state.post_compaction_assistant_generation.is_some_and(|boundary| *generation > boundary));
            if is_new {
                state.post_compaction_assistant_generation = None;
                state.post_compaction_usage_exempt_entries.insert(id);
            }
            is_new
        })
    }

    async fn finish_provider_turn(&self) -> Result<(), String> {
        use crate::retry_fallback::controller::FallbackReason;
        use maho_ai::utils::retry_hint::parse_retry_after_ms_marker;
        let mut overflow_compacted = false;
        let rate_limit_pattern = regex::Regex::new(r"(?i)rate.?limit|(?:^429\s+\{|(?:\bHTTP/1\.[01]\s+|\bHTTP\s+|\bstatus(?:\s+code)?\s+|\berror\s+|\bcode\s+)429\b)|too many requests|resource.?exhausted")
            .expect("pinned rate limit pattern");
        loop {
            self.agent.wait_for_idle().await;
            let Some(message) = self.messages().iter().rev().find_map(AgentMessage::as_assistant).cloned() else { return Ok(()); };
            let assistant_pending_persistence = {
                let state = self.state();
                state.assistant_generation > state.last_persisted_assistant.as_ref().map_or(0, |(_, generation)| *generation)
            };
            let assistant_before_compaction = !assistant_pending_persistence && self.with_session_manager(|manager| {
                let branch = manager.branch(manager.leaf_id());
                let assistant = branch.iter().rposition(|entry| entry["type"] == "message" && entry["message"]["role"] == "assistant");
                let compaction = branch.iter().rposition(|entry| entry["type"] == "compaction");
                matches!((assistant, compaction), (Some(assistant), Some(compaction)) if assistant < compaction)
            });
            if assistant_before_compaction {
                let model = self.model();
                let raw = self.with_settings_manager(|manager| manager.get_value("compaction").cloned());
                let configured = raw.map(serde_json::from_value::<crate::compaction_settings_access::CompactionSettings>)
                    .transpose().map_err(|error| error.to_string())?;
                let settings = crate::compaction_settings_resolver::resolve_compaction_settings(configured.as_ref(), Some(
                    crate::compaction_settings_access::CompactionModelSelector { provider: &model.provider, id: &model.id },
                ))?;
                let reserve = crate::compaction::compaction::resolve_effective_reserve_tokens(
                    model.context_window as f64, settings.reserve_tokens as f64, Some(settings.reserve_scaling_enabled));
                let content_tokens: u64 = self.messages().iter().map(|message|
                    session_message_to_value(message).map(|value| crate::compaction::estimate_tokens(&value)))
                    .sum::<Result<u64, _>>().map_err(|error| error.to_string())?;
                if !settings.enabled || content_tokens as f64 <= model.context_window as f64 - reserve { return Ok(()); }
            }
            if message.stop_reason == StopReason::Stop && !overflow_compacted {
                let exempt = self.consume_post_compaction_usage_exemption();
                if exempt {
                    let has_deferred = self.flush_post_compaction_deferred_messages();
                    if has_deferred && !self.state().user_aborted {
                        self.agent.continue_with_queued_messages(Default::default()).await;
                        continue;
                    }
                    return Ok(());
                }
            }
            let model = self.model();
            let upstream_model_id = self.model_runtime().get_compatibility_request_config(&model).upstream_model_id;
            let same_overflow_source = message.provider == model.provider
                && (message.model == model.id || upstream_model_id.as_deref() == Some(message.model.as_str()));
            let current_context_needs_compaction = if same_overflow_source || !self.auto_compaction_enabled() { false } else {
                let raw = self.with_settings_manager(|manager| manager.get_value("compaction").cloned());
                let configured = raw.map(serde_json::from_value::<crate::compaction_settings_access::CompactionSettings>)
                    .transpose().map_err(|error| error.to_string())?;
                let resolved = crate::compaction_settings_resolver::resolve_compaction_settings(configured.as_ref(), Some(
                    crate::compaction_settings_access::CompactionModelSelector { provider: &model.provider, id: &model.id },
                ))?;
                let reserve = if resolved.reserve_scaling_enabled {
                    crate::compaction::compaction::resolve_reserve_tokens(model.context_window as f64, resolved.reserve_tokens as f64) as u64
                } else { resolved.reserve_tokens as u64 };
                self.get_context_usage().and_then(|usage| usage.tokens).is_some_and(|tokens| tokens > model.context_window.saturating_sub(reserve))
            };
            if (self.auto_compaction_enabled() || crate::compaction::is_turn_stuck_on_context_overflow(&message, self.model().context_window))
                && !self.is_compaction_delegated() && !self.state().user_aborted && !overflow_compacted
                && (same_overflow_source || current_context_needs_compaction)
                && maho_ai::utils::overflow::is_context_overflow(&message, Some(self.model().context_window))
            {
                self.flush_post_compaction_deferred_messages();
                overflow_compacted = true;
                let will_retry = message.stop_reason != StopReason::Stop;
                let execution = self.compact_for_model_with_retry(None, &self.model(), "overflow", will_retry).await;
                if self.is_compaction_delegated() { return Ok(()); }
                execution?;
                if !will_retry { return Ok(()); }
                let mut messages = self.messages();
                if messages.last().and_then(AgentMessage::as_assistant)
                    .is_some_and(|tail| matches!(tail.stop_reason, StopReason::Error | StopReason::Length))
                {
                    messages.pop();
                    self.agent.set_messages(messages);
                    self.state().message_revision += 1;
                }
                self.agent.continue_run(maho_agent::agent::AgentContinuationOptions { defer_queued_messages: Some(true), ..Default::default() }).await;
                continue;
            }
            if !self.will_retry(Some(&message)).await {
                if self.auto_compaction_enabled() && !self.state().user_aborted && !overflow_compacted
                    && message.stop_reason != StopReason::Aborted
                    && !maho_ai::utils::overflow::is_context_overflow(&message, Some(self.model().context_window))
                    && self.consume_post_compaction_usage_exemption()
                {
                    if self.flush_post_compaction_deferred_messages() {
                        self.agent.continue_with_queued_messages(Default::default()).await;
                        continue;
                    }
                    return Ok(());
                }
                if self.auto_compaction_enabled() && !self.is_compaction_delegated() && !self.state().user_aborted
                    && !matches!(message.stop_reason, StopReason::Error | StopReason::Aborted)
                {
                    let model = self.model();
                    let raw = self.with_settings_manager(|manager| manager.get_value("compaction").cloned());
                    let configured = raw.map(serde_json::from_value::<crate::compaction_settings_access::CompactionSettings>)
                        .transpose().map_err(|error| error.to_string())?;
                    let resolved = crate::compaction_settings_resolver::resolve_compaction_settings(configured.as_ref(), Some(
                        crate::compaction_settings_access::CompactionModelSelector { provider: &model.provider, id: &model.id },
                    ))?;
                    let reserve = if resolved.reserve_scaling_enabled {
                        crate::compaction::compaction::resolve_reserve_tokens(model.context_window as f64, resolved.reserve_tokens as f64) as u64
                    } else { resolved.reserve_tokens as u64 };
                    let threshold = model.context_window.saturating_sub(reserve);
                    if self.get_context_usage().and_then(|usage| usage.tokens).is_some_and(|tokens| tokens > threshold) {
                        let result = self.compact_for_model(None, &model, "threshold").await;
                        if !self.is_compaction_delegated() { result?; }
                    }
                }
                return Ok(());
            }
            if self.auto_compaction_enabled() && !self.is_compaction_delegated() {
                let raw = self.with_settings_manager(|manager| manager.get_value("compaction").cloned());
                let configured = raw.map(serde_json::from_value::<crate::compaction_settings_access::CompactionSettings>)
                    .transpose().map_err(|error| error.to_string())?;
                let resolved = crate::compaction_settings_resolver::resolve_compaction_settings(configured.as_ref(), Some(
                    crate::compaction_settings_access::CompactionModelSelector { provider: &model.provider, id: &model.id },
                ))?;
                let reserve = if resolved.reserve_scaling_enabled {
                    crate::compaction::compaction::resolve_reserve_tokens(model.context_window as f64, resolved.reserve_tokens as f64) as u64
                } else { resolved.reserve_tokens as u64 };
                if self.get_context_usage().and_then(|usage| usage.tokens).is_some_and(|tokens| tokens > model.context_window.saturating_sub(reserve)) {
                    self.retire_failed_retry_assistant(&message)?;
                    if let Err(error) = self.revalidate_continuation_admission(false).await {
                        let attempt = self.state().retry_attempt;
                        self.state().retry_attempt = 0;
                        self.reset_hint_tier_state();
                        self.emit(AgentSessionEvent::AutoRetryEnd { success: false, attempt, final_error: Some(error) });
                        return Ok(());
                    }
                }
            }
            let settings = self.with_settings_manager(|manager| manager.get_value("retry").cloned()).unwrap_or(Value::Null);
            let profile = self.resolve_retry_profile();
            let max_attempts = profile.turn.max_retries;
            let base_delay = profile.turn.backoff.base_delay_ms;
            let profile_cap = match &profile.turn.server_hint {
                maho_ai::utils::retry_profile::types::RetryServerHintPolicy::Override { ceiling, .. } => ceiling.max_delay_ms,
                maho_ai::utils::retry_profile::types::RetryServerHintPolicy::Tiered { .. } => Some(60_000),
            };
            let cap = settings.get("maxAgentDelayMs").and_then(Value::as_u64).or(profile_cap).unwrap_or(u64::MAX);
            let error = message.error_message.clone().unwrap_or_else(|| "Unknown error".to_owned());
            let hint = parse_retry_after_ms_marker(&error).or_else(||
                maho_ai::utils::retry_hint::extract_429_retry_after_ms(
                    &maho_ai::utils::retry_hint::RetryHintInput { body_text: &error, ..Default::default() },
                    Some(self.fallback_now() as i64),
                ));
            let refusal = maho_ai::utils::stop_details::is_classifier_refusal(&message);
            let same_model_remint = maho_ai::utils::overflow::is_cursor_zero_token_resource_exhausted(
                &maho_ai::utils::overflow::CursorExhaustionProbe::from_message(&message))
                || self.is_subscription_same_model_remint_error(&message);
            let quota_exhausted = maho_ai::utils::overflow::is_cursor_quota_resource_exhausted(
                &maho_ai::utils::overflow::CursorExhaustionProbe::from_message(&message), model.context_window);
            if quota_exhausted { self.retire_failed_retry_assistant(&message)?; }
            let transient = !quota_exhausted && (maho_ai::utils::retry::is_retryable_assistant_error(&message)
                || maho_ai::utils::retry::is_provider_timeout_error(&message));
            let attempt = self.state().retry_attempt.saturating_add(1);
            if same_model_remint && attempt > max_attempts {
                self.state().retry_attempt = 0;
                self.reset_hint_tier_state();
                if attempt > 1 {
                    self.emit(AgentSessionEvent::AutoRetryEnd { success: false, attempt, final_error: Some(error) });
                }
                return Ok(());
            }
            let rate_limited = !same_model_remint && !quota_exhausted && !maho_ai::utils::retry::is_provider_timeout_error(&message)
                && rate_limit_pattern.is_match(&error);
            let tier_routed = rate_limited && profile.fallback.rate_limited ==
                maho_ai::utils::retry_profile::types::FallbackRateLimited::Tiered;
            let hint_settings = crate::retry_fallback::settings::resolve_hint_policy_settings(Some(&settings));
            let tier = crate::retry_fallback::hint_policy::classify_rate_limited_wait(hint.map(|hint| hint as f64), hint_settings);
            let mut hint_delay = None;
            if tier_routed && tier == crate::retry_fallback::hint_policy::HintTier::Tier1InTurn {
                let mut state = self.state();
                let result = crate::retry_fallback::hint_policy::next_in_turn_delay_ms(
                    crate::retry_fallback::hint_policy::InTurnState {
                        probe_phase: state.probe_phase, hint_deadline_ms: state.hint_deadline_ms,
                        attempt, cumulative_hinted_wait_ms: state.cumulative_hinted_wait_ms,
                    }, hint.map(|hint| hint as f64), base_delay, hint_settings.hinted_wait_cap_ms, self.fallback_now(),
                );
                state.probe_phase = result.probe_phase;
                state.hint_deadline_ms = result.hint_deadline_ms;
                state.cumulative_hinted_wait_ms = result.cumulative_hinted_wait_ms;
                if !result.demote_to_probe_back { hint_delay = Some(result.delay_ms as u64); }
            }
            let needs_fallback = !same_model_remint && (refusal || !transient || attempt > max_attempts ||
                if tier_routed { hint_delay.is_none() } else { hint.is_some_and(|hint| hint > cap) });
            let mut switched = false;
            let mut probe_selector = None;
            if needs_fallback {
                let reason = if refusal { FallbackReason::Refusal } else if transient { FallbackReason::Transient }
                    else if crate::retry_fallback::billing::is_billing_error_message(Some(&error)) { FallbackReason::Billing }
                    else { FallbackReason::HardError };
                let mut controller = self.retry_fallback.lock().await;
                if let Some(controller) = controller.as_mut() {
                    switched = controller.try_fallback(reason, crate::retry_fallback::cooldown::SelectorFailure {
                        error_message: Some(&error), retry_after_ms: hint.map(|hint| hint as f64),
                    }).await?;
                    if switched && tier_routed && tier == crate::retry_fallback::hint_policy::HintTier::Tier2FallbackProbeBack {
                        probe_selector = controller.state.as_ref().map(|state| state.original_selector.clone());
                    }
                    if !switched && let Some(chain_key) = controller.exhausted_chain_key.clone() {
                        self.emit(AgentSessionEvent::RetryFallbackExhausted { chain_key, last_error: error.clone() });
                    }
                }
                if !switched && tier_routed && attempt <= max_attempts && !refusal {
                    match crate::retry_fallback::hint_policy::degrade_without_fallback(
                        tier, hint.map(|hint| hint as f64), attempt, base_delay, hint_settings.hinted_wait_cap_ms,
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
            if let Some(selector) = probe_selector { self.arm_probe_back(&selector, hint.unwrap_or(0) as f64); }
            let attempt = if switched { 1 } else { attempt };
            self.state().retry_attempt = attempt;
            let local_delay = maho_ai::utils::retry_profile::backoff::retry_backoff_delay_ms(
                &profile.turn.backoff, attempt, (self.retry_random)(),
            );
            let delay_ms = if switched { 0 } else {
                hint_delay.or(hint.filter(|hint| *hint > 0)).unwrap_or(local_delay as u64).min(cap)
            };
            let abort = maho_ai::utils::abort::AbortController::new();
            self.state().retry_abort_controller = Some(abort.clone());
            self.emit(AgentSessionEvent::AutoRetryStart { attempt, max_attempts, delay_ms, error_message: error });
            self.retire_failed_retry_assistant(&message)?;
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
            if let Err(error) = self.revalidate_continuation_admission(false).await {
                self.state().retry_attempt = 0;
                self.reset_hint_tier_state();
                self.emit(AgentSessionEvent::AutoRetryEnd { success: false, attempt, final_error: Some(error) });
                return Ok(());
            }
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

    fn resolve_retry_profile(&self) -> maho_ai::utils::retry_profile::types::RetryPolicyProfile {
        let provider = self.model_runtime().get_provider(&self.model().provider);
        self.with_settings_manager(|manager| manager.resolve_retry_profile(provider.as_deref()))
    }

    fn cancel_probe_back(&self) {
        lock(&self.probe_scheduler).cancel();
        if let Some(task) = lock(&self.probe_task).take() { task.abort(); }
    }

    fn arm_probe_back(&self, selector: &str, hint_ms: f64) {
        let Some((provider, id)) = selector.split_once('/') else { return; };
        if selector == format!("{}/{}", self.model().provider, self.model().id) { return; }
        let Some(model) = self.model_runtime().get_model(provider, id) else { return; };
        if !self.model_registry.has_configured_auth(&model) { return; }
        self.cancel_probe_back();
        let schedule = crate::retry_fallback::hint_policy::probe_back_schedule(hint_ms, self.fallback_now());
        let events = lock(&self.probe_scheduler).arm(crate::retry_fallback::probe_scheduler::ProbeBackPlan {
            selector: selector.to_owned(), first_at_ms: schedule.first_at_ms, deadline_ms: schedule.deadline_ms,
        });
        self.emit_probe_events(events);
        let weak = Arc::downgrade(&self.inner);
        *lock(&self.probe_task) = Some(tokio::spawn(async move {
            loop {
                let Some(inner) = weak.upgrade() else { return; };
                let session = AgentSession { inner };
                let Some(deadline) = lock(&session.probe_scheduler).next_deadline() else { return; };
                let delay = (deadline - session.fallback_now()).max(0.0);
                tokio::time::sleep(std::time::Duration::from_secs_f64(delay / 1000.0)).await;
                let (ticket, events) = lock(&session.probe_scheduler).begin_due_probe(session.fallback_now().max(deadline),
                    session.model_registry.has_configured_auth(&model));
                session.emit_probe_events(events);
                let Some(ticket) = ticket else { return; };
                let context = maho_ai::types::Context {
                    system_prompt: Some("Reply with OK.".to_owned()),
                    messages: vec![maho_ai::types::Message::User(maho_ai::types::UserMessage {
                        content: maho_ai::types::UserContent::Text("OK".to_owned()), timestamp: session.fallback_now() as i64,
                    })], tools: None,
                };
                let request = session.model_runtime().stream_simple(&model, &context, Some(maho_ai::types::SimpleStreamOptions {
                    stream: maho_ai::types::StreamOptions {
                        max_tokens: Some(1), request: maho_ai::types::ProviderRequestOptions { signal: Some(ticket.signal.clone()), ..Default::default() },
                        ..Default::default()
                    }, ..Default::default()
                }));
                let remaining = (schedule.deadline_ms - session.fallback_now()).max(0.0);
                let result = if ticket.index == 1 {
                    match tokio::time::timeout(std::time::Duration::from_secs_f64(remaining / 1000.0), request.result()).await {
                        Ok(result) => result.ok(),
                        Err(_) => continue,
                    }
                } else { request.result().await.ok() };
                let ok = result.is_some_and(|message| !matches!(message.stop_reason, StopReason::Error | StopReason::Aborted));
                let events = lock(&session.probe_scheduler).finish_probe(&ticket, ok);
                if ok && let Some(controller) = session.retry_fallback.lock().await.as_mut() { controller.cooldowns.clear(&format!("{}/{}", model.provider, model.id)); }
                session.emit_probe_events(events);
            }
        }));
    }

    fn emit_probe_events(&self, events: Vec<crate::retry_fallback::probe_scheduler::ProbeBackEvent>) {
        for event in events {
            match event {
                crate::retry_fallback::probe_scheduler::ProbeBackEvent::Scheduled { selector, at_ms, probe_index } =>
                    self.emit(AgentSessionEvent::RetryProbeScheduled { selector, at_ms: at_ms as u64, probe_index }),
                crate::retry_fallback::probe_scheduler::ProbeBackEvent::Result { selector, ok, error_message } =>
                    self.emit(AgentSessionEvent::RetryProbeResult { selector, ok, error_message }),
            }
        }
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
        let mut context = maho_ai::types::Context { system_prompt: Some(self.system_prompt()), messages: Vec::new(), tools: None };
        let prompt_tokens = maho_ai::utils::estimate::estimate_context_tokens(&context).tokens;
        context.tools = Some(self.agent.state().tools().iter().map(|tool| tool.tool.clone()).collect());
        let tools = maho_ai::utils::estimate::estimate_context_tokens(&context).tokens.saturating_sub(prompt_tokens);
        let family = |marker: &str| regex::Regex::new(&format!("(?:^|[/.:_-]){}(?:$|[^a-z0-9])", regex::escape(marker)))
            .expect("escaped family marker").is_match(&model.id.to_lowercase());
        let (margin, profile) = if model.provider == "anthropic" || family("claude") { (16_384, "anthropic") }
            else if model.provider == "openai" || ["gpt-5", "o1", "o3", "o4"].iter().any(|marker| family(marker)) { (16_384, "openai-reasoning") }
            else if model.provider == "google" || family("gemini") { (12_288, "google") }
            else if model.provider == "deepseek" || family("deepseek") { (12_288, "deepseek") }
            else { (8_192, "default") };
        let reserve = if settings.enabled {
            let configured = u64::try_from(settings.reserve_tokens).map_err(|error| error.to_string())?;
            if settings.reserve_scaling_enabled {
                crate::compaction::compaction::resolve_reserve_tokens(window as f64, configured as f64) as u64
            } else { configured }
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
        self.cancel_probe_back();
        self.set_model_internal(model, true, maho_ext_api::ModelSelectSource::Set, true).await
    }

    pub async fn set_session_model(&self, model: Model) -> Result<Option<SystemPromptChangeEvent>, String> {
        self.cancel_probe_back();
        self.set_model_internal(model, false, maho_ext_api::ModelSelectSource::Set, true).await
    }

    fn reduce_for_switch_target(&self, model: &Model, live: u64) -> Result<u64, String> {
        let entries = self.with_session_manager(|manager| manager.branch(manager.leaf_id().or(Some(""))));
        let measured = self.with_session_manager(|manager| manager.build_context(manager.leaf_id())).messages.iter()
            .map(crate::compaction::compaction::estimate_tokens).sum();
        let (budget, repairable) = self.model_budget(model, measured, false)?;
        if budget.shortfall_tokens == 0 || !repairable { return Ok(live); }
        let overhead = budget.required_tokens.saturating_sub(measured);
        let previous = entries.iter().rposition(|entry| entry["type"] == "compaction");
        let start = previous.map_or(0, |index| entries.iter().position(|entry| entry["id"] == entries[index]["firstKeptEntryId"])
            .unwrap_or(index + 1));
        let mut summary = "[Resume recovery checkpoint]\nThe restored conversation was larger than this model's context window, so older context was reduced without any provider request.\nThe complete transcript is still recorded in the session file. Continue from the retained messages and treat omitted details as unknown.".to_owned();
        if let Some(carried) = previous.and_then(|index| entries[index]["summary"].as_str()).map(str::trim).filter(|text| !text.is_empty()) {
            let note = "\n[Earlier checkpoint truncated]";
            let carried = if carried.encode_utf16().count() <= 8_000 { carried.to_owned() } else {
                let prefix: Vec<_> = carried.encode_utf16().take(8_000 - note.len()).collect();
                format!("{}{note}", String::from_utf16_lossy(&prefix))
            };
            summary.push_str(&format!("\n\nEarlier checkpoint:\n{carried}"));
        }
        let mut target = model.context_window.saturating_sub(overhead);
        let mut last_cut = None;
        while target >= 1_024 {
            let cut = crate::compaction::compaction::find_cut_point(&entries, start, entries.len(), target as i64).first_kept_entry_index;
            if last_cut != Some(cut) {
                last_cut = Some(cut);
                if let Some(id) = entries.get(cut).and_then(|entry| entry["id"].as_str()) {
                    let mut preview = entries.clone();
                    preview.push(serde_json::json!({"type":"compaction","id":"__senpi_resume_slice_preview__",
                        "parentId":entries.last().map(|entry| &entry["id"]),"timestamp":"1970-01-01T00:00:00.000Z",
                        "summary":summary,"firstKeptEntryId":id,"tokensBefore":measured,"fromHook":false}));
                    let after: u64 = crate::session_manager::build_session_context(&preview, Some("__senpi_resume_slice_preview__"))
                        .messages.iter().map(crate::compaction::compaction::estimate_tokens).sum();
                    if after.saturating_add(overhead) <= model.context_window {
                        self.with_session_manager_mut(|manager| manager.append_compaction(&summary, id, measured as i64,
                            Some(serde_json::json!({"schema":"senpi.compaction.resume-slice.v1","origin":"resume-admission"})), None, None));
                        let context = self.with_session_manager(|manager| manager.build_context(manager.leaf_id()));
                        self.agent.set_messages(context.messages.into_iter().map(session_message_from_value)
                            .collect::<Result<_, _>>().map_err(|error| error.to_string())?);
                        self.state().message_revision += 1;
                        self.emit(AgentSessionEvent::ResumeContextReduced { tokens_before: measured, tokens_after: after,
                            dropped_entries: cut.saturating_sub(start), notice: format!("Restored context of {measured} tokens exceeded this model's window, so older context was reduced to {after} tokens before the first prompt. The full transcript is preserved in the session file.") });
                        return Ok(after);
                    }
                }
            }
            target /= 2;
        }
        Ok(live)
    }

    async fn set_model_internal(&self, model: Model, persist_default: bool, source: maho_ext_api::ModelSelectSource,
        allow_deferral: bool) -> Result<Option<SystemPromptChangeEvent>, String>
    {
        self.switch_model_with_thinking(model, persist_default, source, allow_deferral, None).await
    }

    async fn switch_model_with_thinking(&self, model: Model, persist_default: bool, source: maho_ext_api::ModelSelectSource,
        allow_deferral: bool, thinking: Option<ModelThinkingLevel>) -> Result<Option<SystemPromptChangeEvent>, String>
    {
        let previous = self.model();
        let context_changed = !models_are_equal(Some(&previous), Some(&model))
            || previous.context_window != model.context_window || previous.api != model.api;
        let (current_budget, _) = self.model_budget(&previous, 0, false)?;
        let (target_budget, _) = self.model_budget(&model, 0, false)?;
        let live = if model.context_window.saturating_sub(target_budget.required_tokens)
            < previous.context_window.saturating_sub(current_budget.required_tokens) {
            let prefix = maho_ai::utils::estimate::estimate_context_tokens(&maho_ai::types::Context {
                system_prompt: Some(self.system_prompt()), messages: Vec::new(),
                tools: Some(self.agent.state().tools().iter().map(|tool| tool.tool.clone()).collect()),
            }).tokens;
            self.get_context_usage().and_then(|usage| usage.tokens).unwrap_or(0).saturating_sub(prefix)
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
        if context_changed {
            self.abort_compaction();
            self.abort_branch_summary();
            self.state().delegated_compaction_key = None;
            self.state().message_revision += 1;
        }
        if matches!(source, maho_ext_api::ModelSelectSource::Set | maho_ext_api::ModelSelectSource::Cycle)
            && let Some(controller) = self.retry_fallback.lock().await.as_mut()
        {
            let had_active_fallback = controller.state.is_some();
            controller.clear_for_manual_model_change(&model);
            if had_active_fallback { self.abort_retry(); }
        }
        let old_prompt = self.system_prompt();
        let old_thinking = self.thinking_level();
        let old_tier = self.service_tier();
        self.agent.set_model(model.clone());
        self.agent.set_thinking_level(self.get_thinking_for_model_switch(&model, thinking));
        let result = {
            let mut runner = self.extension_runner.lock().await;
            match runner.as_mut() {
                Some(runner) if context_changed => runner.emit_model_select(maho_ext_api::ModelSelectEvent {
                    model: model.clone(), previous_model: Some(previous.clone()), source,
                    system_prompt: old_prompt.clone(), system_prompt_options: self.extension_system_prompt_options(),
                }).await.map_err(|error| error.to_string()),
                _ => Ok(None),
            }
        };
        let change = match result {
            Ok(result) => {
                let prompt = result.as_ref().and_then(|result| result.system_prompt.clone())
                    .unwrap_or_else(|| Some(old_prompt.clone())).unwrap_or_else(|| self.state().base_system_prompt.clone());
                self.agent.set_system_prompt(prompt.clone());
                if prompt != old_prompt {
                    let system_prompt_name = result.as_ref().and_then(|result| result.system_prompt_name.clone());
                    self.dispatch_extension_event(maho_ext_api::ExtensionEvent::SystemPromptChange {
                        system_prompt: prompt.clone(), previous_system_prompt: old_prompt.clone(), system_prompt_name: system_prompt_name.clone(),
                        model: model.clone(), previous_model: Some(previous.clone()),
                    }).await;
                    self.emit(AgentSessionEvent::SystemPromptChange {
                        system_prompt: prompt.clone(), previous_system_prompt: old_prompt.clone(), system_prompt_name,
                        model: model.clone(), previous_model: Some(previous.clone()),
                    });
                }
                let (post_hook_budget, post_hook_repairable) = self.model_budget(&model, live, self.messages().is_empty())?;
                if allow_deferral && post_hook_budget.shortfall_tokens > 0 && post_hook_repairable {
                    self.agent.set_model(previous);
                    self.agent.set_system_prompt(old_prompt);
                    self.agent.set_thinking_level(old_thinking);
                    let notice = format!("{} needs {} fewer tokens than this conversation holds. It is compacted on your next message, and the switch applies after that.", model.id, post_hook_budget.shortfall_tokens);
                    self.state().pending_model_switch = Some(PendingModelSwitch {
                        model: model.clone(), budget: post_hook_budget.clone(), persist_default, notice: notice.clone(),
                    });
                    self.emit(AgentSessionEvent::ModelChangePending { model, budget: post_hook_budget, notice });
                    return Ok(None);
                }
                let admitted_live = if matches!(source, maho_ext_api::ModelSelectSource::Fallback | maho_ext_api::ModelSelectSource::FallbackRevert) {
                    self.reduce_for_switch_target(&model, live)?
                } else { live };
                let admission = self.assert_model_usable(&model, admitted_live);
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
        let reason = match source {
            maho_ext_api::ModelSelectSource::Fallback => Some("fallback"),
            maho_ext_api::ModelSelectSource::FallbackRevert => Some("fallback-revert"),
            _ => None,
        };
        self.with_session_manager_mut(|manager| manager.append_model_change(&model.provider, &model.id, reason,
            reason.map(|_| (previous.provider.as_str(), previous.id.as_str()))));
        if persist_default {
            self.with_settings_manager_mut(|manager| manager.set(crate::settings_manager::SettingsScope::Global,
                &Map::from_iter([("defaultProvider".to_owned(), Value::String(model.provider.clone())), ("defaultModel".to_owned(), Value::String(model.id.clone()))])))?;
        }
        self.state().pending_model_switch = None;
        self.state().current_service_tier = resolve_service_tier(&model, self.scoped_models().iter().find(|entry|
            models_are_equal(Some(&entry.model), Some(&model))).and_then(|entry| entry.service_tier));
        self.emit(AgentSessionEvent::ModelChanged { model: model.clone(), thinking_level: thinking_level_from_model_level(self.thinking_level()).unwrap_or(ThinkingLevel::Minimal), source });
        if old_tier != self.service_tier() { self.emit(AgentSessionEvent::ServiceTierChanged { tier: self.service_tier(), fast_mode: self.is_fast_mode_active() }); }
        Ok(change)
    }

    pub async fn cycle_model(&self, forward: bool) -> Result<Option<ModelCycleResult>, String> {
        self.cancel_probe_back();
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

    pub(crate) async fn runtime_before_switch(&self, reason: maho_ext_api::SessionReason, path: Option<String>) -> Result<bool, String> {
        Ok(self.session_before(maho_ext_api::ExtensionEvent::SessionBeforeSwitch { reason, target_session_file: path }).await?.cancel == Some(true))
    }

    pub async fn runtime_shutdown(&self, reason: maho_ext_api::SessionReason) { self.emit_session_shutdown(reason).await; }

    pub(crate) async fn runtime_shutdown_to(&self, reason: maho_ext_api::SessionReason, target_session_file: Option<String>) {
        self.dispatch_extension_event(maho_ext_api::ExtensionEvent::SessionShutdown(maho_ext_api::SessionShutdownEvent {
            reason, target_session_file, signal: Some(maho_ext_api::AbortSignal::default()),
        })).await;
    }

    pub(crate) async fn runtime_before_fork(&self, entry_id: &str, include_entry: bool) -> Result<bool, String> {
        Ok(self.session_before(maho_ext_api::ExtensionEvent::SessionBeforeFork {
            entry_id: entry_id.to_owned(), position: if include_entry { maho_ext_api::ForkPosition::At } else { maho_ext_api::ForkPosition::Before },
        }).await?.cancel == Some(true))
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
        self.ensure_extension_recreation().await?;
        self.abort().await;
        let previous = self.session_file();
        self.emit_session_shutdown(maho_ext_api::SessionReason::New).await;
        self.invalidate_extension_runtime().await;
        self.with_session_manager_mut(|manager| manager.new_session(options));
        self.finish_session_replacement(maho_ext_api::SessionReason::New, previous).await?;
        Ok(true)
    }

    pub async fn switch_session(&self, path: &str) -> Result<bool, String> {
        self.switch_session_with_cwd(path, None).await
    }

    pub async fn switch_session_with_cwd(&self, path: &str, cwd_override: Option<&str>) -> Result<bool, String> {
        let entries = crate::session_manager::load_entries_from_file(path);
        if entries.first().and_then(|entry| entry.get("type")).and_then(Value::as_str) != Some("session") {
            return Err(format!("Invalid session file: {path}"));
        }
        if self.session_before(maho_ext_api::ExtensionEvent::SessionBeforeSwitch {
            reason: maho_ext_api::SessionReason::Resume, target_session_file: Some(path.to_owned()),
        }).await?.cancel == Some(true) { return Ok(false); }
        self.ensure_extension_recreation().await?;
        self.abort().await;
        let previous = self.session_file();
        self.emit_session_shutdown(maho_ext_api::SessionReason::Resume).await;
        self.invalidate_extension_runtime().await;
        self.with_session_manager_mut(|manager| {
            if cwd_override.is_some() {
                *manager = crate::session_manager::SessionManager::open(path, None, cwd_override, None);
            } else {
                manager.set_session_file(path, None);
            }
        });
        self.state().cwd = self.with_session_manager(|manager| manager.cwd().to_owned());
        self.finish_session_replacement(maho_ext_api::SessionReason::Resume, previous).await?;
        Ok(true)
    }

    pub async fn fork(&self, entry_id: &str, include_entry: bool) -> Result<AssistantEditResult, String> {
        let entry = self.with_session_manager(|manager| manager.entry(entry_id)).ok_or_else(|| format!("Entry {entry_id} not found"))?;
        let position = if include_entry { maho_ext_api::ForkPosition::At } else { maho_ext_api::ForkPosition::Before };
        if self.session_before(maho_ext_api::ExtensionEvent::SessionBeforeFork { entry_id: entry_id.to_owned(), position }).await?.cancel == Some(true) {
            return Ok(AssistantEditResult { cancelled: true, ..Default::default() });
        }
        self.ensure_extension_recreation().await?;
        self.abort().await;
        let leaf = if include_entry { Some(entry_id) } else { entry.get("parentId").and_then(Value::as_str) };
        let entries = self.with_session_manager(|manager| if leaf.is_some() { manager.branch(leaf) } else { Vec::new() });
        let previous = self.session_file();
        self.emit_session_shutdown(maho_ext_api::SessionReason::Fork).await;
        self.invalidate_extension_runtime().await;
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
        if self.is_streaming() { return Err(crate::edited_assistant_message::SessionStreamingError.to_string()); }
        self.with_session_manager(|manager| crate::edited_assistant_message::assert_expected_leaf(
            options.expected_leaf_id.as_deref(), manager.leaf_id())).map_err(|error| error.to_string())?;
        let entry = self.with_session_manager(|manager| manager.entry(entry_id)).ok_or_else(|| format!("Entry {entry_id} not found"))?;
        if entry["type"] != "message" || entry["message"]["role"] != "assistant" {
            return Err(format!("Entry {entry_id} is not an assistant message"));
        }
        let message: maho_ai::types::AssistantMessage = serde_json::from_value(entry["message"].clone()).map_err(|error| error.to_string())?;
        let replacement = crate::edited_assistant_message::build_edited_assistant_message(&message, text).map_err(|error| error.to_string())?;
        if crate::edited_assistant_message::assistant_text_equals(&message, text) {
            return Ok(AssistantEditResult { unchanged: Some(true), ..Default::default() });
        }
        self.navigate_tree_internal(entry_id, options, Some(AgentMessage::Llm(maho_ai::types::Message::Assistant(Box::new(replacement))))).await
    }

    pub async fn edit_user_message(&self, entry_id: &str, text: &str, options: TreeNavigationOptions) -> Result<UserEditResult, String> {
        if self.is_streaming() { return Err(crate::edited_assistant_message::SessionStreamingError.to_string()); }
        self.with_session_manager(|manager| crate::edited_user_message::assert_expected_user_leaf(
            options.expected_leaf_id.as_deref(), manager.leaf_id())).map_err(|error| error.to_string())?;
        let entry = self.with_session_manager(|manager| manager.entry(entry_id)).ok_or_else(|| format!("Entry {entry_id} not found"))?;
        if entry["type"] != "message" || entry["message"]["role"] != "user" {
            return Err(format!("Entry {entry_id} is not a user message"));
        }
        let message: maho_ai::types::UserMessage = serde_json::from_value(entry["message"].clone()).map_err(|error| error.to_string())?;
        let replacement = crate::edited_user_message::build_edited_user_message(&message, text).map_err(|error| error.to_string())?;
        if crate::edited_user_message::user_text_equals(&message, text) {
            return Ok(UserEditResult { unchanged: Some(true), ..Default::default() });
        }
        self.navigate_tree_internal(entry_id, options, Some(AgentMessage::Llm(maho_ai::types::Message::User(replacement)))).await
    }

    async fn navigate_tree_internal(&self, target_id: &str, mut options: TreeNavigationOptions, replacement: Option<AgentMessage>) -> Result<AssistantEditResult, String> {
        if self.is_streaming() { return Err("Cannot navigate the session tree while streaming".to_owned()); }
        if self.is_compacting() { return Err("Cannot navigate the session tree while compacting".to_owned()); }
        let entry = self.with_session_manager(|manager| manager.entry(target_id)).ok_or_else(|| format!("Entry {target_id} not found"))?;
        let old_leaf = self.with_session_manager(|manager| manager.leaf_id().map(str::to_owned));
        if options.expected_leaf_id.is_some() && options.expected_leaf_id != old_leaf { return Err("Session leaf changed before edit".to_owned()); }
        let collected = self.with_session_manager(|manager| crate::compaction::branch_summarization::collect_entries_for_branch_summary(
            manager, old_leaf.as_deref(), target_id,
        ));
        let controller = maho_ai::utils::abort::AbortController::new();
        let navigation_signal = controller.signal();
        self.state().branch_summary_abort_controller = Some(controller);
        let signal = maho_ext_api::AbortSignal::default();
        let hook_signal = signal.clone();
        let abort_signal = navigation_signal.clone();
        let _bridge = AbortSignalBridge(tokio::spawn(async move { abort_signal.cancelled().await; hook_signal.abort(); }));
        let result = async {
        let before = self.session_before(maho_ext_api::ExtensionEvent::SessionBeforeTree {
            preparation: maho_ext_api::TreePreparation { target_id: target_id.to_owned(), old_leaf_id: old_leaf.clone(), common_ancestor_id: collected.common_ancestor_id,
                entries_to_summarize: collected.entries.iter().cloned().map(session_entry_from_value).collect(), user_wants_summary: options.summarize.unwrap_or(false), custom_instructions: options.custom_instructions.clone(),
                replace_instructions: options.replace_instructions, label: options.label.clone() }, signal,
        }).await?;
        if before.cancel == Some(true) { return Ok(AssistantEditResult { cancelled: true, ..Default::default() }); }
        if let Some(instructions) = before.custom_instructions { options.custom_instructions = Some(instructions); }
        if let Some(replace) = before.replace_instructions { options.replace_instructions = Some(replace); }
        if let Some(label) = before.label { options.label = Some(label); }
        let mut summary = if options.summarize == Some(true) { before.summary.clone() } else { None };
        let from_extension = summary.is_some();
        if options.summarize == Some(true) && !collected.entries.is_empty() && summary.is_none() {
            summary = Some(self.generate_branch_summary_with_signal(&collected.entries, options.custom_instructions.as_deref(),
                options.replace_instructions.unwrap_or(false), navigation_signal.clone()).await?);
            if navigation_signal.aborted() || summary.as_ref().is_some_and(|summary| summary["aborted"] == true) {
                return Ok(AssistantEditResult { cancelled: true, aborted: Some(true), ..Default::default() });
            }
        }
        if self.with_session_manager(|manager| manager.leaf_id().map(str::to_owned)) != old_leaf {
            return Err("Session leaf changed during tree navigation".to_owned());
        }
        let mut editor_text = None;
        let leaf = if replacement.is_some() { entry.get("parentId").and_then(Value::as_str).map(str::to_owned) }
            else if options.intent == Some(TreeNavigationIntent::Resume) { Some(target_id.to_owned()) }
            else if entry.get("message").and_then(|message| message.get("role")).and_then(Value::as_str) == Some("user") {
                editor_text = Some(user_message_text(&session_message_from_value(entry["message"].clone()).map_err(|error| error.to_string())?));
                entry.get("parentId").and_then(Value::as_str).map(str::to_owned)
            } else { Some(target_id.to_owned()) };
        self.with_session_manager_mut(|manager| manager.set_leaf(leaf.as_deref()));
        let summary_entry = summary.as_ref().and_then(|summary| summary.get("summary").and_then(Value::as_str))
            .map(|text| self.with_session_manager_mut(|manager| manager.append_branch_summary(old_leaf.as_deref().unwrap_or_default(), text,
                summary.as_ref().and_then(|summary| summary.get("details")).cloned(),
                summary.as_ref().and_then(|summary| summary.get("usage")).cloned(), Some(from_extension))));
        let replacement_entry = replacement.map(|message| serde_json::to_value(message).map(|message|
            self.with_session_manager_mut(|manager| manager.append_message(message)))).transpose().map_err(|error| error.to_string())?;
        if let Some(label) = options.label.as_deref() {
            let labelled = summary_entry.as_ref().or(replacement_entry.as_ref()).and_then(|entry| entry.get("id")).and_then(Value::as_str).unwrap_or(target_id);
            self.with_session_manager_mut(|manager| manager.append_label(labelled, Some(label)));
        }
        if options.intent == Some(TreeNavigationIntent::Resume) && replacement_entry.is_none() && (summary_entry.is_some() || options.label.is_some()) {
            self.with_session_manager_mut(|manager| manager.set_leaf(Some(target_id)));
        }
        self.rebuild_session_context()?;
        self.dispatch_extension_event(maho_ext_api::ExtensionEvent::SessionTree { new_leaf_id: self.with_session_manager(|manager| manager.leaf_id().map(str::to_owned)),
            old_leaf_id: old_leaf, summary_entry: summary_entry.clone().map(session_entry_from_value), from_extension: Some(from_extension) }).await;
        if options.intent == Some(TreeNavigationIntent::Resume)
            && self.with_session_manager(|manager| manager.leaf_id().map(str::to_owned)).as_deref() != Some(target_id)
        {
            self.with_session_manager_mut(|manager| manager.set_leaf(Some(target_id)));
            self.rebuild_session_context()?;
        }
        Ok(AssistantEditResult { editor_text, summary_entry, entry_id: replacement_entry.as_ref().and_then(|entry| entry.get("id")).and_then(Value::as_str).map(str::to_owned), ..Default::default() })
        }.await;
        self.state().branch_summary_abort_controller = None;
        result
    }

    pub fn abort_branch_summary(&self) {
        if let Some(controller) = &self.state().branch_summary_abort_controller { controller.abort(None); }
    }

    async fn generate_branch_summary_with_signal(&self, entries: &[Value], custom_instructions: Option<&str>, replace_instructions: bool,
        signal: maho_ai::utils::abort::AbortSignal) -> Result<Value, String> {
        use crate::compaction::{branch_summarization, utils};
        let extension_signal = maho_ext_api::AbortSignal::default();
        let hook_signal = extension_signal.clone();
        let provider_signal = signal.clone();
        let _bridge = AbortSignalBridge(tokio::spawn(async move { provider_signal.cancelled().await; hook_signal.abort(); }));
        let model = self.model();
        let reserve = self.with_settings_manager(|manager| manager.get_value("branchSummary")
            .and_then(|value| value.get("reserveTokens")).and_then(Value::as_i64)).unwrap_or(16_384);
        let preparation = branch_summarization::prepare_branch_entries(entries,
            if model.context_window == 0 { 128_000 } else { model.context_window as i64 } - reserve);
        if preparation.messages.is_empty() { return Ok(serde_json::json!({"summary":"No content to summarize"})); }
        let before = self.session_before(maho_ext_api::ExtensionEvent::SessionBeforeCompact(maho_ext_api::SessionBeforeCompactEvent {
            reason: maho_ext_api::CompactionReason::Branch, will_retry: false, request_id: uuid::Uuid::new_v4().to_string(),
            preparation: maho_ext_api::CompactionPreparation {
                settings: maho_ext_api::CompactionSettings { enabled: true, reserve_tokens: reserve as u64, keep_recent_tokens: 0 },
                messages_to_summarize: preparation.messages.iter().cloned().map(serde_json::from_value).collect::<Result<_,_>>()
                    .map_err(|error| error.to_string())?, turn_prefix_messages: Vec::new(), tokens_before: preparation.total_tokens as u64,
                first_kept_entry_id: entries.first().and_then(|entry| entry["id"].as_str()).unwrap_or_default().to_owned(), previous_summary: None,
            }, branch_entries: entries.iter().cloned().map(session_entry_from_value).collect(),
            custom_instructions: custom_instructions.map(str::to_owned), signal: extension_signal,
        })).await?;
        if before.cancel == Some(true) || signal.aborted() { return Ok(serde_json::json!({"aborted":true})); }
        if let Some(result) = before.compaction { return Ok(serde_json::json!({"summary":result.summary})); }
        let instructions = match custom_instructions {
            Some(instructions) if replace_instructions => instructions.to_owned(),
            Some(instructions) => format!("{}\n\nAdditional focus: {instructions}", branch_summarization::BRANCH_SUMMARY_PROMPT),
            None => branch_summarization::BRANCH_SUMMARY_PROMPT.to_owned(),
        };
        let transcript = utils::serialize_conversation(&crate::messages::convert_to_llm(&preparation.messages));
        let prompt = format!("<conversation>\n{transcript}\n</conversation>\n\n{instructions}");
        let auth = self.get_summarization_request_auth(&model).await?;
        let AgentMessage::Llm(message) = make_user_message(&prompt, None) else { return Err("Invalid branch summary prompt".to_owned()); };
        let response = self.complete_summary_stream(&auth.model, &maho_ai::types::Context {
            system_prompt: Some(utils::SUMMARIZATION_SYSTEM_PROMPT.to_owned()), messages: vec![message], tools: None,
        }, Some(maho_ai::types::StreamOptions {
            request: maho_ai::types::ProviderRequestOptions { api_key: auth.api_key, env: auth.env,
                signal: Some(signal),
                headers: auth.headers.map(|headers| headers.into_iter().map(|(key, value)| (key, Some(value))).collect()), ..Default::default() },
            extra_body: self.model_runtime().get_compatibility_request_config(&model).extra_body,
            max_tokens: Some(if model.max_tokens == 0 { 4096 } else { model.max_tokens.min(4096) }),
            ..Default::default()
        })).await.map_err(|error| error.to_string())?;
        if response.stop_reason == maho_ai::types::StopReason::Aborted { return Ok(serde_json::json!({"aborted":true})); }
        if let Some(error) = crate::compaction::compaction::get_summarization_failure(&response, "Branch summarization") { return Err(error); }
        if response.content.iter().any(|content| matches!(content, maho_ai::types::ContentBlock::ToolCall(_))) {
            return Err("Branch summarization attempted to call a tool".to_owned());
        }
        let (read_files, modified_files) = utils::compute_file_lists(&preparation.file_ops);
        let summary = format!("{}{}{}", branch_summarization::BRANCH_SUMMARY_PREAMBLE,
            utils::content_text_for_summary(&serde_json::to_value(&response.content).map_err(|error| error.to_string())?, ""),
            utils::format_file_operations(&read_files, &modified_files));
        Ok(serde_json::json!({"summary":summary,"usage":response.usage,"details":{"readFiles":read_files,"modifiedFiles":modified_files}}))
    }

    pub fn set_prompt_resources(&self, templates: Vec<crate::prompt_templates::PromptTemplate>, skills: Vec<crate::skills::Skill>) {
        let mut state = self.state(); state.prompt_templates = templates; state.skills = skills;
    }

    pub fn prompt_templates(&self) -> Vec<crate::prompt_templates::PromptTemplate> { self.state().prompt_templates.clone() }

    pub async fn generate_session_title_if_needed(&self, prompt: &str) {
        if self.session_name().is_some() || crate::session_title_generator::should_skip_session_title(prompt) { return; }
        let controller = maho_ai::utils::abort::AbortController::new();
        {
            let mut state = self.state();
            if state.disposed || state.session_title_abort_controller.is_some() { return; }
            state.session_title_abort_controller = Some(controller.clone());
        }
        let signal = controller.signal();
        let session_id = self.session_id();
        let generation = async {
            let model = self.model();
            let auth = self.get_summarization_request_auth(&model).await?;
            let context = crate::session_title_generator::build_title_context(prompt);
            let options = maho_ai::types::StreamOptions {
                request: maho_ai::types::ProviderRequestOptions { api_key: auth.api_key, signal: Some(signal.clone()),
                    headers: auth.headers.map(|headers| headers.into_iter().map(|(key,value)| (key,Some(value))).collect()),
                    stream_kind: Some(maho_ai::types::StreamKind::Auxiliary), env: auth.env,
                    timeout_ms: self.agent.timeout_ms(), max_retry_delay_ms: self.agent.max_retry_delay_ms(), ..Default::default() },
                transport: self.agent.transport(), max_tokens: Some(64), session_id: Some(session_id.clone()),
                cache_retention: Some(if auth.model.cache_retention == Some(maho_ai::types::CacheRetention::None) {
                    maho_ai::types::CacheRetention::None
                } else { maho_ai::types::CacheRetention::Short }), ..Default::default()
            };
            let retry = self.with_settings_manager(|manager| manager.get_value("retry").cloned());
            let policy = maho_ai::utils::retry::RetryPolicy {
                enabled: retry.as_ref().and_then(|settings| settings.get("enabled")).and_then(Value::as_bool).unwrap_or(true),
                max_retries: retry.as_ref().and_then(|settings| settings.get("maxRetries")).and_then(Value::as_u64).unwrap_or(3).min(1) as u32,
                base_delay_ms: retry.as_ref().and_then(|settings| settings.get("baseDelayMs")).and_then(Value::as_u64).unwrap_or(2_000).min(2_000),
                max_agent_delay_ms: retry.as_ref().and_then(|settings| settings.get("maxAgentDelayMs")).and_then(Value::as_u64),
                random: Some(self.retry_random.clone()),
            };
            let response = maho_ai::utils::retry::retry_transient_call(|| async {
                let response = self.model_runtime().complete(&auth.model, &context, Some(options.clone())).await.map_err(|error| error.to_string())?;
                if let Some(error) = crate::session_title_generator::title_error_message(&response) { return Err(error); }
                Ok(response)
            }, |error: &String| maho_ai::utils::retry::is_retryable_error_message(error), Some(&policy), Some(&signal), None).await?;
            Ok::<_, String>(crate::session_title_generator::parse_session_title(&response))
        }.await;
        if signal.aborted() { return; }
        self.state().session_title_abort_controller = None;
        match generation {
            Ok(Some(title)) if self.session_id() == session_id && self.session_name().is_none() => self.set_session_name(&title),
            Ok(_) => {}, Err(error) => {
                let error = ExtensionError { extension_path: "<runtime>".to_owned(), event: "session_title_generation".to_owned(), error, stack: None };
                if let Some(runner) = self.extension_runner.lock().await.as_mut() { runner.emit_error(error.clone()); }
                let listener = self.state().extension_error_listener.clone();
                if let Some(listener) = listener { listener(&error); }
            },
        }
    }

    pub async fn native_tool_renderers_snapshot<TState: 'static, TArgs: 'static>(&self) -> BTreeMap<String, Arc<maho_ext_api::ToolRenderers<TState, TArgs>>> {
        self.extension_runner.lock().await.as_ref().map(|runner| runner.native_tool_renderers_snapshot()).unwrap_or_default()
    }

    pub async fn user_bash_hook(&self, command: &str, exclude_from_context: bool) -> Result<maho_ext_api::EventResult, String> {
        let Some(mut runner) = self.extension_runner.lock().await.clone() else { return Ok(maho_ext_api::EventResult::None); };
        runner.emit_user_bash(command.to_owned(), exclude_from_context, self.cwd().into()).await.map_err(|error| error.message)
    }

    pub(crate) async fn replacement_extension_runner(&self, target: &AgentSession) -> Result<Option<maho_ext_host::runner::ExtensionRunner>, String> {
        let runner = self.extension_runner.lock().await.clone();
        match runner {
            Some(runner) => runner.recreate_with_context(crate::sdk::extension_context::create(target)).await
                .map(Some).map_err(|error| error.message),
            None => Ok(None),
        }
    }

    pub async fn execute_user_bash(&self, command: &str, on_chunk: Option<maho_tools::bash_executor::BashChunkCallback>,
        exclude_from_context: bool, id: Option<String>) -> Result<maho_tools::bash_executor::BashResult, String> {
        match self.user_bash_hook(command, exclude_from_context).await? {
            maho_ext_api::EventResult::UserBash { result: Some(result), .. } => {
                self.record_bash_result(command, &result, exclude_from_context);
                Ok(result)
            }
            maho_ext_api::EventResult::UserBash { operations, result: None } =>
                self.execute_bash(command, on_chunk, exclude_from_context, id, operations).await,
            _ => self.execute_bash(command, on_chunk, exclude_from_context, id, None).await,
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
        let (snippets, guidelines) = {
            let state = self.state();
            let mut contributors = active.clone();
            for name in &state.withheld_eval_only_tool_names {
                if state.tool_registry.contains_key(name) && !contributors.contains(name) { contributors.push(name.clone()); }
            }
            let snippets = contributors.iter().filter_map(|name| state.tool_prompt_snippets.get(name).map(|snippet| (name.clone(), snippet.clone()))).collect();
            let guidelines = contributors.iter().filter_map(|name| state.tool_prompt_guidelines.get(name)).flatten().cloned().collect();
            (snippets, guidelines)
        };
        let (custom, append) = self.system_prompt_sources();
        let custom_prompt = crate::resource_loader::resolve_prompt_input(custom.as_deref(), "system prompt");
        let append: Vec<_> = append.iter().filter_map(|source| crate::resource_loader::resolve_prompt_input(Some(source), "append system prompt")).collect();
        let base = crate::system_prompt::build_system_prompt(&crate::system_prompt::BuildSystemPromptOptions {
            custom_prompt, append_system_prompt: (!append.is_empty()).then(|| append.join("\n\n")),
            cwd: self.cwd(), selected_tools: Some(active), skills: Some(skills),
            tool_snippets: Some(snippets), prompt_guidelines: Some(guidelines),
            context_files: Some(self.project_context_files()),
        });
        self.state().base_system_prompt = base.clone();
        let prompt = self.state().system_prompt_override.clone().unwrap_or(base);
        self.agent.set_system_prompt(prompt);
    }

    fn extension_system_prompt_options(&self) -> maho_ext_api::BuildSystemPromptOptions {
        let (custom, append) = self.system_prompt_sources();
        let append: Vec<_> = append.iter().filter_map(|source| crate::resource_loader::resolve_prompt_input(Some(source), "append system prompt")).collect();
        maho_ext_api::BuildSystemPromptOptions {
            custom_prompt: crate::resource_loader::resolve_prompt_input(custom.as_deref(), "system prompt"),
            append_system_prompt: (!append.is_empty()).then(|| append.join("\n\n")),
            context_files: self.project_context_files().into_iter()
                .map(|file| maho_ext_api::ContextFile { path: file.path, content: file.content }).collect(),
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
            }).collect(),
        }
    }

    pub async fn reload(&self) -> Result<bool, String> {
        if self.session_before(maho_ext_api::ExtensionEvent::SessionBeforeReload).await?.cancel == Some(true) { return Ok(false); }
        self.ensure_extension_recreation().await?;
        self.abort().await;
        self.emit_session_shutdown(maho_ext_api::SessionReason::Reload).await;
        self.invalidate_extension_runtime().await;
        self.with_settings_manager_mut(|manager| manager.reload());
        self.state().discovered_resources = maho_ext_api::DiscoveredResources::default();
        let (prompt_paths, skill_paths) = self.with_settings_manager(|manager| {
            let paths = |key| manager.get_value(key).and_then(Value::as_array).map(|values|
                values.iter().filter_map(Value::as_str).map(str::to_owned).collect::<Vec<_>>()).unwrap_or_default();
            (paths("prompts"), paths("skills"))
        });
        let templates = crate::prompt_templates::load_prompt_templates(&crate::prompt_templates::LoadPromptTemplatesOptions {
            cwd: self.cwd(), agent_dir: self.agent_dir(), prompt_paths, include_defaults: true,
        });
        let skills = crate::skills::load_skills(&crate::skills::LoadSkillsOptions {
            cwd: self.cwd(), agent_dir: self.agent_dir(), skill_paths, include_defaults: true,
        });
        self.set_prompt_resources(templates, skills.skills);
        self.rebuild_system_prompt();
        self.publish_eval_only_tool_hints();
        self.renew_extension_runtime(maho_ext_api::SessionReason::Reload).await?;
        self.dispatch_extension_event(maho_ext_api::ExtensionEvent::SessionStart(maho_ext_api::SessionStartEvent {
            reason: maho_ext_api::SessionReason::Reload, initial_model_provenance: None, previous_session_file: self.session_file(),
        })).await;
        self.extend_resources_from_extensions(maho_ext_api::SessionReason::Reload).await;
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
                if blocks.len() >= 5 { break; }
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
            } else if token.position == crate::skill_invocation::SkillInvocationPosition::Leading {
                break;
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
        let mut runner = self.extension_runner.lock().await.clone();
        let Some(runner) = runner.as_mut() else { return Ok(false); };
        let Some(command) = runner.get_command(name) else { return Ok(false); };
        let info = &command.command.source_info;
        self.emit(AgentSessionEvent::CommandInvocation { command: serde_json::json!({"name":name,"source":"extension","syntax":"slash",
            "sourceInfo":{"path":info.path,"source":info.source,"scope":match info.scope {
                maho_ext_api::SourceScope::User => "user", maho_ext_api::SourceScope::Project => "project",
                maho_ext_api::SourceScope::Temporary => "temporary", maho_ext_api::SourceScope::System => "system",
            },"origin":match info.origin { maho_ext_api::SourceOrigin::Package => "package", maho_ext_api::SourceOrigin::TopLevel => "top-level" },
                "baseDir":info.base_dir}
        }) });
        let context = runner.create_command_context(Arc::new(SessionExtensionActions(Arc::downgrade(&self.inner)))).map_err(|error| error.to_string())?;
        if let Err(error) = runner.invoke_command(name, args, &context).await {
            let error = ExtensionError { extension_path: format!("command:{name}"), event: "command".to_owned(),
                error: error.message, stack: error.stack };
            runner.emit_error(error.clone());
            let listener = self.state().extension_error_listener.clone();
            if let Some(listener) = listener { listener(&error); }
        }
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

    async fn complete_summary_stream(&self, model: &Model, context: &maho_ai::types::Context,
        options: Option<maho_ai::types::StreamOptions>) -> Result<maho_ai::types::AssistantMessage, maho_ai::utils::event_stream::StreamError> {
        use crate::compaction::stream_watchdog;
        let mut options = options.unwrap_or_default();
        let override_ms = self.with_settings_manager(|settings| settings.get_value("compaction")
            .and_then(|value| value.get("summarizationMaxDurationMs")).and_then(Value::as_f64));
        let tokens = maho_ai::utils::estimate::estimate_context_tokens(context).tokens;
        let duration = std::time::Duration::from_secs_f64(stream_watchdog::summarization_max_duration_ms(tokens as f64, override_ms) / 1000.0);
        let duration_ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
        let caller = options.request.signal.clone();
        options.request.affinity_session_id = options.request.affinity_session_id.clone().or_else(|| options.session_id.clone());
        options.session_id = Some(uuid::Uuid::new_v4().to_string());
        options.cache_retention.get_or_insert(maho_ai::types::CacheRetention::None);
        let retry = self.with_settings_manager(|manager| manager.get_value("retry").cloned());
        let policy = maho_ai::utils::retry::RetryPolicy {
            enabled: retry.as_ref().and_then(|settings| settings.get("enabled")).and_then(Value::as_bool).unwrap_or(true),
            max_retries: retry.as_ref().and_then(|settings| settings.get("maxRetries")).and_then(Value::as_u64).unwrap_or(3) as u32,
            base_delay_ms: retry.as_ref().and_then(|settings| settings.get("baseDelayMs")).and_then(Value::as_u64).unwrap_or(2_000),
            max_agent_delay_ms: retry.as_ref().and_then(|settings| settings.get("maxAgentDelayMs")).and_then(Value::as_u64),
            random: Some(self.retry_random.clone()),
        };
        maho_ai::utils::retry::retry_transient_call(|| {
            let mut options = options.clone();
            let caller = caller.clone();
            async move {
                let controller = maho_ai::utils::abort::AbortController::new();
                let listener = caller.as_ref().map(|signal| {
                    let request = controller.clone();
                    let listener = signal.add_abort_listener(move |reason| request.abort(Some(reason.clone())));
                    if signal.aborted() { controller.abort(signal.reason()); }
                    listener
                });
                options.request.signal = Some(controller.signal());
                let stream = self.model_runtime().stream(model, context, Some(options));
                let result = stream_watchdog::consume_stream_with_idle_timeout(&stream,
                    u64::try_from(stream_watchdog::DEFAULT_SUMMARIZATION_IDLE_TIMEOUT_MS).unwrap_or_default(),
                    Some(duration_ms), caller.as_ref(), || controller.abort(None)).await;
                if let (Some(signal), Some(listener)) = (caller, listener) { signal.remove_abort_listener(listener); }
                let response = result.map_err(|error| error.to_string())?;
                if response.stop_reason == maho_ai::types::StopReason::Error {
                    return Err(response.error_message.clone().unwrap_or_else(|| "Summarization failed".into()));
                }
                Ok(response)
            }
        }, |error: &String| maho_ai::utils::retry::is_retryable_error_message(error),
            Some(&policy), caller.as_ref(), None).await.map_err(maho_ai::utils::event_stream::StreamError::new)
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

    pub(crate) fn shared_settings_manager(&self) -> Arc<Mutex<SettingsManager>> { self.settings_manager.clone() }

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
        !state.is_streaming && state.pending_tool_calls.is_empty() && !self.work_barrier.has_active_work()
            && !self.is_compacting() && !self.is_retrying()
    }

    pub fn system_prompt(&self) -> String {
        self.agent.state().system_prompt
    }

    pub fn set_system_prompt_sources(&self, custom: Option<String>, append: Vec<String>) {
        let mut state = self.state();
        state.custom_system_prompt_source = custom;
        state.append_system_prompt_sources = append;
        drop(state);
        self.rebuild_system_prompt();
    }

    pub fn set_context_files_enabled(&self, enabled: bool) {
        self.state().context_files_enabled = enabled;
        self.rebuild_system_prompt();
    }

    fn project_context_files(&self) -> Vec<crate::system_prompt::ContextFile> {
        if self.state().context_files_enabled {
            crate::resource_loader::load_project_context_files(&self.cwd(), &self.agent_dir())
        } else {
            Vec::new()
        }
    }

    pub(crate) fn system_prompt_sources(&self) -> (Option<String>, Vec<String>) {
        let state = self.state();
        (state.custom_system_prompt_source.clone(), state.append_system_prompt_sources.clone())
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
            is_compacting: self.is_compacting(),
            is_bash_running: self.is_bash_running(),
            has_session_work: self.work_barrier.has_active_work() || self.is_retrying(),
            has_active_wake_source: self.state().wake_sources.has_active(),
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

    pub(crate) fn replacement_custom_tools(&self) -> Vec<ToolDefinition> {
        self.state().custom_tools.clone()
    }

    pub(crate) fn replacement_auto_title(&self) -> bool {
        self.state().auto_title_sessions
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
        for name in filtered {
            if let Some(tool) = self.state().tool_registry.get(&name).cloned() {
                tools.push(tool);
            }
        }
        let changed = self.get_active_tool_names() != tools.iter().map(|tool| tool.tool.name.clone()).collect::<Vec<_>>();
        self.agent.set_tools(tools);
        let mut state = self.state();
        state.withheld_eval_only_tool_names = withheld;
        state.requested_active_tool_names = Some(requested);
        drop(state);
        self.rebuild_system_prompt();
        if changed {
            self.abort_compaction();
            self.state().message_revision += 1;
        }
    }

    /// The active-tool selection as requested by callers, before eval-only filtering.
    pub fn requested_active_tool_names(&self) -> Option<Vec<String>> {
        self.state().requested_active_tool_names.clone()
    }

    /// Register a tool definition and its executable tool; used by the runtime build and by SDK
    /// custom tools.
    pub fn register_tool_definition(&self, definition: ToolDefinition, source_info: SourceInfo, tool: AgentTool) {
        let mut state = self.state();
        if let Some(snippet) = definition.prompt_snippet.as_deref().map(|snippet| snippet.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|snippet| !snippet.is_empty()) { state.tool_prompt_snippets.insert(definition.name.clone(), snippet); }
        if let Some(guidelines) = &definition.prompt_guidelines {
            let mut normalized = Vec::new();
            for guideline in guidelines {
                let guideline = guideline.trim().to_owned();
                if !guideline.is_empty() && !normalized.contains(&guideline) { normalized.push(guideline); }
            }
            state.tool_prompt_guidelines.insert(definition.name.clone(), normalized);
        }
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
        self.execute_tool_with_updates(tool_name, params, options, None, None).await
    }

    /// Execute a shared child tool without replacing its invocation identity.
    /// Validation, permission admission and result hooks use the supplied ID.
    pub async fn execute_tool_with_call_id(
        &self,
        tool_call_id: &str,
        tool_name: &str,
        params: Value,
        options: ExecuteToolOptions,
    ) -> Result<AgentToolResult, ExecuteToolError> {
        self.execute_tool_with_updates(tool_name, params, options, None, Some(tool_call_id)).await
    }

    pub async fn execute_tool_with_call_id_and_updates(
        &self,
        tool_call_id: &str,
        tool_name: &str,
        params: Value,
        options: ExecuteToolOptions,
        on_update: Option<maho_agent::types::AgentToolUpdateCallback>,
    ) -> Result<AgentToolResult, ExecuteToolError> {
        self.execute_tool_with_updates(tool_name, params, options, on_update, Some(tool_call_id)).await
    }

    async fn execute_tool_with_updates(
        &self,
        tool_name: &str,
        params: Value,
        options: ExecuteToolOptions,
        on_update: Option<maho_agent::types::AgentToolUpdateCallback>,
        tool_call_id: Option<&str>,
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
            id: tool_call_id.map(str::to_owned).unwrap_or_else(|| format!("codemode-{}", uuid::Uuid::new_v4())),
            name: tool_name.to_owned(),
            arguments: params.as_object().cloned().unwrap_or_default(),
            ..Default::default()
        };
        let prepared = maho_agent::tool_arguments::prepare_agent_tool_call_arguments(&tool, &tool_call);
        let mut input = maho_ai::utils::validation::validate_tool_arguments(&tool.tool, &prepared)
            .map_err(|error| ExecuteToolError { code: "invalid_params".to_owned(), tool_name: tool_name.to_owned(),
                message: error.to_string(), active_tools: active_tools.clone() })?;
        let invocation = monitor_invocation::Invocation::new(Arc::as_ptr(&self.inner) as usize,
            self.monitor_generation.load(Ordering::SeqCst), prepared.id.clone(), prepared.name.clone(), input.clone(), self.cwd().into());
        let retirement = monitor_invocation::Retirement(invocation.clone());
        monitor_invocation::CURRENT.scope(invocation.clone(), async move {
        let _retirement = retirement;
        let hook_result = {
            let mut guard = self.extension_runner.lock().await.clone();
            if let Some(runner) = guard.as_mut() {
                let mut event = ToolCallEvent { tool_call_id: prepared.id.clone(), tool_name: prepared.name.clone(), input };
                let hook = runner.emit_tool_call(&mut event);
                let result = if let Some(signal) = &options.signal {
                    tokio::select! {
                        biased;
                        () = signal.cancelled() => return Err(ExecuteToolError { code: "blocked".into(), tool_name: tool_name.into(),
                            message: "Tool execution cancelled".into(), active_tools }),
                        result = hook => result,
                    }
                } else { hook.await }.map_err(|error| ExecuteToolError {
                    code: "blocked".to_owned(), tool_name: tool_name.to_owned(), message: error.to_string(), active_tools: active_tools.clone(),
                })?;
                input = event.input;
                result
            } else { None }
        };
        if let Some(block) = hook_result
            && block.block == Some(true)
        {
            return Err(ExecuteToolError {
                code: "blocked".to_owned(),
                tool_name: tool_name.to_owned(),
                message: block.reason.unwrap_or_else(|| "Tool execution was blocked".to_owned()),
                active_tools,
            });
        }
        invocation.admit(&prepared.name, &input).map_err(|message| ExecuteToolError {
            code: "blocked".into(), tool_name: tool_name.into(), message, active_tools: active_tools.clone(),
        })?;
        if options.signal.as_ref().is_some_and(|signal| signal.aborted()) {
            return Err(ExecuteToolError { code: "blocked".into(), tool_name: tool_name.into(),
                message: "Tool execution cancelled".into(), active_tools });
        }
        let execution_signal = options.signal.clone();
        let execution = (tool.execute)(
            prepared.id.clone(),
            input.clone(),
            options.signal,
            on_update,
        );
        let mut result = if let Some(signal) = execution_signal {
            tokio::select! {
                biased;
                () = signal.cancelled() => return Err(ExecuteToolError { code: "blocked".into(), tool_name: tool_name.into(),
                    message: "Tool execution cancelled".into(), active_tools }),
                result = execution => result,
            }
        } else { execution.await };
        let mut guard = self.extension_runner.lock().await.clone();
        if let Some(runner) = guard.as_mut()
            && runner.has_handlers(maho_ext_api::EventKind::ToolResult)
        {
            let hook = async {
                let content = serde_json::from_value(serde_json::to_value(&result.content)
                    .map_err(|error| error.to_string())?).map_err(|error| error.to_string())?;
                let rewrite = runner.emit_tool_result(maho_ext_api::ToolResultEvent {
                    tool_call_id: prepared.id, tool_name: prepared.name, input, content,
                    details: Some(result.details.clone()), is_error: result.is_error.unwrap_or(false), usage: result.usage,
                }).await.map_err(|error| error.to_string())?;
                if let Some(rewrite) = rewrite {
                    if let Some(content) = rewrite.content {
                        result.content = serde_json::from_value(serde_json::to_value(content)
                            .map_err(|error| error.to_string())?).map_err(|error| error.to_string())?;
                    }
                    if let Some(details) = rewrite.details { result.details = details; }
                    if let Some(is_error) = rewrite.is_error { result.is_error = Some(is_error); }
                    if let Some(usage) = rewrite.usage { result.usage = Some(usage); }
                }
                Ok::<(), String>(())
            }.await;
            hook.map_err(|message| ExecuteToolError { code: "blocked".to_owned(), tool_name: tool_name.to_owned(), message, active_tools })?;
        }
        Ok(result)
        }).await
    }

    /// `preflightToolCall`: run the `tool_call` extension hook. The Rust agent exposes no
    /// `_agentEventQueue`, so the `waitForEventQueue` wait is not applied.
    pub async fn preflight_tool_call(
        &self,
        tool_call: &maho_agent::types::AgentToolCall,
        input: Value,
    ) -> Option<maho_ext_api::ToolCallEventResult> {
        let mut guard = self.extension_runner.lock().await.clone();
        let runner = guard.as_mut()?;
        if !runner.has_handlers(maho_ext_api::EventKind::ToolCall) {
            return None;
        }
        let mut event = ToolCallEvent {
            tool_call_id: tool_call.id.clone(),
            tool_name: tool_call.name.clone(),
            input,
        };
        match runner.emit_tool_call(&mut event).await {
            Ok(result) => result,
            Err(error) => Some(maho_ext_api::ToolCallEventResult {
                block: Some(true), reason: Some(error.message), terminate: None,
            }),
        }
    }

    /// The extension runner currently bound to the session, if any.
    pub async fn extension_runner_bound(&self) -> bool {
        self.extension_runner.lock().await.is_some()
    }

    pub fn extension_context_actions(&self) -> Arc<dyn maho_ext_api::ExtensionContextActions> {
        Arc::new(SessionExtensionActions(Arc::downgrade(&self.inner)))
    }

    pub fn weak_accessor(&self) -> Arc<dyn Fn() -> Option<Self> + Send + Sync> {
        let inner = Arc::downgrade(&self.inner);
        Arc::new(move || inner.upgrade().map(|inner| Self { inner }))
    }

    async fn renew_extension_runtime(&self, reason: maho_ext_api::SessionReason) -> Result<(), String> {
        let old = self.extension_runner.lock().await.clone();
        let Some(mut old) = old else { return Ok(()); };
        let next = old.recreate_with_context(crate::sdk::extension_context::create(self)).await.map_err(|error| error.message)?;
        let paths: BTreeSet<_> = next.extensions.iter().map(|extension| extension.identity.resolved_path.clone()).collect();
        let removed: Vec<_> = old.extensions.iter().filter(|extension| !paths.contains(&extension.identity.resolved_path))
            .map(|extension| extension.identity.clone()).collect();
        old.invalidate("This extension ctx is stale after session replacement or reload.");
        old.runtime.dispose_providers().map_err(|error| error.message)?;
        let mut active = self.get_active_tool_names();
        let mut hints = self.agent.removed_tool_hints();
        {
            let mut state = self.state();
            let activators = std::mem::take(&mut state.extension_lazy_activators);
            state.lazy_tool_activators.retain(|activator| !activators.iter().any(|owned| Arc::ptr_eq(owned, activator)));
            for (name, previous) in std::mem::take(&mut state.extension_hint_backups) {
                if let Some(previous) = previous { hints.insert(name, previous); } else { hints.remove(&name); }
            }
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
        self.agent.set_removed_tool_hints(hints);
        self.set_active_tools_by_name(active);
        self.set_extension_runner(next).await;
        if !removed.is_empty() { old.emit_removed_extensions(reason, removed).await; }
        Ok(())
    }

    async fn ensure_extension_recreation(&self) -> Result<(), String> {
        if let Some(runner) = self.extension_runner.lock().await.as_ref() {
            runner.ensure_recreation_available().map_err(|error| error.message)?;
        }
        Ok(())
    }

    async fn invalidate_extension_runtime(&self) {
        self.monitor_generation.fetch_add(1, Ordering::SeqCst);
        lock(&self.settled_delivery).cancel();
        if let Some(runner) = self.extension_runner.lock().await.as_ref() {
            runner.invalidate("This extension ctx is stale after session replacement or reload.");
        }
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
        let weak = Arc::downgrade(&self.inner);
        *lock(&self.wake_source_subscription) = Some(runner.events.on("wake_source_state", Arc::new(move |data| {
            if let Some(inner) = weak.upgrade() { lock(&inner.state).wake_sources.observe(data); }
        })));
        if let Ok(mut context) = runner.create_context() {
            context.cwd = self.cwd().into();
            context.agent_dir = self.agent_dir().into();
            context.model = Some(self.model());
            context.model_registry = Arc::new(ExtensionModelRegistryView::new(self, runner.events.clone()));
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
        if self.state().uses_default_stream_function {
            let weak = Arc::downgrade(&self.inner);
            self.agent.set_stream_function(Arc::new(move |model, context, options| {
                let stream = maho_ai::utils::event_stream::create_assistant_message_event_stream();
                let output = stream.clone();
                let weak = weak.clone();
                let model = model.clone();
                let context = context.clone();
                tokio::spawn(async move {
                    let Some(inner) = weak.upgrade() else {
                        output.fail(maho_ai::utils::event_stream::StreamError::new("Session disposed")); return;
                    };
                    let session = AgentSession { inner };
                    let mut options = options.unwrap_or_default();
                    let headers = options.simple.stream.request.headers.take().unwrap_or_default();
                    let transformed = {
                        let mut runner = session.extension_runner.lock().await.clone();
                        match runner.as_mut() {
                            Some(runner) => runner.emit_before_provider_headers(headers).await,
                            None => Ok(headers),
                        }
                    };
                    match transformed {
                        Ok(headers) => options.simple.stream.request.headers = Some(headers),
                        Err(error) => { output.fail(maho_ai::utils::event_stream::StreamError::new(error.message)); return; }
                    }
                    let previous_payload = options.simple.stream.request.async_on_payload.take();
                    let payload_session = Arc::downgrade(&session.inner);
                    options.simple.stream.request.async_on_payload = Some(Arc::new(move |payload, model, metadata| {
                        let previous = previous_payload.clone();
                        let weak = payload_session.clone();
                        Box::pin(async move {
                            let payload = match previous {
                                Some(previous) => previous(payload.clone(), model, metadata.clone()).await?.unwrap_or(payload),
                                None => payload,
                            };
                            let inner = weak.upgrade().ok_or("Session disposed")?;
                            let runner = AgentSession { inner }.extension_runner.lock().await.clone();
                            match runner {
                                Some(mut runner) => runner.emit_before_provider_request_with_metadata(payload,
                                    metadata.as_ref().map(|request| request.model.clone()),
                                    metadata.map(|request| request.headers), None).await.map(Some).map_err(|error| error.message),
                                None => Ok(Some(payload)),
                            }
                        })
                    }));
                    let previous_response = options.simple.stream.request.async_on_response.take();
                    let response_session = Arc::downgrade(&session.inner);
                    options.simple.stream.request.async_on_response = Some(Arc::new(move |response, model| {
                        let previous = previous_response.clone();
                        let weak = response_session.clone();
                        Box::pin(async move {
                            if let Some(previous) = previous { previous(response.clone(), model).await?; }
                            let inner = weak.upgrade().ok_or("Session disposed")?;
                            let runner = AgentSession { inner }.extension_runner.lock().await.clone();
                            if let Some(mut runner) = runner {
                                runner.emit(maho_ext_api::ExtensionEvent::AfterProviderResponse {
                                    status: response.status, headers: response.headers,
                                }).await.map_err(|error| error.message)?;
                            }
                            Ok(())
                        })
                    }));
                    let upstream = session.model_runtime().stream_simple(&model, &context, Some(options.simple));
                    loop {
                        match upstream.next().await {
                            Ok(Some(event)) => output.push(event),
                            Ok(None) => { match upstream.result().await {
                                Ok(message) => output.end(Some(message)), Err(error) => output.fail(error),
                            }; return; }
                            Err(error) => { output.fail(error); return; }
                        }
                    }
                });
                stream
            }));
        }
        let weak = Arc::downgrade(&self.inner);
        self.agent.set_before_tool_call(Some(Arc::new(move |context, _| {
            let weak = weak.clone();
            Box::pin(async move {
                let inner = weak.upgrade()?;
                let session = AgentSession { inner };
                session.preflight_tool_call(&context.tool_call, context.args).await.map(|result|
                    maho_agent::types::BeforeToolCallResult { block: result.block, reason: result.reason, terminate: result.terminate })
            })
        })));
        let weak = Arc::downgrade(&self.inner);
        self.agent.set_after_tool_call(Some(Arc::new(move |context, _| {
            let weak = weak.clone();
            Box::pin(async move {
                let inner = weak.upgrade()?;
                let session = AgentSession { inner };
                let mut guard = session.extension_runner.lock().await;
                let runner = guard.as_mut()?;
                let content = serde_json::from_value(serde_json::to_value(context.result.content).ok()?).ok()?;
                let rewrite = runner.emit_tool_result(maho_ext_api::ToolResultEvent {
                    tool_call_id: context.tool_call.id, tool_name: context.tool_call.name, input: context.args,
                    content, details: Some(context.result.details), is_error: context.is_error, usage: context.result.usage,
                }).await.ok().flatten()?;
                Some(maho_agent::types::AfterToolCallResult {
                    content: rewrite.content.and_then(|content| serde_json::from_value(serde_json::to_value(content).ok()?).ok()),
                    details: rewrite.details, is_error: rewrite.is_error, usage: rewrite.usage, terminate: None,
                })
            })
        })));
        let weak = Arc::downgrade(&self.inner);
        self.agent.set_transform_context(Some(Arc::new(move |messages, signal| {
            let weak = weak.clone(); Box::pin(async move {
                let Some(inner) = weak.upgrade() else { return messages; };
                let session = AgentSession { inner };
                let extension_signal = maho_ext_api::AbortSignal::default();
                if signal.as_ref().is_some_and(maho_ai::utils::abort::AbortSignal::aborted) { extension_signal.abort(); }
                session.state().extension_event_signal = Some(extension_signal.clone());
                let _bridge = signal.map(|signal| AbortSignalBridge(tokio::spawn(async move { signal.cancelled().await; extension_signal.abort(); })));
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
        self.continue_session_internal(None).await
    }

    async fn continue_session_internal(&self, expected_abort_generation: Option<u64>) -> Result<(), String> {
        let _admission = self.prompt_admission.lock().await;
        let _work = self.work_barrier.begin();
        if expected_abort_generation.is_some_and(|generation| generation != self.user_abort_generation.load(Ordering::SeqCst)) {
            return Ok(());
        }
        self.state().user_aborted = false;
        let settings = self.with_settings_manager(|manager| manager.get_value("retry").cloned()).unwrap_or(Value::Null);
        let policy = crate::retry_fallback::settings::resolve_retry_fallback_settings(Some(&settings)).revert_policy;
        if let Some(controller) = self.retry_fallback.lock().await.as_mut() { controller.maybe_restore_primary(policy).await?; }
        let prior_compaction_generation = self.compaction_state().generation();
        self.revalidate_scheduled_continuation_admission().await?;
        if expected_abort_generation.is_some_and(|generation| generation != self.user_abort_generation.load(Ordering::SeqCst)) {
            return Ok(());
        }
        let lifecycle = self.compaction_state();
        if lifecycle.generation() > prior_compaction_generation && lifecycle.status() == "completed" {
            let mut messages = self.messages();
            if messages.last().and_then(AgentMessage::as_assistant)
                .is_some_and(|message| matches!(message.stop_reason, StopReason::Error | StopReason::Aborted))
            {
                messages.pop();
                self.agent.set_messages(messages);
                self.state().message_revision += 1;
            }
        }
        self.agent.continue_with_queued_messages(Default::default()).await;
        self.finish_provider_turn().await?;
        self.flush_pending_bash_messages();
        drop(_work);
        drop(_admission);
        self.emit_agent_settled().await;
        Ok(())
    }

    async fn revalidate_scheduled_continuation_admission(&self) -> Result<(), String> {
        self.revalidate_continuation_admission(true).await
    }

    async fn revalidate_continuation_admission(&self, proactive: bool) -> Result<(), String> {
        let model = self.model();
        let latest_assistant_entry = self.with_session_manager(|manager| manager.branch(manager.leaf_id()).iter().rev()
            .find(|entry| entry["type"] == "message" && entry["message"]["role"] == "assistant")
            .and_then(|entry| entry["id"].as_str()).map(str::to_owned));
        if latest_assistant_entry.is_some_and(|id| self.state().post_compaction_usage_exempt_entries.contains(&id)) {
            return Ok(());
        }
        let raw = self.with_settings_manager(|manager| manager.get_value("compaction").cloned());
        let settings = raw.map(serde_json::from_value::<crate::compaction_settings_access::CompactionSettings>)
            .transpose().map_err(|error| error.to_string())?;
        let resolved = crate::compaction_settings_resolver::resolve_compaction_settings(settings.as_ref(), Some(
            crate::compaction_settings_access::CompactionModelSelector { provider: &model.provider, id: &model.id },
        ))?;
        if !resolved.enabled || self.is_compaction_delegated() { return Ok(()); }
        let messages = self.with_session_manager(|manager| manager.build_context(manager.leaf_id()).messages);
        let messages = crate::messages::filter_context_excluded_messages(messages);
        let estimate = crate::compaction::estimate_context_tokens(&messages);
        let stale_usage = proactive && self.with_session_manager(|manager| {
            let branch = manager.branch(manager.leaf_id());
            let Some(boundary) = branch.iter().rposition(|entry| entry["type"] == "compaction") else { return false; };
            crate::compaction::get_last_assistant_usage(&branch[boundary + 1..]).is_none()
        });
        let tokens = if stale_usage { messages.iter().map(crate::compaction::estimate_tokens).sum() } else { estimate.tokens };
        let reserve = if resolved.reserve_scaling_enabled {
            crate::compaction::compaction::resolve_reserve_tokens(model.context_window as f64, resolved.reserve_tokens as f64) as u64
        } else { resolved.reserve_tokens as u64 };
        let at_hard_limit = tokens > model.context_window.saturating_sub(reserve);
        let ratio = match model.context_window {
            0 => 0.5, 1..=16_000 => 0.45, 16_001..=32_000 => 0.5, 32_001..=64_000 => 0.55,
            64_001..=128_000 => 0.6, 128_001..=512_000 => 0.7, _ => 0.8,
        };
        let over_proactive_threshold = model.context_window > 0 && tokens as f64 >= model.context_window as f64 * ratio;
        if !(at_hard_limit || proactive && over_proactive_threshold) { return Ok(()); }
        let result = self.compact_for_model(None, &model, if proactive { "pre-prompt" } else { "threshold" }).await;
        let on_cooldown = matches!(self.compaction_state(), crate::compaction::lifecycle::CompactionLifecycleState::Failed(_, _, Some(cause), _)
            if cause == "circuit-breaker");
        if result.is_err() && at_hard_limit && !self.is_compaction_delegated() && !on_cooldown {
            return Err("Compaction required before provider request".to_owned());
        }
        Ok(())
    }

    async fn emit_agent_settled(&self) {
        let generation = self.user_abort_generation.load(Ordering::SeqCst);
        let runtime_generation = self.monitor_generation.load(Ordering::SeqCst);
        let epoch = self.settlement_epoch.fetch_add(1, Ordering::SeqCst) + 1;
        lock(&self.settled_delivery).begin(generation);
        self.dispatch_extension_event(maho_ext_api::ExtensionEvent::AgentSettled).await;
        self.emit(AgentSessionEvent::AgentSettled);
        self.emit_late_user_abort().await;
        self.state().abort_provenance.close_agent_end_boundary();
        let batch = lock(&self.settled_delivery).finish(self.user_abort_generation.load(Ordering::SeqCst));
        for action in batch.actions { action(); }
        let session = self.clone();
        tokio::spawn(async move {
            for claim in batch.turn_claims {
                if claim.disposition().await == Some(crate::agent_settled_delivery::DeferredTurnDisposition::Started) { return; }
            }
            session.wait_for_idle().await;
            if !session.state().disposed && session.monitor_generation.load(Ordering::SeqCst) == runtime_generation
                && session.settlement_epoch.load(Ordering::SeqCst) == epoch && !session.is_streaming()
                && !session.work_barrier.has_active_work()
            { session.emit(AgentSessionEvent::AgentIdle); }
        });
    }

    async fn emit_late_user_abort(&self) {
        let joined = self.state().abort_provenance.take_late_user_join();
        if joined {
            self.dispatch_extension_event(maho_ext_api::ExtensionEvent::SessionAbort).await;
            self.emit(AgentSessionEvent::SessionAbort);
        }
    }

    pub async fn bind_extensions(&self, bindings: ExtensionBindings) {
        let binding_work = self.work_barrier.begin();
        let pending = Arc::new(Mutex::new(Vec::new()));
        *lock(&self.binding_readiness) = Some(pending.clone());
        let readiness_scope = ExtensionBindingReadiness { session: self, pending: pending.clone() };
        {
            let mut state = self.state();
            if let Some(ui) = bindings.ui_context { state.extension_ui_context = Some(ui); }
            if let Some(mode) = bindings.mode { state.extension_mode = mode; }
            if let Some(handler) = bindings.abort_handler { state.extension_abort_handler = Some(handler); }
            if let Some(listener) = bindings.on_error { state.extension_error_listener = Some(listener); }
        }
        let event = self.state().session_start_event.clone();
        let reason = if event.reason == maho_ext_api::SessionReason::Reload { maho_ext_api::SessionReason::Reload }
            else { maho_ext_api::SessionReason::Startup };
        self.dispatch_extension_event(maho_ext_api::ExtensionEvent::SessionStart(event)).await;
        let defaults = self.state().default_tool_names.clone();
        if let Some(defaults) = defaults {
            let active = self.get_active_tool_names().into_iter().filter(|name| {
                if defaults.contains(name) { return true; }
                self.state().tool_definitions.get(name).is_some_and(|entry|
                    entry.source_info.source != "builtin" && !entry.source_info.path.starts_with("<builtin:"))
            }).collect();
            self.set_active_tools_by_name(active);
        }
        self.extend_resources_from_extensions(reason).await;
        drop(readiness_scope);
        drop(binding_work);
        let receivers = std::mem::take(&mut *lock(&pending));
        for receiver in receivers {
            // Sender drop signals admission or an early exit, including cancellation.
            match receiver.await { Ok(()) | Err(_) => {} }
        }
    }

    async fn extend_resources_from_extensions(&self, reason: maho_ext_api::SessionReason) {
        let discovered = {
            let mut guard = self.extension_runner.lock().await.clone();
            let Some(runner) = guard.as_mut() else { return; };
            if !runner.has_handlers(maho_ext_api::EventKind::ResourcesDiscover) { return; }
            let resources = runner.emit_resources_discover(self.cwd().into(), reason).await;
            resources.map(|mut resources| {
                for entry in resources.skill_paths.iter_mut().chain(&mut resources.prompt_paths).chain(&mut resources.theme_paths).chain(&mut resources.hook_paths) {
                    if entry.scope.is_some() { continue; }
                    let contributor = runner.extensions.iter().find(|extension| extension.identity.path == entry.extension_path);
                    if contributor.is_some_and(|extension| extension.source_info.scope == maho_ext_api::SourceScope::System
                        && (extension.identity.path.starts_with("<builtin:") || extension.source_info.base_dir.as_ref().is_some_and(|root| {
                            let path = crate::paths::lexical_resolve(&entry.path);
                            let root = crate::paths::lexical_resolve(root);
                            std::path::Path::new(&path).starts_with(&root)
                        }))) { entry.scope = Some(maho_ext_api::SourceScope::System); }
                }
                resources
            })
        };
        match discovered {
            Ok(resources) => self.extend_discovered_resources(resources),
            Err(error) => self.emit(AgentSessionEvent::ContinuationError { error_message: error.message }),
        }
    }

    pub fn set_hook_source_paths(&self, resources: Vec<crate::package_manager::ResolvedResource>, additional: Vec<String>) {
        let cwd = self.cwd();
        let mut state = self.state();
        state.global_hook_source_paths.clear();
        state.project_hook_source_paths.clear();
        for resource in resources.into_iter().filter(|resource| resource.enabled) {
            let path = std::path::PathBuf::from(crate::paths::resolve_path(&resource.path,
                resource.metadata.base_dir.as_deref().unwrap_or(&cwd), &Default::default()));
            let paths = if resource.metadata.scope == crate::source_info::SourceScope::Project {
                &mut state.project_hook_source_paths
            } else { &mut state.global_hook_source_paths };
            if !paths.contains(&path) { paths.push(path); }
        }
        state.pre_session_hook_source_paths.clear();
        for path in additional {
            let path = std::path::PathBuf::from(crate::paths::resolve_path(&path, &cwd, &Default::default()));
            if !state.pre_session_hook_source_paths.contains(&path) { state.pre_session_hook_source_paths.push(path); }
        }
    }

    pub fn set_hook_sources(&self, sources: Option<maho_ext_api::LoadedHookSources>) {
        self.state().loaded_hook_sources = sources;
    }

    pub fn extension_actions(&self) -> Arc<dyn maho_ext_api::ExtensionActions> {
        Arc::new(SessionExtensionActions(Arc::downgrade(&self.inner)))
    }

    pub fn extension_context(&self, ui: Arc<dyn maho_ext_api::ExtensionUi>) -> maho_ext_api::ExtensionContext {
        let mut context = crate::sdk::extension_context::create(self);
        context.ui = ui;
        context
    }

    pub async fn bind_loaded_extensions(&self, loaded: &maho_ext_host::loader::LoadExtensionsResult,
        ui: Arc<dyn maho_ext_api::ExtensionUi>, loader: Arc<dyn Fn() -> Vec<maho_ext_host::loader::NativeExtensionFactory> + Send + Sync>) {
        let mut runner = maho_ext_host::runner::ExtensionRunner::new(loaded.extensions.clone(), loaded.runtime.clone(),
            loaded.events.clone(), self.extension_context(ui));
        runner.bind_native_factory_loader(loader, Default::default());
        self.set_extension_runner(runner).await;
    }

    fn extend_discovered_resources(&self, mut resources: maho_ext_api::DiscoveredResources) {
        let refresh_prompts = !resources.prompt_paths.is_empty();
        let refresh_skills = !resources.skill_paths.is_empty();
        let rebuild_prompt = refresh_prompts || refresh_skills || !resources.theme_paths.is_empty();
        if !rebuild_prompt && resources.hook_paths.is_empty() { return; }
        let cwd = self.cwd();
        for entry in resources.skill_paths.iter_mut().chain(&mut resources.prompt_paths).chain(&mut resources.theme_paths).chain(&mut resources.hook_paths) {
            entry.path = crate::paths::resolve_path(&entry.path, &cwd, &crate::paths::PathInputOptions::default());
        }
        let mut state = self.state();
        let stored = &mut state.discovered_resources;
        for (existing, additions) in [(&mut stored.skill_paths, resources.skill_paths),
            (&mut stored.prompt_paths, resources.prompt_paths),
            (&mut stored.theme_paths, resources.theme_paths),
            (&mut stored.hook_paths, resources.hook_paths)] {
            for entry in additions {
                if let Some(known) = existing.iter_mut().find(|known| known.path == entry.path) { *known = entry; }
                else { existing.push(entry); }
            }
        }
        let resources = state.discovered_resources.clone();
        drop(state);
        let mut templates = self.prompt_templates();
        let mut skills = self.state().skills.clone();
        let source_info = |path: &str, entry: &maho_ext_api::DiscoveredResourceEntry| {
            crate::source_info::create_synthetic_source_info(path,
                crate::source_info::SyntheticSourceInfoOptions {
                    source: crate::discovered_resource_scope::get_extension_source_label(&entry.extension_path),
                    scope: Some(match entry.scope.unwrap_or(maho_ext_api::SourceScope::Temporary) {
                        maho_ext_api::SourceScope::User => crate::source_info::SourceScope::User,
                        maho_ext_api::SourceScope::Project => crate::source_info::SourceScope::Project,
                        maho_ext_api::SourceScope::System => crate::source_info::SourceScope::System,
                        maho_ext_api::SourceScope::Temporary => crate::source_info::SourceScope::Temporary,
                    }),
                    base_dir: if entry.extension_path.starts_with('<') { None } else {
                        std::path::Path::new(&entry.extension_path).parent().map(|path| path.to_string_lossy().into_owned())
                    }, ..Default::default()
                })
        };
        for entry in resources.prompt_paths.into_iter().filter(|_| refresh_prompts) {
            for mut template in crate::prompt_templates::load_prompt_templates(&crate::prompt_templates::LoadPromptTemplatesOptions {
                cwd: cwd.clone(), agent_dir: self.agent_dir(), prompt_paths: vec![entry.path.clone()], include_defaults: false,
            }) {
                template.source_info = source_info(&template.file_path, &entry);
                if let Some(known) = templates.iter_mut().find(|known| known.name == template.name) {
                    if known.file_path == template.file_path { *known = template; }
                } else { templates.push(template); }
            }
        }
        for entry in resources.skill_paths.into_iter().filter(|_| refresh_skills) {
            for mut skill in crate::skills::load_skills(&crate::skills::LoadSkillsOptions {
                cwd: cwd.clone(), agent_dir: self.agent_dir(), skill_paths: vec![entry.path.clone()], include_defaults: false,
            }).skills {
                skill.source_info = source_info(&skill.file_path, &entry);
                if let Some(known) = skills.iter_mut().find(|known| known.name == skill.name) {
                    if known.file_path == skill.file_path { *known = skill; }
                } else { skills.push(skill); }
            }
        }
        if rebuild_prompt {
            self.set_prompt_resources(templates, skills);
            self.rebuild_system_prompt();
        }
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
        self.monitor_generation.fetch_add(1, Ordering::SeqCst);
        self.state().disposed = true;
        lock(&self.settled_delivery).cancel();
        if let Some(controller) = self.state().session_title_abort_controller.take() { controller.abort(None); }
        self.cancel_probe_back();
        self.abort_retry();
        self.abort_compaction();
        self.abort_branch_summary();
        self.abort_bash();
        self.agent.abort(None);
        lock(&self.wake_source_subscription).take();
        lock(&self.settings_source_subscription).take();
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

    #[cfg(test)]
    fn flush_pending_next_turn_messages(&self) -> Result<(), String> {
        let pending = std::mem::take(&mut self.state().pending_next_turn_messages);
        for message in pending {
            if let AgentMessage::Custom(maho_agent::types::CustomAgentMessage::Custom(custom)) = message {
                self.append_extension_custom_message(custom)?;
            }
        }
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
        let branch = self.with_session_manager(|manager| manager.branch(manager.leaf_id().or(Some(""))));
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
        let branch = self.with_session_manager(|manager| manager.branch(manager.leaf_id().or(Some(""))));
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
                signal: Some(maho_ext_api::AbortSignal::default()),
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
        if changing && let Ok(mut controller) = self.retry_fallback.try_lock()
            && let Some(controller) = controller.as_mut() {
            controller.note_manual_thinking_level();
        }

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

fn session_message_to_value(message: &AgentMessage) -> Result<Value, serde_json::Error> {
    use maho_agent::types::CustomAgentMessage;
    match message {
        AgentMessage::Llm(message) => serde_json::to_value(message),
        AgentMessage::Custom(CustomAgentMessage::Custom(message)) => serde_json::to_value(message),
        AgentMessage::Custom(CustomAgentMessage::BashExecution(message)) => serde_json::to_value(message),
        AgentMessage::Custom(CustomAgentMessage::BranchSummary(message)) => serde_json::to_value(message),
        AgentMessage::Custom(CustomAgentMessage::CompactionSummary(message)) => serde_json::to_value(message),
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

    struct TestExtensionUi;
    impl maho_ext_api::ExtensionUi for TestExtensionUi {
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
    struct TestExtensionRegistry;
    impl maho_ext_api::ModelRegistry for TestExtensionRegistry {
        fn get_all(&self) -> Vec<Model> { Vec::new() }
        fn get_available(&self) -> Vec<Model> { Vec::new() }
        fn find(&self, _: &str, _: &str) -> Option<Model> { None }
        fn has_configured_auth(&self, _: &Model) -> bool { false }
        fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> maho_ext_api::ExtensionFuture<'a, Option<String>> { Box::pin(async { Ok(None) }) }
    }
    fn test_extension_context(session: &AgentSession) -> maho_ext_api::ExtensionContext {
        maho_ext_api::ExtensionContext {
            ui: Arc::new(TestExtensionUi), mode: ExtensionMode::Print, has_ui: false,
            cwd: session.cwd().into(), agent_dir: session.agent_dir().into(),
            session_manager: Arc::new(SessionContextManager::new(session)), model_registry: Arc::new(TestExtensionRegistry),
            model: Some(session.model()), thinking_level: None, service_tier: None, effective_service_tier: None,
            scoped_models: Vec::new(), goal_store_file: None, loaded_extension_paths: Vec::new(), signal: None, steering_signal: None,
            is_idle_fn: Arc::new(|| true), wait_for_idle_fn: Arc::new(|| Box::pin(async {})),
            is_project_trusted_fn: Arc::new(|| true), is_compacting_fn: Arc::new(|| false),
            get_system_prompt_fn: Arc::new(String::new), get_system_prompt_options_fn: Arc::new(Default::default),
            registered_mcp_servers: Vec::new(), update_tool_hook_status: None,
        }
    }

    #[tokio::test]
    async fn native_provider_tool_calls_dispatch_call_and_result_hooks() {
        let mut assistant = maho_ai::providers::faux::faux_assistant_message("", Default::default());
        assistant.stop_reason = StopReason::ToolUse;
        assistant.content = vec![maho_ai::types::ContentBlock::ToolCall(maho_ai::types::ToolCall {
            id: "call".to_owned(), name: "echo".to_owned(), arguments: Map::new(), ..Default::default()
        })];
        let session = retry_session(vec![assistant, maho_ai::providers::faux::faux_assistant_message("done", Default::default())], 0);
        session.register_tool_definition(test_definition("echo"), empty_source_info(), test_tool("echo"));
        session.set_active_tools_by_name(vec!["echo".to_owned()]);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut extension = maho_ext_api::LoadedExtension::new("<inline:hooks>", session.cwd().into(), Default::default());
        for kind in [maho_ext_api::EventKind::ToolCall, maho_ext_api::EventKind::ToolResult] {
            let seen = seen.clone();
            extension.handlers.insert(kind, vec![Arc::new(move |event, _| {
                let seen = seen.clone();
                Box::pin(async move {
                    lock(&seen).push(kind);
                    if let maho_ext_api::ExtensionEvent::ToolResult(_) = event {
                        Ok(maho_ext_api::EventResult::ToolResult(maho_ext_api::ToolResultEventResult {
                            content: Some(vec![maho_tools::definition::ToolContent::text("rewritten")]), ..Default::default()
                        }))
                    } else { Ok(maho_ext_api::EventResult::None) }
                })
            })]);
        }
        session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
        tokio::time::timeout(std::time::Duration::from_secs(5), session.prompt("input", Default::default()))
            .await.expect("bounded prompt").expect("prompt");
        assert_eq!(*lock(&seen), [maho_ext_api::EventKind::ToolCall, maho_ext_api::EventKind::ToolResult]);
        let messages = session.messages();
        let result = messages.iter().find_map(|message| match message {
            AgentMessage::Llm(maho_ai::types::Message::ToolResult(result)) => Some(result), _ => None,
        }).expect("tool result");
        assert_eq!(maho_ai::utils::text::content_text(&result.content, ""), "rewritten");
    }

    #[tokio::test]
    async fn prompt_admission_receipt_precedes_provider_completion() {
        use std::{future::Future, task::Poll};
        let session = retry_session(Vec::new(), 0);
        session.agent.set_stream_function(Arc::new(|_, _, _|
            maho_ai::utils::event_stream::create_assistant_message_event_stream()));
        let receipts = Arc::new(Mutex::new(Vec::new()));
        let observed = receipts.clone();
        let mut prompt = Box::pin(session.prompt("input", PromptOptions {
            prompt_admitted: Some(Arc::new(move |disposition| lock(&observed).push(disposition))),
            session_title_prompt: Some(SessionTitlePrompt::Disabled), ..Default::default()
        }));
        let pending = std::future::poll_fn(|cx| Poll::Ready(prompt.as_mut().poll(cx).is_pending())).await;
        let admitted = lock(&receipts).clone();
        drop(prompt);
        session.dispose().await;
        assert!(pending, "provider response remains pending");
        assert_eq!(admitted, [PromptDisposition::Started]);
    }

    #[tokio::test]
    async fn queued_prompt_admission_receipt_settles_once_without_provider_completion() {
        let session = retry_session(Vec::new(), 0);
        session.state().prompt_start_pending = true;
        let receipts = Arc::new(Mutex::new(Vec::new()));
        let observed = receipts.clone();
        let result = session.prompt("queued", PromptOptions {
            streaming_behavior: Some(StreamingBehavior::FollowUp),
            prompt_admitted: Some(Arc::new(move |disposition| lock(&observed).push(disposition))),
            ..Default::default()
        }).await;
        session.state().prompt_start_pending = false;
        session.dispose().await;
        assert_eq!(result, Ok(PromptDisposition::Queued));
        assert_eq!(*lock(&receipts), [PromptDisposition::Queued]);
    }

    #[tokio::test]
    async fn handled_prompt_admission_receipt_settles_once_after_input_handler() {
        let session = retry_session(Vec::new(), 0);
        let receipts = Arc::new(Mutex::new(Vec::new()));
        let handler_receipts = receipts.clone();
        let mut extension = maho_ext_api::LoadedExtension::new("<inline:handled>", session.cwd().into(), Default::default());
        extension.handlers.insert(maho_ext_api::EventKind::Input, vec![Arc::new(move |_, _| {
            assert!(lock(&handler_receipts).is_empty());
            Box::pin(async { Ok(maho_ext_api::EventResult::Input(maho_ext_api::InputEventResult::Handled)) })
        })]);
        session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
        let observed = receipts.clone();
        let result = session.prompt("handled", PromptOptions {
            prompt_admitted: Some(Arc::new(move |disposition| lock(&observed).push(disposition))), ..Default::default()
        }).await;
        session.dispose().await;
        assert_eq!(result, Ok(PromptDisposition::Handled));
        assert_eq!(*lock(&receipts), [PromptDisposition::Handled]);
    }

    #[tokio::test]
    async fn rejected_and_cancelled_prompt_never_publish_success_admission() {
        let session = retry_session(Vec::new(), 0);
        let receipts = Arc::new(Mutex::new(Vec::new()));
        let mut model = session.model();
        model.context_window = 128;
        session.agent.set_model(model);
        session.with_settings_manager_mut(|manager| manager.apply_overrides(&Map::from_iter([
            ("compaction".to_owned(), serde_json::json!({"reserveTokens":0,"reserveScalingEnabled":false})),
        ])));
        let mut extension = maho_ext_api::LoadedExtension::new("<inline:reject>", session.cwd().into(), Default::default());
        extension.handlers.insert(maho_ext_api::EventKind::BeforeAgentStart, vec![Arc::new(|_, _|
            Box::pin(async { Ok(maho_ext_api::EventResult::BeforeAgentStart(maho_ext_api::BeforeAgentStartEventResult {
                message: Some(maho_ext_api::CustomMessage { custom_type: "admission-fixture".into(),
                    content: vec![maho_tools::definition::ToolContent::text("large input ".repeat(200))],
                    display: true, details: None }), system_prompt: None,
            })) }))]);
        session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
        let observed = receipts.clone();
        let rejected = session.prompt("rejected", PromptOptions {
            prompt_admitted: Some(Arc::new(move |disposition| lock(&observed).push(disposition))), ..Default::default()
        }).await;
        let controller = maho_ai::utils::abort::AbortController::new();
        controller.abort(None);
        let observed = receipts.clone();
        let cancelled = session.prompt("cancelled", PromptOptions {
            signal: Some(controller.signal()),
            prompt_admitted: Some(Arc::new(move |disposition| lock(&observed).push(disposition))), ..Default::default()
        }).await;
        session.dispose().await;
        assert_eq!(rejected, Err("Compaction required before provider request".into()));
        assert_eq!(cancelled, Err("Prompt cancelled".into()));
        assert!(lock(&receipts).is_empty());
    }

    #[tokio::test]
    async fn failed_provider_tool_preflight_blocks_executor() {
        let mut assistant = maho_ai::providers::faux::faux_assistant_message("", Default::default());
        assistant.stop_reason = StopReason::ToolUse;
        assistant.content = vec![maho_ai::types::ContentBlock::ToolCall(maho_ai::types::ToolCall {
            id: "call".to_owned(), name: "echo".to_owned(), arguments: Map::new(), ..Default::default()
        })];
        let session = retry_session(vec![assistant, maho_ai::providers::faux::faux_assistant_message("done", Default::default())], 0);
        let calls = Arc::new(AtomicU64::new(0));
        let captured = calls.clone();
        let mut tool = test_tool("echo");
        tool.execute = Arc::new(move |_, _, _, _| {
            captured.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { AgentToolResult::text("unexpected") })
        });
        session.register_tool_definition(test_definition("echo"), empty_source_info(), tool);
        session.set_active_tools_by_name(vec!["echo".to_owned()]);
        let mut extension = maho_ext_api::LoadedExtension::new("<inline:failure>", session.cwd().into(), Default::default());
        extension.handlers.insert(maho_ext_api::EventKind::ToolCall, vec![Arc::new(|_, _| Box::pin(async { Err("hook refused".into()) }))]);
        session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
        tokio::time::timeout(std::time::Duration::from_secs(5), session.prompt("input", Default::default()))
            .await.expect("bounded prompt").expect("prompt");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let messages = session.messages();
        let result = messages.iter().find_map(|message| match message {
            AgentMessage::Llm(maho_ai::types::Message::ToolResult(result)) => Some(result), _ => None,
        }).expect("blocked tool result");
        assert!(result.is_error);
        assert!(maho_ai::utils::text::content_text(&result.content, "").contains("hook refused"));
    }

    #[tokio::test]
    async fn registered_direct_tool_consumes_private_monitor_approval_once() {
        let session = retry_session(Vec::new(), 0);
        let observed = Arc::new(Mutex::new(None));
        let captured = observed.clone();
        let actions = session.extension_context_actions();
        let mut tool = test_tool("monitor_fixture");
        tool.execute = Arc::new(move |id, input, _, _| {
            let actions = actions.clone();
            let captured = captured.clone();
            Box::pin(async move {
                let parent = actions.take_approved_monitor_parent(&id, &input).expect("execution take");
                assert_eq!(actions.take_approved_monitor_parent(&id, &input).expect("second take"), None);
                *lock(&captured) = parent;
                AgentToolResult::text("registered execution")
            })
        });
        session.register_tool_definition(test_definition("monitor_fixture"), empty_source_info(), tool);
        session.set_active_tools_by_name(vec!["monitor_fixture".into()]);
        let mut extension = maho_ext_api::LoadedExtension::new("<inline:monitor-admission>", session.cwd().into(), Default::default());
        extension.handlers.insert(maho_ext_api::EventKind::ToolCall, vec![Arc::new(|event, context| {
            let maho_ext_api::ExtensionEvent::ToolCall(event) = event else { panic!("tool call"); };
            context.set_approved_monitor_parent(&event.tool_call_id, &event.input, std::path::Path::new("/approved")).expect("preflight attachment");
            assert!(context.take_approved_monitor_parent(&event.tool_call_id, &event.input).is_err());
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })]);
        session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
        let result = session.execute_tool("monitor_fixture", serde_json::json!({}), Default::default()).await;
        session.dispose().await;
        assert!(result.is_ok());
        assert_eq!(*lock(&observed), Some(std::path::PathBuf::from("/approved")));
        assert!(session.extension_context_actions().take_approved_monitor_parent("any", &serde_json::json!({})).is_err());
    }

    #[tokio::test]
    async fn registered_non_monitor_tool_receives_valid_input_rewritten_by_hook() {
        let session = retry_session(Vec::new(), 0);
        let observed = Arc::new(Mutex::new(None));
        let captured = observed.clone();
        let actions = session.extension_context_actions();
        let mut tool = test_tool("rewrite_fixture");
        tool.tool.parameters = serde_json::json!({"type":"object","properties":{"value":{"type":"string"}},"required":["value"]});
        tool.execute = Arc::new(move |id, input, _, _| {
            assert!(actions.take_approved_monitor_parent(&id, &input).is_err());
            *lock(&captured) = Some(input);
            Box::pin(async { AgentToolResult::text("rewritten execution") })
        });
        session.register_tool_definition(test_definition("rewrite_fixture"), empty_source_info(), tool);
        session.set_active_tools_by_name(vec!["rewrite_fixture".into()]);
        let mut extension = maho_ext_api::LoadedExtension::new("<inline:rewrite>", session.cwd().into(), Default::default());
        extension.handlers.insert(maho_ext_api::EventKind::ToolCall, vec![Arc::new(|event, _| {
            let maho_ext_api::ExtensionEvent::ToolCall(event) = event else { panic!("tool call"); };
            event.input["value"] = "changed".into();
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })]);
        session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
        let result = session.execute_tool("rewrite_fixture", serde_json::json!({"value":"original"}), Default::default()).await;
        assert!(session.extension_context_actions().take_approved_monitor_parent("no-scope", &serde_json::json!({})).is_err());
        session.dispose().await;
        assert!(result.is_ok(), "unrelated tool rewriting must not require monitor identity: {result:?}");
        assert_eq!(*lock(&observed), Some(serde_json::json!({"value":"changed"})));
    }

    #[tokio::test]
    async fn monitor_approval_retires_on_same_cwd_runtime_invalidation() {
        let session = retry_session(Vec::new(), 0);
        let actions = session.extension_context_actions();
        let input = serde_json::json!({});
        let invocation = monitor_invocation::Invocation::new(Arc::as_ptr(&session.inner) as usize,
            session.monitor_generation.load(Ordering::SeqCst), "id".into(), "monitor".into(), input.clone(), session.cwd().into());
        let retirement = monitor_invocation::Retirement(invocation.clone());
        monitor_invocation::CURRENT.scope(invocation.clone(), async {
            actions.set_approved_monitor_parent("id", &input, std::path::Path::new("/approved")).expect("attachment");
            invocation.admit("monitor", &input).expect("admission");
            session.invalidate_extension_runtime().await;
            assert!(actions.take_approved_monitor_parent("id", &input).is_err());
            assert!(actions.set_approved_monitor_parent("id", &input, std::path::Path::new("/late")).is_err());
        }).await;
        drop(retirement);
        session.dispose().await;
    }

    #[tokio::test]
    async fn cancellation_during_monitor_preflight_prevents_executor_admission() {
        let session = retry_session(Vec::new(), 0);
        let calls = Arc::new(AtomicU64::new(0));
        let executed = calls.clone();
        let mut tool = test_tool("monitor_fixture");
        tool.execute = Arc::new(move |_, _, _, _| {
            executed.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { AgentToolResult::text("unexpected") })
        });
        session.register_tool_definition(test_definition("monitor_fixture"), empty_source_info(), tool);
        session.set_active_tools_by_name(vec!["monitor_fixture".into()]);
        let (entered, entry) = tokio::sync::oneshot::channel();
        let entered = Arc::new(Mutex::new(Some(entered)));
        let mut extension = maho_ext_api::LoadedExtension::new("<inline:cancel-preflight>", session.cwd().into(), Default::default());
        extension.handlers.insert(maho_ext_api::EventKind::ToolCall, vec![Arc::new(move |event, context| {
            let maho_ext_api::ExtensionEvent::ToolCall(event) = event else { panic!("tool call"); };
            context.set_approved_monitor_parent(&event.tool_call_id, &event.input, std::path::Path::new("/approved")).expect("attachment");
            let entered = entered.clone();
            Box::pin(async move {
                lock(&entered).take().expect("one hook").send(()).expect("entry observer");
                std::future::pending::<Result<maho_ext_api::EventResult, maho_ext_api::ExtensionFailure>>().await
            })
        })]);
        session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
        let controller = maho_ai::utils::abort::AbortController::new();
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(session.execute_tool("monitor_fixture", serde_json::json!({}), ExecuteToolOptions {
                signal: Some(controller.signal()), ..Default::default()
            }), async { entry.await.expect("hook entered"); controller.abort(None); }).0
        }).await;
        session.dispose().await;
        assert!(result.expect("bounded preflight cancellation").is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn direct_monitor_cancellation_drops_pending_execution_and_retires_scope() {
        let session = retry_session(Vec::new(), 0);
        let (entered, entry) = tokio::sync::oneshot::channel();
        let entered = Arc::new(Mutex::new(Some(entered)));
        let retained = Arc::new(Mutex::new(None));
        let captured = retained.clone();
        let mut tool = test_tool("monitor_fixture");
        tool.execute = Arc::new(move |id, input, _, _| {
            let entered = entered.clone();
            let captured = captured.clone();
            Box::pin(async move {
                *lock(&captured) = Some((monitor_invocation::CURRENT.with(Arc::clone), id, input));
                lock(&entered).take().expect("one execution").send(()).expect("entry observer");
                std::future::pending::<AgentToolResult>().await
            })
        });
        session.register_tool_definition(test_definition("monitor_fixture"), empty_source_info(), tool);
        session.set_active_tools_by_name(vec!["monitor_fixture".into()]);
        let controller = maho_ai::utils::abort::AbortController::new();
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(session.execute_tool("monitor_fixture", serde_json::json!({}), ExecuteToolOptions {
                signal: Some(controller.signal()), ..Default::default()
            }), async { entry.await.expect("execution entered"); controller.abort(None); }).0
        }).await;
        session.dispose().await;
        assert!(result.expect("bounded cancelled execution").is_err());
        let (invocation, id, input) = lock(&retained).take().expect("retained scope");
        assert!(invocation.take(&id, &input, std::path::Path::new(&session.cwd())).is_err());
    }

    #[tokio::test]
    async fn registered_monitor_attachment_does_not_survive_later_block_or_cancel() {
        for cancelled in [false, true] {
            let session = retry_session(Vec::new(), 0);
            let calls = Arc::new(AtomicU64::new(0));
            let executed = calls.clone();
            let mut tool = test_tool("monitor_fixture");
            tool.execute = Arc::new(move |_, _, _, _| {
                executed.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { AgentToolResult::text("unexpected") })
            });
            session.register_tool_definition(test_definition("monitor_fixture"), empty_source_info(), tool);
            session.set_active_tools_by_name(vec!["monitor_fixture".into()]);
            let controller = maho_ai::utils::abort::AbortController::new();
            let abort = controller.clone();
            let retained = Arc::new(Mutex::new(None));
            let captured = retained.clone();
            let mut extension = maho_ext_api::LoadedExtension::new("<inline:monitor-block>", session.cwd().into(), Default::default());
            extension.handlers.insert(maho_ext_api::EventKind::ToolCall, vec![Arc::new(move |event, context| {
                let maho_ext_api::ExtensionEvent::ToolCall(event) = event else { panic!("tool call"); };
                context.set_approved_monitor_parent(&event.tool_call_id, &event.input, std::path::Path::new("/approved")).expect("attachment");
                *lock(&captured) = Some((monitor_invocation::CURRENT.with(Arc::clone), event.tool_call_id.clone(), event.input.clone()));
                if cancelled { abort.abort(None); }
                Box::pin(async move { Ok(maho_ext_api::EventResult::ToolCall(maho_ext_api::ToolCallEventResult {
                    block: Some(!cancelled), reason: Some("later admission refused".into()), terminate: None,
                })) })
            })]);
            session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
            let result = session.execute_tool("monitor_fixture", serde_json::json!({}), ExecuteToolOptions {
                signal: Some(controller.signal()), ..Default::default()
            }).await;
            let (invocation, id, input) = lock(&retained).take().expect("captured private scope");
            session.dispose().await;
            assert!(result.is_err());
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert!(invocation.take(&id, &input, std::path::Path::new(&session.cwd())).is_err());
        }
    }

    #[tokio::test]
    async fn tree_preparation_hook_observes_navigation_abort() {
        let session = test_session();
        let root = session.with_session_manager_mut(|manager| manager.append_message(serde_json::json!({"role":"user","content":"root","timestamp":0})));
        session.with_session_manager_mut(|manager| manager.append_message(serde_json::json!({"role":"user","content":"branch","timestamp":1})));
        session.rebuild_session_context().expect("context");
        let leaf = session.with_session_manager(|manager| manager.leaf_id().map(str::to_owned));
        let messages = session.messages();
        let (started, entered) = tokio::sync::oneshot::channel();
        let started = Arc::new(Mutex::new(Some(started)));
        let mut extension = maho_ext_api::LoadedExtension::new("<inline:tree-abort>", session.cwd().into(), Default::default());
        extension.handlers.insert(maho_ext_api::EventKind::SessionBeforeTree, vec![Arc::new(move |event, _| {
            let started = started.clone();
            Box::pin(async move {
                let maho_ext_api::ExtensionEvent::SessionBeforeTree { signal, .. } = event else { panic!("tree event"); };
                lock(&started).take().expect("single hook invocation").send(()).expect("observer");
                signal.cancelled().await;
                Ok(maho_ext_api::EventResult::SessionBefore(maho_ext_api::SessionBeforeEventResult { cancel: Some(true), ..Default::default() }))
            })
        })]);
        session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
        let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(session.navigate_tree(root["id"].as_str().expect("root"), Default::default()), async {
                entered.await.expect("hook started");
                session.abort_branch_summary();
            })
        }).await.expect("bounded navigation cancellation");
        assert!(result.expect("navigation").cancelled);
        assert_eq!(session.with_session_manager(|manager| manager.leaf_id().map(str::to_owned)), leaf);
        assert_eq!(session.messages(), messages);
        assert!(!session.is_compacting());
    }

    #[tokio::test]
    async fn resource_discovery_follows_startup_and_reload_with_builtin_scope() {
        let session = test_session();
        let dir = tempfile::tempdir().expect("directory");
        let prompt = dir.path().join("from-extension.md");
        std::fs::write(&prompt, "---\ndescription: extension prompt\n---\noriginal $1").expect("prompt fixture");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut extension = maho_ext_api::LoadedExtension::new("<builtin:resources>", session.cwd().into(), maho_ext_api::SourceInfo {
            scope: maho_ext_api::SourceScope::System, ..Default::default()
        });
        let started = seen.clone();
        extension.handlers.insert(maho_ext_api::EventKind::SessionStart, vec![Arc::new(move |event, _| {
            if let maho_ext_api::ExtensionEvent::SessionStart(event) = event { lock(&started).push((maho_ext_api::EventKind::SessionStart, event.reason)); }
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })]);
        let discovered = seen.clone();
        let path = prompt.to_string_lossy().into_owned();
        extension.handlers.insert(maho_ext_api::EventKind::ResourcesDiscover, vec![Arc::new(move |event, _| {
            let path = path.clone();
            if let maho_ext_api::ExtensionEvent::ResourcesDiscover(event) = event { lock(&discovered).push((maho_ext_api::EventKind::ResourcesDiscover, event.reason)); }
            Box::pin(async move { Ok(maho_ext_api::EventResult::ResourcesDiscover(maho_ext_api::ResourcesDiscoverResult {
                prompt_paths: vec![path.into()], ..Default::default()
            })) })
        })]);
        let retained_extension = extension.clone();
        let mut runner = maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session));
        runner.set_runtime_factory(Arc::new(move |context| {
            let extension = retained_extension.clone();
            Box::pin(async move { Ok(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), context)) })
        }));
        session.set_extension_runner(runner).await;
        session.bind_extensions(Default::default()).await;
        assert_eq!(session.prompt_templates()[0].source_info.scope, crate::source_info::SourceScope::System);
        assert_eq!(session.expand_input("/from-extension input", true).expect("expansion"), "original input");
        std::fs::write(&prompt, "---\ndescription: extension prompt\n---\nreloaded $1").expect("updated fixture");
        session.reload().await.expect("reload");
        assert_eq!(session.expand_input("/from-extension input", true).expect("reloaded expansion"), "reloaded input");
        assert_eq!(*lock(&seen), [
            (maho_ext_api::EventKind::SessionStart, maho_ext_api::SessionReason::Startup),
            (maho_ext_api::EventKind::ResourcesDiscover, maho_ext_api::SessionReason::Startup),
            (maho_ext_api::EventKind::SessionStart, maho_ext_api::SessionReason::Reload),
            (maho_ext_api::EventKind::ResourcesDiscover, maho_ext_api::SessionReason::Reload),
        ]);
    }

    #[tokio::test]
    async fn exact_tree_resume_retains_hook_metadata_off_conversation_tail() {
        let session = test_session();
        let root = session.with_session_manager_mut(|manager| manager.append_message(serde_json::json!({"role":"user","content":"root","timestamp":0})));
        session.with_session_manager_mut(|manager| manager.append_message(serde_json::json!({"role":"user","content":"branch","timestamp":1})));
        session.rebuild_session_context().expect("context");
        let mut extension = maho_ext_api::LoadedExtension::new("<inline:tree-metadata>", session.cwd().into(), Default::default());
        let captured = session.clone();
        extension.handlers.insert(maho_ext_api::EventKind::SessionTree, vec![Arc::new(move |_, _| {
            captured.with_session_manager_mut(|manager| manager.append_custom("tree-metadata", Some(serde_json::json!({"retained":true}))));
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })]);
        session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
        session.navigate_tree(root["id"].as_str().expect("root"), TreeNavigationOptions {
            intent: Some(TreeNavigationIntent::Resume), ..Default::default()
        }).await.expect("resume");
        assert_eq!(session.with_session_manager(|manager| manager.leaf_id().map(str::to_owned)), root["id"].as_str().map(str::to_owned));
        assert_eq!(session.messages().len(), 1);
        assert!(session.with_session_manager(|manager| manager.entries()).iter().any(|entry| entry["customType"] == "tree-metadata"));
    }

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

    #[test]
    fn extension_resolved_settings_project_all_fields_and_follow_live_model_and_override() {
        use maho_ext_api::ExtensionContextActions;
        let session = test_session();
        session.agent.set_model(test_model());
        let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
        session.with_settings_manager_mut(|manager| manager.apply_overrides(&Map::from_iter([
            ("compaction".into(), serde_json::json!({
                "reserveTokens":101,"keepRecentTokens":202,
                "speculativeEnabled":false,"speculativeFraction":0.31,"speculativeCooldownMs":321.5,
                "restorationEnabled":false,"restorationMaxItems":2.5,"restorationMaxTokensPerItem":11.5,
                "restorationMaxTotalTokens":22.5,"restorationContextRatio":0.21,
                "idleCompactionEnabled":false,"graceBandEnabled":false,"toolAdmissionEnabled":false,
                "reminderEnabled":false,"reserveScalingEnabled":false,"speculativeLeadTokens":12000.5,
                "summarizationMaxDurationMs":45678.5,
                "modelOverrides":{"faux/faux-1":{"reserveTokens":303,"keepRecentTokens":404}}
            })),
        ])));
        let projected = actions.get_resolved_compaction_settings().expect("resolved settings");
        assert_eq!(projected, maho_ext_api::ResolvedCompactionSettings {
            enabled: true, reserve_tokens: 303, keep_recent_tokens: 404,
            speculative_enabled: false, speculative_fraction: 0.31, speculative_cooldown_ms: 321.5,
            restoration_enabled: false, restoration_max_items: 2.5, restoration_max_tokens_per_item: 11.5,
            restoration_max_total_tokens: 22.5, restoration_context_ratio: 0.21,
            idle_compaction_enabled: false, grace_band_enabled: false, tool_admission_enabled: false,
            reminder_enabled: false, reserve_scaling_enabled: false, speculative_lead_tokens: Some(12000.5),
            summarization_max_duration_ms: Some(45678.5),
        });
        let mut model = test_model();
        model.id = "other".into();
        session.agent.set_model(model);
        let changed = actions.get_resolved_compaction_settings().expect("live model settings");
        assert_eq!((changed.reserve_tokens, changed.keep_recent_tokens), (101, 202));
        session.set_auto_compaction_enabled(false);
        assert!(!actions.get_resolved_compaction_settings().expect("live enabled override").enabled);
        assert!(!actions.get_compaction_settings().enabled);
        session.with_settings_manager_mut(|manager| manager.apply_overrides(&Map::from_iter([
            ("compaction".into(), serde_json::json!({"reserveTokens":505,"keepRecentTokens":606})),
        ])));
        let revised = actions.get_resolved_compaction_settings().expect("live config revision");
        assert_eq!((revised.reserve_tokens, revised.keep_recent_tokens), (505, 606));
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
        let registry = ExtensionModelRegistryView::new(&session, Default::default());
        assert_eq!(maho_ext_api::ModelRegistry::get_all(&registry), session.model_registry().get_all());
        assert_eq!(maho_ext_api::ModelRegistry::find(&registry, "faux", "faux-1"), session.model_registry().find("faux", "faux-1"));
        assert!(maho_ext_api::ModelRegistry::get_api_key_for_provider(&registry, "faux").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn extension_registry_services_use_configured_runtime_and_shared_credential_kind() {
        use maho_ext_api::ModelRegistry as _;
        let session = test_session();
        let provider = maho_ai::providers::faux::faux_provider(maho_ai::providers::faux::RegisterFauxProviderOptions {
            tokens_per_second: Some(0.0), ..Default::default()
        });
        provider.set_responses(vec![maho_ai::providers::faux::faux_assistant_message("configured stream", Default::default()).into()]);
        let mut runtime = session.model_runtime().clone();
        runtime.register_native_provider(provider.provider.clone());
        session.model_registry().auth_storage.set("faux", Some(serde_json::json!({"type":"api_key","key":"fixture-stored"}))).expect("stored auth");
        let registry = ExtensionModelRegistryView::new(&session, Default::default());
        let auth = registry.get_provider_auth("faux").await.expect("configured auth").expect("auth resolution");
        let kind = registry.get_stored_credential_type("faux").expect("metadata");
        let absent = registry.get_stored_credential_type("absent-fixture").expect("absent metadata");
        let model = provider.get_model(Some("faux-1")).expect("model");
        let stream = registry.stream_simple(&model, &maho_ai::types::Context::default(), None).expect("configured stream creation");
        let response = tokio::time::timeout(std::time::Duration::from_secs(5), stream.result()).await;
        session.dispose().await;
        assert_eq!(auth.auth.api_key.as_deref(), Some("fixture-stored"));
        assert_eq!(kind, Some(maho_ai::auth::types::CredentialType::ApiKey));
        assert_eq!(absent, None);
        assert_eq!(maho_ai::utils::text::content_text(&response.expect("bounded stream").expect("stream response").content, ""), "configured stream");
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
    async fn queued_account_mutation_rechecks_generation_after_storage_admission() {
        use maho_ext_api::ModelRegistry as _;
        let session = test_session();
        let storage = session.model_registry().auth_storage.clone();
        storage.set("fixture-account", Some(serde_json::json!({"type":"api_key","key":"fixture-key",
            "accounts":[{"name":"default","key":"fixture-key"}]}))).expect("seed");
        let before = storage.get("fixture-account");
        let registry = ExtensionModelRegistryView::new(&session, Default::default());
        let permit = storage.account_mutation.lock().await;
        let mut mutation = Box::pin(registry.rename_credential_account("fixture-account", "default", Some("Late")));
        assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(std::future::Future::poll(mutation.as_mut(), cx).is_pending())).await);
        session.invalidate_extension_runtime().await;
        drop(permit);
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), mutation).await;
        session.dispose().await;
        assert!(result.expect("bounded queued admission").is_err());
        assert_eq!(storage.get("fixture-account"), before);
    }

    #[tokio::test]
    async fn returned_deferred_batch_cannot_start_after_runtime_replacement() {
        use maho_ext_api::ExtensionActions as _;
        for cancelled in [false, true] {
        let session = test_session();
        lock(&session.settled_delivery).begin(session.user_abort_generation.load(Ordering::SeqCst));
        SessionExtensionActions(Arc::downgrade(&session.inner)).send_user_message(
            maho_ext_api::UserMessageContent::Text("retired deferred fixture".into()), Default::default()).expect("defer");
        let batch = lock(&session.settled_delivery).finish(session.user_abort_generation.load(Ordering::SeqCst));
        assert_eq!(batch.turn_claims.len(), 1);
        if cancelled { session.abort().await; } else { session.invalidate_extension_runtime().await; }
        let before = session.messages();
        for action in batch.actions { action(); }
        let disposition = tokio::time::timeout(std::time::Duration::from_secs(5), batch.turn_claims[0].disposition()).await;
        session.wait_for_idle().await;
        let after = session.messages();
        session.dispose().await;
        assert_eq!(disposition.expect("bounded retired action"), Some(crate::agent_settled_delivery::DeferredTurnDisposition::FinishedWithoutStart));
        assert_eq!(after, before);
        }
    }

    #[tokio::test]
    async fn runtime_retirement_discards_pending_settled_delivery_before_execution() {
        for disposed in [false, true] {
            let session = test_session();
            let ran = Arc::new(AtomicU64::new(0));
            let counter = ran.clone();
            {
                let mut delivery = lock(&session.settled_delivery);
                delivery.begin(session.user_abort_generation.load(Ordering::SeqCst));
                assert!(delivery.defer_trigger_turn(move |_| { counter.fetch_add(1, Ordering::SeqCst); }));
            }
            if disposed { session.dispose().await; } else { session.invalidate_extension_runtime().await; }
            let batch = lock(&session.settled_delivery).finish(session.user_abort_generation.load(Ordering::SeqCst));
            session.dispose().await;
            assert!(batch.actions.is_empty());
            assert!(batch.turn_claims.is_empty());
            assert_eq!(ran.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn extension_account_facade_mutates_shared_store_and_emits_only_success() {
        use maho_ext_api::ModelRegistry as _;
        let session = test_session();
        let dir = tempfile::tempdir().expect("account sidecar");
        let events = maho_ext_api::EventBus::default();
        let observed = Arc::new(Mutex::new(Vec::new()));
        let captured = observed.clone();
        let _subscription = events.on("provider-accounts-changed", Arc::new(move |value| lock(&captured).push(value.clone())));
        session.model_registry().auth_storage.set("fixture-account", Some(serde_json::json!({
            "type":"api_key","key":"fixture-flat-secret",
            "accounts":[{"name":"default","key":"fixture-first-secret","source":"login"},
                {"name":"work","key":"fixture-second-secret","source":"import"}]
        }))).expect("seed shared storage");
        let mut registry = ExtensionModelRegistryView::new(&session, events);
        registry.agent_dir = dir.path().to_string_lossy().into_owned();
        let exercise = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let before = registry.get_credential_accounts("fixture-account").await?;
            registry.pin_credential_account("fixture-account", Some("work")).await?;
            registry.rename_credential_account("fixture-account", "work", Some("Work account")).await?;
            let changed = registry.get_credential_accounts("fixture-account").await?;
            registry.pin_credential_account("fixture-account", None).await?;
            let error = registry.remove_credential_account("fixture-account", "missing").await;
            registry.remove_credential_account("fixture-account", "work").await?;
            let after = registry.get_credential_accounts("fixture-account").await?;
            Ok::<_, maho_ext_api::ExtensionFailure>((before, changed, after, error))
        }).await;
        let shared = session.model_runtime().credentials.get("fixture-account");
        session.dispose().await;
        let (before, changed, after, error) = exercise.expect("bounded account operations").expect("account facade");
        assert_eq!(before.len(), 2);
        assert_eq!(before[1].source, maho_ext_api::CredentialAccountSource::Import);
        assert!(changed[1].pinned);
        assert_eq!(changed[1].display_name.as_deref(), Some("Work account"));
        assert!(error.is_err());
        assert_eq!(after.len(), 1);
        assert!(!after[0].pinned);
        assert_eq!(shared.expect("same runtime store")["accounts"].as_array().expect("accounts").len(), 1);
        assert_eq!(lock(&observed).len(), 4);
        assert!(lock(&observed).iter().all(|value| value == &serde_json::json!({"type":"accounts_changed","provider":"fixture-account"})));
        assert!(!format!("{before:?}{changed:?}{after:?}").contains("secret"));
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
            model_registry: Arc::new(ExtensionModelRegistryView::new(session, Default::default())), model: None, thinking_level: None,
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
        let runtimes = Arc::new(Mutex::new(Vec::<maho_ext_api::ExtensionRuntime>::new()));
        let captured = runtimes.clone();
        let factory = maho_ext_host::loader::NativeAsyncExtensionFactory {
            path: "replacement".into(), source_info: empty_source_info(),
            factory: Arc::new(move |api| {
                let first = lock(&captured).is_empty();
                if let Some(previous) = lock(&captured).last() {
                    assert!(previous.assert_active().is_err(), "invalidate before invoking replacement factories");
                }
                lock(&captured).push(api.runtime.clone());
                if first {
                    api.register_tool(test_definition("old-only"));
                    api.register_lazy_tool_activator(Arc::new(|_| false));
                    api.register_removed_tool_hint("old-only", "old hint");
                }
                api.register_command("generation", Some(lock(&captured).len().to_string()), None, Arc::new(|_, _| Box::pin(async { Ok(()) })));
                Box::pin(async { Ok(()) })
            }),
        };
        let runner = ExtensionRunner::from_async_factories(vec![factory], replacement_test_context(&session), Default::default()).await.unwrap();
        session.set_extension_runner(runner).await;
        let old = session.extension_runner.lock().await.as_ref().unwrap().create_context().unwrap();
        assert!(session.get_registered_tool("old-only").is_some());
        assert_eq!(session.state().extension_lazy_activators.len(), 1);
        assert_eq!(session.agent.removed_tool_hints().get("old-only").map(String::as_str), Some("old hint"));
        let command = session.extension_runner.lock().await.as_ref().unwrap().create_command_context(Arc::new(SessionExtensionActions(Arc::downgrade(&session.inner)))).unwrap();
        let callback_id = Arc::new(Mutex::new(None));
        let captured_id = callback_id.clone();
        let result = command.new_session(maho_ext_api::NewSessionOptions {
            with_session: Some(Arc::new(move |ctx| {
                *lock(&captured_id) = Some(ctx.session_manager.session_id().to_owned());
                Box::pin(async move {
                    ctx.send_message(maho_ext_api::CustomMessage { custom_type: "replacement".into(), content: vec![maho_ext_api::ToolContent::text("ready")], display: true, details: None }, Default::default()).await
                })
            })), ..Default::default()
        }).await.unwrap();
        assert!(!result.cancelled);
        assert_eq!(*lock(&callback_id), Some(session.session_id()));
        assert_eq!(session.messages().last().unwrap().role(), "custom");
        assert!(lock(&runtimes)[0].assert_active().is_err());
        assert!(old.actions().is_err());
        assert!(session.get_registered_tool("old-only").is_none());
        assert!(session.state().extension_lazy_activators.is_empty());
        assert!(!session.agent.removed_tool_hints().contains_key("old-only"));
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
    async fn repeated_compaction_hook_receives_previous_summary() {
        let session = retry_session(Vec::new(), 0);
        session.with_settings_manager_mut(|manager| manager.apply_overrides(&Map::from_iter([
            ("compaction".into(), serde_json::json!({"keepRecentTokens":1,"reserveTokens":0})),
        ])));
        for text in ["old task".repeat(200), "recent task".repeat(200)] {
            session.with_session_manager_mut(|manager| manager.append_message(serde_json::json!({"role":"user","content":text,"timestamp":0})));
        }
        session.rebuild_session_context().expect("history");
        let summaries = Arc::new(Mutex::new(Vec::new()));
        let captured = summaries.clone();
        let mut extension = maho_ext_api::LoadedExtension::new("<inline:repeat-compaction>", session.cwd().into(), Default::default());
        extension.handlers.insert(maho_ext_api::EventKind::SessionBeforeCompact, vec![Arc::new(move |event, _| {
            let maho_ext_api::ExtensionEvent::SessionBeforeCompact(event) = event else { panic!("preparation"); };
            let has_summary = EXTENSION_COMPACTION_PREPARATION.with(|details| details.source_messages.as_ref()
                .expect("source messages").iter().any(|message| message.role() == "compactionSummary"));
            lock(&captured).push(has_summary);
            Box::pin(async move { Ok(maho_ext_api::EventResult::SessionBefore(maho_ext_api::SessionBeforeEventResult {
                compaction: Some(maho_ext_api::CompactionResult { summary: "digest".into(),
                    first_kept_entry_id: event.preparation.first_kept_entry_id.clone(),
                    tokens_before: event.preparation.tokens_before, details: None }), ..Default::default()
            })) })
        })]);
        session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
        session.compact(None).await.expect("first compaction");
        session.with_session_manager_mut(|manager| manager.append_message(serde_json::json!({"role":"user","content":"next task".repeat(200),"timestamp":0})));
        session.rebuild_session_context().expect("new entry");
        session.compact(None).await.expect("second compaction");
        assert_eq!(*lock(&summaries), [false, true]);
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
    fn exporting_uses_selected_leaf_and_empty_selection() {
        let dir = tempfile::tempdir().expect("tempdir");
        let session = test_session();
        let selected = session.with_session_manager_mut(|manager| {
            let selected = manager.append_message(serde_json::json!({"role":"user","content":"selected","timestamp":0}));
            manager.append_message(serde_json::json!({"role":"user","content":"abandoned","timestamp":1}));
            selected["id"].as_str().expect("id").to_owned()
        });
        for leaf in [Some(selected.as_str()), None] {
            session.with_session_manager_mut(|manager| manager.set_leaf(leaf));
            let path = dir.path().join("selected.jsonl");
            session.export_to_jsonl(Some(path.to_str().expect("path"))).expect("export");
            let entries = crate::session_manager::load_entries_from_file(path.to_str().expect("path"));
            assert_eq!(entries.len(), if leaf.is_some() { 2 } else { 1 });
            if leaf.is_some() { assert_eq!(entries[1]["id"], selected); }
        }
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

    #[tokio::test]
    async fn direct_tool_execution_validates_and_coerces_prepared_arguments() {
        let session = test_session();
        let mut tool = test_tool("validated");
        tool.tool.parameters = serde_json::json!({"type":"object", "properties":{"count":{"type":"number"}}, "required":["count"]});
        tool.execute = Arc::new(|_, args, _, _| Box::pin(async move {
            AgentToolResult { details: args, terminate: Some(true), ..AgentToolResult::text("executed") }
        }));
        session.agent().set_tools(vec![tool]);
        let error = session.execute_tool("validated", serde_json::json!({}), Default::default()).await.expect_err("required argument");
        assert_eq!(error.code, "invalid_params");
        assert_eq!(error.tool_name, "validated");
        let result = session.execute_tool("validated", serde_json::json!({"count":"3"}), Default::default()).await.expect("coerced argument");
        assert_eq!(result.details, serde_json::json!({"count":3}));
        assert_eq!(result.terminate, Some(true));
    }

    #[tokio::test]
    async fn next_turn_asides_stay_out_of_history_until_admitted() {
        let session = test_session();
        session.agent().set_model(test_model());
        let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
        maho_ext_api::ExtensionActions::send_message(&actions, maho_ext_api::CustomMessage {
            custom_type: "aside".to_owned(), content: vec![maho_tools::definition::ToolContent::text("x".repeat(600_000))],
            display: false, details: None,
        }, maho_ext_api::SendMessageOptions { deliver_as: Some(maho_ext_api::DeliverAs::NextTurn), trigger_turn: false }).expect("queue aside");
        assert!(session.messages().is_empty());
        assert!(session.with_session_manager(|manager| manager.entries()).is_empty());
        assert_eq!(session.state().pending_next_turn_messages.len(), 1);
        let additions = session.state().pending_next_turn_messages.clone();
        assert!(session.enforce_final_provider_admission(&additions).await.is_err());
        assert_eq!(session.state().pending_next_turn_messages.len(), 1);
        assert!(session.enforce_final_provider_admission(&[make_user_message(&"x".repeat(600_000), None)]).await.is_ok());
    }

    #[tokio::test]
    async fn prompt_delivers_next_turn_aside_once_after_user_message() {
        let session = retry_session(vec![maho_ai::providers::faux::faux_assistant_message("reply", Default::default())], 0);
        let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
        maho_ext_api::ExtensionActions::send_message(&actions, maho_ext_api::CustomMessage {
            custom_type: "aside".to_owned(), content: vec![maho_tools::definition::ToolContent::text("remember")],
            display: false, details: None,
        }, maho_ext_api::SendMessageOptions { deliver_as: Some(maho_ext_api::DeliverAs::NextTurn), trigger_turn: false }).expect("queue");
        tokio::time::timeout(std::time::Duration::from_secs(5), session.prompt("input", Default::default()))
            .await.expect("bounded prompt").expect("prompt");
        let messages = session.messages();
        assert_eq!(messages.iter().map(AgentMessage::role).collect::<Vec<_>>(), ["user", "custom", "assistant"]);
        assert!(session.state().pending_next_turn_messages.is_empty());
        let entries = session.with_session_manager(|manager| manager.entries());
        assert_eq!(entries.iter().filter(|entry| entry["type"] == "custom_message" && entry["customType"] == "aside").count(), 1);
    }

    #[tokio::test]
    async fn rejected_prompt_restores_next_turn_asides_without_provider_call() {
        let session = test_session_with_stream_function(false);
        session.agent().set_model(test_model());
        let calls = Arc::new(AtomicU64::new(0));
        let captured = calls.clone();
        session.agent().set_stream_function(Arc::new(move |_, _, _| {
            captured.fetch_add(1, Ordering::SeqCst);
            let stream = maho_ai::utils::event_stream::create_assistant_message_event_stream();
            stream.end(Some(maho_ai::providers::faux::faux_assistant_message("unexpected", Default::default())));
            stream
        }));
        let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
        for (kind, content) in [("oversized", "x".repeat(600_000)), ("second", "retained".to_owned())] {
            maho_ext_api::ExtensionActions::send_message(&actions, maho_ext_api::CustomMessage {
                custom_type: kind.to_owned(), content: vec![maho_tools::definition::ToolContent::text(content)],
                display: false, details: None,
            }, maho_ext_api::SendMessageOptions { deliver_as: Some(maho_ext_api::DeliverAs::NextTurn), trigger_turn: false }).expect("queue");
        }
        for _ in 0..2 {
            tokio::time::timeout(std::time::Duration::from_secs(5), session.prompt("input", Default::default()))
                .await.expect("bounded rejection").expect_err("required compaction");
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert!(session.messages().is_empty());
            assert!(session.with_session_manager(|manager| manager.entries()).is_empty());
            let state = session.state();
            let kinds = state.pending_next_turn_messages.iter().map(|message| match message {
                AgentMessage::Custom(maho_agent::types::CustomAgentMessage::Custom(message)) => message.custom_type.as_str(),
                _ => panic!("custom aside"),
            }).collect::<Vec<_>>();
            assert_eq!(kinds, ["oversized", "second"]);
        }
    }

    #[test]
    fn hook_source_context_uses_branded_json_paths() {
        let session = test_session();
        let sources = maho_ext_api::ExtensionContextActions::get_loaded_hook_sources(
            &SessionExtensionActions(Arc::downgrade(&session.inner)));
        assert_eq!(sources.global_hooks_path, std::path::Path::new(&session.agent_dir()).join("hooks.json"));
        assert_eq!(sources.project_hooks_path, std::path::Path::new(&session.cwd()).join(crate::config::config_dir_name()).join("hooks.json"));
    }

    #[test]
    fn compaction_context_reports_model_and_session_overrides() {
        let session = test_session();
        session.agent().set_model(test_model());
        session.with_settings_manager_mut(|manager| manager.set(crate::settings_manager::SettingsScope::Global,
            &Map::from_iter([("compaction".to_owned(), serde_json::json!({"enabled":true,"reserveTokens":100,
                "keepRecentTokens":200,"modelOverrides":{"faux/faux-1":{"reserveTokens":300,"keepRecentTokens":400}}}))])))
            .expect("settings");
        session.set_auto_compaction_enabled(false);
        let settings = maho_ext_api::ExtensionContextActions::get_compaction_settings(
            &SessionExtensionActions(Arc::downgrade(&session.inner)));
        assert!(!settings.enabled);
        assert_eq!(settings.reserve_tokens, 300);
        assert_eq!(settings.keep_recent_tokens, 400);
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
        let revision = session.message_revision();
        session.set_active_tools_by_name(vec!["read".to_owned(), "nope".to_owned()]);
        assert_eq!(session.get_active_tool_names(), vec!["read".to_owned()]);
        assert!(!session.system_prompt().is_empty());
        assert_eq!(session.message_revision(), revision + 1);
        session.state().system_prompt_override = Some("turn hook override".to_owned());
        session.set_active_tools_by_name(vec!["read".to_owned()]);
        assert_eq!(session.system_prompt(), "turn hook override");
        assert_eq!(session.message_revision(), revision + 1);
    }

    #[tokio::test]
    async fn binding_waits_for_startup_user_message_admission() {
        let session = retry_session(vec![maho_ai::providers::faux::faux_assistant_message("welcome", Default::default())], 0);
        let captured = session.clone();
        let admitted = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = admitted.clone();
        let mut extension = maho_ext_api::LoadedExtension::new("<inline:startup-admission>", session.cwd().into(), Default::default());
        extension.handlers.insert(maho_ext_api::EventKind::SessionStart, vec![Arc::new(move |_, _| {
            maho_ext_api::ExtensionActions::send_user_message(&SessionExtensionActions(Arc::downgrade(&captured.inner)),
                maho_ext_api::UserMessageContent::Text("start".into()), Default::default()).expect("startup message");
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })]);
        extension.handlers.insert(maho_ext_api::EventKind::InputDisposition, vec![Arc::new(move |event, _| {
            if matches!(event, maho_ext_api::ExtensionEvent::InputDisposition { disposition: maho_ext_api::InputDisposition::Started, .. }) {
                observed.store(true, Ordering::SeqCst);
            }
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })]);
        session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
        tokio::time::timeout(std::time::Duration::from_secs(5), session.bind_extensions(Default::default())).await.expect("bounded binding");
        let ready_at_return = admitted.load(Ordering::SeqCst);
        tokio::time::timeout(std::time::Duration::from_secs(5), session.wait_for_idle()).await.expect("cleanup turn");
        assert!(ready_at_return, "binding returned before startup user-message admission");
    }

    #[tokio::test]
    async fn startup_user_binding_does_not_wait_for_provider_completion() {
        use maho_ai::providers::faux::{faux_provider, faux_streams, RegisterFauxProviderOptions};
        let (started, entered) = tokio::sync::oneshot::channel();
        let started = Arc::new(Mutex::new(Some(started)));
        let (release, released) = tokio::sync::watch::channel(false);
        let provider = faux_provider(RegisterFauxProviderOptions {
            tokens_per_second: Some(0.0),
            scheduler_hook: Some(Arc::new(move || {
                let started = started.clone();
                let mut released = released.clone();
                Box::pin(async move {
                    if let Some(started) = lock(&started).take() { started.send(()).expect("provider observer"); }
                    released.wait_for(|value| *value).await.expect("release provider");
                })
            })), ..Default::default()
        });
        provider.set_responses(vec![maho_ai::providers::faux::faux_assistant_message("welcome", Default::default()).into()]);
        let streams = faux_streams(provider.core.clone());
        let session = test_session_with_stream_function(false);
        session.agent.set_model(provider.get_model(Some("faux-1")).expect("model"));
        session.agent.set_stream_function(Arc::new(move |model, context, options| {
            streams.stream_simple(model, context, options.map(|options| options.simple))
        }));
        let captured = session.clone();
        let mut extension = maho_ext_api::LoadedExtension::new("<inline:startup-held>", session.cwd().into(), Default::default());
        extension.handlers.insert(maho_ext_api::EventKind::SessionStart, vec![Arc::new(move |_, _| {
            maho_ext_api::ExtensionActions::send_user_message(&SessionExtensionActions(Arc::downgrade(&captured.inner)),
                maho_ext_api::UserMessageContent::Text("start".into()), Default::default()).expect("startup message");
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })]);
        session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(session.bind_extensions(Default::default()), async { entered.await.expect("provider entered"); });
            assert!(session.get_last_assistant_text().is_none(), "provider is still held");
        }).await;
        release.send_replace(true);
        tokio::time::timeout(std::time::Duration::from_secs(5), session.wait_for_idle()).await.expect("cleanup provider");
        result.expect("binding must finish without provider completion");
        assert_eq!(session.get_last_assistant_text().as_deref(), Some("welcome"));
    }

    #[tokio::test]
    async fn startup_binding_releases_readiness_for_handled_input() {
        let session = test_session();
        let captured = session.clone();
        let handled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = handled.clone();
        let mut extension = maho_ext_api::LoadedExtension::new("<inline:startup-handled>", session.cwd().into(), Default::default());
        extension.handlers.insert(maho_ext_api::EventKind::SessionStart, vec![Arc::new(move |_, _| {
            maho_ext_api::ExtensionActions::send_user_message(&SessionExtensionActions(Arc::downgrade(&captured.inner)),
                maho_ext_api::UserMessageContent::Text("handled".into()), Default::default()).expect("startup message");
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })]);
        extension.handlers.insert(maho_ext_api::EventKind::Input, vec![Arc::new(move |_, _| {
            observed.store(true, Ordering::SeqCst);
            Box::pin(async { Ok(maho_ext_api::EventResult::Input(maho_ext_api::InputEventResult::Handled)) })
        })]);
        session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
        tokio::time::timeout(std::time::Duration::from_secs(5), session.bind_extensions(Default::default())).await.expect("handled input releases binding");
        assert!(handled.load(Ordering::SeqCst));
        assert!(lock(&session.binding_readiness).is_none());
        assert!(!session.work_barrier.has_active_work());
        assert!(session.messages().is_empty());
    }

    #[tokio::test]
    async fn startup_binding_accepts_queued_input_during_compaction() {
        use maho_ext_api::ExtensionContextActions;
        let session = test_session();
        let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
        let signal = actions.begin_compaction(maho_ext_api::BeginCompactionOptions {
            reason: maho_ext_api::CompactionReason::Manual,
        }).expect("active compaction");
        let captured = session.clone();
        let mut extension = maho_ext_api::LoadedExtension::new("<inline:startup-queued>", session.cwd().into(), Default::default());
        extension.handlers.insert(maho_ext_api::EventKind::SessionStart, vec![Arc::new(move |_, _| {
            maho_ext_api::ExtensionActions::send_user_message(&SessionExtensionActions(Arc::downgrade(&captured.inner)),
                maho_ext_api::UserMessageContent::Text("queued startup".into()), maho_ext_api::SendUserMessageOptions {
                    deliver_as: Some(maho_ext_api::StreamingBehavior::FollowUp), ..Default::default()
                }).expect("startup message");
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })]);
        session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), session.bind_extensions(Default::default())).await;
        let queued = session.get_follow_up_messages();
        let compacting_at_return = session.is_compacting();
        session.clear_queue(false);
        actions.end_compaction(maho_ext_api::EndCompactionOptions {
            reason: maho_ext_api::CompactionReason::Manual, signal: Some(signal), aborted: Some(false), error_message: None,
        });
        result.expect("queued input releases binding without waiting for compaction");
        assert!(compacting_at_return);
        assert_eq!(queued, ["queued startup"]);
        assert!(lock(&session.binding_readiness).is_none());
    }

    #[tokio::test]
    async fn rejected_startup_input_releases_binding_readiness() {
        use maho_ext_api::ExtensionContextActions;
        let session = test_session();
        let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
        let signal = actions.begin_compaction(maho_ext_api::BeginCompactionOptions {
            reason: maho_ext_api::CompactionReason::Manual,
        }).expect("active compaction");
        let errors = Arc::new(Mutex::new(Vec::new()));
        let observed = errors.clone();
        let _subscription = session.subscribe(Arc::new(move |event| {
            if let AgentSessionEvent::ContinuationError { error_message } = event {
                lock(&observed).push(error_message.clone());
            }
        }));
        let captured = session.clone();
        let mut extension = maho_ext_api::LoadedExtension::new("<inline:startup-rejected>", session.cwd().into(), Default::default());
        let input_calls = Arc::new(AtomicU64::new(0));
        let calls = input_calls.clone();
        extension.handlers.insert(maho_ext_api::EventKind::Input, vec![Arc::new(move |_, _| {
            calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(maho_ext_api::EventResult::Input(maho_ext_api::InputEventResult::Transform {
                text: "must not transform retained input".into(), images: None,
            })) })
        })]);
        extension.handlers.insert(maho_ext_api::EventKind::SessionStart, vec![Arc::new(move |_, _| {
            maho_ext_api::ExtensionActions::send_user_message(&SessionExtensionActions(Arc::downgrade(&captured.inner)),
                maho_ext_api::UserMessageContent::Text("no queue mode".into()), Default::default()).expect("dispatch startup");
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })]);
        session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), session.bind_extensions(Default::default())).await;
        let retained = session.get_follow_up_messages();
        session.clear_queue(false);
        actions.end_compaction(maho_ext_api::EndCompactionOptions {
            reason: maho_ext_api::CompactionReason::Manual, signal: Some(signal), aborted: Some(false), error_message: None,
        });
        result.expect("rejected input releases binding");
        assert_eq!(retained, ["no queue mode"]);
        assert_eq!(input_calls.load(Ordering::SeqCst), 0);
        assert_eq!(lock(&errors).len(), 1);
        assert!(lock(&session.binding_readiness).is_none());
        assert!(!session.work_barrier.has_active_work());
    }

    #[tokio::test]
    async fn rejected_startup_images_survive_one_later_provider_turn() {
        use maho_ext_api::ExtensionContextActions;
        let session = retry_session(vec![maho_ai::providers::faux::faux_assistant_message("delivered", Default::default())], 0);
        let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
        let signal = actions.begin_compaction(maho_ext_api::BeginCompactionOptions {
            reason: maho_ext_api::CompactionReason::Manual,
        }).expect("compaction");
        let captured = session.clone();
        let mut extension = maho_ext_api::LoadedExtension::new("<inline:retained-images>", session.cwd().into(), Default::default());
        extension.handlers.insert(maho_ext_api::EventKind::SessionStart, vec![Arc::new(move |_, _| {
            maho_ext_api::ExtensionActions::send_user_message(&SessionExtensionActions(Arc::downgrade(&captured.inner)),
                maho_ext_api::UserMessageContent::Blocks(vec![
                    maho_ext_api::ToolContent::text("first"),
                    maho_ext_api::ToolContent::Image { data: "aW1hZ2Ux".into(), mime_type: "image/png".into() },
                    maho_ext_api::ToolContent::text("second"),
                    maho_ext_api::ToolContent::Image { data: "aW1hZ2Uy".into(), mime_type: "image/jpeg".into() },
                ]), Default::default()).expect("startup message");
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })]);
        session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
        tokio::time::timeout(std::time::Duration::from_secs(5), session.bind_extensions(Default::default())).await.expect("binding");
        assert_eq!(session.get_follow_up_messages(), ["first\nsecond"]);
        actions.end_compaction(maho_ext_api::EndCompactionOptions {
            reason: maho_ext_api::CompactionReason::Manual, signal: Some(signal), aborted: Some(false), error_message: None,
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), session.continue_session()).await.expect("bounded delivery").expect("delivery");
        let users: Vec<_> = session.messages().into_iter().filter(|message| message.role() == "user").collect();
        assert_eq!(users.len(), 1);
        let value = serde_json::to_value(&users[0]).expect("user payload");
        assert_eq!(value["content"][0]["text"], "first\nsecond");
        assert_eq!(value["content"][1]["data"], "aW1hZ2Ux");
        assert_eq!(value["content"][2]["data"], "aW1hZ2Uy");
        assert_eq!(value["content"][1]["mimeType"], "image/png");
        assert_eq!(value["content"][2]["mimeType"], "image/jpeg");
        assert!(!session.agent.has_queued_messages());
        assert_eq!(session.get_last_assistant_text().as_deref(), Some("delivered"));
    }

    #[tokio::test]
    async fn pre_admission_settings_error_preserves_requested_steering() {
        let session = test_session();
        session.with_settings_manager_mut(|manager| manager.apply_overrides(&Map::from_iter([
            ("compaction".to_owned(), serde_json::json!({"enabled":true,"reserveTokens":"invalid"})),
        ])));
        let captured = session.clone();
        let mut extension = maho_ext_api::LoadedExtension::new("<inline:admission-rejected>", session.cwd().into(), Default::default());
        extension.handlers.insert(maho_ext_api::EventKind::SessionStart, vec![Arc::new(move |_, _| {
            maho_ext_api::ExtensionActions::send_user_message(&SessionExtensionActions(Arc::downgrade(&captured.inner)),
                maho_ext_api::UserMessageContent::Text("retained steering".into()), maho_ext_api::SendUserMessageOptions {
                    deliver_as: Some(maho_ext_api::StreamingBehavior::Steer), ..Default::default()
                }).expect("dispatch");
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })]);
        session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
        tokio::time::timeout(std::time::Duration::from_secs(5), session.bind_extensions(Default::default())).await.expect("bounded admission");
        let retained = session.get_steering_messages();
        let follow_up = session.get_follow_up_messages();
        session.clear_queue(false);
        assert_eq!(retained, ["retained steering"]);
        assert!(follow_up.is_empty());
        assert!(session.messages().is_empty());
        assert!(!session.work_barrier.has_active_work());
    }

    #[tokio::test]
    async fn started_extension_failure_does_not_requeue_user_input() {
        let session = retry_session(vec![maho_ai::providers::faux::faux_assistant_message("accepted", Default::default())], 0);
        let errors = Arc::new(Mutex::new(Vec::new()));
        let captured_errors = errors.clone();
        let _subscription = session.subscribe(Arc::new(move |event| {
            if let AgentSessionEvent::ContinuationError { error_message } = event {
                lock(&captured_errors).push(error_message.clone());
            }
        }));
        let post_admission = session.clone();
        let captured = session.clone();
        let started = Arc::new(AtomicU64::new(0));
        let observed = started.clone();
        let mut extension = maho_ext_api::LoadedExtension::new("<inline:started-error>", session.cwd().into(), Default::default());
        extension.handlers.insert(maho_ext_api::EventKind::SessionStart, vec![Arc::new(move |_, _| {
            maho_ext_api::ExtensionActions::send_user_message(&SessionExtensionActions(Arc::downgrade(&captured.inner)),
                maho_ext_api::UserMessageContent::Text("once".into()), Default::default()).expect("dispatch");
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })]);
        extension.handlers.insert(maho_ext_api::EventKind::InputDisposition, vec![Arc::new(move |event, _| {
            if matches!(event, maho_ext_api::ExtensionEvent::InputDisposition { disposition: maho_ext_api::InputDisposition::Started, .. }) {
                observed.fetch_add(1, Ordering::SeqCst);
                post_admission.with_settings_manager_mut(|manager| manager.apply_overrides(&Map::from_iter([
                    ("compaction".to_owned(), serde_json::json!({"enabled":true,"reserveTokens":"invalid"})),
                ])));
            }
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })]);
        session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            session.bind_extensions(Default::default()).await;
            session.wait_for_idle().await;
        }).await.expect("bounded failed turn");
        assert_eq!(started.load(Ordering::SeqCst), 1);
        assert_eq!(session.messages().iter().filter(|message| message.role() == "user").count(), 1);
        assert_eq!(session.pending_message_count(), 0);
        assert!(!session.agent.has_queued_messages());
        assert_eq!(lock(&errors).len(), 1, "completion must reach the adapter error branch");
        assert_eq!(session.get_last_assistant_text().as_deref(), Some("accepted"));
    }

    #[tokio::test]
    async fn extension_prompt_waits_for_pending_manual_compaction() {
        use std::{future::Future, task::Poll};
        let session = test_session();
        let admission = session.prompt_admission.lock().await;
        let mut compact = Box::pin(session.compact(None));
        std::future::poll_fn(|cx| {
            assert!(compact.as_mut().poll(cx).is_pending(), "compaction waits for admission");
            Poll::Ready(())
        }).await;
        assert!(session.state().pending_compaction_admission.is_some());
        let mut prompt = Box::pin(session.prompt("extension input", PromptOptions {
            source: Some(InputSource::Extension), ..Default::default()
        }));
        let pending = std::future::poll_fn(|cx| Poll::Ready(prompt.as_mut().poll(cx).is_pending())).await;
        drop(prompt);
        drop(compact);
        drop(admission);
        assert!(!session.work_barrier.has_active_work());
        assert!(session.state().pending_compaction_admission.is_none());
        assert!(pending, "extension prompt must wait rather than reject during manual compaction");
    }

    #[tokio::test]
    async fn extension_prompt_stays_queued_after_pending_compaction_is_cancelled() {
        use std::{future::Future, task::Poll};
        let session = retry_session(vec![maho_ai::providers::faux::faux_assistant_message("after compaction", Default::default())], 0);
        let admission = session.prompt_admission.lock().await;
        let mut compact = Box::pin(session.compact(None));
        std::future::poll_fn(|cx| {
            assert!(compact.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        }).await;
        let mut prompt = Box::pin(session.prompt("waiting extension", PromptOptions {
            source: Some(InputSource::Extension), ..Default::default()
        }));
        std::future::poll_fn(|cx| {
            assert!(prompt.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        }).await;
        session.abort_compaction();
        drop(admission);
        let (compacted, prompted) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(compact, prompt)
        }).await.expect("released admission");
        assert_eq!(compacted.expect_err("cancelled compaction"), "Compaction cancelled");
        assert_eq!(prompted.expect("extension retained"), PromptDisposition::Queued);
        assert!(session.get_last_assistant_text().is_none());
        assert!(session.messages().is_empty());
        assert_eq!(session.get_steering_messages(), ["waiting extension"]);
        assert!(session.agent.has_queued_messages());
        session.clear_queue(false);
        assert!(!session.work_barrier.has_active_work());
        assert!(!session.is_compacting());
    }

    #[tokio::test]
    async fn extension_prompt_stays_queued_after_pending_compaction_fails() {
        use std::{future::Future, task::Poll};
        let session = retry_session(vec![maho_ai::providers::faux::faux_assistant_message("must not run", Default::default())], 0);
        let admission = session.prompt_admission.lock().await;
        let mut compact = Box::pin(session.compact(None));
        std::future::poll_fn(|cx| {
            assert!(compact.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        }).await;
        let mut prompt = Box::pin(session.prompt("after failed compaction", PromptOptions {
            source: Some(InputSource::Extension), ..Default::default()
        }));
        std::future::poll_fn(|cx| {
            assert!(prompt.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        }).await;
        drop(admission);
        let (compacted, prompted) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(compact, prompt)
        }).await.expect("released admission");
        assert!(compacted.is_err());
        let queued = session.get_steering_messages();
        session.clear_queue(false);
        assert_eq!(prompted.expect("retained prompt"), PromptDisposition::Queued);
        assert_eq!(queued, ["after failed compaction"]);
        assert!(session.messages().is_empty());
        assert!(!session.work_barrier.has_active_work());
    }

    #[tokio::test]
    async fn extension_prompt_runs_after_pending_compaction_succeeds() {
        use std::{future::Future, task::Poll};
        let session = retry_session(vec![maho_ai::providers::faux::faux_assistant_message("after digest", Default::default())], 0);
        session.with_settings_manager_mut(|manager| manager.apply_overrides(&Map::from_iter([
            ("compaction".into(), serde_json::json!({"reserveTokens":0,"reserveScalingEnabled":false,"keepRecentTokens":1})),
        ])));
        for text in ["old task".repeat(200), "recent task".repeat(200)] {
            session.with_session_manager_mut(|manager| manager.append_message(serde_json::json!({"role":"user","content":text,"timestamp":0})));
        }
        session.rebuild_session_context().expect("history");
        let mut extension = maho_ext_api::LoadedExtension::new("<inline:accepted-compaction>", session.cwd().into(), Default::default());
        extension.handlers.insert(maho_ext_api::EventKind::SessionBeforeCompact, vec![Arc::new(|event, _| {
            let maho_ext_api::ExtensionEvent::SessionBeforeCompact(event) = event else { panic!("preparation"); };
            Box::pin(async move { Ok(maho_ext_api::EventResult::SessionBefore(maho_ext_api::SessionBeforeEventResult {
                compaction: Some(maho_ext_api::CompactionResult {
                    summary: "digest".into(), first_kept_entry_id: event.preparation.first_kept_entry_id.clone(),
                    tokens_before: event.preparation.tokens_before, details: None,
                }), ..Default::default()
            })) })
        })]);
        session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
        let admission = session.prompt_admission.lock().await;
        let mut compact = Box::pin(session.compact(None));
        std::future::poll_fn(|cx| { assert!(compact.as_mut().poll(cx).is_pending()); Poll::Ready(()) }).await;
        let mut prompt = Box::pin(session.prompt("after successful compaction", PromptOptions {
            source: Some(InputSource::Extension), ..Default::default()
        }));
        std::future::poll_fn(|cx| { assert!(prompt.as_mut().poll(cx).is_pending()); Poll::Ready(()) }).await;
        drop(admission);
        let (compacted, prompted) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(compact, prompt)
        }).await.expect("successful release");
        assert_eq!(compacted.expect("compaction").summary, "digest");
        assert_eq!(prompted.expect("provider turn"), PromptDisposition::Started);
        assert_eq!(session.get_last_assistant_text().as_deref(), Some("after digest"));
        assert!(session.with_session_manager(|manager| manager.entries()).iter().any(|entry| entry["type"] == "compaction"));
        assert_eq!(session.pending_message_count(), 0);
        assert!(!session.agent.has_queued_messages());
        assert!(!session.work_barrier.has_active_work());
    }

    #[tokio::test]
    async fn extension_prompt_cancels_without_releasing_pending_compaction() {
        use std::{future::Future, task::Poll};
        let session = test_session();
        let admission = session.prompt_admission.lock().await;
        let mut compact = Box::pin(session.compact(None));
        std::future::poll_fn(|cx| {
            assert!(compact.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        }).await;
        let controller = maho_ai::utils::abort::AbortController::new();
        let mut prompt = Box::pin(session.prompt("cancelled extension", PromptOptions {
            source: Some(InputSource::Extension), signal: Some(controller.signal()), ..Default::default()
        }));
        std::future::poll_fn(|cx| {
            assert!(prompt.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        }).await;
        controller.abort(None);
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), &mut prompt).await.expect("cancelled admission");
        assert_eq!(result, Err("Prompt cancelled".into()));
        assert!(session.state().pending_compaction_admission.is_some());
        assert!(session.messages().is_empty());
        drop(prompt);
        drop(compact);
        drop(admission);
        assert!(!session.work_barrier.has_active_work());
        assert!(session.state().pending_compaction_admission.is_none());
    }

    #[tokio::test]
    async fn cancelled_binding_clears_its_readiness_scope() {
        let session = test_session();
        let (entered, observed) = tokio::sync::oneshot::channel();
        let entered = Arc::new(Mutex::new(Some(entered)));
        let mut extension = maho_ext_api::LoadedExtension::new("<inline:cancel-binding>", session.cwd().into(), Default::default());
        extension.handlers.insert(maho_ext_api::EventKind::SessionStart, vec![Arc::new(move |_, _| {
            let entered = entered.clone();
            Box::pin(async move {
                lock(&entered).take().expect("single startup").send(()).expect("observer");
                std::future::pending::<()>().await;
                Ok(maho_ext_api::EventResult::None)
            })
        })]);
        session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
        let mut binding = Box::pin(session.bind_extensions(Default::default()));
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::select! {
                () = &mut binding => panic!("startup handler must stay pending"),
                result = observed => result.expect("startup entered"),
            }
        }).await.expect("bounded startup signal");
        assert!(lock(&session.binding_readiness).is_some());
        drop(binding);
        assert!(lock(&session.binding_readiness).is_none());
        assert!(!session.work_barrier.has_active_work());
    }

    #[tokio::test]
    async fn binding_enforces_builtin_defaults_without_removing_user_tools() {
        let session = test_session();
        session.state().default_tool_names = Some(BTreeSet::from(["read".to_owned()]));
        for (name, builtin) in [("read", true), ("bash", true), ("user-tool", false)] {
            let mut source = empty_source_info();
            source.source = if builtin { "builtin" } else { "inline" }.to_owned();
            source.path = if builtin { format!("<builtin:{name}>") } else { format!("<inline:{name}>") };
            session.register_tool_definition(test_definition(name), source, test_tool(name));
        }
        session.set_active_tools_by_name(vec!["read".to_owned(), "bash".to_owned(), "user-tool".to_owned()]);
        session.bind_extensions(Default::default()).await;
        assert_eq!(session.get_active_tool_names(), ["read", "user-tool"]);
        assert!(session.get_tool_definition("bash").is_some());
    }

    #[test]
    fn all_tools_includes_inactive_definitions_with_source_metadata() {
        let session = test_session();
        let mut definition = test_definition("inactive");
        definition.label = "Inactive label".to_owned();
        definition.exposure = Some(ToolExposure::Search);
        definition.prompt_guidelines = Some(vec!["use with context".to_owned()]);
        let source = SourceInfo { path: "inline-extension".to_owned(), source: "inline".to_owned(),
            scope: SourceScope::Project, origin: SourceOrigin::Package, base_dir: Some("/tmp".to_owned()) };
        session.register_tool_definition(definition, source.clone(), test_tool("inactive"));
        assert!(!session.get_active_tool_names().contains(&"inactive".to_owned()));
        let tools = session.get_all_tools();
        let info = tools.iter().find(|tool| tool.name == "inactive").unwrap();
        assert_eq!(info.label, "Inactive label");
        assert_eq!(info.source_info, source);
        assert_eq!(info.exposure, ToolExposure::Search);
        assert_eq!(info.prompt_guidelines, Some(vec!["use with context".to_owned()]));
        let exposed = maho_ext_api::ExtensionActions::get_all_tools(&SessionExtensionActions(Arc::downgrade(&session.inner))).unwrap();
        assert_eq!(exposed.iter().find(|tool| tool.name == "inactive").unwrap().label, "Inactive label");
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

    #[test]
    fn declared_retry_profile_ignores_global_budget_and_accepts_provider_override() {
        let session = test_session();
        session.agent.set_model(test_model());
        session.with_settings_manager_mut(|manager| manager.set(
            crate::settings_manager::SettingsScope::Global,
            &Map::from_iter([("retry".to_owned(), serde_json::json!({"maxRetries": 2, "baseDelayMs": 1}))]),
        )).expect("settings");
        let mut runtime = session.model_runtime().clone();
        runtime.register_provider("faux", crate::provider_composer::ProviderConfigInput {
            retry_policy: Some(maho_ai::utils::retry_profile::profiles::KIMI_CODE_RETRY_PROFILE.clone()),
            ..Default::default()
        }).expect("provider");
        let profile = session.resolve_retry_profile();
        assert_eq!((profile.turn.max_retries, profile.turn.backoff.base_delay_ms), (9, 500.0));
        session.with_settings_manager_mut(|manager| manager.set(
            crate::settings_manager::SettingsScope::Global,
            &Map::from_iter([("retry".to_owned(), serde_json::json!({"providers": {"faux": {
                "turn": {"maxRetries": 3, "baseDelayMs": 7, "enabled": false}
            }}}))]),
        )).expect("override");
        let profile = session.resolve_retry_profile();
        assert_eq!((profile.turn.max_retries, profile.turn.backoff.base_delay_ms), (3, 7.0));
        assert!(!profile.turn.enabled);
    }

    #[test]
    fn extension_context_reads_configured_settings_and_timeout_bounds() {
        use maho_ext_api::ExtensionContextActions;
        let session = test_session();
        session.register_tool_definition(test_definition("read"), empty_source_info(), test_tool("read"));
        session.set_active_tools_by_name(vec!["read".to_owned()]);
        let options = SessionExtensionActions(Arc::downgrade(&session.inner)).get_system_prompt_options();
        assert_eq!(options.tools, vec!["read"]);
        assert_eq!(options.cwd, std::path::PathBuf::from(session.cwd()));
        session.set_system_prompt_sources(Some("custom baseline".to_owned()), vec!["append context".to_owned()]);
        let options = SessionExtensionActions(Arc::downgrade(&session.inner)).get_system_prompt_options();
        assert_eq!(options.custom_prompt.as_deref(), Some("custom baseline"));
        assert_eq!(options.append_system_prompt.as_deref(), Some("append context"));
        session.agent.set_system_prompt("hook selected prompt".to_owned());
        assert_eq!(session.system_prompt(), "hook selected prompt");
        session.with_settings_manager_mut(|manager| manager.set(
            crate::settings_manager::SettingsScope::Global,
            &Map::from_iter([
                ("promptCache".to_owned(), serde_json::json!({"goalBackstopMaxSeconds": 99, "cacheAwareTimeouts": false,
                    "keepAlive": {"enabled": true, "maxRequestsPerSession": 8, "maxCostUsdPerSession": 0.2, "marginSeconds": 12}})),
                ("lookAt".to_owned(), serde_json::json!({"enabled": false, "models": ["faux/faux-1"]})),
                ("askUser".to_owned(), serde_json::json!({"enabled": false, "timeoutMinutes": 130.5})),
                ("images".to_owned(), serde_json::json!({"autoResize": false, "blockImages": true})),
            ]),
        )).expect("settings");
        let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
        assert_eq!(actions.get_prompt_cache_safe_wait_seconds(), None);
        assert_eq!(actions.get_prompt_cache_goal_backstop_max_seconds(), 99.0);
        let keep_alive = actions.get_prompt_cache_keep_alive_settings();
        assert!(keep_alive.enabled);
        assert_eq!((keep_alive.max_requests_per_session, keep_alive.margin_seconds), (8, 12.0));
        assert_eq!(keep_alive.max_cost_usd_per_session, 0.2);
        assert!(!actions.get_look_at_settings().enabled);
        assert_eq!(actions.get_look_at_settings().models, Some(vec!["faux/faux-1".to_owned()]));
        assert!(!actions.get_ask_user_settings().enabled);
        assert_eq!(actions.get_ask_user_settings().timeout_minutes, 120.0);
        assert!(!actions.get_image_settings().auto_resize);
        assert!(actions.get_image_settings().block_images);
    }

    #[test]
    fn provider_turn_override_applies_all_validated_backoff_knobs() {
        let session = test_session();
        session.agent.set_model(test_model());
        let mut runtime = session.model_runtime().clone();
        runtime.register_provider("faux", crate::provider_composer::ProviderConfigInput {
            retry_policy: Some(maho_ai::utils::retry_profile::profiles::KIMI_CODE_RETRY_PROFILE.clone()),
            ..Default::default()
        }).expect("provider");
        session.with_settings_manager_mut(|manager| manager.apply_overrides(&Map::from_iter([
            ("retry".to_owned(), serde_json::json!({"providers": {"faux": {"turn": {
                "growthFactor": 3, "perAttemptCapMs": null, "serverHintMaxDelayMs": 45,
                "jitter": {"mode": "subtractive", "ratio": 0.5}
            }}}})),
        ])));
        let profile = session.resolve_retry_profile();
        assert_eq!(profile.turn.backoff.growth_factor, 3.0);
        assert_eq!(profile.turn.backoff.per_attempt_cap_ms, None);
        assert_eq!(profile.turn.backoff.jitter,
            maho_ai::utils::retry_profile::types::RetryJitterPolicy::Subtractive { ratio: 0.5 });
        assert!(matches!(profile.turn.server_hint,
            maho_ai::utils::retry_profile::types::RetryServerHintPolicy::Override { ceiling, .. }
                if ceiling.max_delay_ms == Some(45)));
    }

    #[tokio::test]
    async fn extension_compaction_signal_tracks_abort_and_feedback_completion() {
        use maho_ext_api::ExtensionContextActions;
        let session = test_session();
        let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
        let signal = actions.begin_compaction(maho_ext_api::BeginCompactionOptions {
            reason: maho_ext_api::CompactionReason::Manual,
        }).expect("feedback admitted");
        assert!(session.is_compacting());
        assert!(actions.begin_compaction(maho_ext_api::BeginCompactionOptions {
            reason: maho_ext_api::CompactionReason::Manual,
        }).is_none());
        let cancelled = signal.cancelled();
        tokio::pin!(cancelled);
        session.abort_compaction();
        tokio::time::timeout(std::time::Duration::from_secs(1), cancelled).await.expect("abort signal");
        actions.end_compaction(maho_ext_api::EndCompactionOptions {
            reason: maho_ext_api::CompactionReason::Manual, signal: Some(signal.clone()),
            aborted: Some(true), error_message: None,
        });
        assert!(signal.is_aborted());
        assert!(!session.is_compacting());
        assert_eq!(session.compaction_state().status(), "aborted");
        assert_eq!(session.compaction_state().generation(), 1);
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
    async fn generated_branch_summary_is_attached_at_target_with_source_identity() {
        use maho_ai::providers::faux::{faux_provider, RegisterFauxProviderOptions};
        let provider = faux_provider(RegisterFauxProviderOptions { tokens_per_second: Some(0.0), ..Default::default() });
        provider.set_responses(vec![maho_ai::providers::faux::faux_assistant_message("branch digest", Default::default()).into()]);
        let session = test_session_with_stream_function(false);
        session.agent.set_model(provider.get_model(Some("faux-1")).expect("model"));
        let mut runtime = session.model_runtime().clone();
        runtime.register_native_provider(provider.provider.clone());
        runtime.register_provider("faux", crate::provider_composer::ProviderConfigInput {
            config: crate::model_config_schema::ModelsJsonProvider { api_key: Some("faux-test".to_owned()), ..Default::default() },
            ..Default::default()
        }).expect("faux auth");
        let root = session.with_session_manager_mut(|manager| manager.append_message(serde_json::json!({"role":"user","content":"root","timestamp":0})));
        let old = session.with_session_manager_mut(|manager| manager.append_message(serde_json::json!({"role":"user","content":"branch","timestamp":1})));
        session.rebuild_session_context().expect("context");
        let result = session.navigate_tree(root["id"].as_str().expect("root"), TreeNavigationOptions {
            summarize: Some(true), intent: Some(TreeNavigationIntent::Resume), ..Default::default()
        }).await.expect("summary navigation");
        let summary = result.summary_entry.expect("summary entry");
        assert_eq!(summary["fromId"], old["id"]);
        assert!(summary["summary"].as_str().expect("summary").ends_with("branch digest"));
        assert_eq!(summary["fromHook"], false);
        assert_eq!(session.with_session_manager(|manager| manager.leaf_id().map(str::to_owned)), root["id"].as_str().map(str::to_owned));
        assert_eq!(session.messages().len(), 1);
    }

    #[tokio::test]
    async fn stalled_branch_summary_aborts_request_without_changing_session() {
        struct StalledStreams(std::sync::Mutex<Option<maho_ai::utils::abort::AbortSignal>>);
        impl maho_ai::types::ProviderStreams for StalledStreams {
            fn stream(&self, _: &Model, _: &maho_ai::types::Context, options: Option<maho_ai::types::StreamOptions>) -> maho_ai::types::AssistantMessageEventStream {
                *self.0.lock().expect("signal") = options.and_then(|options| options.request.signal);
                maho_ai::types::AssistantMessageEventStream::assistant()
            }
            fn stream_simple(&self, _: &Model, _: &maho_ai::types::Context, _: Option<maho_ai::types::SimpleStreamOptions>) -> maho_ai::types::AssistantMessageEventStream {
                panic!("summary uses full options")
            }
        }
        let streams = Arc::new(StalledStreams(std::sync::Mutex::new(None)));
        let session = test_session_with_stream_function(false);
        let mut model = test_model();
        model.provider = "stalled-summary".to_owned();
        session.agent.set_model(model.clone());
        let provider = maho_ai::models::create_provider(maho_ai::models::CreateProviderOptions {
            id: model.provider.clone(), name: None, base_url: None, headers: None,
            models: vec![model], fetch_models: None, restore_models: None, filter_models: None,
            api: maho_ai::models::ProviderApi::Single(streams.clone()),
        });
        let mut runtime = session.model_runtime().clone();
        runtime.register_native_provider(provider);
        runtime.register_provider("stalled-summary", crate::provider_composer::ProviderConfigInput {
            config: crate::model_config_schema::ModelsJsonProvider { api_key: Some("stalled-fixture".to_owned()), ..Default::default() },
            ..Default::default()
        }).expect("fixture auth");
        session.with_settings_manager_mut(|settings| settings.apply_overrides(&Map::from_iter([
            ("compaction".to_owned(), serde_json::json!({"summarizationMaxDurationMs":1})),
        ])));
        let controller = maho_ai::utils::abort::AbortController::new();
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), session.generate_branch_summary_with_signal(
            &[serde_json::json!({"type":"message","id":"fixture","message":{"role":"user","content":"summarize","timestamp":0}})],
            None, false, controller.signal())).await.expect("summary deadline");
        let error = result.expect_err("stalled provider");
        assert!(error.contains("wall-clock budget"), "{error}");
        assert!(streams.0.lock().expect("signal").as_ref().expect("request signal").aborted());
        assert!(!controller.signal().aborted());
        assert!(session.with_session_manager(|manager| manager.entries().is_empty()));
        session.dispose().await;
    }

    #[tokio::test]
    async fn aborted_branch_summary_does_not_request_or_change_context() {
        let session = test_session_with_stream_function(false);
        let controller = maho_ai::utils::abort::AbortController::new();
        let signal = controller.signal();
        session.state().branch_summary_abort_controller = Some(controller);
        session.abort_branch_summary();
        let result = session.generate_branch_summary_with_signal(&[serde_json::json!({
            "type":"message","id":"branch","parentId":null,"message":{"role":"user","content":"branch task","timestamp":0}
        })], None, false, signal).await.unwrap();
        assert_eq!(result["aborted"], true);
        assert!(session.messages().is_empty());
        assert!(session.with_session_manager(|manager| manager.entries()).is_empty());
    }

    #[tokio::test]
    async fn aborted_branch_provider_response_preserves_navigation_context() {
        use maho_ai::providers::faux::{faux_provider, RegisterFauxProviderOptions};
        let provider = faux_provider(RegisterFauxProviderOptions { tokens_per_second: Some(0.0), ..Default::default() });
        provider.set_responses(vec![maho_ai::providers::faux::faux_assistant_message("", maho_ai::providers::faux::FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Aborted), ..Default::default()
        }).into()]);
        let session = test_session_with_stream_function(false);
        session.agent.set_model(provider.get_model(Some("faux-1")).expect("model"));
        let mut runtime = session.model_runtime().clone();
        runtime.register_native_provider(provider.provider.clone());
        runtime.register_provider("faux", crate::provider_composer::ProviderConfigInput {
            config: crate::model_config_schema::ModelsJsonProvider { api_key: Some("faux-test".to_owned()), ..Default::default() },
            ..Default::default()
        }).expect("faux auth");
        let root = session.with_session_manager_mut(|manager| manager.append_message(serde_json::json!({"role":"user","content":"root","timestamp":0})));
        let leaf = session.with_session_manager_mut(|manager| manager.append_message(serde_json::json!({"role":"user","content":"branch","timestamp":1})));
        session.rebuild_session_context().expect("context");
        let result = session.navigate_tree(root["id"].as_str().expect("root"), TreeNavigationOptions {
            summarize: Some(true), ..Default::default()
        }).await.expect("cancelled navigation");
        assert!(result.cancelled);
        assert_eq!(result.aborted, Some(true));
        assert_eq!(session.with_session_manager(|manager| manager.leaf_id().map(str::to_owned)), leaf["id"].as_str().map(str::to_owned));
        assert_eq!(session.with_session_manager(|manager| manager.entries().len()), 2);
        assert!(!session.is_compacting());
    }

    #[tokio::test]
    async fn manual_compaction_runs_faux_summary_and_records_file_details() {
        use maho_ai::providers::faux::{faux_provider, RegisterFauxProviderOptions};
        let provider = faux_provider(RegisterFauxProviderOptions {
            tokens_per_second: Some(0.0), models: Some(vec![
                maho_ai::providers::faux::FauxModelDefinition { id: "faux-1".to_owned(), ..Default::default() },
                maho_ai::providers::faux::FauxModelDefinition { id: "summary".to_owned(), ..Default::default() },
            ]), ..Default::default()
        });
        provider.set_responses(vec![
            maho_ai::providers::faux::faux_assistant_message("digest", Default::default()).into(),
            maho_ai::providers::faux::faux_assistant_message("prefix digest", Default::default()).into(),
        ]);
        let session = test_session_with_stream_function(false);
        session.agent.set_model(provider.get_model(Some("faux-1")).expect("model"));
        let mut runtime = session.model_runtime().clone();
        runtime.register_native_provider(provider.provider.clone());
        runtime.register_provider("faux", crate::provider_composer::ProviderConfigInput {
            config: crate::model_config_schema::ModelsJsonProvider { api_key: Some("faux-test".to_owned()), ..Default::default() },
            ..Default::default()
        }).expect("auth");
        session.with_settings_manager_mut(|manager| manager.apply_overrides(&Map::from_iter([
            ("compaction".to_owned(), serde_json::json!({"keepRecentTokens":1,"model":"faux/summary"})),
        ])));
        session.with_session_manager_mut(|manager| manager.append_message(serde_json::json!({"role":"user","content":"old task","timestamp":0})));
        session.with_session_manager_mut(|manager| manager.append_message(serde_json::json!({"role":"user","content":"recent task","timestamp":1})));
        session.with_session_manager_mut(|manager| manager.append_message(serde_json::to_value(
            maho_ai::providers::faux::faux_assistant_message("recent answer", Default::default())).expect("assistant")));
        session.rebuild_session_context().expect("context");
        let result = session.compact(None).await.expect("compaction");
        assert_eq!(result.summary, "digest\n\n---\n\n**Turn Context (split turn):**\n\nprefix digest");
        assert!(result.details.expect("file details")["readFiles"].is_array());
        assert!(result.usage.is_some());
        assert_eq!(result.estimated_tokens_after, Some(session.with_session_manager(|manager| manager.build_context(manager.leaf_id()))
            .messages.iter().map(crate::compaction::compaction::estimate_tokens).sum::<u64>() as i64));
        assert!(!session.is_compacting());
        assert_eq!(session.compaction_state().status(), "completed");
        assert_eq!(session.compaction_state().generation(), 1);
        assert_eq!(session.model().id, "faux-1");
        assert_eq!(provider.get_call_log().iter().map(|call| call.model_id.as_str()).collect::<Vec<_>>(), ["summary", "summary"]);
        assert_eq!(session.with_session_manager(|manager| manager.entries().last().expect("entry")["type"].clone()), "compaction");
    }

    #[tokio::test]
    async fn cancelled_compaction_hook_makes_no_summary_request() {
        use maho_ai::providers::faux::{faux_provider, RegisterFauxProviderOptions};
        let provider = faux_provider(RegisterFauxProviderOptions { tokens_per_second: Some(0.0), ..Default::default() });
        let session = test_session_with_stream_function(false);
        session.agent.set_model(provider.get_model(Some("faux-1")).expect("model"));
        let mut runtime = session.model_runtime().clone();
        runtime.register_native_provider(provider.provider.clone());
        runtime.register_provider("faux", crate::provider_composer::ProviderConfigInput {
            config: crate::model_config_schema::ModelsJsonProvider { api_key: Some("faux-test".to_owned()), ..Default::default() },
            ..Default::default()
        }).expect("auth");
        session.with_settings_manager_mut(|manager| manager.apply_overrides(&Map::from_iter([
            ("compaction".to_owned(), serde_json::json!({"keepRecentTokens":1})),
        ])));
        for (index, text) in ["old task", "recent task"].into_iter().enumerate() {
            session.with_session_manager_mut(|manager| manager.append_message(serde_json::json!({"role":"user","content":text,"timestamp":index})));
        }
        session.rebuild_session_context().expect("context");
        let messages = session.messages();
        let entries = session.with_session_manager(|manager| manager.entries());
        let (started, entered) = tokio::sync::oneshot::channel();
        let started = Arc::new(Mutex::new(Some(started)));
        let mut extension = maho_ext_api::LoadedExtension::new("<inline:compaction-abort>", session.cwd().into(), Default::default());
        extension.handlers.insert(maho_ext_api::EventKind::SessionBeforeCompact, vec![Arc::new(move |event, _| {
            let started = started.clone();
            Box::pin(async move {
                let maho_ext_api::ExtensionEvent::SessionBeforeCompact(event) = event else { panic!("compaction event"); };
                lock(&started).take().expect("single hook").send(()).expect("observer");
                event.signal.cancelled().await;
                Ok(maho_ext_api::EventResult::None)
            })
        })]);
        session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
        let (result, prompted) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(session.compact(None), async {
                use std::{future::Future, task::Poll};
                entered.await.expect("hook started");
                let mut prompt = Box::pin(session.prompt("retained after running compaction", PromptOptions {
                    source: Some(InputSource::Extension), streaming_behavior: Some(StreamingBehavior::FollowUp), ..Default::default()
                }));
                std::future::poll_fn(|cx| {
                    assert!(prompt.as_mut().poll(cx).is_pending());
                    Poll::Ready(())
                }).await;
                session.abort_compaction();
                prompt.await
            })
        }).await.expect("bounded compaction cancellation");
        assert!(result.is_err());
        assert_eq!(prompted.expect("retained extension prompt"), PromptDisposition::Queued);
        assert_eq!(session.get_follow_up_messages(), ["retained after running compaction"]);
        session.clear_queue(false);
        assert!(provider.get_call_log().is_empty());
        assert_eq!(session.messages(), messages);
        assert_eq!(session.with_session_manager(|manager| manager.entries()), entries);
        assert_eq!(session.compaction_state().status(), "aborted");
        assert!(!session.is_compacting());
    }

    #[tokio::test]
    async fn summary_retry_preserves_affinity_and_isolates_request_identity() {
        use maho_ai::providers::faux::{faux_provider, faux_assistant_message, FauxAssistantMessageOptions,
            FauxResponseStep, RegisterFauxProviderOptions};
        let provider = faux_provider(RegisterFauxProviderOptions { tokens_per_second: Some(0.0), ..Default::default() });
        let options_seen = Arc::new(Mutex::new(Vec::new()));
        let make_response = |failure: bool| {
            let options_seen = options_seen.clone();
            FauxResponseStep::Factory(Arc::new(move |_, options, _, _| {
                lock(&options_seen).push(options.expect("summary options").stream.clone());
                Box::pin(async move { faux_assistant_message(if failure { "" } else { "summary" }, FauxAssistantMessageOptions {
                    stop_reason: failure.then_some(maho_ai::types::StopReason::Error),
                    error_message: failure.then(|| "socket hang up".to_owned()), ..Default::default()
                }) })
            }))
        };
        provider.set_responses(vec![make_response(true), make_response(false)]);
        let session = test_session();
        let mut runtime = session.model_runtime().clone();
        runtime.register_native_provider(provider.provider.clone());
        runtime.register_provider("faux", crate::provider_composer::ProviderConfigInput {
            config: crate::model_config_schema::ModelsJsonProvider { api_key: Some("faux-test".to_owned()), ..Default::default() },
            ..Default::default()
        }).expect("auth");
        session.with_settings_manager_mut(|manager| manager.apply_overrides(&Map::from_iter([
            ("retry".into(), serde_json::json!({"enabled":true,"maxRetries":1,"baseDelayMs":0})),
        ])));
        let model = provider.get_model(Some("faux-1")).expect("model");
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), session.complete_summary_stream(&model,
            &maho_ai::types::Context::default(), Some(maho_ai::types::StreamOptions {
                session_id: Some("caller-affinity".into()), ..Default::default()
            }))).await;
        session.dispose().await;
        let response = result.expect("bounded retries").expect("retried summary");
        assert_eq!(maho_ai::utils::text::content_text(&response.content, ""), "summary");
        let seen = lock(&options_seen);
        assert_eq!(seen.len(), 2);
        assert_ne!(seen[0].session_id.as_deref(), Some("caller-affinity"));
        assert_eq!(seen[0].session_id, seen[1].session_id);
        assert_eq!(seen[0].request.affinity_session_id.as_deref(), Some("caller-affinity"));
        assert_eq!(seen[1].request.affinity_session_id.as_deref(), Some("caller-affinity"));
        assert_eq!(seen[0].cache_retention, Some(maho_ai::types::CacheRetention::None));
        assert_eq!(seen[1].cache_retention, Some(maho_ai::types::CacheRetention::None));
    }

    #[tokio::test]
    async fn deterministic_summary_failure_preserves_explicit_retention_without_retry() {
        use maho_ai::providers::faux::{faux_provider, faux_assistant_message, FauxAssistantMessageOptions,
            FauxResponseStep, RegisterFauxProviderOptions};
        let provider = faux_provider(RegisterFauxProviderOptions { tokens_per_second: Some(0.0), ..Default::default() });
        let seen = Arc::new(Mutex::new(Vec::new()));
        let captured = seen.clone();
        provider.set_responses(vec![FauxResponseStep::Factory(Arc::new(move |_, options, _, _| {
            lock(&captured).push(options.expect("summary options").stream.clone());
            Box::pin(async { faux_assistant_message("", FauxAssistantMessageOptions {
                stop_reason: Some(maho_ai::types::StopReason::Error),
                error_message: Some("invalid request fixture".into()), ..Default::default()
            }) })
        }))]);
        let session = test_session();
        let mut runtime = session.model_runtime().clone();
        runtime.register_native_provider(provider.provider.clone());
        runtime.register_provider("faux", crate::provider_composer::ProviderConfigInput {
            config: crate::model_config_schema::ModelsJsonProvider { api_key: Some("faux-test".into()), ..Default::default() },
            ..Default::default()
        }).expect("auth");
        session.with_settings_manager_mut(|manager| manager.apply_overrides(&Map::from_iter([
            ("retry".into(), serde_json::json!({"enabled":true,"maxRetries":3,"baseDelayMs":0})),
        ])));
        let model = provider.get_model(Some("faux-1")).expect("model");
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), session.complete_summary_stream(&model,
            &maho_ai::types::Context::default(), Some(maho_ai::types::StreamOptions {
                cache_retention: Some(maho_ai::types::CacheRetention::Short),
                session_id: Some("caller".into()), request: maho_ai::types::ProviderRequestOptions {
                    affinity_session_id: Some("explicit-affinity".into()), ..Default::default()
                }, ..Default::default()
            }))).await;
        session.dispose().await;
        assert_eq!(result.expect("bounded deterministic error").expect_err("failure").to_string(), "invalid request fixture");
        let seen = lock(&seen);
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].cache_retention, Some(maho_ai::types::CacheRetention::Short));
        assert_eq!(seen[0].request.affinity_session_id.as_deref(), Some("explicit-affinity"));
        assert_ne!(seen[0].session_id.as_deref(), Some("caller"));
    }

    #[tokio::test]
    async fn session_abort_cancels_started_summary_without_late_apply() {
        use maho_ai::providers::faux::{faux_provider, FauxResponseStep, RegisterFauxProviderOptions};
        let provider = faux_provider(RegisterFauxProviderOptions { tokens_per_second: Some(0.0), ..Default::default() });
        let (started, entered) = tokio::sync::oneshot::channel();
        let started = Arc::new(Mutex::new(Some(started)));
        let (cancelled, observed) = tokio::sync::oneshot::channel();
        let cancelled = Arc::new(Mutex::new(Some(cancelled)));
        provider.set_responses(vec![FauxResponseStep::Factory(Arc::new(move |_, options, _, _| {
            let signal = options.expect("summary options").stream.request.signal.clone().expect("request signal");
            let started = started.clone();
            let cancelled = cancelled.clone();
            Box::pin(async move {
                lock(&started).take().expect("single summary request").send(()).expect("request observer");
                signal.cancelled().await;
                lock(&cancelled).take().expect("single cancellation").send(()).expect("cancellation observer");
                maho_ai::providers::faux::faux_assistant_message("late summary", Default::default())
            })
        }))]);
        let session = test_session_with_stream_function(false);
        session.agent.set_model(provider.get_model(Some("faux-1")).expect("model"));
        let mut runtime = session.model_runtime().clone();
        runtime.register_native_provider(provider.provider.clone());
        runtime.register_provider("faux", crate::provider_composer::ProviderConfigInput {
            config: crate::model_config_schema::ModelsJsonProvider { api_key: Some("faux-test".to_owned()), ..Default::default() },
            ..Default::default()
        }).expect("auth");
        session.with_settings_manager_mut(|manager| manager.apply_overrides(&Map::from_iter([
            ("compaction".to_owned(), serde_json::json!({"keepRecentTokens":1})),
        ])));
        for (index, text) in ["old task", "recent task"].into_iter().enumerate() {
            session.with_session_manager_mut(|manager| manager.append_message(serde_json::json!({"role":"user","content":text,"timestamp":index})));
        }
        session.rebuild_session_context().expect("context");
        let messages = session.messages();
        let entries = session.with_session_manager(|manager| manager.entries());
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let (result, ()) = tokio::join!(session.compact(None), async {
                entered.await.expect("actual summary started");
                session.abort().await;
                observed.await.expect("request cancelled");
            });
            result
        }).await;
        let after_messages = session.messages();
        let after_entries = session.with_session_manager(|manager| manager.entries());
        let status = session.compaction_state().status().to_owned();
        let compacting = session.is_compacting();
        let calls = provider.get_call_log().len();
        let cleanup = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            if result.is_err() { session.abort().await; }
            session.dispose().await;
        }).await;
        cleanup.expect("bounded session cleanup");
        assert!(result.expect("bounded request cancellation").is_err());
        assert_eq!(calls, 1);
        assert_eq!(after_messages, messages);
        assert_eq!(after_entries, entries);
        assert_eq!(status, "aborted");
        assert!(!compacting);
    }

    #[tokio::test]
    async fn disposal_cancels_title_and_prevents_late_background_launch() {
        use maho_ai::providers::faux::{faux_provider, RegisterFauxProviderOptions};
        let (started, entered) = tokio::sync::oneshot::channel();
        let started = Arc::new(Mutex::new(Some(started)));
        let (release, released) = tokio::sync::watch::channel(false);
        let provider = faux_provider(RegisterFauxProviderOptions {
            scheduler_hook: Some(Arc::new(move || {
                let started = started.clone(); let mut released = released.clone();
                Box::pin(async move {
                    if let Some(started) = lock(&started).take() { started.send(()).expect("observer"); }
                    released.wait_for(|released| *released).await.expect("release");
                })
            })), ..Default::default()
        });
        provider.set_responses(vec![maho_ai::providers::faux::faux_assistant_message("<title>Stale Title</title>", Default::default()).into()]);
        let session = test_session_with_stream_function(false);
        session.agent.set_timeout_ms(Some(12_345));
        session.agent.set_max_retry_delay_ms(Some(678));
        session.agent.set_transport(Some(maho_ai::types::Transport::Sse));
        session.agent.set_model(provider.get_model(Some("faux-1")).expect("model"));
        let mut runtime = session.model_runtime().clone();
        runtime.register_native_provider(provider.provider.clone());
        runtime.register_provider("faux", crate::provider_composer::ProviderConfigInput {
            config: crate::model_config_schema::ModelsJsonProvider { api_key: Some("faux-test".to_owned()), ..Default::default() },
            ..Default::default()
        }).expect("auth");
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(session.generate_session_title_if_needed("Implement a native session"), async {
                entered.await.expect("title request started");
                assert!(session.state().session_title_abort_controller.is_some());
                session.dispose().await;
                release.send_replace(true);
            });
        }).await.expect("bounded title cancellation");
        assert!(session.session_name().is_none());
        let calls = provider.get_call_log();
        let options = calls[0].options.as_ref().expect("title options");
        assert_eq!(options.request.timeout_ms, Some(12_345));
        assert_eq!(options.request.max_retry_delay_ms, Some(678));
        assert_eq!(options.transport, Some(maho_ai::types::Transport::Sse));
        assert!(provider.get_call_log()[0].options.as_ref().expect("options").request.signal.as_ref().expect("signal").aborted());
        session.generate_session_title_if_needed("Implement another feature").await;
        assert_eq!(provider.get_call_log().len(), 1);
        assert!(session.state().session_title_abort_controller.is_none());
    }

    #[tokio::test]
    async fn title_generation_retries_one_transient_error() {
        use maho_ai::providers::faux::{faux_provider, RegisterFauxProviderOptions};
        let provider = faux_provider(RegisterFauxProviderOptions { tokens_per_second: Some(0.0), ..Default::default() });
        provider.set_responses(vec![
            maho_ai::providers::faux::faux_assistant_message("", maho_ai::providers::faux::FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Error), error_message: Some("503 Service Unavailable".to_owned()), ..Default::default()
            }).into(),
            maho_ai::providers::faux::faux_assistant_message("<title>Native Session Recovery</title>", Default::default()).into(),
        ]);
        let session = test_session_with_stream_function(false);
        session.agent.set_model(provider.get_model(Some("faux-1")).expect("model"));
        let mut runtime = session.model_runtime().clone();
        runtime.register_native_provider(provider.provider.clone());
        runtime.register_provider("faux", crate::provider_composer::ProviderConfigInput {
            config: crate::model_config_schema::ModelsJsonProvider { api_key: Some("faux-test".to_owned()), ..Default::default() },
            ..Default::default()
        }).expect("auth");
        session.with_settings_manager_mut(|manager| manager.apply_overrides(&Map::from_iter([
            ("retry".to_owned(), serde_json::json!({"enabled":true,"maxRetries":5,"baseDelayMs":0})),
        ])));
        session.generate_session_title_if_needed("Recover native session calls").await;
        assert_eq!(session.session_name().as_deref(), Some("Native Session Recovery"));
        assert_eq!(provider.get_call_log().len(), 2);
        assert!(session.state().session_title_abort_controller.is_none());
    }

    #[tokio::test]
    async fn scheduled_continuation_does_not_clear_a_later_user_abort() {
        let session = retry_session(vec![maho_ai::providers::faux::faux_assistant_message("unexpected", Default::default())], 0);
        session.agent.follow_up(session_message_from_value(serde_json::json!({"role":"user","content":"queued","timestamp":0})).expect("queued message"));
        let generation = session.user_abort_generation.load(Ordering::SeqCst);
        let admission = session.prompt_admission.lock().await;
        let continuation = session.continue_session_internal(Some(generation));
        tokio::pin!(continuation);
        std::future::poll_fn(|context| {
            assert!(std::future::Future::poll(continuation.as_mut(), context).is_pending());
            std::task::Poll::Ready(())
        }).await;
        session.abort().await;
        drop(admission);
        continuation.await.expect("suppressed continuation");
        assert!(session.state().user_aborted);
        assert!(session.agent.has_queued_messages());
        assert!(session.messages().is_empty());
        assert!(!session.work_barrier.has_active_work());
    }

    #[tokio::test]
    async fn idle_custom_trigger_respects_abort_before_scheduled_admission() {
        let session = retry_session(vec![maho_ai::providers::faux::faux_assistant_message("unexpected", Default::default())], 0);
        let admission = session.prompt_admission.lock().await;
        maho_ext_api::ExtensionActions::send_message(&SessionExtensionActions(Arc::downgrade(&session.inner)),
            maho_ext_api::CustomMessage { custom_type: "notice".to_owned(), content: vec![maho_tools::definition::ToolContent::text("saved notice")], display: true, details: None },
            maho_ext_api::SendMessageOptions { trigger_turn: true, deliver_as: None }).expect("custom trigger");
        session.abort().await;
        drop(admission);
        tokio::time::timeout(std::time::Duration::from_secs(5), session.wait_for_idle()).await.expect("settled scheduled work");
        assert!(session.state().user_aborted);
        assert_eq!(session.messages().len(), 1);
        assert_eq!(session.messages()[0].role(), "custom");
        assert_eq!(session.with_session_manager(|manager| manager.entries()).len(), 1);
    }

    #[tokio::test]
    async fn dropped_pending_compaction_releases_its_admission_owner() {
        let session = test_session();
        let admission = session.prompt_admission.lock().await;
        {
            let compact = session.compact(None);
            tokio::pin!(compact);
            std::future::poll_fn(|context| {
                assert!(std::future::Future::poll(compact.as_mut(), context).is_pending());
                std::task::Poll::Ready(())
            }).await;
            assert!(session.is_compacting());
        }
        assert!(!session.is_compacting());
        assert!(!session.work_barrier.has_active_work());
        drop(admission);
    }

    #[tokio::test]
    async fn pending_manual_compaction_is_visible_and_abortable_before_admission() {
        let session = test_session();
        let admission = session.prompt_admission.lock().await;
        let compact = session.compact(None);
        tokio::pin!(compact);
        std::future::poll_fn(|context| {
            assert!(std::future::Future::poll(compact.as_mut(), context).is_pending());
            std::task::Poll::Ready(())
        }).await;
        assert!(session.is_compacting());
        session.abort_compaction();
        assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(1), compact).await.expect("cancellation without lock release")
            .expect_err("cancelled admission"), "Compaction cancelled");
        drop(admission);
        assert!(!session.is_compacting());
        assert_eq!(session.compaction_state().generation(), 0);
        assert!(!session.work_barrier.has_active_work());
    }

    #[tokio::test]
    async fn precomputed_compaction_reuses_feedback_id_and_reports_rejection() {
        use maho_ext_api::ExtensionContextActions;
        for reject in [false, true] {
            let session = test_session();
            session.agent.set_model(test_model());
            let retained = session.with_session_manager_mut(|manager| manager.append_message(serde_json::json!({"role":"user","content":"recent","timestamp":0})));
            let events = Arc::new(Mutex::new(Vec::new()));
            let captured = events.clone();
            let _subscription = session.subscribe(Arc::new(move |event| {
                if matches!(event, AgentSessionEvent::CompactionStart { .. } | AgentSessionEvent::CompactionEnd { .. }) { lock(&captured).push(event.clone()); }
            }));
            let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
            let signal = actions.begin_compaction(maho_ext_api::BeginCompactionOptions { reason: maho_ext_api::CompactionReason::Extension }).expect("feedback");
            let applied = actions.apply_compaction(maho_ext_api::CompactionResult { summary: if reject { "x".repeat(600_000) } else { "digest".to_owned() },
                first_kept_entry_id: retained["id"].as_str().expect("retained").to_owned(), tokens_before: 100, details: None },
                maho_ext_api::ApplyCompactionOptions { reason: maho_ext_api::CompactionReason::Extension,
                    expected_revision: Some(session.message_revision()), expected_warm_anchor: None, signal: Some(signal) }).await.expect("application result");
            assert_eq!(applied, if reject { maho_ext_api::ApplyCompactionResult::Rejected } else { maho_ext_api::ApplyCompactionResult::Applied });
            assert!(!session.is_compacting());
            assert_eq!(session.compaction_state().generation(), 1);
            assert_eq!(session.compaction_state().status(), if reject { "failed" } else { "completed" });
            let events = lock(&events);
            assert_eq!(events.len(), 2);
            let AgentSessionEvent::CompactionStart { request_id, .. } = &events[0] else { panic!("start"); };
            assert!(matches!(&events[1], AgentSessionEvent::CompactionEnd { request_id: end, accepted: Some(accepted), error_message, .. }
                if end == request_id && *accepted != reject && error_message.is_some() == reject));
            assert_eq!(session.with_session_manager(|manager| manager.entries()).len(), if reject { 1 } else { 2 });
        }
    }

    #[tokio::test]
    async fn precomputed_extension_compaction_preserves_provenance_and_rejects_stale_revision() {
        use maho_ext_api::ExtensionContextActions;
        let session = test_session();
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = events.clone();
        let _subscription = session.subscribe(Arc::new(move |event| {
            if matches!(event, AgentSessionEvent::CompactionStart { .. } | AgentSessionEvent::CompactionEnd { .. }) { lock(&captured).push(event.clone()); }
        }));
        session.agent.set_model(test_model());
        let retained = session.with_session_manager_mut(|manager| manager.append_message(serde_json::json!({"role":"user","content":"recent","timestamp":0})));
        let actions = SessionExtensionActions(Arc::downgrade(&session.inner));
        let result = maho_ext_api::CompactionResult { summary: "digest".to_owned(), first_kept_entry_id: retained["id"].as_str().expect("retained").to_owned(),
            tokens_before: 100, details: None };
        let options = |revision| maho_ext_api::ApplyCompactionOptions { reason: maho_ext_api::CompactionReason::Extension,
            expected_revision: Some(revision), expected_warm_anchor: None, signal: None };
        assert_eq!(actions.apply_compaction(result.clone(), options(session.message_revision() + 1)).await.expect("stale"), maho_ext_api::ApplyCompactionResult::Stale);
        assert_eq!(session.with_session_manager(|manager| manager.entries()).len(), 1);
        assert!(lock(&events).is_empty());
        assert_eq!(actions.apply_compaction(result, options(session.message_revision())).await.expect("applied"), maho_ext_api::ApplyCompactionResult::Applied);
        assert_eq!(session.with_session_manager(|manager| manager.entries().last().expect("compaction")["fromHook"].clone()), true);
        assert_eq!(session.compaction_state().status(), "completed");
        let events = lock(&events);
        let AgentSessionEvent::CompactionStart { request_id, .. } = &events[0] else { panic!("start"); };
        assert!(matches!(&events[1], AgentSessionEvent::CompactionEnd { request_id: completed, accepted: Some(true), .. } if completed == request_id));
        assert_eq!(events.len(), 2);
    }

    #[tokio::test]
    async fn superseded_pending_compaction_cannot_release_the_new_admission() {
        let session = test_session();
        let admission = session.prompt_admission.lock().await;
        let first = session.compact(None);
        tokio::pin!(first);
        std::future::poll_fn(|context| {
            assert!(std::future::Future::poll(first.as_mut(), context).is_pending());
            std::task::Poll::Ready(())
        }).await;
        let second = session.compact(None);
        tokio::pin!(second);
        std::future::poll_fn(|context| {
            assert!(std::future::Future::poll(second.as_mut(), context).is_pending());
            std::task::Poll::Ready(())
        }).await;
        assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(1), first).await.expect("superseded claimant")
            .expect_err("superseded"), "Compaction cancelled");
        assert!(session.is_compacting());
        assert!(session.work_barrier.has_active_work());
        session.abort_compaction();
        assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(1), second).await.expect("current claimant")
            .expect_err("aborted"), "Compaction cancelled");
        drop(admission);
        assert!(!session.is_compacting());
        assert!(!session.work_barrier.has_active_work());
    }

    #[tokio::test]
    async fn runtime_shutdown_carries_replacement_target_and_live_signal() {
        let session = test_session();
        let observed = Arc::new(Mutex::new(Vec::new()));
        let captured = observed.clone();
        let mut extension = maho_ext_api::LoadedExtension::new("<inline:replacement-shutdown>", session.cwd().into(), Default::default());
        extension.handlers.insert(maho_ext_api::EventKind::SessionShutdown, vec![Arc::new(move |event, _| {
            let maho_ext_api::ExtensionEvent::SessionShutdown(event) = event else { panic!("shutdown"); };
            assert_eq!(event.reason, maho_ext_api::SessionReason::Resume);
            assert!(!event.signal.as_ref().expect("shutdown signal").is_aborted());
            lock(&captured).push(event.target_session_file.clone());
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })]);
        session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
        session.runtime_shutdown_to(maho_ext_api::SessionReason::Resume, Some("target.jsonl".to_owned())).await;
        session.emit_session_shutdown(maho_ext_api::SessionReason::Resume).await;
        assert_eq!(*lock(&observed), [Some("target.jsonl".to_owned()), None]);
    }

    #[tokio::test]
    async fn title_failure_is_reported_as_runtime_extension_error() {
        let session = test_session_with_stream_function(false);
        session.agent.set_model(test_model());
        let errors = Arc::new(Mutex::new(Vec::new()));
        let observed = errors.clone();
        session.state().extension_error_listener = Some(Arc::new(move |error| lock(&observed).push(error.clone())));
        let events = Arc::new(Mutex::new(Vec::new()));
        let observed = events.clone();
        let _subscription = session.subscribe(Arc::new(move |event| lock(&observed).push(event.clone())));
        session.generate_session_title_if_needed("Inspect the workspace").await;
        let errors = lock(&errors);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].extension_path, "<runtime>");
        assert_eq!(errors[0].event, "session_title_generation");
        assert!(!errors[0].error.is_empty());
        assert!(!lock(&events).iter().any(|event| matches!(event, AgentSessionEvent::ContinuationError { .. })));
        assert!(session.state().session_title_abort_controller.is_none());
    }

    #[tokio::test]
    async fn extension_compaction_rejection_has_no_execution_failure_event() {
        let session = test_session();
        let terminal = Arc::new(Mutex::new(Vec::new()));
        let observed = terminal.clone();
        let _subscription = session.subscribe(Arc::new(move |event| {
            if matches!(event, AgentSessionEvent::CompactionEnd { .. }) { lock(&observed).push(event.clone()); }
        }));
        session.agent.set_model(test_model());
        session.with_settings_manager_mut(|manager| manager.apply_overrides(&Map::from_iter([
            ("compaction".to_owned(), serde_json::json!({"keepRecentTokens":1})),
        ])));
        for text in ["old task", "recent task"] {
            session.with_session_manager_mut(|manager| manager.append_message(serde_json::json!({"role":"user","content":text,"timestamp":0})));
        }
        session.rebuild_session_context().expect("context");
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut extension = maho_ext_api::LoadedExtension::new("<inline:compaction-rejection>", session.cwd().into(), Default::default());
        extension.handlers.insert(maho_ext_api::EventKind::SessionBeforeCompact, vec![Arc::new(|_, _| Box::pin(async {
            Ok(maho_ext_api::EventResult::SessionBefore(maho_ext_api::SessionBeforeEventResult {
                cancel: Some(true), rejection_cause: Some(maho_ext_api::CompactionRejectionCause::CircuitBreaker),
                reason: Some("cooldown active".to_owned()), ..Default::default()
            }))
        }))]);
        for kind in [maho_ext_api::EventKind::SessionCompact, maho_ext_api::EventKind::SessionCompactFailed] {
            let captured = events.clone();
            extension.handlers.insert(kind, vec![Arc::new(move |event, _| {
                lock(&captured).push(event.clone());
                Box::pin(async { Ok(maho_ext_api::EventResult::None) })
            })]);
        }
        session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
        assert!(session.compact(None).await.is_err());
        assert_eq!(session.compaction_state().status(), "failed");
        assert!(matches!(&lock(&terminal)[0], AgentSessionEvent::CompactionEnd {
            aborted: true, error_message: Some(message), accepted: Some(false), ..
        } if message == "Compaction rejected: cooldown active"));
        assert!(matches!(session.compaction_state(), crate::compaction::lifecycle::CompactionLifecycleState::Failed(_, _, Some(cause), _)
            if cause == "circuit-breaker"));
        let events = lock(&events);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], maho_ext_api::ExtensionEvent::SessionCompact(maho_ext_api::SessionCompactEvent::Rejected {
            rejection_cause: maho_ext_api::CompactionRejectionCause::CircuitBreaker, ..
        })));
    }

    #[tokio::test]
    async fn provider_owned_compaction_bypasses_native_final_admission() {
        let session = test_session();
        let mut model = test_model();
        model.context_window = 128;
        session.agent.set_model(model.clone());
        session.with_settings_manager_mut(|manager| manager.apply_overrides(&Map::from_iter([
            ("compaction".to_owned(), serde_json::json!({"keepRecentTokens":1,"reserveTokens":0,"reserveScalingEnabled":false})),
        ])));
        for text in ["old task", "recent task"] {
            session.with_session_manager_mut(|manager| manager.append_message(serde_json::json!({"role":"user","content":text,"timestamp":0})));
        }
        session.rebuild_session_context().expect("context");
        let mut extension = maho_ext_api::LoadedExtension::new("<inline:provider-owner>", session.cwd().into(), Default::default());
        extension.handlers.insert(maho_ext_api::EventKind::SessionBeforeCompact, vec![Arc::new(|_, _| Box::pin(async {
            Ok(maho_ext_api::EventResult::SessionBefore(maho_ext_api::SessionBeforeEventResult {
                cancel: Some(true), rejection_cause: Some(maho_ext_api::CompactionRejectionCause::ExternalOwner), ..Default::default()
            }))
        }))]);
        session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
        assert!(session.compact(None).await.is_err());
        assert!(session.is_compaction_delegated());
        let additions = [session_message_from_value(serde_json::json!({
            "role":"custom", "customType":"admission-aside", "content":"large input ".repeat(200), "display":true, "timestamp":0,
        })).expect("custom addition")];
        session.enforce_final_provider_admission(&additions).await.expect("provider owns oversized admission");
        model.id = "other-model".to_owned();
        session.agent.set_model(model);
        assert!(!session.is_compaction_delegated());
        assert!(session.enforce_final_provider_admission(&additions).await.is_err());
    }

    #[tokio::test]
    async fn late_queued_input_requires_final_admission_without_consuming_queue() {
        let session = test_session();
        let mut model = test_model();
        model.context_window = 128;
        session.agent.set_model(model);
        session.with_settings_manager_mut(|manager| manager.apply_overrides(&Map::from_iter([
            ("compaction".to_owned(), serde_json::json!({"reserveTokens":0,"reserveScalingEnabled":false})),
        ])));
        let additions = [make_user_message("small prompt", None)];
        session.enforce_final_provider_admission(&additions).await.expect("user-only admission");
        session.steer(&"late steering ".repeat(200), None, Default::default()).await.expect("steer");
        session.follow_up("late followup", None, Default::default()).await.expect("followup");
        assert!(session.enforce_final_provider_admission(&additions).await.is_err());
        assert!(session.messages().is_empty());
        assert!(session.with_session_manager(|manager| manager.entries()).is_empty());
        let cleared = session.clear_queue(false);
        assert_eq!(cleared.steering, ["late steering ".repeat(200)]);
        assert_eq!(cleared.follow_up, ["late followup"]);
    }

    #[tokio::test]
    async fn context_usage_ignores_compaction_on_an_abandoned_branch() {
        let session = test_session();
        session.agent.set_model(test_model());
        let mut assistant = maho_ai::providers::faux::faux_assistant_message("answer", Default::default());
        assistant.usage.input = 1000;
        let selected = session.with_session_manager_mut(|manager| {
            let root = manager.append_message(serde_json::json!({"role":"user","content":"small","timestamp":0}));
            let selected = manager.append_message(serde_json::to_value(&assistant).expect("assistant"));
            manager.append_compaction("abandoned digest", root["id"].as_str().expect("root"), 1000, None, None, None);
            selected
        });
        session.with_session_manager_mut(|manager| manager.set_leaf(selected["id"].as_str()));
        session.rebuild_session_context().expect("selected context");
        assert_eq!(session.get_context_usage().expect("usage").tokens, Some(1000));
    }

    #[tokio::test]
    async fn continuation_uses_post_compaction_usage_by_persistence_order() {
        for post_boundary in [false, true] {
            let session = test_session();
            let mut model = test_model();
            model.context_window = 128;
            session.agent.set_model(model);
            session.with_settings_manager_mut(|manager| manager.apply_overrides(&Map::from_iter([
                ("compaction".to_owned(), serde_json::json!({"reserveTokens":0,"reserveScalingEnabled":false,"keepRecentTokens":1})),
            ])));
            let mut assistant = maho_ai::providers::faux::faux_assistant_message("answer", Default::default());
            assistant.usage.input = 1000;
            assistant.timestamp = 0;
            session.with_session_manager_mut(|manager| {
                let kept = manager.append_message(serde_json::json!({"role":"user","content":"small","timestamp":0}));
                manager.append_message(serde_json::to_value(&assistant).expect("assistant"));
                manager.append_compaction("digest", kept["id"].as_str().expect("kept"), 1000, None, None, None);
                if post_boundary { manager.append_message(serde_json::to_value(&assistant).expect("assistant")); }
            });
            session.rebuild_session_context().expect("context");
            let mut extension = maho_ext_api::LoadedExtension::new("<inline:usage-admission>", session.cwd().into(), Default::default());
            extension.handlers.insert(maho_ext_api::EventKind::SessionBeforeCompact, vec![Arc::new(|_, _| Box::pin(async {
                Ok(maho_ext_api::EventResult::SessionBefore(maho_ext_api::SessionBeforeEventResult { cancel: Some(true), ..Default::default() }))
            }))]);
            session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
            assert_eq!(session.revalidate_scheduled_continuation_admission().await.is_err(), post_boundary);
        }
    }

    #[tokio::test]
    async fn recompact_continuation_excludes_failed_assistant_from_provider_context() {
        let session = retry_session(vec![maho_ai::providers::faux::faux_assistant_message("resumed", Default::default())], 0);
        let mut model = session.model();
        model.context_window = 128_000;
        session.agent.set_model(model);
        session.with_settings_manager_mut(|manager| manager.apply_overrides(&Map::from_iter([
            ("compaction".to_owned(), serde_json::json!({"reserveTokens":0,"reserveScalingEnabled":false,"keepRecentTokens":1})),
        ])));
        for text in ["old task".repeat(18_000), "recent task".repeat(18_000)] {
            session.with_session_manager_mut(|manager| manager.append_message(serde_json::json!({"role":"user","content":text,"timestamp":0})));
        }
        let mut failed = maho_ai::providers::faux::faux_assistant_message("failed tail", Default::default());
        failed.stop_reason = StopReason::Error;
        failed.error_message = Some("failed request".to_owned());
        session.with_session_manager_mut(|manager| manager.append_message(serde_json::to_value(failed).expect("failed assistant")));
        session.rebuild_session_context().expect("context");
        session.agent.follow_up(session_message_from_value(serde_json::json!({"role":"user","content":"queued","timestamp":0})).expect("queued"));
        let mut extension = maho_ext_api::LoadedExtension::new("<inline:retire-failed-tail>", session.cwd().into(), Default::default());
        extension.handlers.insert(maho_ext_api::EventKind::SessionBeforeCompact, vec![Arc::new(|event, _| {
            let maho_ext_api::ExtensionEvent::SessionBeforeCompact(event) = event else { panic!("preparation"); };
            Box::pin(async move { Ok(maho_ext_api::EventResult::SessionBefore(maho_ext_api::SessionBeforeEventResult {
                compaction: Some(maho_ext_api::CompactionResult { summary: "digest".to_owned(), first_kept_entry_id: event.preparation.first_kept_entry_id.clone(),
                    tokens_before: event.preparation.tokens_before, details: None }), ..Default::default()
            })) })
        })]);
        session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
        session.continue_session().await.expect("recompacted continuation");
        assert_eq!(session.messages().iter().filter(|message| message.role() == "assistant").count(), 1);
        assert!(session.with_session_manager(|manager| manager.entries()).iter().any(|entry| entry["message"]["errorMessage"] == "failed request"));
    }

    #[tokio::test]
    async fn continuation_admission_only_hard_failure_blocks() {
        for (hard_limit, reject, cooldown) in [(false, false, false), (false, true, false), (true, true, false), (true, true, true)] {
            let session = test_session();
            let mut model = test_model();
            model.context_window = 128;
            session.agent.set_model(model);
            session.with_settings_manager_mut(|manager| manager.apply_overrides(&Map::from_iter([
                ("compaction".to_owned(), serde_json::json!({"reserveTokens":0,"reserveScalingEnabled":false,"keepRecentTokens":1})),
            ])));
            for text in ["old task", "recent task"] {
                let text = text.repeat(if hard_limit { 800 } else { 13 });
                session.with_session_manager_mut(|manager| manager.append_message(serde_json::json!({"role":"user","content":text,"timestamp":0})));
            }
            session.rebuild_session_context().expect("context");
            let mut extension = maho_ext_api::LoadedExtension::new("<inline:continuation-admission>", session.cwd().into(), Default::default());
            extension.handlers.insert(maho_ext_api::EventKind::SessionBeforeCompact, vec![Arc::new(move |event, _| {
                let maho_ext_api::ExtensionEvent::SessionBeforeCompact(event) = event else { panic!("preparation"); };
                assert_eq!(event.reason, maho_ext_api::CompactionReason::PrePrompt);
                let result = maho_ext_api::CompactionResult { summary: "digest".to_owned(),
                    first_kept_entry_id: event.preparation.first_kept_entry_id.clone(), tokens_before: event.preparation.tokens_before, details: None };
                Box::pin(async move { Ok(maho_ext_api::EventResult::SessionBefore(maho_ext_api::SessionBeforeEventResult {
                    cancel: reject.then_some(true), rejection_cause: cooldown.then_some(maho_ext_api::CompactionRejectionCause::CircuitBreaker),
                    compaction: (!reject).then_some(result), ..Default::default()
                })) })
            })]);
            session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
            let result = session.revalidate_scheduled_continuation_admission().await;
            assert_eq!(result.is_err(), hard_limit && reject && !cooldown);
            assert_eq!(session.compaction_state().generation(), 1);
        }
    }

    #[tokio::test]
    async fn non_triggering_custom_message_waits_for_streaming_turn_end() {
        let session = retry_session(vec![maho_ai::providers::faux::faux_assistant_message("finished", Default::default())], 0);
        let captured = session.clone();
        let mut extension = maho_ext_api::LoadedExtension::new("<inline:quiet-custom>", session.cwd().into(), Default::default());
        extension.handlers.insert(maho_ext_api::EventKind::MessageStart, vec![Arc::new(move |event, _| {
            if let maho_ext_api::ExtensionEvent::MessageStart { message } = event
                && message.role() == "user"
            {
                assert!(captured.is_streaming());
                maho_ext_api::ExtensionActions::send_message(&SessionExtensionActions(Arc::downgrade(&captured.inner)), maho_ext_api::CustomMessage {
                    custom_type: "quiet-notice".to_owned(), content: vec![maho_tools::definition::ToolContent::text("notice")],
                    display: true, details: None,
                }, maho_ext_api::SendMessageOptions { trigger_turn: false, deliver_as: None }).expect("non-triggering message");
                assert!(!captured.agent.has_queued_messages());
                assert!(!captured.messages().iter().any(|message| message.role() == "custom"));
            }
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })]);
        session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
        tokio::time::timeout(std::time::Duration::from_secs(5), session.prompt("work", Default::default())).await.expect("bounded prompt").expect("prompt");
        assert_eq!(session.messages().iter().filter(|message| message.role() == "assistant").count(), 1);
        assert_eq!(session.messages().last().expect("notice").role(), "custom");
        assert_eq!(session.with_session_manager(|manager| manager.entries()).iter().filter(|entry| entry["customType"] == "quiet-notice").count(), 1);
        assert!(session.state().pending_custom_messages.is_empty());
    }

    #[tokio::test]
    async fn compaction_preserves_only_its_matching_hook_diagnostic() {
        for (matching_request, replace_prefix) in [(true, false), (false, false), (true, true)] {
            let session = test_session();
            session.agent.set_model(test_model());
            session.with_settings_manager_mut(|manager| manager.apply_overrides(&Map::from_iter([
                ("compaction".to_owned(), serde_json::json!({"keepRecentTokens":1})),
            ])));
            for text in ["old task", "recent task"] {
                session.with_session_manager_mut(|manager| manager.append_message(serde_json::json!({"role":"user","content":text,"timestamp":0})));
            }
            session.rebuild_session_context().expect("context");
            let captured = session.clone();
            let mut extension = maho_ext_api::LoadedExtension::new("<inline:compaction-diagnostic>", session.cwd().into(), Default::default());
            extension.handlers.insert(maho_ext_api::EventKind::SessionBeforeCompact, vec![Arc::new(move |event, _| {
                let maho_ext_api::ExtensionEvent::SessionBeforeCompact(event) = event else { panic!("preparation"); };
                maho_ext_api::ExtensionActions::send_message(&SessionExtensionActions(Arc::downgrade(&captured.inner)), maho_ext_api::CustomMessage {
                    custom_type: "senpi.hook".to_owned(), content: vec![maho_tools::definition::ToolContent::text("diagnostic")],
                    display: true, details: Some(serde_json::json!({"event":"PreCompact", "compactionRequestId":
                        if matching_request { event.request_id.as_str() } else { "stale-request" }})),
                }, maho_ext_api::SendMessageOptions { trigger_turn: false, deliver_as: None }).expect("diagnostic");
                if replace_prefix {
                    let mut messages = captured.messages();
                    messages[0] = make_user_message("revision-neutral replacement", None);
                    captured.agent.set_messages(messages);
                }
                let result = maho_ext_api::CompactionResult { summary: "digest".to_owned(),
                    first_kept_entry_id: event.preparation.first_kept_entry_id.clone(), tokens_before: event.preparation.tokens_before, details: None };
                Box::pin(async move { Ok(maho_ext_api::EventResult::SessionBefore(maho_ext_api::SessionBeforeEventResult {
                    compaction: Some(result), ..Default::default()
                })) })
            })]);
            session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
            let result = session.compact(None).await;
            assert_eq!(result.is_ok(), matching_request && !replace_prefix);
            assert_eq!(session.messages().iter().filter(|message| message.role() == "custom").count(), 1);
            let entries = session.with_session_manager(|manager| manager.entries());
            assert_eq!(entries.iter().filter(|entry| entry["customType"] == "senpi.hook").count(), 1);
            assert_eq!(entries.iter().any(|entry| entry["type"] == "compaction"), matching_request && !replace_prefix);
        }
    }

    #[tokio::test]
    async fn custom_trigger_during_compaction_is_queued_then_delivered_once() {
        let session = retry_session(vec![maho_ai::providers::faux::faux_assistant_message("resumed", Default::default())], 0);
        session.with_settings_manager_mut(|manager| manager.apply_overrides(&Map::from_iter([
            ("compaction".to_owned(), serde_json::json!({"keepRecentTokens":1})),
        ])));
        for text in ["old task", "recent task"] {
            session.with_session_manager_mut(|manager| manager.append_message(serde_json::json!({"role":"user","content":text,"timestamp":0})));
        }
        session.rebuild_session_context().expect("context");
        let captured = session.clone();
        let mut extension = maho_ext_api::LoadedExtension::new("<inline:compaction-trigger>", session.cwd().into(), Default::default());
        extension.handlers.insert(maho_ext_api::EventKind::SessionBeforeCompact, vec![Arc::new(move |event, _| {
            let maho_ext_api::ExtensionEvent::SessionBeforeCompact(event) = event else { panic!("preparation"); };
            let revision = captured.message_revision();
            maho_ext_api::ExtensionActions::send_message(&SessionExtensionActions(Arc::downgrade(&captured.inner)), maho_ext_api::CustomMessage {
                custom_type: "after-compaction".to_owned(), content: vec![maho_tools::definition::ToolContent::text("continue")],
                display: true, details: None,
            }, maho_ext_api::SendMessageOptions { trigger_turn: true, deliver_as: None }).expect("queue trigger");
            assert_eq!(captured.message_revision(), revision);
            assert!(captured.agent.has_queued_messages());
            let result = maho_ext_api::CompactionResult { summary: "digest".to_owned(),
                first_kept_entry_id: event.preparation.first_kept_entry_id.clone(), tokens_before: event.preparation.tokens_before, details: None };
            Box::pin(async move { Ok(maho_ext_api::EventResult::SessionBefore(maho_ext_api::SessionBeforeEventResult {
                compaction: Some(result), ..Default::default()
            })) })
        })]);
        session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
        let (finished, received) = tokio::sync::oneshot::channel();
        let finished = Arc::new(Mutex::new(Some(finished)));
        let _subscription = session.subscribe(Arc::new(move |event| {
            if matches!(event, AgentSessionEvent::AgentIdle)
                && let Some(finished) = lock(&finished).take() { finished.send(()).expect("observer"); }
        }));
        session.compact(None).await.expect("compact");
        tokio::time::timeout(std::time::Duration::from_secs(5), received).await.expect("bounded continuation").expect("settled");
        let entries = session.with_session_manager(|manager| manager.entries());
        assert_eq!(entries.iter().filter(|entry| entry["customType"] == "after-compaction").count(), 1);
        assert_eq!(session.messages().iter().filter(|message| message.role() == "assistant").count(), 1);
        assert!(!session.agent.has_queued_messages());
    }

    #[test]
    fn idle_custom_message_persists_before_paired_message_events() {
        let session = test_session();
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = session.clone();
        let observed = events.clone();
        let _subscription = session.subscribe(Arc::new(move |event| {
            if let AgentSessionEvent::Agent(event) = event {
                match event {
                    maho_agent::types::AgentEvent::MessageStart { message } | maho_agent::types::AgentEvent::MessageEnd { message } => {
                        assert_eq!(message.role(), "custom");
                        assert_eq!(captured.messages().as_slice(), std::slice::from_ref(message));
                        assert_eq!(captured.with_session_manager(|manager| manager.entries())[0]["customType"], "idle-notice");
                        lock(&observed).push(matches!(event, maho_agent::types::AgentEvent::MessageEnd { .. }));
                    }
                    _ => {}
                }
            }
        }));
        maho_ext_api::ExtensionActions::send_message(&SessionExtensionActions(Arc::downgrade(&session.inner)), maho_ext_api::CustomMessage {
            custom_type: "idle-notice".to_owned(), content: vec![maho_tools::definition::ToolContent::text("notice")],
            display: true, details: None,
        }, maho_ext_api::SendMessageOptions { trigger_turn: false, deliver_as: None }).expect("append");
        assert_eq!(*lock(&events), [false, true]);
        assert_eq!(session.with_session_manager(|manager| manager.entries()).len(), 1);
    }

    #[tokio::test]
    async fn compaction_success_event_precedes_completed_extension_hook() {
        let session = test_session();
        session.agent.set_model(test_model());
        session.with_settings_manager_mut(|manager| manager.apply_overrides(&Map::from_iter([
            ("compaction".to_owned(), serde_json::json!({"keepRecentTokens":1})),
        ])));
        for text in ["old task", "recent task"] {
            session.with_session_manager_mut(|manager| manager.append_message(serde_json::json!({"role":"user","content":text,"timestamp":0})));
        }
        session.rebuild_session_context().expect("context");
        let order = Arc::new(Mutex::new(Vec::new()));
        let events = order.clone();
        let _subscription = session.subscribe(Arc::new(move |event| {
            if matches!(event, AgentSessionEvent::CompactionEnd { accepted: Some(true), .. }) { lock(&events).push("end"); }
        }));
        let mut extension = maho_ext_api::LoadedExtension::new("<inline:compaction-result>", session.cwd().into(), Default::default());
        extension.handlers.insert(maho_ext_api::EventKind::SessionBeforeCompact, vec![Arc::new(|event, _| {
            let maho_ext_api::ExtensionEvent::SessionBeforeCompact(event) = event else { panic!("preparation"); };
            let result = maho_ext_api::CompactionResult { summary: "digest".to_owned(),
                first_kept_entry_id: event.preparation.first_kept_entry_id.clone(), tokens_before: event.preparation.tokens_before, details: None };
            Box::pin(async move { Ok(maho_ext_api::EventResult::SessionBefore(maho_ext_api::SessionBeforeEventResult {
                compaction: Some(result), ..Default::default()
            })) })
        })]);
        let hooks = order.clone();
        let captured = session.clone();
        extension.handlers.insert(maho_ext_api::EventKind::SessionCompact, vec![Arc::new(move |event, _| {
            assert!(matches!(event, maho_ext_api::ExtensionEvent::SessionCompact(maho_ext_api::SessionCompactEvent::Accepted { from_extension: true, .. })));
            assert_eq!(captured.compaction_state().status(), "completed");
            assert!(!captured.is_compacting());
            lock(&hooks).push("hook");
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })]);
        session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
        session.with_settings_manager_mut(|manager| manager.apply_overrides(&Map::from_iter([
            ("retry".to_owned(), serde_json::json!({"revertPolicy":"never"})),
        ])));
        session.retry_fallback.lock().await.as_mut().expect("controller").state = Some(crate::retry_fallback::controller::ActiveFallbackState {
            chain_key: "fixture".to_owned(), original_selector: "faux/primary".to_owned(), original_thinking_level: None,
            last_applied_thinking_level: None, pinned_by_refusal: true, pinned_by_billing: false, pinned: true,
        });
        session.compact(None).await.expect("compaction");
        assert_eq!(*lock(&order), ["end", "hook"]);
        assert_eq!(session.with_session_manager(|manager| manager.entries().last().expect("compaction")["fromHook"].clone()), true);
        let guard = session.retry_fallback.lock().await;
        let fallback = guard.as_ref().expect("controller").state.as_ref().expect("fallback remains for never policy");
        assert!(!fallback.pinned_by_refusal);
        assert!(!fallback.pinned);
    }

    #[tokio::test]
    async fn auto_retry_creates_fresh_invoke_recovery_wrapper() {
        use maho_ai::types::{AssistantMessageEvent as Event, DoneReason, ErrorReason};
        let session = retry_session(Vec::new(), 2);
        let executed = Arc::new(Mutex::new(Vec::new()));
        let captured = executed.clone();
        let mut tool = test_tool("Echo");
        tool.tool.parameters = serde_json::json!({"type":"object","properties":{"value":{"type":"string"}},"required":["value"]});
        tool.execute = Arc::new(move |_, arguments, _, _| {
            lock(&captured).push(arguments["value"].as_str().expect("argument").to_owned());
            Box::pin(async { AgentToolResult::text("echoed") })
        });
        session.agent.set_tools(vec![tool.clone()]);
        let calls = Arc::new(AtomicU64::new(0));
        let stream_calls = calls.clone();
        session.agent.set_stream_function(Arc::new(move |_, _, _| {
            let call = stream_calls.fetch_add(1, Ordering::SeqCst);
            let text = match call {
                0 => "<invoke na", 1 => "<invoke name=\"Ec",
                2 => "<invoke name=\"Echo\"><parameter name=\"value\">second-attempt</parameter></invoke>",
                _ => "Final",
            };
            let failed = call < 2;
            let mut message = maho_ai::providers::faux::faux_assistant_message(text,
                maho_ai::providers::faux::FauxAssistantMessageOptions {
                    stop_reason: Some(if failed { StopReason::Error } else { StopReason::Stop }),
                    error_message: failed.then(|| "overloaded_error".to_owned()), ..Default::default()
                });
            let inner = maho_ai::utils::event_stream::create_assistant_message_event_stream();
            let stream = maho_ai::tool_call_middleware::recovery_stream_wrapper::wrap_stream_with_invoke_recovery(
                inner.clone(), vec![tool.tool.clone()], Default::default());
            let mut partial = message.clone();
            partial.content.clear();
            inner.push(Event::Start { partial });
            inner.push(Event::TextStart { content_index: 0, partial: message.clone() });
            inner.push(Event::TextDelta { content_index: 0, delta: text.to_owned(), partial: message.clone() });
            if failed {
                inner.push(Event::Error { reason: ErrorReason::Error, error: message });
            } else {
                inner.push(Event::TextEnd { content_index: 0, content: text.to_owned(), partial: message.clone() });
                message.stop_reason = StopReason::Stop;
                inner.push(Event::Done { reason: DoneReason::Stop, message });
            }
            stream
        }));
        tokio::time::timeout(std::time::Duration::from_secs(5), session.prompt("Test", Default::default()))
            .await.expect("bounded prompt").expect("prompt");
        assert_eq!(*lock(&executed), vec!["second-attempt"]);
        assert_eq!(calls.load(Ordering::SeqCst), 4);
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
    async fn rate_limit_body_hint_drives_tier_delay_without_marker() {
        let session = retry_session(vec![maho_ai::providers::faux::faux_assistant_message("",
            maho_ai::providers::faux::FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Error),
                error_message: Some("rate_limit_error: retry in 1 s".to_owned()), ..Default::default()
            })], 2);
        let observed = Arc::new(Mutex::new(None));
        let captured = observed.clone();
        let cancelling = session.clone();
        let _subscription = session.subscribe(Arc::new(move |event| {
            if let AgentSessionEvent::AutoRetryStart { delay_ms, .. } = event {
                *lock(&captured) = Some(*delay_ms);
                cancelling.abort_retry();
            }
        }));
        tokio::time::timeout(std::time::Duration::from_secs(5), session.prompt("input", Default::default()))
            .await.expect("bounded prompt").expect("prompt");
        assert_eq!(*lock(&observed), Some(500));
    }

    #[tokio::test]
    async fn settled_handler_followup_owns_work_until_second_turn_is_idle() {
        let session = retry_session(vec![
            maho_ai::providers::faux::faux_assistant_message("first", Default::default()),
            maho_ai::providers::faux::faux_assistant_message("second", Default::default()),
        ], 0);
        let once = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let followup = session.clone();
        let (idle_tx, idle_rx) = tokio::sync::oneshot::channel();
        let idle_tx = Arc::new(Mutex::new(Some(idle_tx)));
        let _subscription = session.subscribe(Arc::new(move |event| {
            if matches!(event, AgentSessionEvent::AgentSettled) && !once.swap(true, Ordering::SeqCst) {
                maho_ext_api::ExtensionActions::send_user_message(
                    &SessionExtensionActions(Arc::downgrade(&followup.inner)),
                    maho_ext_api::UserMessageContent::Text("followup".to_owned()), Default::default(),
                ).expect("deferred followup");
            }
            if matches!(event, AgentSessionEvent::AgentIdle) && let Some(sender) = lock(&idle_tx).take() { let _ = sender.send(()); }
        }));
        tokio::time::timeout(std::time::Duration::from_secs(5), session.prompt("start", Default::default()))
            .await.expect("bounded prompt").expect("prompt");
        tokio::time::timeout(std::time::Duration::from_secs(5), idle_rx).await.expect("bounded idle").expect("idle");
        assert_eq!(session.messages().len(), 4);
        assert_eq!(session.get_last_assistant_text().as_deref(), Some("second"));
    }

    #[tokio::test]
    async fn subscription_auth_miss_does_not_admit_configured_provider_fallback() {
        let session = retry_session(Vec::new(), 1);
        let provider = maho_ai::providers::faux::faux_provider(Default::default());
        let mut runtime = session.model_runtime().clone();
        runtime.register_native_provider(provider.provider.clone());
        let subscription = maho_ai::providers::faux::faux_provider(maho_ai::providers::faux::RegisterFauxProviderOptions {
            provider: Some("anthropic-subscription".to_owned()), ..Default::default()
        });
        runtime.register_native_provider(subscription.provider.clone());
        runtime.register_provider("faux", crate::provider_composer::ProviderConfigInput {
            config: crate::model_config_schema::ModelsJsonProvider { api_key: Some("faux-test".to_owned()), ..Default::default() },
            ..Default::default()
        }).expect("fallback provider");
        let mut model = test_model();
        model.provider = "anthropic-subscription".to_owned();
        session.agent.set_model(model);
        session.with_settings_manager_mut(|manager| manager.apply_overrides(&Map::from_iter([
            ("retry".to_owned(), serde_json::json!({"enabled":true,"modelFallback":true,"fallbackChains":{
                "anthropic-subscription/faux-1":["faux/faux-1"]
            }})),
        ])));
        assert!(session.retry_fallback.lock().await.as_mut().expect("controller").can_try_fallback());
        let failed = maho_ai::providers::faux::faux_assistant_message("", maho_ai::providers::faux::FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error), error_message: Some(maho_ai::auth::resolve::provider_not_configured_message("anthropic-subscription")), ..Default::default()
        });
        assert!(!session.will_retry(Some(&failed)).await);
    }

    #[tokio::test]
    async fn quota_resource_exhaustion_admits_recovery_with_orphaned_tool_call() {
        let session = retry_session(Vec::new(), 1);
        session.agent.set_model(test_model());
        let mut failed = maho_ai::providers::faux::faux_assistant_message("", maho_ai::providers::faux::FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error), error_message: Some("RESOURCE_EXHAUSTED".to_owned()), ..Default::default()
        });
        failed.usage.input = 1000;
        failed.content.push(maho_ai::types::ContentBlock::ToolCall(maho_ai::types::ToolCall {
            id: "orphan".to_owned(), name: "read".to_owned(), arguments: Map::new(), thought_signature: None,
            incomplete: None, error_message: None, namespace: None,
        }));
        assert!(session.will_retry(Some(&failed)).await);
        session.with_session_manager_mut(|manager| {
            manager.append_message(serde_json::json!({"role":"user","content":[{"type":"text","text":"task"}],"timestamp":0}));
            manager.append_message(serde_json::to_value(&failed).expect("quota failure"));
        });
        session.rebuild_session_context().expect("context");
        session.follow_up("retained quota input", None, Default::default()).await.expect("queue");
        let retries = Arc::new(Mutex::new(0));
        let captured = retries.clone();
        let _subscription = session.subscribe(Arc::new(move |event| {
            if matches!(event, AgentSessionEvent::AutoRetryStart { .. }) { *lock(&captured) += 1; }
        }));
        tokio::time::timeout(std::time::Duration::from_secs(5), session.finish_provider_turn()).await.expect("bounded quota recovery").expect("quota recovery");
        assert_eq!(*lock(&retries), 0);
        assert_eq!(session.get_follow_up_messages(), vec!["retained quota input"]);
        assert!(session.agent.has_queued_messages());
        assert_eq!(session.messages().len(), 1);
        assert_eq!(session.with_session_manager(|manager| manager.entries()).iter().filter(|entry| entry["type"] == "message").count(), 2);
    }

    #[tokio::test]
    async fn subscription_remint_replays_and_exhausts_without_fallback() {
        use maho_ai::providers::faux::{faux_provider, faux_streams, RegisterFauxProviderOptions};
        for recovered in [true, false] {
            let provider = faux_provider(RegisterFauxProviderOptions {
                provider: Some("anthropic-subscription".to_owned()), tokens_per_second: Some(0.0), ..Default::default()
            });
            let failure = || maho_ai::providers::faux::faux_assistant_message("", maho_ai::providers::faux::FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Error), error_message: Some("invalid_request".to_owned()), ..Default::default()
            });
            provider.set_responses(vec![failure().into(), if recovered {
                maho_ai::providers::faux::faux_assistant_message("recovered", Default::default()).into()
            } else { failure().into() }]);
            let streams = faux_streams(provider.core.clone());
            let session = retry_session(Vec::new(), 1);
            session.agent.set_model(provider.get_model(Some("faux-1")).expect("model"));
            session.agent.set_stream_function(Arc::new(move |model, context, options| {
                streams.stream_simple(model, context, options.map(|options| options.simple))
            }));
            tokio::time::timeout(std::time::Duration::from_secs(5), session.prompt("task", Default::default())).await.expect("bounded prompt").expect("prompt");
            let calls = provider.get_call_log();
            assert_eq!(calls.len(), 2);
            assert!(calls.iter().all(|call| call.model_id == "faux-1"));
            assert_eq!(session.model().provider, "anthropic-subscription");
            assert!(!session.is_retrying());
            if recovered { assert_eq!(session.get_last_assistant_text().as_deref(), Some("recovered")); }
            else { assert_eq!(session.messages().last().and_then(AgentMessage::as_assistant).and_then(|message| message.error_message.as_deref()), Some("invalid_request")); }
        }
    }

    #[tokio::test]
    async fn subscription_session_errors_admit_same_model_recovery_only_on_subscription() {
        let session = retry_session(Vec::new(), 1);
        for error in ["Lock file is already being held", "invalid_request"] {
            let failed = maho_ai::providers::faux::faux_assistant_message("", maho_ai::providers::faux::FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Error), error_message: Some(error.to_owned()), ..Default::default()
            });
            let mut model = test_model();
            model.provider = "anthropic-subscription".to_owned();
            session.agent.set_model(model);
            assert!(session.will_retry(Some(&failed)).await, "subscription recovery: {error}");
            assert!(session.is_subscription_same_model_remint_error(&failed));
            session.agent.set_model(test_model());
            assert!(!session.is_subscription_same_model_remint_error(&failed), "provider-scoped recovery: {error}");
            assert_eq!(session.will_retry(Some(&failed)).await, maho_ai::utils::retry::is_retryable_assistant_error(&failed));
        }
    }

    #[tokio::test]
    async fn zero_token_resource_exhaustion_recovers_on_same_model() {
        let session = retry_session(vec![maho_ai::providers::faux::faux_assistant_message("reminted", Default::default())], 1);
        let failed = maho_ai::providers::faux::faux_assistant_message("", maho_ai::providers::faux::FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error), error_message: Some("RESOURCE_EXHAUSTED".to_owned()), ..Default::default()
        });
        session.with_session_manager_mut(|manager| {
            manager.append_message(serde_json::json!({"role":"user","content":[{"type":"text","text":"task"}],"timestamp":0}));
            manager.append_message(serde_json::to_value(failed).expect("failed"));
        });
        session.rebuild_session_context().expect("context");
        let model = session.model();
        tokio::time::timeout(std::time::Duration::from_secs(5), session.finish_provider_turn()).await.expect("bounded recovery").expect("recovery");
        assert_eq!(session.get_last_assistant_text().as_deref(), Some("reminted"));
        assert_eq!(session.model().id, model.id);
        assert_eq!(session.model().provider, model.provider);
    }

    #[tokio::test]
    async fn required_threshold_compaction_precedes_retry_start() {
        let session = retry_session(vec![maho_ai::providers::faux::faux_assistant_message("recovered", Default::default())], 1);
        session.agent.set_model(test_model());
        let mut prior = maho_ai::providers::faux::faux_assistant_message("prior", Default::default());
        prior.usage.input = 120_000;
        let failed = maho_ai::providers::faux::faux_assistant_message("", maho_ai::providers::faux::FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error), error_message: Some("overloaded_error".to_owned()), ..Default::default()
        });
        session.with_session_manager_mut(|manager| {
            manager.append_message(serde_json::json!({"role":"user","content":[{"type":"text","text":"task"}],"timestamp":0}));
            manager.append_message(serde_json::to_value(prior).expect("prior"));
            manager.append_message(serde_json::json!({"role":"user","content":[{"type":"text","text":"next task"}],"timestamp":1}));
            manager.append_message(serde_json::to_value(failed).expect("failed"));
        });
        session.rebuild_session_context().expect("context");
        session.with_settings_manager_mut(|manager| manager.apply_overrides(&Map::from_iter([
            ("compaction".to_owned(), serde_json::json!({"keepRecentTokens":1,"reserveTokens":20000,"reserveScalingEnabled":false})),
        ])));
        let mut extension = maho_ext_api::LoadedExtension::new("<inline:retry-order>", session.cwd().into(), Default::default());
        extension.handlers.insert(maho_ext_api::EventKind::SessionBeforeCompact, vec![Arc::new(|event, _| {
            let maho_ext_api::ExtensionEvent::SessionBeforeCompact(event) = event else { panic!("preparation"); };
            let result = maho_ext_api::CompactionResult { summary: "digest".to_owned(), first_kept_entry_id: event.preparation.first_kept_entry_id.clone(),
                tokens_before: event.preparation.tokens_before, details: None };
            Box::pin(async move { Ok(maho_ext_api::EventResult::SessionBefore(maho_ext_api::SessionBeforeEventResult { compaction: Some(result), ..Default::default() })) })
        })]);
        session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
        let order = Arc::new(Mutex::new(Vec::new()));
        let captured = order.clone();
        let _subscription = session.subscribe(Arc::new(move |event| {
            match event {
                AgentSessionEvent::CompactionEnd { accepted: Some(true), .. } => lock(&captured).push("compacted"),
                AgentSessionEvent::AutoRetryStart { .. } => lock(&captured).push("retry"),
                _ => {}
            }
        }));
        tokio::time::timeout(std::time::Duration::from_secs(5), session.finish_provider_turn()).await.expect("bounded recovery").expect("recovery");
        assert_eq!(*lock(&order), vec!["compacted", "retry"]);
        assert_eq!(session.get_last_assistant_text().as_deref(), Some("recovered"));
    }

    #[tokio::test]
    async fn rejected_required_retry_compaction_retains_follow_up_queue() {
        let session = retry_session(vec![maho_ai::providers::faux::faux_assistant_message("unexpected retry", Default::default())], 1);
        session.agent.set_model(test_model());
        let mut prior = maho_ai::providers::faux::faux_assistant_message("prior", Default::default());
        prior.usage.input = 120_000;
        let failed = maho_ai::providers::faux::faux_assistant_message("", maho_ai::providers::faux::FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error), error_message: Some("overloaded_error".to_owned()), ..Default::default()
        });
        session.with_session_manager_mut(|manager| {
            manager.append_message(serde_json::json!({"role":"user","content":[{"type":"text","text":"task"}],"timestamp":0}));
            manager.append_message(serde_json::to_value(prior).expect("prior"));
            manager.append_message(serde_json::json!({"role":"user","content":[{"type":"text","text":"next task"}],"timestamp":1}));
            manager.append_message(serde_json::to_value(failed).expect("failed"));
        });
        session.rebuild_session_context().expect("context");
        session.with_settings_manager_mut(|manager| manager.apply_overrides(&Map::from_iter([
            ("compaction".to_owned(), serde_json::json!({"keepRecentTokens":1,"reserveTokens":20000,"reserveScalingEnabled":false})),
        ])));
        let mut extension = maho_ext_api::LoadedExtension::new("<inline:retry-rejection>", session.cwd().into(), Default::default());
        extension.handlers.insert(maho_ext_api::EventKind::SessionBeforeCompact, vec![Arc::new(|_, _| Box::pin(async {
            Ok(maho_ext_api::EventResult::SessionBefore(maho_ext_api::SessionBeforeEventResult { cancel: Some(true), ..Default::default() }))
        }))]);
        session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
        session.follow_up("retained input", None, Default::default()).await.expect("queue");
        let retries = Arc::new(Mutex::new(0));
        let captured = retries.clone();
        let _subscription = session.subscribe(Arc::new(move |event| {
            if matches!(event, AgentSessionEvent::AutoRetryStart { .. }) { *lock(&captured) += 1; }
        }));
        tokio::time::timeout(std::time::Duration::from_secs(5), session.finish_provider_turn()).await.expect("bounded recovery").expect("recovery");
        assert_eq!(*lock(&retries), 0);
        assert_eq!(session.get_follow_up_messages(), vec!["retained input"]);
        assert!(session.agent.has_queued_messages());
        assert_eq!(session.get_last_assistant_text().as_deref(), Some("prior"));
        assert_eq!(session.compaction_state().status(), "failed");
    }

    #[tokio::test]
    async fn successful_overflow_compacts_without_retrying_completed_assistant() {
        let mut completed = maho_ai::providers::faux::faux_assistant_message("completed answer", Default::default());
        completed.usage.input = 200_000;
        let session = retry_session(Vec::new(), 0);
        session.with_session_manager_mut(|manager| {
            manager.append_message(serde_json::json!({"role":"user","content":"task","timestamp":0}));
            manager.append_message(serde_json::to_value(completed).expect("completed assistant"));
        });
        session.rebuild_session_context().expect("context");
        session.with_settings_manager_mut(|manager| manager.apply_overrides(&Map::from_iter([
            ("compaction".to_owned(), serde_json::json!({"keepRecentTokens":1,"reserveTokens":0})),
        ])));
        let mut extension = maho_ext_api::LoadedExtension::new("<inline:completed-overflow>", session.cwd().into(), Default::default());
        extension.handlers.insert(maho_ext_api::EventKind::SessionBeforeCompact, vec![Arc::new(|event, _| {
            let maho_ext_api::ExtensionEvent::SessionBeforeCompact(event) = event else { panic!("preparation"); };
            let result = maho_ext_api::CompactionResult { summary: "digest".to_owned(), first_kept_entry_id: event.preparation.first_kept_entry_id.clone(),
                tokens_before: event.preparation.tokens_before, details: None };
            Box::pin(async move { Ok(maho_ext_api::EventResult::SessionBefore(maho_ext_api::SessionBeforeEventResult { compaction: Some(result), ..Default::default() })) })
        })]);
        session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
        let retries = Arc::new(Mutex::new(Vec::new()));
        let captured = retries.clone();
        let _subscription = session.subscribe(Arc::new(move |event| {
            if let AgentSessionEvent::CompactionEnd { will_retry, .. } = event { lock(&captured).push(*will_retry); }
        }));
        tokio::time::timeout(std::time::Duration::from_secs(5), session.finish_provider_turn()).await.expect("bounded completion").expect("completion");
        assert_eq!(session.get_last_assistant_text().as_deref(), Some("completed answer"));
        assert_eq!(*lock(&retries), vec![false]);
        assert_eq!(session.compaction_state().status(), "completed");
        session.finish_provider_turn().await.expect("old assistant resampled");
        assert_eq!(*lock(&retries), vec![false], "pre-compaction usage cannot retrigger compaction");
        let prior_messages = session.messages();
        let original_model = session.model();
        let mut small_model = original_model.clone();
        small_model.context_window = 128;
        session.agent.set_model(small_model);
        let mut oversized_messages = prior_messages.clone();
        oversized_messages.push(session_message_from_value(serde_json::json!({
            "role":"custom", "customType":"fresh-content", "content":"large input ".repeat(1000),
            "display":true, "timestamp":0,
        })).expect("custom content"));
        session.agent.set_messages(oversized_messages);
        session.finish_provider_turn().await.expect_err("fresh oversized content still requires compaction");
        session.agent.set_model(original_model);
        session.agent.set_messages(prior_messages.clone());
        let mut pending_assistant = maho_ai::providers::faux::faux_assistant_message("pending new answer", Default::default());
        pending_assistant.usage.input = 200_000;
        pending_assistant.usage.total_tokens = 200_000;
        pending_assistant.stop_reason = StopReason::Error;
        pending_assistant.error_message = Some("prompt is too long".into());
        let pending_assistant = AgentMessage::from(maho_ai::types::Message::Assistant(Box::new(pending_assistant)));
        session.process_agent_event(maho_agent::types::AgentEvent::MessageStart { message: pending_assistant.clone() },
            maho_ai::utils::abort::AbortController::new().signal()).await;
        let mut pending_messages = prior_messages.clone();
        pending_messages.push(pending_assistant);
        session.agent.set_messages(pending_messages);
        let pending_result = session.finish_provider_turn().await;
        assert!(pending_result.is_err() || lock(&retries).len() > 1, "unpersisted assistant is not the old branch assistant");
    }

    #[tokio::test]
    async fn overflow_from_previous_model_does_not_compact_small_current_context() {
        let session = test_session();
        session.agent.set_model(test_model());
        let mut failed = maho_ai::providers::faux::faux_assistant_message("", maho_ai::providers::faux::FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error), error_message: Some("prompt is too long".to_owned()), ..Default::default()
        });
        failed.model = "previous-model".to_owned();
        session.with_session_manager_mut(|manager| manager.append_message(serde_json::json!({"role":"user","content":"task","timestamp":0})));
        session.with_session_manager_mut(|manager| manager.append_message(serde_json::to_value(failed).expect("failure")));
        session.rebuild_session_context().expect("context");
        session.finish_provider_turn().await.expect("stale overflow ignored");
        assert_eq!(session.compaction_state().generation(), 0);
        assert_eq!(session.with_session_manager(|manager| manager.entries()).len(), 2);
    }

    #[tokio::test]
    async fn compaction_preserves_assistant_waiting_for_message_end_persistence() {
        let mut assistant = maho_ai::providers::faux::faux_assistant_message("delayed assistant payload", Default::default());
        assistant.timestamp = 7;
        let session = retry_session(vec![assistant], 0);
        let first = session.with_session_manager_mut(|manager| manager.append_message(
            serde_json::json!({"role":"user","content":"seed history","timestamp":0})));
        session.rebuild_session_context().expect("seed history");
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let entered = Arc::new(Mutex::new(Some(entered_tx)));
        let release = Arc::new(Mutex::new(Some(release_rx)));
        let mut extension = maho_ext_api::LoadedExtension::new("<inline:delay-persistence>", session.cwd().into(), Default::default());
        extension.handlers.insert(maho_ext_api::EventKind::MessageEnd, vec![Arc::new(move |event, _| {
            let is_assistant = matches!(event, maho_ext_api::ExtensionEvent::MessageEnd { message } if message.as_assistant().is_some());
            let signals = if is_assistant { lock(&entered).take().zip(lock(&release).take()) } else { None };
            Box::pin(async move {
                if let Some((entered, release)) = signals {
                    entered.send(()).expect("hook observer");
                    release.await.expect("release hook");
                }
                Ok(maho_ext_api::EventResult::None)
            })
        })]);
        session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
        let (prompted, (applied, reapplied, retained)) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(session.prompt("produce delayed assistant", Default::default()), async {
                entered_rx.await.expect("message end entered");
                let applied = session.apply_compaction(&crate::compaction::compaction::CompactionResult {
                    summary: "digest".into(), first_kept_entry_id: first["id"].as_str().expect("entry").into(),
                    tokens_before: 42, estimated_tokens_after: None, usage: None, details: None,
                });
                let reapplied = session.apply_compaction(&crate::compaction::compaction::CompactionResult {
                    summary: "second digest".into(), first_kept_entry_id: first["id"].as_str().expect("entry").into(),
                    tokens_before: 42, estimated_tokens_after: None, usage: None, details: None,
                });
                let retained = session.messages().iter().filter(|message| message.as_assistant().is_some()).count();
                release_tx.send(()).expect("release pending persistence");
                (applied, reapplied, retained)
            })
        }).await.expect("bounded persistence");
        applied.expect("compaction");
        reapplied.expect("repeated compaction");
        prompted.expect("prompt completion");
        assert_eq!(retained, 1, "pending assistant remains in runtime context");
        let branch = session.with_session_manager(|manager| manager.branch(manager.leaf_id()));
        let compacted = branch.iter().rposition(|entry| entry["type"] == "compaction").expect("compaction entry");
        let assistants: Vec<_> = branch.iter().enumerate().filter(|(_, entry)| entry["message"]["role"] == "assistant").collect();
        assert_eq!(assistants.len(), 1);
        assert!(assistants[0].0 > compacted);
        assert_eq!(assistants[0].1["message"]["timestamp"], 7);
        assert_eq!(session.get_last_assistant_text().as_deref(), Some("delayed assistant payload"));
        assert!(session.state().messages_awaiting_persistence.is_empty());
    }

    #[tokio::test]
    async fn dropped_message_end_releases_pending_persistence_owner() {
        use std::{future::Future, task::Poll};
        let session = retry_session(Vec::new(), 0);
        let first = session.with_session_manager_mut(|manager| manager.append_message(
            serde_json::json!({"role":"user","content":"seed history","timestamp":0})));
        session.rebuild_session_context().expect("history");
        let mut extension = maho_ext_api::LoadedExtension::new("<inline:cancel-persistence>", session.cwd().into(), Default::default());
        extension.handlers.insert(maho_ext_api::EventKind::MessageEnd, vec![Arc::new(|_, _| {
            Box::pin(std::future::pending())
        })]);
        session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
        let assistant = maho_ai::providers::faux::faux_assistant_message("abandoned payload", Default::default());
        let mut event = Box::pin(session.process_agent_event(maho_agent::types::AgentEvent::MessageEnd {
            message: AgentMessage::from(maho_ai::types::Message::Assistant(Box::new(assistant))),
        }, maho_ai::utils::abort::AbortController::new().signal()));
        std::future::poll_fn(|cx| { assert!(event.as_mut().poll(cx).is_pending()); Poll::Ready(()) }).await;
        assert_eq!(session.state().messages_awaiting_persistence.len(), 1);
        drop(event);
        assert!(session.state().messages_awaiting_persistence.is_empty());
        session.apply_compaction(&crate::compaction::compaction::CompactionResult {
            summary: "digest".into(), first_kept_entry_id: first["id"].as_str().expect("entry").into(),
            tokens_before: 42, estimated_tokens_after: None, usage: None, details: None,
        }).expect("later compaction");
        assert!(session.messages().iter().all(|message| message.as_assistant().is_none()));
        assert!(session.with_session_manager(|manager| manager.entries()).iter().all(|entry| entry["message"]["role"] != "assistant"));
    }

    #[tokio::test]
    async fn first_successful_assistant_after_compaction_is_usage_exempt() {
        let mut completed = maho_ai::providers::faux::faux_assistant_message("completed after digest", Default::default());
        completed.usage.input = 200_000;
        completed.usage.total_tokens = 200_000;
        let session = retry_session(Vec::new(), 0);
        session.agent.set_stream_function(Arc::new(move |_, _, _| {
            let stream = maho_ai::utils::event_stream::create_assistant_message_event_stream();
            stream.push(maho_ai::types::AssistantMessageEvent::Start { partial: completed.clone() });
            stream.push(maho_ai::types::AssistantMessageEvent::Done {
                reason: maho_ai::types::DoneReason::Stop, message: completed.clone(),
            });
            stream
        }));
        session.with_settings_manager_mut(|manager| manager.apply_overrides(&Map::from_iter([
            ("compaction".into(), serde_json::json!({"keepRecentTokens":1,"reserveTokens":0,"reserveScalingEnabled":false})),
        ])));
        for text in ["old task".repeat(200), "recent task".repeat(200)] {
            session.with_session_manager_mut(|manager| manager.append_message(serde_json::json!({"role":"user","content":text,"timestamp":0})));
        }
        session.rebuild_session_context().expect("history");
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let captured = calls.clone();
        let mut extension = maho_ext_api::LoadedExtension::new("<inline:usage-exemption>", session.cwd().into(), Default::default());
        extension.handlers.insert(maho_ext_api::EventKind::SessionBeforeCompact, vec![Arc::new(move |event, _| {
            captured.fetch_add(1, Ordering::SeqCst);
            let maho_ext_api::ExtensionEvent::SessionBeforeCompact(event) = event else { panic!("preparation"); };
            Box::pin(async move { Ok(maho_ext_api::EventResult::SessionBefore(maho_ext_api::SessionBeforeEventResult {
                compaction: Some(maho_ext_api::CompactionResult { summary: "digest".into(),
                    first_kept_entry_id: event.preparation.first_kept_entry_id.clone(),
                    tokens_before: event.preparation.tokens_before, details: None }), ..Default::default()
            })) })
        })]);
        session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
        session.compact(None).await.expect("manual compaction");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        tokio::time::timeout(std::time::Duration::from_secs(5), session.prompt("next task", Default::default()))
            .await.expect("bounded provider turn").expect("successful turn");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "first new successful assistant is exempt");
        assert_eq!(session.get_last_assistant_text().as_deref(), Some("completed after digest"));
        assert_eq!(session.messages().iter().rev().find_map(AgentMessage::as_assistant).expect("delivered assistant").usage.input, 200_000);
        assert!(!session.agent.has_queued_messages());
        session.finish_provider_turn().await.expect("same assistant resampled");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        session.revalidate_scheduled_continuation_admission().await.expect("exempt continuation admission");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "admission preserves the consumed exemption");
        let identical = session.messages().iter().rev().find_map(AgentMessage::as_assistant).cloned().expect("assistant");
        session.agent.set_stream_function(Arc::new(|_, _, _| {
            let response = maho_ai::providers::faux::faux_assistant_message("zero usage followup", Default::default());
            let stream = maho_ai::utils::event_stream::create_assistant_message_event_stream();
            stream.push(maho_ai::types::AssistantMessageEvent::Start { partial: response.clone() });
            stream.push(maho_ai::types::AssistantMessageEvent::Done {
                reason: maho_ai::types::DoneReason::Stop, message: response,
            });
            stream
        }));
        session.follow_up("zero usage task", None, Default::default()).await.expect("followup");
        session.agent.continue_with_queued_messages(Default::default()).await;
        session.finish_provider_turn().await.expect("zero usage continuation");
        assert_eq!(session.get_last_assistant_text().as_deref(), Some("zero usage followup"));
        assert_eq!(session.messages().iter().rev().find_map(AgentMessage::as_assistant).expect("zero usage assistant").usage.total_tokens, 0);
        assert_eq!(calls.load(Ordering::SeqCst), 2, "actual pinned stream recompacts after zero usage");
        session.with_session_manager_mut(|manager| manager.append_message(serde_json::to_value(identical).expect("identical contents")));
        session.rebuild_session_context().expect("distinct persisted assistant");
        session.finish_provider_turn().await.expect("distinct assistant checked");
        assert_eq!(calls.load(Ordering::SeqCst), 3, "equal contents do not share the exemption");
    }

    #[tokio::test]
    async fn pending_post_compaction_prompt_defers_agent_queue_ownership() {
        use std::{future::Future, task::Poll};
        let session = retry_session(Vec::new(), 0);
        let first = session.with_session_manager_mut(|manager| manager.append_message(
            serde_json::json!({"role":"user","content":"old task","timestamp":0})));
        session.with_session_manager_mut(|manager| manager.append_message(
            serde_json::json!({"role":"user","content":"recent task","timestamp":0})));
        session.rebuild_session_context().expect("history");
        session.apply_compaction(&crate::compaction::compaction::CompactionResult {
            summary: "digest".into(), first_kept_entry_id: first["id"].as_str().expect("entry").into(),
            tokens_before: 10, estimated_tokens_after: None, usage: None, details: None,
        }).expect("compaction");
        let admission = session.prompt_admission.lock().await;
        let mut prompt = Box::pin(session.prompt("pending turn", Default::default()));
        std::future::poll_fn(|cx| { assert!(prompt.as_mut().poll(cx).is_pending()); Poll::Ready(()) }).await;
        session.steer("later steering", None, Default::default()).await.expect("steering");
        session.follow_up("later followup", None, Default::default()).await.expect("followup");
        assert_eq!(session.get_steering_messages(), ["later steering"]);
        assert_eq!(session.get_follow_up_messages(), ["later followup"]);
        let mut queued_prompt = Box::pin(session.prompt("explicit followup", PromptOptions {
            streaming_behavior: Some(StreamingBehavior::FollowUp), ..Default::default()
        }));
        let queued = std::future::poll_fn(|cx| Poll::Ready(queued_prompt.as_mut().poll(cx))).await;
        drop(queued_prompt);
        let mut extension = maho_ext_api::LoadedExtension::new("<inline:reject-pending-input>", session.cwd().into(), Default::default());
        let input_calls = Arc::new(AtomicU64::new(0));
        let observed_calls = input_calls.clone();
        extension.handlers.insert(maho_ext_api::EventKind::Input, vec![Arc::new(move |event, _| {
            observed_calls.fetch_add(1, Ordering::SeqCst);
            *event = maho_ext_api::ExtensionEvent::AgentStart;
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        })]);
        session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
        let (sent, received) = tokio::sync::oneshot::channel();
        let sent = Arc::new(Mutex::new(Some(sent)));
        let _subscription = session.subscribe(Arc::new(move |event| {
            if matches!(event, AgentSessionEvent::ContinuationError { .. })
                && let Some(sent) = lock(&sent).take() { sent.send(()).expect("error observer"); }
        }));
        maho_ext_api::ExtensionActions::send_user_message(&SessionExtensionActions(Arc::downgrade(&session.inner)),
            maho_ext_api::UserMessageContent::Text("raw rejected input".into()), maho_ext_api::SendUserMessageOptions {
                deliver_as: Some(StreamingBehavior::FollowUp), ..Default::default()
            }).expect("extension input");
        tokio::time::timeout(std::time::Duration::from_secs(5), received).await.expect("bounded rejection").expect("rejection event");
        assert_eq!(session.get_follow_up_messages(), ["later followup", "explicit followup", "raw rejected input"]);
        assert_eq!(input_calls.load(Ordering::SeqCst), 1, "fallback retains raw input without re-running hooks");
        let agent_owned = session.agent.has_queued_messages();
        drop(prompt);
        drop(admission);
        session.clear_queue(false);
        assert!(matches!(queued, Poll::Ready(Ok(PromptDisposition::Queued))), "explicit queue mode must not wait for pending prompt");
        assert!(!agent_owned, "pending post-compaction inputs belong to the session until checked");
        assert!(!session.state().prompt_start_pending);
        assert!(!session.work_barrier.has_active_work());
        assert!(!session.agent.has_queued_messages());
    }

    #[tokio::test]
    async fn deferred_post_compaction_inputs_deliver_once_after_first_response() {
        use std::{future::Future, task::Poll};
        for (clear_before_release, failed_first) in [(false, false), (true, false), (false, true)] {
        let mut responses: Vec<_> = ["initial response", "steering response", "followup response"].into_iter()
            .map(|text| maho_ai::providers::faux::faux_assistant_message(text, Default::default())).collect();
        if failed_first {
            responses[0].stop_reason = StopReason::Error;
            responses[0].error_message = Some("invalid fixture request".into());
        }
        let session = retry_session(responses, 0);
        let first = session.with_session_manager_mut(|manager| manager.append_message(
            serde_json::json!({"role":"user","content":"old task","timestamp":0})));
        session.with_session_manager_mut(|manager| manager.append_message(
            serde_json::json!({"role":"user","content":"recent task","timestamp":0})));
        session.rebuild_session_context().expect("history");
        session.apply_compaction(&crate::compaction::compaction::CompactionResult {
            summary: "digest".into(), first_kept_entry_id: first["id"].as_str().expect("entry").into(),
            tokens_before: 10, estimated_tokens_after: None, usage: None, details: None,
        }).expect("compaction");
        let admission = session.prompt_admission.lock().await;
        let mut prompt = Box::pin(session.prompt("pending turn", Default::default()));
        std::future::poll_fn(|cx| { assert!(prompt.as_mut().poll(cx).is_pending()); Poll::Ready(()) }).await;
        session.steer("later steering", None, Default::default()).await.expect("steering");
        session.follow_up("later followup", None, Default::default()).await.expect("followup");
        assert!(!session.agent.has_queued_messages());
        if clear_before_release {
            let cleared = session.clear_queue(false);
            assert_eq!(cleared.steering, ["later steering"]);
            assert_eq!(cleared.follow_up, ["later followup"]);
        }
        drop(admission);
        assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(5), prompt).await
            .expect("bounded delivery").expect("prompt"), PromptDisposition::Started);
        assert_eq!(session.get_last_assistant_text().as_deref(), Some(if clear_before_release { "initial response" } else { "followup response" }));
        let persisted = session.with_session_manager(|manager| manager.entries());
        for text in ["pending turn", "later steering", "later followup"] {
            assert_eq!(persisted.iter().filter(|entry| entry["message"]["role"] == "user"
                && entry["message"]["content"][0]["text"] == text).count(),
                usize::from(!clear_before_release || text == "pending turn"), "{text}");
        }
        assert_eq!(session.pending_message_count(), 0);
        assert!(!session.agent.has_queued_messages());
        assert!(!session.state().prompt_start_pending);
        assert!(!session.work_barrier.has_active_work());
        }
    }

    #[tokio::test]
    async fn provider_overflow_recovers_when_proactive_compaction_is_disabled() {
        let failed = maho_ai::providers::faux::faux_assistant_message("", maho_ai::providers::faux::FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error), error_message: Some("prompt is too long".to_owned()), ..Default::default()
        });
        let session = retry_session(vec![failed, maho_ai::providers::faux::faux_assistant_message("recovered", Default::default())], 0);
        session.with_settings_manager_mut(|manager| manager.apply_overrides(&Map::from_iter([
            ("compaction".to_owned(), serde_json::json!({"enabled":false,"keepRecentTokens":1})),
        ])));
        session.with_session_manager_mut(|manager| manager.append_message(serde_json::json!({"role":"user","content":"previous task","timestamp":0})));
        session.rebuild_session_context().expect("context");
        let mut extension = maho_ext_api::LoadedExtension::new("<inline:overflow-recovery>", session.cwd().into(), Default::default());
        extension.handlers.insert(maho_ext_api::EventKind::SessionBeforeCompact, vec![Arc::new(|event, _| {
            let maho_ext_api::ExtensionEvent::SessionBeforeCompact(event) = event else { panic!("preparation"); };
            assert_eq!(event.reason, maho_ext_api::CompactionReason::Overflow);
            let result = maho_ext_api::CompactionResult { summary: "digest".to_owned(), first_kept_entry_id: event.preparation.first_kept_entry_id.clone(),
                tokens_before: event.preparation.tokens_before, details: None };
            Box::pin(async move { Ok(maho_ext_api::EventResult::SessionBefore(maho_ext_api::SessionBeforeEventResult {
                compaction: Some(result), ..Default::default()
            })) })
        })]);
        session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
        tokio::time::timeout(std::time::Duration::from_secs(5), session.prompt("current task", Default::default())).await.expect("bounded recovery").expect("prompt");
        assert_eq!(session.messages().last().and_then(AgentMessage::as_assistant).expect("assistant").stop_reason, StopReason::Stop);
        assert_eq!(session.compaction_state().status(), "completed");
    }

    #[tokio::test]
    async fn rejected_overflow_recovery_retains_deferred_followup_in_agent_queue() {
        use std::{future::Future, task::Poll};
        let failed = maho_ai::providers::faux::faux_assistant_message("", maho_ai::providers::faux::FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error), error_message: Some("prompt is too long".into()), ..Default::default()
        });
        let session = retry_session(vec![failed], 0);
        session.with_settings_manager_mut(|manager| manager.apply_overrides(&Map::from_iter([
            ("compaction".into(), serde_json::json!({"keepRecentTokens":1,"reserveTokens":0})),
        ])));
        let first = session.with_session_manager_mut(|manager| manager.append_message(
            serde_json::json!({"role":"user","content":"old task","timestamp":0})));
        session.with_session_manager_mut(|manager| manager.append_message(
            serde_json::json!({"role":"user","content":"recent task","timestamp":0})));
        session.rebuild_session_context().expect("history");
        session.apply_compaction(&crate::compaction::compaction::CompactionResult {
            summary: "digest".into(), first_kept_entry_id: first["id"].as_str().expect("entry").into(),
            tokens_before: 10, estimated_tokens_after: None, usage: None, details: None,
        }).expect("compaction");
        let observed = Arc::new(Mutex::new(Vec::new()));
        let captured = observed.clone();
        let weak = Arc::downgrade(&session.inner);
        let mut extension = maho_ext_api::LoadedExtension::new("<inline:reject-deferred-overflow>", session.cwd().into(), Default::default());
        extension.handlers.insert(maho_ext_api::EventKind::SessionBeforeCompact, vec![Arc::new(move |_, _| {
            let session = AgentSession { inner: weak.upgrade().expect("session") };
            lock(&captured).push(session.agent.has_queued_messages());
            Box::pin(async { Ok(maho_ext_api::EventResult::SessionBefore(maho_ext_api::SessionBeforeEventResult {
                cancel: Some(true), ..Default::default()
            })) })
        })]);
        session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
        let admission = session.prompt_admission.lock().await;
        let mut prompt = Box::pin(session.prompt("current task", Default::default()));
        std::future::poll_fn(|cx| { assert!(prompt.as_mut().poll(cx).is_pending()); Poll::Ready(()) }).await;
        session.follow_up("retained input", None, Default::default()).await.expect("followup");
        assert!(!session.agent.has_queued_messages());
        drop(admission);
        tokio::time::timeout(std::time::Duration::from_secs(5), prompt).await.expect("bounded rejection").expect_err("rejected compaction");
        let queued = session.get_follow_up_messages();
        let agent_owned = session.agent.has_queued_messages();
        session.clear_queue(false);
        assert_eq!(*lock(&observed), [true], "overflow transfers queue before recovery hook");
        assert_eq!(queued, ["retained input"]);
        assert!(agent_owned);
        assert!(!session.state().prompt_start_pending);
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
        let branch = session.with_session_manager(|manager| manager.branch(manager.leaf_id().or(Some(""))));
        assert_eq!(branch.len(), 2);
        assert_eq!(branch[0]["message"]["role"], "user");
        assert_eq!(branch[1]["message"]["stopReason"], "error");
        assert_eq!(session.messages().len(), 2);
    }

    #[tokio::test]
    async fn retry_admission_rejection_stops_before_second_provider_request() {
        let failed = maho_ai::providers::faux::faux_assistant_message("", maho_ai::providers::faux::FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Error), error_message: Some("overloaded_error".to_owned()), ..Default::default()
        });
        let session = retry_session(vec![failed], 2);
        let captured = session.clone();
        let events = Arc::new(Mutex::new(Vec::new()));
        let observed = events.clone();
        let _subscription = session.subscribe(Arc::new(move |event| {
            if matches!(event, AgentSessionEvent::AutoRetryStart { .. }) {
                let mut model = captured.model();
                model.context_window = 128;
                captured.agent.set_model(model);
            }
            if matches!(event, AgentSessionEvent::AutoRetryEnd { .. }) { lock(&observed).push(event.clone()); }
        }));
        session.with_settings_manager_mut(|manager| manager.apply_overrides(&Map::from_iter([
            ("compaction".to_owned(), serde_json::json!({"reserveTokens":0,"reserveScalingEnabled":false,"keepRecentTokens":1})),
        ])));
        let mut extension = maho_ext_api::LoadedExtension::new("<inline:retry-admission>", session.cwd().into(), Default::default());
        extension.handlers.insert(maho_ext_api::EventKind::SessionBeforeCompact, vec![Arc::new(|event, _| {
            let maho_ext_api::ExtensionEvent::SessionBeforeCompact(event) = event else { panic!("preparation"); };
            assert_eq!(event.reason, maho_ext_api::CompactionReason::Threshold);
            Box::pin(async { Ok(maho_ext_api::EventResult::SessionBefore(maho_ext_api::SessionBeforeEventResult {
                cancel: Some(true), ..Default::default()
            })) })
        })]);
        session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
        session.with_session_manager_mut(|manager| manager.append_message(serde_json::json!({"role":"user","content":"old context ".repeat(100),"timestamp":0})));
        session.rebuild_session_context().expect("context");
        tokio::time::timeout(std::time::Duration::from_secs(5), session.prompt("next", Default::default())).await.expect("bounded prompt").expect("blocked retry settles");
        assert_eq!(session.with_session_manager(|manager| manager.entries()).iter().filter(|entry| entry["message"]["role"] == "assistant").count(), 1);
        assert!(matches!(&lock(&events)[0], AgentSessionEvent::AutoRetryEnd { success: false, attempt: 1, final_error: Some(error) }
            if error == "Compaction required before provider request"));
        assert!(!session.is_retrying());
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
    async fn fallback_reduction_retains_selected_branch_not_abandoned_tail() {
        let session = test_session_with_stream_function(false);
        let mut primary = test_model();
        primary.context_window = 1_000_000;
        session.agent.set_model(primary);
        let selected = session.with_session_manager_mut(|manager| {
            let root = manager.append_message(serde_json::json!({"role":"user","content":"root","timestamp":0}));
            for _ in 0..20 {
                manager.append_message(serde_json::json!({"role":"user","content":[{"type":"text","text":"selected".repeat(5000)}],"timestamp":0}));
            }
            let selected = manager.leaf_id().expect("selected leaf").to_owned();
            manager.set_leaf(root["id"].as_str());
            manager.append_message(serde_json::json!({"role":"user","content":"abandoned","timestamp":1}));
            selected
        });
        session.with_session_manager_mut(|manager| manager.set_leaf(Some(&selected)));
        session.rebuild_session_context().expect("selected context");
        session.with_session_manager_mut(|manager| {
            let root = manager.entries().into_iter().find(|entry| entry["type"] == "message").expect("root");
            let selected = manager.leaf_id().expect("selected context leaf").to_owned();
            manager.set_leaf(root["id"].as_str());
            manager.append_custom("abandoned-metadata", None);
            manager.set_leaf(Some(&selected));
        });
        let mut target = test_model();
        target.id = "fallback-small".to_owned();
        let measured = session.with_session_manager(|manager| manager.build_context(manager.leaf_id())).messages.iter()
            .map(crate::compaction::compaction::estimate_tokens).sum();
        let reduced = session.reduce_for_switch_target(&target, measured).expect("fallback reduction");
        session.assert_model_usable(&target, reduced).expect("reduced budget");
        let actual: u64 = session.with_session_manager(|manager| manager.build_context(manager.leaf_id())).messages.iter()
            .map(crate::compaction::compaction::estimate_tokens).sum();
        assert_eq!(actual, reduced);
        assert!(session.with_session_manager(|manager| manager.entries()).iter().any(|entry| entry["details"]["schema"] == "senpi.compaction.resume-slice.v1"));
        assert!(!session.messages().iter().any(|message| user_message_text(message) == "abandoned"));
        assert!(session.messages().iter().any(|message| user_message_text(message).starts_with("selected")));
        assert_eq!(session.with_session_manager(|manager| manager.entries()).iter().filter(|entry| entry["type"] == "message").count(), 22);
    }

    #[tokio::test]
    async fn fallback_switch_reduces_context_without_losing_recorded_transcript() {
        let session = test_session_with_stream_function(false);
        let mut primary = test_model();
        primary.context_window = 1_000_000;
        session.agent.set_model(primary);
        for _ in 0..20 {
            session.with_session_manager_mut(|manager| manager.append_message(serde_json::json!({
                "role":"user","content":[{"type":"text","text":"x".repeat(40_000)}],"timestamp":0
            })));
        }
        session.rebuild_session_context().unwrap();
        let mut target = test_model();
        target.id = "fallback-small".to_owned();
        target.context_window = 128_000;
        let mut deps = SessionFallbackDeps(Arc::downgrade(&session.inner));
        crate::retry_fallback::controller::RetryFallbackDeps::switch_model(&mut deps, target.clone(), ModelThinkingLevel::Off, false).await.unwrap();
        assert_eq!(session.model().id, target.id);
        let entries = session.with_session_manager(|manager| manager.entries());
        assert_eq!(entries.iter().filter(|entry| entry["type"] == "message").count(), 20);
        assert!(entries.iter().any(|entry| entry["details"]["schema"] == "senpi.compaction.resume-slice.v1"));
        assert!(entries.iter().any(|entry| entry["type"] == "model_change" && entry["reason"] == "fallback"));
        let live = session.with_session_manager(|manager| manager.build_context(manager.leaf_id())).messages.iter()
            .map(crate::compaction::compaction::estimate_tokens).sum();
        session.assert_model_usable(&target, live).unwrap();
    }

    #[tokio::test]
    async fn empty_manual_compaction_emits_matched_terminal_events() {
        let session = test_session_with_stream_function(false);
        session.agent.set_model(test_model());
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = events.clone();
        let _subscription = session.subscribe(Arc::new(move |event| lock(&captured).push(event.clone())));
        assert!(session.compact(None).await.is_err());
        let events = lock(&events);
        let start = events.iter().find_map(|event| match event {
            AgentSessionEvent::CompactionStart { reason: maho_ext_api::CompactionReason::Manual, request_id } => request_id.clone(),
            _ => None,
        }).expect("start");
        assert!(events.iter().any(|event| matches!(event,
            AgentSessionEvent::CompactionEnd { reason: maho_ext_api::CompactionReason::Manual, request_id: Some(id),
                aborted: false, result: None, will_retry: false, accepted: Some(false), .. } if id == &start)));
        assert!(!session.is_compacting());
        assert!(session.with_session_manager(|manager| manager.entries()).is_empty());
    }

    #[tokio::test]
    async fn user_abort_joins_settling_boundary_and_is_delivered_once() {
        let session = test_session_with_stream_function(false);
        let boundary = session.state().abort_provenance.begin_agent_end(Vec::new(), false, false);
        session.state().abort_provenance.end_agent_end(&boundary);
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = events.clone();
        let _subscription = session.subscribe(Arc::new(move |event| lock(&captured).push(event.clone())));
        tokio::time::timeout(std::time::Duration::from_secs(2), session.abort()).await.unwrap();
        assert!(lock(&events).is_empty());
        session.emit_late_user_abort().await;
        session.emit_late_user_abort().await;
        assert_eq!(lock(&events).iter().filter(|event| matches!(event, AgentSessionEvent::SessionAbort)).count(), 1);
        session.state().abort_provenance.close_agent_end_boundary();
        assert!(!session.state().abort_provenance.has_open_agent_end_boundary());
    }

    #[tokio::test]
    async fn impossible_model_switch_leaves_active_model_and_history_unchanged() {
        let session = test_session_with_stream_function(false);
        session.agent.set_model(test_model());
        let mut model = test_model();
        model.id = "tiny".to_owned();
        model.context_window = 4_096;
        let revision = session.message_revision();
        assert!(session.set_model(model).await.is_err());
        assert_eq!(session.message_revision(), revision);
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
        let revision = session.message_revision();
        session.set_model(model).await.expect("held switch");
        assert_eq!(session.message_revision(), revision);
        assert_eq!(session.model().id, "faux-1");
        assert_eq!(session.pending_model_switch().expect("pending").model.id, "smaller");
        assert!(session.with_session_manager(|manager| manager.entries()).is_empty());
    }

    #[tokio::test]
    async fn equal_window_switch_accounts_for_smaller_usable_context() {
        let session = test_session_with_stream_function(false);
        session.agent.set_model(test_model());
        let (budget, _) = session.model_budget(&session.model(), 0, false).expect("budget");
        let available = budget.context_window - budget.required_tokens;
        session.agent.set_messages(vec![make_user_message(&"x".repeat((available * 4 - 8_000) as usize), None)]);
        let mut model = test_model();
        model.id = "same-window-larger-output".to_owned();
        model.max_tokens += 8_000;
        session.set_session_model(model).await.expect("held switch");
        assert_eq!(session.model().id, "faux-1");
        assert_eq!(session.pending_model_switch().expect("pending").model.id, "same-window-larger-output");
        assert!(session.with_session_manager(|manager| manager.entries()).is_empty());
    }

    #[test]
    fn compaction_application_rebuilds_only_summary_and_retained_suffix() {
        let session = test_session();
        session.agent.set_model(test_model());
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
    fn compaction_retention_uses_selected_leaf_instead_of_last_appended_branch() {
        let session = test_session();
        session.agent.set_model(test_model());
        let selected = session.with_session_manager_mut(|manager| manager.append_message(serde_json::json!({"role":"user","content":"selected","timestamp":0})));
        let abandoned = session.with_session_manager_mut(|manager| manager.append_message(serde_json::json!({"role":"user","content":"abandoned","timestamp":1})));
        session.with_session_manager_mut(|manager| manager.set_leaf(selected["id"].as_str()));
        let result = crate::compaction::compaction::CompactionResult { summary: "summary".to_owned(),
            first_kept_entry_id: abandoned["id"].as_str().expect("abandoned").to_owned(), tokens_before: 100,
            estimated_tokens_after: None, usage: None, details: None };
        assert_eq!(session.apply_compaction(&result).expect_err("off branch"), "Compaction first kept entry is not on the current branch");
        assert_eq!(session.with_session_manager(|manager| manager.entries()).len(), 2);
        let context = SessionContextManager::new(&session);
        assert_eq!(maho_ext_api::SessionManager::get_branch(&context).len(), 1);
        session.apply_compaction(&crate::compaction::compaction::CompactionResult {
            first_kept_entry_id: selected["id"].as_str().expect("selected").to_owned(), ..result
        }).expect("selected branch compaction");
        assert_eq!(user_message_text(&session.messages()[1]), "selected");
    }

    #[test]
    fn oversized_compaction_is_rejected_without_mutating_history() {
        let session = test_session();
        session.agent.set_model(test_model());
        let retained = session.with_session_manager_mut(|manager| manager.append_message(serde_json::json!({"role":"user","content":"recent","timestamp":1})));
        let revision = session.message_revision();
        let error = session.apply_compaction(&crate::compaction::compaction::CompactionResult {
            summary: "x".repeat(600_000), first_kept_entry_id: retained["id"].as_str().expect("id").to_owned(),
            tokens_before: 100, estimated_tokens_after: None, usage: None, details: None,
        }).expect_err("overflow rejected");
        assert_eq!(error, "Compaction rejected: summary-would-overflow");
        assert_eq!(session.message_revision(), revision);
        assert_eq!(session.with_session_manager(|manager| manager.entries().len()), 1);
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
    async fn unchanged_message_edits_reject_stale_leaf_and_append_nothing() {
        let session = test_session();
        let user = session.with_session_manager_mut(|manager| manager.append_message(serde_json::json!({"role":"user","content":"same","timestamp":0})));
        let assistant = maho_ai::providers::faux::faux_assistant_message("reply", Default::default());
        let reply = session.with_session_manager_mut(|manager| manager.append_message(serde_json::to_value(&assistant).expect("message")));
        let user_id = user["id"].as_str().expect("id");
        let reply_id = reply["id"].as_str().expect("id");
        let stale = TreeNavigationOptions { expected_leaf_id: Some("stale".to_owned()), ..Default::default() };
        assert!(session.edit_assistant_message(reply_id, "reply", stale.clone()).await.is_err());
        assert!(session.edit_user_message(user_id, "same", stale).await.is_err());
        assert_eq!(session.edit_assistant_message(reply_id, "reply", Default::default()).await.expect("unchanged").unchanged, Some(true));
        assert_eq!(session.edit_user_message(user_id, "same", Default::default()).await.expect("unchanged").unchanged, Some(true));
        assert_eq!(session.with_session_manager(|manager| manager.entries().len()), 2);
        assert_eq!(session.with_session_manager(|manager| manager.leaf_id().map(str::to_owned)), Some(reply_id.to_owned()));
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
    fn enabled_hook_sources_preserve_scope_order_and_session_directory() {
        let session = test_session();
        let resource = |name: &str, scope, enabled| crate::package_manager::ResolvedResource {
            path: name.into(), enabled, metadata: crate::package_manager::PathMetadata {
                scope, base_dir: Some(session.cwd().to_owned()), ..Default::default()
            }
        };
        session.set_hook_source_paths(vec![
            resource("global.json", crate::source_info::SourceScope::User, true),
            resource("project.json", crate::source_info::SourceScope::Project, true),
            resource("ignored.json", crate::source_info::SourceScope::User, false),
            resource("global.json", crate::source_info::SourceScope::User, true),
        ], vec!["extra.json".into(), "extra.json".into()]);
        let context = crate::sdk::extension_context::create(&session);
        let sources = context.get_loaded_hook_sources();
        assert_eq!(sources.global_hook_source_paths.len(), 1);
        assert_eq!(sources.project_hook_source_paths.len(), 1);
        assert_eq!(sources.pre_session_hook_source_paths.len(), 1);
        assert_eq!(context.session_manager.get_session_dir(), Some(session.with_session_manager(|manager| manager.session_dir().into())));
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
    async fn skill_expansion_caps_unique_skills_and_preserves_unexpanded_tokens() {
        let session = test_session();
        let dir = tempfile::tempdir().expect("directory");
        let skills = (0..6).map(|index| {
            let name = format!("skill{index}");
            let path = dir.path().join(format!("{name}.md"));
            std::fs::write(&path, format!("---\nname: {name}\ndescription: test\n---\nbody {index}")).expect("skill");
            crate::skills::Skill { name, description: "test".to_owned(), file_path: path.to_string_lossy().into_owned(),
                base_dir: dir.path().to_string_lossy().into_owned(), disable_model_invocation: false,
                source_info: crate::source_info::create_synthetic_source_info(&path.to_string_lossy(), Default::default()) }
        }).collect();
        session.set_prompt_resources(Vec::new(), skills);
        let expanded = session.expand_input("$skill0 $skill1 $skill2 $skill3 $skill4 $skill5 task", true).expect("expand");
        assert_eq!(expanded.matches("<skill-instruction name=").count(), 5);
        assert!(expanded.contains("$skill5"));
        let unknown = session.expand_input("/skill:missing /skill:skill0 task", true).expect("unknown");
        assert!(!unknown.contains("<skill-instruction name="));
        assert!(unknown.contains("/skill:skill0"));
    }

    #[test]
    fn discovered_resources_merge_once_and_keep_extension_metadata() {
        let session = test_session();
        let dir = tempfile::tempdir().expect("directory");
        let prompt = dir.path().join("discovered.md");
        let skill = dir.path().join("skill.md");
        std::fs::write(&prompt, "---\ndescription: discovered prompt\n---\nreview $1").expect("prompt fixture");
        std::fs::write(&skill, "---\nname: discovered-skill\ndescription: discovered skill\n---\nskill body").expect("skill fixture");
        let entry = |path: &std::path::Path| maho_ext_api::DiscoveredResourceEntry {
            path: path.to_string_lossy().into_owned(), extension_path: "<inline:resources>".to_owned(),
            scope: Some(maho_ext_api::SourceScope::System),
        };
        let resources = maho_ext_api::DiscoveredResources {
            prompt_paths: vec![entry(&prompt)], skill_paths: vec![entry(&skill)],
            hook_paths: vec![entry(&dir.path().join("hooks.json"))], ..Default::default()
        };
        session.extend_discovered_resources(resources.clone());
        session.extend_discovered_resources(resources);
        let templates = session.prompt_templates();
        assert_eq!(templates.len(), 1);
        assert_eq!(templates[0].source_info.scope, crate::source_info::SourceScope::System);
        assert_eq!(templates[0].source_info.source, "extension:inline:resources");
        assert_eq!(session.expand_input("/discovered file", true).expect("prompt expansion"), "review file");
        let state = session.state();
        assert_eq!(state.skills.len(), 1);
        assert_eq!(state.skills[0].source_info.scope, crate::source_info::SourceScope::System);
        assert_eq!(state.discovered_resources.hook_paths.len(), 1);
        drop(state);
        let sources = maho_ext_api::ExtensionContextActions::get_loaded_hook_sources(
            &SessionExtensionActions(Arc::downgrade(&session.inner)));
        assert_eq!(sources.runtime_hook_source_paths, vec![dir.path().join("hooks.json")]);
        session.set_hook_sources(Some(sources));
        session.extend_discovered_resources(maho_ext_api::DiscoveredResources {
            hook_paths: vec![entry(&dir.path().join("later-hooks.json"))], ..Default::default()
        });
        let combined = maho_ext_api::ExtensionContextActions::get_loaded_hook_sources(
            &SessionExtensionActions(Arc::downgrade(&session.inner)));
        assert_eq!(combined.runtime_hook_source_paths, vec![dir.path().join("hooks.json"), dir.path().join("later-hooks.json")]);
        std::fs::write(&prompt, "---\ndescription: updated prompt\n---\nupdated $1").expect("updated fixture");
        session.extend_discovered_resources(maho_ext_api::DiscoveredResources {
            prompt_paths: vec![entry(&prompt)], ..Default::default()
        });
        assert_eq!(session.prompt_templates().len(), 1);
        assert_eq!(session.expand_input("/discovered file", true).expect("updated expansion"), "updated file");
        let mut updated = entry(&prompt);
        updated.scope = Some(maho_ext_api::SourceScope::Project);
        session.extend_discovered_resources(maho_ext_api::DiscoveredResources {
            prompt_paths: vec![updated], ..Default::default()
        });
        assert_eq!(session.prompt_templates()[0].source_info.scope, crate::source_info::SourceScope::Project);
        let revision = session.message_revision();
        session.state().system_prompt_override = Some("active turn prompt".to_owned());
        session.agent.set_system_prompt("active turn prompt".to_owned());
        session.extend_discovered_resources(Default::default());
        session.extend_discovered_resources(maho_ext_api::DiscoveredResources {
            hook_paths: vec![entry(&dir.path().join("other-hooks.json"))], ..Default::default()
        });
        assert_eq!(session.system_prompt(), "active turn prompt");
        assert_eq!(session.message_revision(), revision);
    }

    #[tokio::test]
    async fn native_user_bash_hook_result_is_recorded_without_host_command_execution() {
        let session = test_session();
        let mut extension = maho_ext_api::LoadedExtension::new("<inline:user-bash>", session.cwd().into(), Default::default());
        extension.handlers.insert(maho_ext_api::EventKind::UserBash, vec![Arc::new(|_, _| Box::pin(async {
            Ok(maho_ext_api::EventResult::UserBash { operations: None, result: Some(maho_tools::bash_executor::BashResult {
                output: "hook result".into(), exit_code: Some(0), cancelled: false, truncated: false, full_output_path: None,
            }) })
        }))]);
        session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension], Default::default(), Default::default(), test_extension_context(&session))).await;
        let result = session.execute_user_bash("exit 99", None, true, None).await;
        let messages = session.messages();
        session.dispose().await;
        let result = result.expect("hook result");
        assert_eq!(result.output, "hook result");
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role(), "bashExecution");
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
