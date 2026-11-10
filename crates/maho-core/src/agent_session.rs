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

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
#[path = "monitor_invocation.rs"]
mod monitor_invocation;

thread_local! {
    /// The recovery carrier of the record each session is emitting on this thread, innermost last: listeners run
    /// synchronously inside `emit`, so `AgentSession::recovery_carrier` reads the record's carrier here (senpi keys
    /// it to the event object through a WeakMap).
    static EMITTING_RECOVERY: std::cell::RefCell<Vec<(usize, Option<crate::session_recovery_identity::RecoveryCarrier>)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

tokio::task_local! {
    static EXTENSION_EVENT_SIGNAL: maho_ext_api::AbortSignal;
    static EXTENSION_COMPACTION_PREPARATION: maho_ext_api::CompactionPreparationDetails;
    static NESTED_TOOL_SCOPE: (String, Arc<Mutex<crate::session_record::NestedCallRecorder>>, Arc<AtomicU64>);
    static NESTED_TOOL_QUEUE_HELD: bool;
    static HELD_COMMAND_SESSION: String;
}
struct HeldInputReservation {
    session: AgentSession,
    ids: Vec<String>,
}
impl Drop for HeldInputReservation {
    fn drop(&mut self) {
        let mut reserved = lock(&self.session.reserved_held_inputs);
        for id in &self.ids {
            reserved.remove(id);
        }
    }
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
    fn drop(&mut self) {
        self.signal.remove_abort_listener(self.listener);
    }
}

use maho_agent::Agent;
use maho_agent::heap_future::heap_future;
use maho_agent::tool_name_alias::resolve_tool_name_alias;
use maho_agent::types::{AgentMessage, AgentState, AgentTool, AgentToolResult};
use maho_ai::model::Model;
use maho_ai::models::models_are_equal;
use maho_ai::types::{
    ImageContent, ModelThinkingLevel, ProviderEnv, ProviderHeaders, ServiceTierPreference,
    StopReason, ThinkingLevel, ThinkingSelection,
};
use maho_ext_api::{
    AgentSessionEvent, CompactionRejectionCause, ExtensionError, ExtensionMode, ExtensionUi,
    FlagValue, InputSource, ServiceTier, SessionReason, SessionStartEvent, SourceInfo,
    SourceOrigin, SourceScope, StreamingBehavior, ToolCallEvent, ToolDefinition, ToolExposure,
    ToolInfo, normalize_tool_exposure,
};
use maho_ext_host::ExtensionRunner;
use serde_json::{Map, Value, json};

use crate::event_bus::{EventBus, EventHandler, EventSubscription};

use crate::model_registry::ModelRegistry;
use crate::model_runtime::ModelRuntime;
use crate::session_activity::{
    SessionActivitySnapshot, WakeSourceTracker, is_session_busy_snapshot,
};
use crate::session_log::{SessionLogger, SessionLoggerOptions};
use crate::session_manager::SessionManager;
use crate::settings_manager::SettingsManager;
use crate::utils::tool_result_images::normalize_tool_result_images;

/// Sample eval-cell call for an eval-only tool, using the argument name that tool actually takes.
/// Sample eval-cell call for an eval-only tool, using the argument name that tool actually takes.
const EVAL_ONLY_TOOL_NAMES: [&str; 2] = ["workflow", "monitor"];

/// Live per-session MCP wire-status channel on the extension bus (native payload, no JSON).
pub const MCP_WIRE_STATUS_CHANGED_EVENT: &str = "senpi.rpc.mcp_wire_status.changed";

#[allow(dead_code)] // consumed by the eval-only hint publisher in the tool-registry slice
fn eval_helper_call(name: &str) -> String {
    match name {
        "bash" | "powershell" => format!("tool.{name}({{ command: \"...\" }})"),
        "grep" => "tool.grep({ pattern: \"...\", path: \"...\" })".to_owned(),
        "workflow" => format!("tool.{name}({{ action: \"...\" }})"),
        "monitor" => {
            "tool.monitor({ description: \"...\", command: \"...\", filter: \"...\" })".to_owned()
        }
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

/// Session-bus channel carrying the engine-turn hold's `session_paused` / `session_resumed` events.
pub const SESSION_HOLD_CHANNEL: &str = "session:hold";

/// senpi `UserActivityKind`; one type shared with the extension API so `onUserActivity` forwards as-is.
pub use maho_ext_api::{UserActivityKind, UserActivityListener};

/// Session-bus channel published after an in-place session replacement (`reason` new/resume/fork)
/// or a reload (`reason` reload). Native adaptation of senpi `AgentSessionRuntime.setRebindSession`:
/// single-session hosts replace the session in place, so connections learn of it here.
pub const SESSION_REBIND_CHANNEL: &str = "maho:session-rebind";

#[derive(Clone, Default)]
pub struct ExtensionBindings {
    pub ui_context: Option<Arc<dyn ExtensionUi>>,
    pub mode: Option<ExtensionMode>,
    pub abort_handler: Option<Arc<dyn Fn() + Send + Sync>>,
    pub on_error: Option<ExtensionErrorListener>,
    /// senpi `ExtensionBindings.shutdownHandler`: what `ctx.shutdown()` requests in this mode.
    pub shutdown_handler: Option<Arc<dyn Fn() + Send + Sync>>,
    pub sizes_only: bool,
    pub prompt_size_inventory: Option<Arc<dyn crate::prompt_size_inventory::PromptSizeInventory>>,
}

/// Builds a session's first extension bindings right before `bind_extensions`, so a mode can
/// subscribe to the session (for example its extension RPC events) before `session_start` runs.
pub type ExtensionBindingsFactory = Arc<dyn Fn(&AgentSession) -> ExtensionBindings + Send + Sync>;

/// senpi `extensionRunner.onRpcEvent` handler: `(name, data)` of one `senpi:extension-rpc-event`.
pub type ExtensionRpcEventListener = Arc<dyn Fn(&str, &Value) + Send + Sync>;

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

#[derive(Debug, Clone)]
pub struct MessageEditFailure {
    pub message: String,
    pub code: Option<&'static str>,
}

/// senpi `_navigateTree`'s refusal while a compaction or tree navigation runs.
const NAVIGATE_WHILE_COMPACTING: &str = "Wait for the current compaction or tree navigation to finish before navigating the session tree.";
impl From<String> for MessageEditFailure {
    fn from(message: String) -> Self {
        Self {
            message,
            code: None,
        }
    }
}

/// A failed session switch or import; `missing_cwd` carries senpi's `MissingSessionCwdError` so a
/// host can offer the continue-in-current-cwd prompt (RPC `errorData`).
#[derive(Debug, Clone)]
pub struct SessionSwitchFailure {
    pub message: String,
    pub missing_cwd: Option<crate::session_cwd::MissingSessionCwdError>,
}
impl From<String> for SessionSwitchFailure {
    fn from(message: String) -> Self {
        Self {
            message,
            missing_cwd: None,
        }
    }
}
impl From<&str> for SessionSwitchFailure {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}
impl From<crate::session_cwd::MissingSessionCwdError> for SessionSwitchFailure {
    fn from(error: crate::session_cwd::MissingSessionCwdError) -> Self {
        Self {
            message: error.to_string(),
            missing_cwd: Some(error),
        }
    }
}

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
    /// senpi `queuedId`: session-unique id of this queued input (`q_<n>`), stable until it is delivered or removed.
    pub queued_id: String,
    /// senpi `revision`: 1 when queued, +1 on every edit; a change can name the revision it was based on (senpi#3459).
    pub revision: u64,
    /// senpi `ClientMessageIdentity.clientMessageId` of the input, when it was admitted with one.
    pub client_message_id: Option<String>,
}

/// senpi `QueuedInputChange`: what a dequeue or edit of one queued input did.
pub use crate::pending_input_session::QueuedInputChangeOutcome as QueuedInputChange;

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
    /// Internal: the input already went through `prompt()`, so a held session was already released for it.
    pub from_prompt: bool,
}

#[derive(Clone)]
pub struct ToolDefinitionEntry {
    pub definition: ToolDefinition,
    pub source_info: SourceInfo,
}

pub type LazyToolActivator = Arc<dyn Fn(&str) -> bool + Send + Sync>;

pub type AgentSessionEventListener = Arc<dyn Fn(&AgentSessionEvent) + Send + Sync>;
type BeforeSessionReplaceListener = Arc<dyn Fn() + Send + Sync>;

/// Copy-on-write listener list: `emit` snapshots it with one `Arc` clone.
type SharedEventListeners = Arc<Mutex<Arc<Vec<(u64, AgentSessionEventListener)>>>>;

/// Unsubscribes its listener when dropped, matching the function `subscribe` returns in senpi.
pub struct AgentSessionSubscription {
    listeners: SharedEventListeners,
    id: u64,
}

impl Drop for AgentSessionSubscription {
    fn drop(&mut self) {
        // PERF-DIVERGENCE(senpi agent-session.ts): copy-on-write listener list; an in-flight emit keeps its own snapshot.
        let mut guard = self
            .listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::make_mut(&mut guard).retain(|(id, _)| *id != self.id);
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
    pub tool_barrier: Option<crate::session_work_barrier::ToolBarrierSpec>,
    pub client_message_id: Option<String>,
    pub observed_pause_seq: Option<u64>,
    pub hold_after_preparation: bool,
    pub expand_prompt_templates: Option<bool>,
    pub images: Option<Vec<ImageContent>>,
    pub streaming_behavior: Option<StreamingBehavior>,
    pub thinking_level: Option<ThinkingLevel>,
    pub source: Option<InputSource>,
    pub signal: Option<maho_ai::utils::abort::AbortSignal>,
    pub prompt_admitted: Option<Arc<dyn Fn(PromptDisposition) + Send + Sync>>,
    pub session_title_prompt: Option<SessionTitlePrompt>,
    /// Internal: the caller already reported this input to `on_human_input_admitted`.
    pub human_input_reported: bool,
    pub held_messages: Vec<AgentMessage>,
    pub held_input_ids: Vec<String>,
    /// Internal: who started this input when it differs from `source` (a person's answer sent through the extension API).
    pub input_origin: Option<InputOrigin>,
}

/// Who started an input, classified once at its entry point (senpi input-origin.ts).
pub use crate::input_origin::{InputOrigin, input_origin_of};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HumanInputKind {
    Prompt,
    Steer,
    FollowUp,
    Answer,
    Resume,
}

/// A human input about to be queued or admitted (senpi#3811 design 4.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HumanInputAdmittedEvent {
    pub kind: HumanInputKind,
    pub at: u64,
}

pub type HumanInputListener = Arc<dyn Fn(&HumanInputAdmittedEvent) + Send + Sync>;

fn is_human_input(source: Option<InputSource>) -> bool {
    input_origin_of(source) == InputOrigin::Person
}

fn prompt_kind(options: &PromptOptions) -> HumanInputKind {
    match options.streaming_behavior {
        Some(StreamingBehavior::Steer) => HumanInputKind::Steer,
        Some(StreamingBehavior::FollowUp) => HumanInputKind::FollowUp,
        None => HumanInputKind::Prompt,
    }
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

#[derive(Clone, Debug, Default, PartialEq)]
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
    pub cost_unknown: bool,
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

/// senpi `OVERFLOW_RECOVERY_RUNGS`: compact-and-retry attempts per overflow episode; the second keeps no recent tail.
const OVERFLOW_RECOVERY_RUNGS: u8 = 2;
/// senpi `OVERFLOW_RECOVERY_EXHAUSTED_MESSAGE`: terminal message after both compact-and-retry rungs still overflow.
pub const OVERFLOW_RECOVERY_FAILED_MESSAGE: &str = "Context overflow recovery failed after two compact-and-retry attempts. Try reducing context or switching to a larger-context model.";

/// senpi `_promptAgent`: the refusal of an automatic turn after an interrupted run.
pub const INTERRUPTED_RUN_REFUSAL: &str =
    "The previous run was interrupted. Human input or Resume is required before another turn.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequiredCompactionError;

impl std::fmt::Display for RequiredCompactionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(
            "Context remains above the compaction threshold because compaction did not complete",
        )
    }
}

impl std::error::Error for RequiredCompactionError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MissingModelAccessError;

impl std::fmt::Display for MissingModelAccessError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("AgentSession requires modelRuntime or modelRegistry")
    }
}

impl std::error::Error for MissingModelAccessError {}

/// The reason a compaction ran, as the internal `reason` string its callers pass (`compact`, the
/// automatic routes) and the extension API sees.
fn compaction_reason_from_str(reason: &str) -> maho_ext_api::CompactionReason {
    use maho_ext_api::CompactionReason;
    match reason {
        "manual" => CompactionReason::Manual,
        "extension" => CompactionReason::Extension,
        "overflow" => CompactionReason::Overflow,
        "pre-prompt" => CompactionReason::PrePrompt,
        _ => CompactionReason::Threshold,
    }
}

/// The `details.structuralYield` block senpi's speculative route embeds at generation time: the
/// tokens the summarized prefix and the turn prefix held, minus the summary that replaced them.
/// The compaction extension counts an ineffective summary from it, and the core route produced none,
/// so a summary the size of the context it replaced never counted and repeated until the session
/// aborted itself.
fn structural_yield_details(
    details: Option<Value>,
    preparation: &crate::compaction::compaction::CompactionPreparation,
    summary: &str,
    tokens_before: f64,
) -> Value {
    let text_tokens = |text: &str| {
        crate::compaction::compaction::estimate_tokens(&serde_json::json!({
            "role": "assistant",
            "content": [{"type": "text", "text": text}],
        })) as f64
    };
    let summarized: u64 = preparation
        .messages_to_summarize
        .iter()
        .map(crate::compaction::compaction::estimate_tokens)
        .sum::<u64>()
        + preparation
            .turn_prefix_messages
            .iter()
            .map(crate::compaction::compaction::estimate_tokens)
            .sum::<u64>();
    let replaced = text_tokens(preparation.previous_summary.as_deref().unwrap_or_default())
        + summarized as f64;
    let saved_tokens = replaced - text_tokens(summary);
    let mut details = match details {
        Some(Value::Object(object)) => Value::Object(object),
        _ => Value::Object(Map::new()),
    };
    details["structuralYield"] = serde_json::json!({
        "savedTokens": saved_tokens,
        "savingsRatio": if tokens_before > 0.0 { saved_tokens / tokens_before } else { 0.0 },
        "tokensBefore": tokens_before,
    });
    details
}

/// Human-readable message for a rejection cause; exhaustive over the union so a new cause cannot
/// reintroduce a silent failure at the `compaction_end` UI seam.
pub fn describe_compaction_rejection(cause: CompactionRejectionCause) -> &'static str {
    use CompactionRejectionCause as Cause;
    match cause {
        Cause::WouldOverflow => {
            "Compaction rejected: the produced summary would still overflow the model context window. Reduce context (e.g. /new, drop attachments) or switch to a larger-context model."
        }
        Cause::CancelledByExtension => "Compaction rejected: cancelled by an extension.",
        Cause::ExternalOwner => {
            "Compaction rejected: the active provider owns compaction for this session."
        }
        Cause::CircuitBreaker => {
            "Compaction rejected: the compaction circuit breaker is open after repeated failures. Wait for the cooldown and retry."
        }
        Cause::PerTurnCap => {
            "Compaction rejected: automatic compaction refused after repeated ineffective attempts this turn. Run /compact to force one."
        }
        Cause::StaleRevision => {
            "Compaction rejected: the session changed while the summary was being prepared. Retry compaction against the latest context."
        }
    }
}

pub fn provider_retry_watchdog_abort_message(
    retry_timeout_ms: Option<u64>,
    stream_start_timeout_ms: Option<u64>,
) -> String {
    let timeout =
        retry_timeout_ms.map_or_else(|| "undefined".to_owned(), |value| value.to_string());
    match stream_start_timeout_ms {
        None => format!(
            "Provider retry continuation watchdog timed out after {timeout}ms (stream-start guard disabled; raise retry.provider.streamStartTimeoutMs, 0 disables)"
        ),
        Some(stream_start) => format!(
            "Provider retry continuation watchdog timed out after {timeout}ms (stream-start guard: {stream_start}ms; raise retry.provider.streamStartTimeoutMs, 0 disables)"
        ),
    }
}

/// Host refusal of a prompt-dispatched extension command: `(name, args, source of the registering
/// extension)` -> `Some(reason)` refuses.
pub type ExtensionCommandGate =
    Arc<dyn Fn(&str, &str, &maho_ext_api::SourceInfo) -> Option<String> + Send + Sync>;

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
    /// Names a registration offered that `excluded_tool_names` withheld: lets a caller tell a real
    /// excluded tool from an exclusion naming nothing.
    withheld_excluded_tool_names: BTreeSet<String>,
    /// Host refusal of prompt-dispatched extension commands (`/name args`).
    extension_command_gate: Option<ExtensionCommandGate>,
    /// The host marked the session's role restrictive: client-submitted user bash is refused.
    user_bash_denied: bool,
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
    /// senpi `DefaultResourceLoader.agentsFiles` / `systemPrompt` / `appendSystemPrompt`: resolved
    /// once per resource load (or source change) and read by every prompt rebuild until `/reload`.
    loaded_context_files: Option<Vec<crate::system_prompt::ContextFile>>,
    resolved_system_prompt: Option<(Option<String>, Vec<String>)>,
    system_prompt_override: Option<String>,
    /// senpi `_promptSurface` (`None` falls back to `resolvePromptSurface(process.env)`).
    prompt_surface: Option<maho_ext_api::PromptSurface>,
    /// senpi `_browserEngine` (`connected` / `builtin` / `none`).
    browser_engine: Option<String>,
    wake_sources: WakeSourceTracker,
    security_notices: crate::security_notices::SecurityNoticeSnapshot,
    shown_high_reasoning_warning_keys: BTreeSet<String>,
    extension_mode: ExtensionMode,
    extension_ui_context: Option<Arc<dyn ExtensionUi>>,
    extension_bound_ui: Option<(Arc<dyn ExtensionUi>, Arc<dyn ExtensionUi>)>,
    extension_abort_handler: Option<Arc<dyn Fn() + Send + Sync>>,
    extension_shutdown_handler: Option<Arc<dyn Fn() + Send + Sync>>,
    extension_error_listener: Option<ExtensionErrorListener>,
    /// Tracks startup dispatch so RPC attach can install UI without repeating the lifecycle.
    extension_lifecycle_started: bool,
    message_revision: u64,
    assistant_generation: u64,
    last_persisted_assistant: Option<(String, u64)>,
    post_compaction_assistant_generation: Option<u64>,
    post_compaction_usage_exempt_entries: BTreeSet<String>,
    /// senpi `_overflowRecoveryRungs`: compact-and-retry rungs spent in the current overflow episode.
    overflow_recovery_rungs: u8,
    /// senpi `_compactionSkippedTooSmall`: the last overflow compaction found nothing to compact.
    compaction_skipped_too_small: bool,
    messages_awaiting_persistence: Vec<(uuid::Uuid, AgentMessage)>,
    steering_messages: Vec<String>,
    follow_up_messages: Vec<String>,
    queued_input_order: Vec<QueuedInput>,
    queued_input_ages: crate::queued_input_age::QueuedInputAges,
    next_queued_input_order: u64,
    /// senpi `_nextQueuedId`.
    next_queued_id: u64,
    /// senpi `_queuedMessages`: the message queued for each `queuedId`, so removal and edits hit the native queue.
    queued_messages: Vec<(String, AgentMessage)>,
    /// senpi `_deliveredQueuedIds`: recently delivered ids (bounded), so a late change answers `already_delivered`.
    delivered_queued_ids: std::collections::VecDeque<String>,
    post_compaction_deferred_steering_messages: Vec<AgentMessage>,
    post_compaction_deferred_follow_up_messages: Vec<AgentMessage>,
    prompt_start_pending: bool,
    had_cleared_queued_messages: bool,
    /// senpi `_triggerTurnAdmissionAbortGeneration`: the user-abort generation an idle
    /// `triggerTurn` custom message is being admitted under.
    trigger_turn_admission_abort_generation: Option<u64>,
    /// senpi `_requiredCompactionTurnError`: the message of the `RequiredCompactionError` that
    /// ended the current run from the next-turn preparation.
    required_compaction_turn_error: Option<String>,
    /// senpi `_requiredCompactionAdmissionError`: rethrown by the originating prompt when queued
    /// input could not be admitted because required compaction failed.
    required_compaction_admission_error: Option<String>,
    auto_compaction_session_override: Option<bool>,
    turn_index: u64,
    message_replacements: Vec<(AgentMessage, AgentMessage)>,
    retry_attempt: u32,
    /// senpi `_modelConfigErrors`: one `model_config_error` warning per model and session (senpi#4282).
    model_config_errors: crate::retry_fallback::model_config_error::ModelConfigErrorReporter,
    retry_abort_controller: Option<maho_ai::utils::abort::AbortController>,
    user_aborted: bool,
    abort_source: Option<maho_ext_api::AbortSource>,
    abort_provenance: crate::agent_abort_provenance::AgentAbortProvenance,
    probe_phase: crate::retry_fallback::hint_policy::ProbePhase,
    hint_deadline_ms: Option<f64>,
    cumulative_hinted_wait_ms: f64,
    pending_model_switch: Option<PendingModelSwitch>,
    /// senpi `_compactionModelFallbackNotice`: the last `compaction_model_fallback` logged, so it logs once.
    compaction_model_fallback_notice: Option<String>,
    /// The retry controller's spent accounts, copied wherever it notes or clears one, so the synchronous
    /// compaction-model resolver reads them without contending for the controller's async lock.
    spent_accounts: crate::retry_fallback::unusable::SpentAccounts,
    compaction_abort_controller: Option<crate::compaction::lifecycle::CompactionAbortController>,
    /// senpi `_autoCompactionAbortController`: the active controller belongs to post-turn auto compaction.
    auto_compaction_owned: bool,
    compaction_signal_bridge: Option<CompactionSignalBridge>,
    pending_compaction_admission: Option<Arc<PendingCompactionAdmission>>,
    compaction_lifecycle: crate::compaction::lifecycle::CompactionLifecycleCoordinator,
    delegated_compaction_key: Option<(String, String)>,
    prompt_templates: Vec<crate::prompt_templates::PromptTemplate>,
    extension_commands: Vec<maho_ext_api::SlashCommandInfo>,
    extension_command_catalog:
        Option<Arc<dyn Fn() -> Vec<maho_ext_api::SlashCommandInfo> + Send + Sync>>,
    /// The bound runner for synchronous UI lookups (senpi's `session.extensionRunner` getter).
    extension_runner_snapshot: Option<ExtensionRunner>,
    extension_event_sender:
        Option<tokio::sync::mpsc::UnboundedSender<maho_ext_api::ExtensionEvent>>,
    extension_events: Option<maho_ext_api::EventBus>,
    /// Latest MCP wire-status snapshot published for this session (pinned
    /// `mcpService.getWireStatusSnapshot(threadId)`), type-erased so maho-core needs no dependency
    /// on the MCP crate.
    mcp_wire_status: Option<Arc<dyn std::any::Any + Send + Sync>>,
    extension_tool_context: Option<(
        maho_ext_api::ExtensionRuntime,
        maho_ext_host::wrapper::ToolContextFactory,
    )>,
    extension_tool_backups: BTreeMap<String, Option<(ToolDefinitionEntry, AgentTool)>>,
    extension_lazy_activators: Vec<LazyToolActivator>,
    native_tool_search_injection_failure_source:
        Option<maho_ext_api::NativeToolSearchInjectionFailureSource>,
    extension_hint_backups: BTreeMap<String, Option<String>>,
    skills: Vec<crate::skills::Skill>,
    skill_diagnostics: Vec<crate::diagnostics::ResourceDiagnostic>,
    prompt_diagnostics: Vec<crate::diagnostics::ResourceDiagnostic>,
    theme_diagnostics: Vec<crate::diagnostics::ResourceDiagnostic>,
    resource_loader_inputs: crate::resource_loader::ResourceLoaderInputs,
    skills_loaded: bool,
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
    active_compaction_log_attempt: Option<CompactionLogAttempt>,
    superseded_compaction_log_attempt_ids: BTreeSet<String>,
    resume_compaction_requirement: Option<(serde_json::Value, String)>,
    resume_slice: Option<ResumeSliceState>,
    /// The last repair of the session file, replayed to surfaces that attach before the next prompt (senpi#4295).
    standing_recovery: Option<AgentSessionEvent>,
    sizes_only_startup: bool,
    prompt_size_inventory: Option<Arc<dyn crate::prompt_size_inventory::PromptSizeInventory>>,
    disposed: bool,
    /// Memoized entry aggregate of `get_session_stats`, keyed on (session id, entry count, leaf id).
    session_stats_cache: Option<(
        SessionStatsCacheKey,
        SessionStats,
        crate::usage_totals::UsageTotals,
    )>,
    /// Memoized `get_context_usage`, keyed on every input that can change its result.
    context_usage_cache: Option<(ContextUsageCacheKey, Option<ContextUsage>)>,
}

type SessionStatsCacheKey = (String, usize, Option<String>);

/// Per-entry counters of `get_session_stats`, folded over entries in append order.
#[derive(Clone, Copy, Debug, Default)]
struct SessionStatsCounts {
    user_messages: usize,
    assistant_messages: usize,
    tool_results: usize,
    total_messages: usize,
    tool_calls: usize,
}

impl SessionStatsCounts {
    fn from_stats(stats: &SessionStats) -> Self {
        Self {
            user_messages: stats.user_messages,
            assistant_messages: stats.assistant_messages,
            tool_results: stats.tool_results,
            total_messages: stats.total_messages,
            tool_calls: stats.tool_calls,
        }
    }

    fn fold(&mut self, entries: &[Value], totals: &mut crate::usage_totals::UsageTotals) {
        for entry in entries {
            self.fold_entry(entry, totals);
        }
    }

    fn fold_entry(&mut self, entry: &Value, totals: &mut crate::usage_totals::UsageTotals) {
        {
            let kind = entry
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if (kind == "branch_summary" || kind == "compaction")
                && let Some(usage) = entry
                    .get("usage")
                    .and_then(|usage| serde_json::from_value(usage.clone()).ok())
            {
                crate::usage_totals::add_usage_to_totals(totals, &usage);
            }
            if kind != "message" {
                return;
            }
            self.total_messages += 1;
            let Some(message) = entry.get("message") else {
                return;
            };
            match message.get("role").and_then(Value::as_str) {
                Some("user") => self.user_messages += 1,
                Some("toolResult") => {
                    self.tool_results += 1;
                    if let Some(usage) = message
                        .get("usage")
                        .and_then(|usage| serde_json::from_value(usage.clone()).ok())
                    {
                        crate::usage_totals::add_usage_to_totals(totals, &usage);
                    }
                }
                Some("assistant") => {
                    self.assistant_messages += 1;
                    if let Some(content) = message.get("content").and_then(Value::as_array) {
                        self.tool_calls += content
                            .iter()
                            .filter(|block| {
                                block.get("type").and_then(Value::as_str) == Some("toolCall")
                            })
                            .count();
                    }
                    if let Some(usage) = message
                        .get("usage")
                        .and_then(|usage| serde_json::from_value(usage.clone()).ok())
                    {
                        crate::usage_totals::add_usage_to_totals(totals, &usage);
                    }
                }
                _ => {}
            }
        }
    }
}

fn message_ticket(message: &AgentMessage) -> Option<String> {
    match message {
        AgentMessage::Custom(maho_agent::types::CustomAgentMessage::Custom(custom)) => {
            custom.details.as_ref().and_then(|d| {
                d.as_object()
                    .and_then(|obj| obj.get("ticketId"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
        }
        _ => None,
    }
}

/// Changes whenever a message's content grows or its usage is filled in, without walking other messages.
pub fn message_fingerprint(message: Option<&AgentMessage>) -> String {
    let Some(message) = message else {
        return String::new();
    };
    let mut size = 0usize;
    match message {
        AgentMessage::Llm(llm) => match llm {
            maho_ai::types::Message::User(u) => match &u.content {
                maho_ai::types::UserContent::Text(text) => size = text.len(),
                maho_ai::types::UserContent::Blocks(blocks) => {
                    for block in blocks {
                        size += 1;
                        match block {
                            maho_ai::types::ContentBlock::Text(t) => size += t.text.len(),
                            maho_ai::types::ContentBlock::Thinking(t) => size += t.thinking.len(),
                            _ => {}
                        }
                    }
                }
            },
            maho_ai::types::Message::Assistant(a) => {
                for block in &a.content {
                    size += 1;
                    match block {
                        maho_ai::types::ContentBlock::Text(t) => size += t.text.len(),
                        maho_ai::types::ContentBlock::Thinking(t) => size += t.thinking.len(),
                        _ => {}
                    }
                }
            }
            _ => {}
        },
        AgentMessage::Custom(c) => {
            if let maho_agent::types::CustomAgentMessage::Custom(custom) = c {
                size = match &custom.content {
                    maho_agent::harness::messages::CustomMessageContent::Text(text) => text.len(),
                    maho_agent::harness::messages::CustomMessageContent::Blocks(blocks) => {
                        blocks.len()
                    }
                };
            }
        }
    }
    let usage = match message {
        AgentMessage::Llm(maho_ai::types::Message::Assistant(a)) => Some(&a.usage),
        _ => None,
    };
    match usage {
        Some(usage) => {
            let (req_input, req_output, req_is_null) = match usage.context_usage {
                Some(Some(ctx)) => (ctx.input.to_string(), ctx.output.to_string(), "false"),
                Some(None) => ("undefined".to_string(), "undefined".to_string(), "true"),
                None => ("undefined".to_string(), "undefined".to_string(), "false"),
            };
            format!(
                "{}:{}:{}:{}:{}:{}:{}",
                size,
                usage.input,
                usage.output,
                usage.total_tokens,
                req_input,
                req_output,
                req_is_null
            )
        }
        None => format!("{size}::::undefined:undefined:false"),
    }
}

#[derive(PartialEq)]
struct ContextUsageCacheKey {
    session_id: String,
    message_revision: u64,
    entry_count: usize,
    leaf_id: Option<String>,
    awaiting_persistence: usize,
    provider: String,
    model_id: String,
    context_window: u64,
    last_fingerprint: String,
}

static RATE_LIMIT_PATTERN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"(?i)rate.?limit|(?:^429\s+\{|(?:\bHTTP/1\.[01]\s+|\bHTTP\s+|\bstatus(?:\s+code)?\s+|\berror\s+|\bcode\s+)429\b)|too many requests|resource.?exhausted")
        .expect("pinned rate limit pattern")
});

static MODEL_FAMILY_PATTERNS: std::sync::LazyLock<Vec<(&'static str, regex::Regex)>> =
    std::sync::LazyLock::new(|| {
        ["claude", "gpt-5", "o1", "o3", "o4", "gemini", "deepseek"]
            .into_iter()
            .map(|marker| {
                (
                    marker,
                    regex::Regex::new(&format!(
                        "(?:^|[/.:_-]){}(?:$|[^a-z0-9])",
                        regex::escape(marker)
                    ))
                    .expect("escaped family marker"),
                )
            })
            .collect()
    });

#[derive(Clone, Debug)]
pub struct ResumeSliceState {
    pub tokens_before: u64,
    pub tokens_after: u64,
    pub dropped_entries: usize,
    pub notice: String,
}

#[derive(Clone, Debug)]
struct CompactionLogAttempt {
    id: String,
    reason: maho_ext_api::CompactionReason,
    tokens_before: Option<u64>,
}

fn name_set(names: Option<Vec<String>>) -> Option<BTreeSet<String>> {
    names.map(|names| names.into_iter().collect())
}

/// The `list_sessions` row of a session's tool child (senpi `SessionToolChildRow`): `{state: "none" | "live" | "exited",
/// generation?, pid?}`; absent fields are omitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ToolChildRow {
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<i64>,
}

#[derive(Clone)]
pub struct AgentSession {
    inner: Arc<AgentSessionInner>,
}

/// Settles the session start when dropped, including on cancellation (senpi's `finally`).
struct SessionStartSettlement(tokio::sync::watch::Sender<bool>);
impl Drop for SessionStartSettlement {
    fn drop(&mut self) {
        self.0.send_replace(true);
    }
}

/// One parent tool call's nested host-call recorder and the counter behind its `<parent>/<n>` ids.
type NestedHostCallSlot = (
    Arc<Mutex<crate::session_record::NestedCallRecorder>>,
    Arc<AtomicU64>,
);

#[doc(hidden)]
pub struct AgentSessionInner {
    agent: Agent,
    session_manager: Mutex<SessionManager>,
    lifecycle: Mutex<crate::lifecycle_ledger::LifecycleLedger>,
    external_admission: crate::external_admission::ExternalAdmission,
    held_inputs: Mutex<crate::held_inputs::HeldInputs>,
    /// senpi `_pendingInputs`: identified queued input in durable custody (senpi#3549). Set once the session exists.
    pending_inputs: std::sync::OnceLock<Arc<crate::pending_input_session::SessionPendingInputs>>,
    delivered_held_inputs: Mutex<Vec<(String, u64)>>,
    held_preparation: tokio::sync::Mutex<()>,
    reserved_held_inputs: Mutex<BTreeSet<String>>,
    observed_held_inputs: Mutex<BTreeSet<String>>,
    stop_interrupt: Mutex<Option<Value>>,
    lifecycle_human_pending: std::sync::atomic::AtomicBool,
    /// Mirrors `state.disposed` for the lifecycle write gate: lifecycle records are written while the
    /// caller may hold the state lock (the AgentStart arm), so the gate must not lock state again.
    lifecycle_disposed: std::sync::atomic::AtomicBool,
    /// senpi agent_end `failure`: the structured provider failure of the last agent_end, naming the selected
    /// model when the failing request ran on another one (senpi#4328). `None` after a success or an abort.
    last_provider_failure: Mutex<Option<crate::provider_failure::ProviderFailure>>,
    lifecycle_goal: Mutex<Option<Value>>,
    lifecycle_turn_facts: Mutex<Option<Value>>,
    /// senpi `RunStopFacts` of the run whose agent_end was bound, and the run restored from disk, which keeps exactly
    /// the stop its record carries (senpi#3864).
    lifecycle_stop_facts: Mutex<(Option<crate::lifecycle_stop::RunStopFacts>, Option<String>)>,
    /// Live run activity projected on the lifecycle (senpi#3864): wire-only, never written.
    lifecycle_activity: Mutex<crate::session_lifecycle::LifecycleActivity>,
    /// senpi `_recoveryIdentity`: present only under compact updates; mints each message's recovery carrier.
    recovery_identity: Mutex<Option<crate::session_recovery_identity::SessionRecoveryIdentity>>,
    /// senpi `_structuredLane` (senpi#4272).
    structured_lane: crate::structured_session_lane::StructuredSessionLane,
    /// senpi `_unregisterMemoryReport`: a no-op unless `SENPI_MEMORY_REPORT=1`; released on dispose.
    memory_report_unregister: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    settings_manager: Arc<Mutex<SettingsManager>>,
    model_registry: ModelRegistry,
    state: Mutex<AgentSessionState>,
    extension_runner: tokio::sync::Mutex<Option<ExtensionRunner>>,
    boundary_context_changed: AtomicBool,
    /// Bumped each time a runner is installed, so a renderer snapshot keyed on it (with the
    /// runtime's tool generation) is rebuilt only when the bound runner or its tools change.
    extension_runner_epoch: AtomicU64,
    /// senpi `_handlerTurnScope`: bumped per admitted prompt turn (`_promptAgent`), so the turn deadline
    /// releases only the held extension handler waits its own turn started (senpi#3650).
    handler_turn_scope: Arc<AtomicU64>,
    /// senpi `_deadlineTracker`: the turn's inactivity lease and max-runtime cap (senpi#3227), armed at
    /// prompt admission and disarmed when the run settles idle. Set once, right after construction.
    deadline_tracker: std::sync::OnceLock<crate::turn_deadline::AgentDeadlineTracker>,
    /// senpi `SessionLifecycleController.deadlineStop`: the deadline that stopped the current run,
    /// captured before its durable write. Lock order: `lifecycle`, then this.
    lifecycle_deadline_stop: Mutex<Option<crate::lifecycle_stop::DeadlineStopCapture>>,
    /// The host's project-trust inputs for a switch into another project (senpi `createServices` re-run
    /// for the target cwd with `projectTrustByCwd`, `--approve`, and `projectTrustContextFactory`).
    switch_project_trust: Mutex<Option<SwitchProjectTrust>>,
    event_bus: EventBus,
    event_listeners: SharedEventListeners,
    next_listener_id: Arc<AtomicU64>,
    human_input_listeners: Arc<Mutex<Vec<(u64, HumanInputListener)>>>,
    session_logger: SessionLogger,
    fallback_now: Arc<dyn Fn() -> f64 + Send + Sync>,
    retry_random: Arc<dyn Fn() -> f64 + Send + Sync>,
    fallback_validation_warnings: Vec<String>,
    agent_subscription: Mutex<Option<maho_agent::agent::AgentSubscription>>,
    prompt_admission: tokio::sync::Mutex<()>,
    nested_tool_queue: tokio::sync::Mutex<()>,
    reload_admission: tokio::sync::Mutex<()>,
    binding_readiness: Mutex<Option<BindingPromptReadiness>>,
    retry_fallback: tokio::sync::Mutex<
        Option<crate::retry_fallback::controller::RetryFallbackController<SessionFallbackDeps>>,
    >,
    work_barrier: Arc<crate::session_work_barrier::SessionWorkBarrier>,
    relocation_host: Mutex<Option<Arc<dyn crate::session_relocation::SessionRelocationHost>>>,
    /// Installed by the interactive mode before it binds extensions; `pi.session.registerControlEndpoint` needs one.
    control_endpoint_host:
        Mutex<Option<Arc<dyn crate::session_control_actions::ControlEndpointHost>>>,
    /// The current extension generation's `pi.session`: it owns the control endpoints that generation registers,
    /// and a reload retires it (senpi#3246).
    session_control:
        Mutex<Option<Arc<crate::session_control_actions::GenerationSessionControlActions>>>,
    tool_barrier: crate::session_work_barrier::ToolBarrier,
    session_thinking_memory: Mutex<crate::session_thinking_memory::SessionThinkingMemory>,
    settling_with_background_work: std::sync::atomic::AtomicBool,
    /// senpi `_isAgentRunActive`: set right before the agent run starts, cleared when it settles.
    agent_run_active: std::sync::atomic::AtomicBool,
    /// senpi `_runtimeMessagesTokenized`: the idle release tokenized the agent's runtime messages
    /// in place; the next reader or run re-materializes them through `runtime_messages()`.
    runtime_messages_tokenized: std::sync::atomic::AtomicBool,
    /// Bumped by every child park that clears the runtime messages. The idle release captures it
    /// before reading the transcript and commits its tokenized copy under this lock only when no
    /// park ran in between, so a parked child never gets its messages back.
    idle_park_generation: Mutex<u64>,
    wake_source_subscription: Mutex<Option<maho_ext_api::BusSubscription>>,
    lifecycle_fact_subscriptions: Mutex<Vec<maho_ext_api::BusSubscription>>,
    /// Forwards the bound runner's `senpi:extension-rpc-event` channel onto the session bus, so a
    /// connection subscribed once keeps receiving across runner recreation (senpi `onRpcEvent`).
    extension_rpc_event_forward: Mutex<Option<maho_ext_api::BusSubscription>>,
    settings_source_subscription:
        Mutex<Option<crate::settings_manager::SettingsSourceSubscription>>,
    /// senpi `runtimeHost.setBeforeSessionInvalidate`: runs once a replacement or reload proceeds,
    /// right before the extension runtime is invalidated and the next `session_start` re-registers UI.
    before_session_invalidate: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    /// senpi `onBeforeSessionReplace` for a session replaced in place (new, switch/import, fork): runs after the
    /// outgoing `session_shutdown` handlers and before its extension runtime is invalidated.
    before_session_replace: Mutex<Vec<(u64, BeforeSessionReplaceListener)>>,
    next_before_session_replace: AtomicU64,
    settled_delivery: Mutex<crate::agent_settled_delivery::AgentSettledDelivery>,
    user_abort_generation: AtomicU64,
    monitor_generation: AtomicU64,
    settlement_epoch: AtomicU64,
    probe_scheduler: Mutex<crate::retry_fallback::probe_scheduler::ProbeBackScheduler>,
    /// senpi `_requestShapeRecovery`: one same-model recovery per request-shape failing streak.
    request_shape_recovery: Mutex<crate::retry_fallback::request_shape::RequestShapeRecovery>,
    /// senpi `_engineTurnHold`: the durable hold ledger. Lock order: hold, then `engine_turn_input`.
    engine_turn_hold: Arc<Mutex<crate::engine_turn_hold::EngineTurnHold>>,
    engine_turn_release: crate::engine_turn_release::EngineTurnRelease,
    engine_turn_input: Mutex<crate::engine_turn_input::EngineTurnInput>,
    /// senpi `_engineTurnStarts`: this process's own record of engine turns since the user's last message.
    engine_turn_starts: Mutex<Vec<crate::engine_turn_limit::EngineTurnStartRecord>>,
    engine_resume_window: std::sync::atomic::AtomicBool,
    engine_turn_pause_recorded: std::sync::atomic::AtomicBool,
    engine_turn_pause_notified: std::sync::atomic::AtomicBool,
    prompts_in_flight: std::sync::atomic::AtomicUsize,
    /// senpi `_userInputSources`: where each user message came from, keyed by message identity.
    user_input_sources: Mutex<Vec<(AgentMessage, InputSource)>>,
    user_activity_listeners: Mutex<Vec<(u64, UserActivityListener)>>,
    /// senpi `_promptCachePrefixBuilds`: in-flight prefix builds a real turn cancels (senpi#2115).
    prompt_cache_prefix_builds: crate::prompt_cache_prefix_request::PromptCachePrefixBuilds,
    /// senpi `_sessionStartSettled`: `true` once the session start the prefix depends on has run.
    session_start_settled: tokio::sync::watch::Sender<bool>,
    probe_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// senpi `_userAbortPromise`: the in-flight user abort every concurrent abort joins.
    user_abort_wait:
        Mutex<Option<futures::future::Shared<futures::future::BoxFuture<'static, ()>>>>,
    /// Serializes MCP wire-status publish against bind so a subscriber never misses or
    /// double-receives an update (pinned subscribe-before-read ordering).
    mcp_wire_status_order: tokio::sync::Mutex<()>,
    /// The session's own retention subscription on the runner bus. It is installed at runner bind
    /// (before extension attach settles), so an extension that publishes the MCP wire-status
    /// snapshot on the shared bus reaches the session's retained snapshot without holding the
    /// `AgentSession`.
    mcp_wire_status_retention: Mutex<Option<maho_ext_api::BusSubscription>>,
    /// Relays the bound runner's `senpi:extension-rpc-event` channel to the session's listeners.
    extension_rpc_event_forwarding: Mutex<Option<maho_ext_api::BusSubscription>>,
    /// Held outside `state` so a relay never contends with a caller holding the state lock.
    extension_rpc_event_listeners: Mutex<Vec<ExtensionRpcEventListener>>,
    tool_context_disposers: Mutex<Vec<Box<dyn FnOnce() + Send>>>,
    /// senpi `_toolDispatcher`: where tool calls and `ctx.offload` modules are placed for this session's tool runtime.
    tool_dispatcher: Arc<dyn crate::tool_dispatch::tool_dispatcher::ToolDispatcher>,
    /// senpi `_toolChild`: the session's tool child; present only under `--tool-runtime child`.
    tool_child: Option<Arc<maho_tool_child::session_child::SessionToolChild>>,
    /// senpi `_toolChildHostTargets`: the `completion` / `reserved` host-call targets registered on the session.
    tool_child_host_targets: ToolChildHostTargets,
    /// senpi `_toolChildClosed`: settles once `dispose` has stopped the tool child and closed in-process offload jobs.
    tool_child_closed:
        Mutex<Option<futures::future::Shared<futures::future::BoxFuture<'static, ()>>>>,
    /// The placement fields senpi reads off each `ToolDefinition` (`placement`, `serverPinned`, `childModule`,
    /// `childJob`); maho's `ToolDefinition` carries none, so the registrar states them here by tool name.
    tool_placements:
        Mutex<BTreeMap<String, crate::tool_dispatch::tool_dispatcher::ToolPlacementInfo>>,
    /// Per parent tool call: the recorder and the counter that give each nested host call its `<parent>/<n>` id.
    nested_host_calls: Mutex<BTreeMap<String, NestedHostCallSlot>>,
    html_theme_resolver: Mutex<Option<HtmlThemeResolver>>,
    /// senpi `getToolDefinition` over `_baseToolDefinitions`: the built-in tool definitions carry
    /// renderers (`grep`, `find`) that `/export` uses when no extension registered the tool.
    html_built_in_tool_renderers:
        Mutex<Option<crate::export_html::tool_renderer::ToolRendererLookup>>,
    /// The runtime the session was created on. Extension actions may be invoked from a foreign
    /// thread or a short-lived runtime (a completion notifier's drain); work they start must run on
    /// the session's runtime, or it dies with the caller's runtime after the action returned Ok.
    runtime_handle: Option<tokio::runtime::Handle>,
    /// Receipts for custom messages that rode a queue instead of appending immediately.
    transcript_write_failures: Mutex<crate::transcript_write_failures::TranscriptWriteFailures>,
    work_obligations: Mutex<Option<crate::work_obligations::SessionWorkObligations>>,
    custody_run_id: Mutex<String>,
    queued_custom_receipts: Mutex<
        Vec<(
            maho_agent::harness::messages::CustomMessage,
            maho_ext_api::MessageDeliveryReceipt,
        )>,
    >,
    /// Session-file repairs reported inside a session-manager append; emitted only after its lock is released.
    pending_session_recoveries: Mutex<Vec<crate::session_write_recovery::SessionRecovery>>,
}

pub type HtmlThemeResolver = Arc<
    dyn Fn(
            Option<&str>,
        ) -> Result<
            (
                maho_ext_api::Theme,
                Option<(BTreeMap<String, String>, BTreeMap<String, String>)>,
            ),
            String,
        > + Send
        + Sync,
>;

struct SessionFallbackDeps(
    std::sync::Weak<AgentSessionInner>,
    crate::retry_fallback::log::FallbackLogger,
);

struct SessionExtensionActions(std::sync::Weak<AgentSessionInner>);

/// senpi `SessionLifecycleController.lastStop`: derived only from the current run's own bound end; an end processed
/// after a later run started sets nothing, and an unchanged stop keeps its first `at`. A run restored from disk keeps
/// exactly the stop its record carries. The run's own turn deadline, captured when it fired, explains its stop
/// first (senpi `deadlineStop`, only with a turn-end input and only for the run it stopped).
fn lifecycle_last_stop(
    ledger: &crate::lifecycle_ledger::LifecycleLedger,
    restored_run_id: Option<&str>,
    facts: Option<&crate::lifecycle_stop::RunStopFacts>,
    deadline: Option<&crate::lifecycle_stop::DeadlineStopCapture>,
    input: Option<&Value>,
    stalled: bool,
) -> Option<Value> {
    let existing_value = ledger.current.get("lastStop").cloned();
    let run_id = ledger.run_id.as_deref()?;
    if Some(run_id) == restored_run_id {
        return existing_value;
    }
    let Some(facts) = facts.filter(|facts| facts.run_id.as_deref() == Some(run_id)) else {
        return existing_value;
    };
    let deadline = deadline.filter(|deadline| input.is_some() && deadline.run_id == run_id);
    let Some(detail) = crate::lifecycle_stop::stop_detail(input, Some(facts), deadline, stalled)
    else {
        return existing_value;
    };
    let existing = existing_value.as_ref().and_then(|stop| {
        serde_json::from_value::<crate::session_lifecycle_types::LifecycleStop>(stop.clone()).ok()
    });
    if existing
        .as_ref()
        .is_some_and(|existing| crate::lifecycle_stop::same_stop(existing, &detail))
    {
        return existing_value;
    }
    let stop = crate::session_lifecycle_types::LifecycleStop {
        cause: detail.cause,
        run_id: run_id.to_owned(),
        at: detail.at.unwrap_or_else(|| {
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        }),
        epoch: ledger.current["epoch"].as_u64().unwrap_or(0),
        deadline: detail.deadline,
        guard: detail.guard,
        todo: detail.todo,
        abort_source: detail.abort_source,
        persisted: detail.persisted,
    };
    serde_json::to_value(stop).ok()
}

/// senpi `_bindExtensionCore` provider callbacks: registration updates the catalog, then the live
/// session model is replaced so a reload does not keep the pre-reload context window.
struct SessionProviderActions {
    runtime: crate::agent_session_runtime::ExtensionModelRuntimeActions,
    session: std::sync::Weak<AgentSessionInner>,
}
impl SessionProviderActions {
    fn refresh(&self) {
        if let Some(inner) = self.session.upgrade() {
            AgentSession { inner }.refresh_current_model_from_registry();
        }
    }
}
impl maho_ext_api::ExtensionProviderActions for SessionProviderActions {
    fn register_provider(
        &self,
        registration: maho_ext_api::ProviderRegistration,
        path: &str,
    ) -> Result<(), maho_ext_api::ExtensionFailure> {
        maho_ext_api::ExtensionProviderActions::register_provider(
            &self.runtime,
            registration,
            path,
        )?;
        self.refresh();
        Ok(())
    }
    fn unregister_provider(
        &self,
        name: &str,
        path: &str,
    ) -> Result<(), maho_ext_api::ExtensionFailure> {
        maho_ext_api::ExtensionProviderActions::unregister_provider(&self.runtime, name, path)?;
        self.refresh();
        Ok(())
    }
}

/// Restores the ctx idle gate's settling flag on every exit path, matching senpi's `finally` in
/// `_emitAgentSettled` (`agent-session.ts:1905`) so a dropped or cancelled settlement future
/// cannot leave the session permanently reported as settling.
struct SettlingBackgroundWorkGuard<'a>(&'a std::sync::atomic::AtomicBool);
impl Drop for SettlingBackgroundWorkGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

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
        Self {
            registry: session.model_registry().clone(),
            agent_dir: session.agent_dir().to_owned(),
            events,
            session: Arc::downgrade(&session.inner),
            generation: session.monitor_generation.load(Ordering::SeqCst),
        }
    }
    fn assert_account_active(&self) -> Result<(), maho_ext_api::ExtensionFailure> {
        let inner = self.session.upgrade().ok_or_else(|| {
            maho_ext_api::ExtensionFailure::new("Agent session has been disposed")
        })?;
        if lock(&inner.state).disposed
            || inner.monitor_generation.load(Ordering::SeqCst) != self.generation
        {
            return Err(maho_ext_api::ExtensionFailure::new(
                "Account registry context is stale",
            ));
        }
        Ok(())
    }
    fn accounts_changed(&self, provider: &str) {
        self.events.emit(
            "provider-accounts-changed",
            &serde_json::json!({"type":"accounts_changed","provider":provider}),
        );
        crate::provider_account_events::emit_provider_accounts_changed(provider);
    }
}
impl maho_ext_api::ModelRegistry for ExtensionModelRegistryView {
    fn get_all(&self) -> Vec<Model> {
        self.registry.get_all()
    }
    fn get_available(&self) -> Vec<Model> {
        self.registry.get_available()
    }
    fn find(&self, provider: &str, id: &str) -> Option<Model> {
        self.registry.find(provider, id)
    }
    fn upstream_model_id(&self, model: &Model) -> String {
        self.registry
            .get_upstream_model_id(model)
            .unwrap_or_else(|| model.id.clone())
    }
    fn catalog_service_tier(&self, model: &Model) -> Option<maho_ext_api::ServiceTier> {
        self.registry
            .get_service_tier(model)
            .map(|tier| match tier {
                maho_ai::types::ServiceTierPreference::Auto => maho_ext_api::ServiceTier::Auto,
                maho_ai::types::ServiceTierPreference::Flex => maho_ext_api::ServiceTier::Flex,
                maho_ai::types::ServiceTierPreference::Priority => {
                    maho_ext_api::ServiceTier::Priority
                }
                maho_ai::types::ServiceTierPreference::Ultrafast => {
                    maho_ext_api::ServiceTier::Ultrafast
                }
            })
    }
    fn prompt_preset(&self, model: &Model) -> Option<String> {
        self.registry.get_prompt_preset(model)
    }
    fn has_configured_auth(&self, model: &Model) -> bool {
        self.registry.has_configured_auth(model)
    }
    fn get_api_key_for_provider<'a>(
        &'a self,
        provider: &'a str,
    ) -> maho_ext_api::ExtensionFuture<'a, Option<String>> {
        Box::pin(async move { Ok(self.registry.get_api_key_for_provider(provider).await) })
    }
    fn get_provider_auth<'a>(
        &'a self,
        provider: &'a str,
    ) -> maho_ext_api::ExtensionFuture<'a, Option<maho_ai::models::AuthResolution>> {
        self.get_provider_auth_for_slot(provider, None)
    }
    fn get_provider_auth_for_slot<'a>(
        &'a self,
        provider: &'a str,
        slot_name: Option<&'a str>,
    ) -> maho_ext_api::ExtensionFuture<'a, Option<maho_ai::models::AuthResolution>> {
        Box::pin(async move {
            self.registry
                .get_provider_auth_for_slot(provider, slot_name)
                .await
                .map_err(|error| {
                    maho_ext_api::ExtensionFailure::classified(
                        error.message,
                        Some("ModelsError".into()),
                        Some(error.code.as_str().to_owned()),
                    )
                })
        })
    }
    fn get_provider_auth_status(&self, provider: &str) -> maho_ext_api::ProviderAuthStatus {
        let status = self.registry.get_provider_auth_status(provider);
        maho_ext_api::ProviderAuthStatus {
            ambient: status.ambient,
            configured: status.configured,
            source: status.source,
            label: status.label,
        }
    }
    fn get_stored_credential_type(
        &self,
        provider: &str,
    ) -> Result<Option<maho_ai::auth::types::CredentialType>, maho_ext_api::ExtensionFailure> {
        Ok(self
            .registry
            .auth_storage
            .get(provider)
            .as_ref()
            .and_then(crate::auth_storage::credential_kind)
            .map(|kind| match kind {
                crate::auth_storage::CredentialKind::ApiKey => {
                    maho_ai::auth::types::CredentialType::ApiKey
                }
                crate::auth_storage::CredentialKind::Oauth => {
                    maho_ai::auth::types::CredentialType::OAuth
                }
            }))
    }
    fn stream_simple(
        &self,
        model: &Model,
        context: &maho_ai::types::Context,
        options: Option<maho_ai::types::SimpleStreamOptions>,
    ) -> Result<
        maho_ai::utils::event_stream::AssistantMessageEventStream,
        maho_ext_api::ExtensionFailure,
    > {
        Ok(self.registry.stream_simple(model, context, options))
    }
    fn stream(
        &self,
        model: &Model,
        context: &maho_ai::types::Context,
        options: Option<maho_ai::types::StreamOptions>,
    ) -> Result<
        maho_ai::utils::event_stream::AssistantMessageEventStream,
        maho_ext_api::ExtensionFailure,
    > {
        Ok(self.registry.stream(model, context, options))
    }
    fn get_api_key_and_headers<'a>(
        &'a self,
        model: &'a Model,
    ) -> maho_ext_api::ExtensionFuture<'a, maho_ext_api::ResolvedRequestAuth> {
        self.get_api_key_and_headers_for_slot(model, None)
    }
    fn get_api_key_and_headers_for_slot<'a>(
        &'a self,
        model: &'a Model,
        slot_name: Option<&'a str>,
    ) -> maho_ext_api::ExtensionFuture<'a, maho_ext_api::ResolvedRequestAuth> {
        Box::pin(async move {
            match self
                .registry
                .get_api_key_and_headers_for_slot(model, slot_name)
                .await
            {
                crate::model_registry::ResolvedRequestAuth::Resolved {
                    ambient,
                    auth,
                    compatibility,
                    env,
                } => Ok(maho_ext_api::ResolvedRequestAuth {
                    ambient,
                    auth,
                    extra_body: compatibility.extra_body,
                    upstream_model_id: compatibility.upstream_model_id,
                    service_tier: compatibility.service_tier,
                    env,
                }),
                crate::model_registry::ResolvedRequestAuth::Failed { error, class, code } => Err(
                    maho_ext_api::ExtensionFailure::classified(error, class, code),
                ),
            }
        })
    }
    fn get_credential_accounts<'a>(
        &'a self,
        provider: &'a str,
    ) -> maho_ext_api::ExtensionFuture<'a, Vec<maho_ext_api::CredentialAccountSummary>> {
        Box::pin(async move {
            self.assert_account_active()?;
            let accounts = self
                .registry
                .get_credential_accounts(provider, &self.agent_dir)
                .await
                .map_err(maho_ext_api::ExtensionFailure::new)?;
            self.assert_account_active()?;
            Ok(accounts)
        })
    }
    fn pin_credential_account<'a>(
        &'a self,
        provider: &'a str,
        name: Option<&'a str>,
    ) -> maho_ext_api::ExtensionFuture<'a, ()> {
        Box::pin(async move {
            self.assert_account_active()?;
            self.registry
                .pin_credential_account_guarded(provider, name, &self.agent_dir, &|| {
                    self.assert_account_active().map_err(|error| error.message)
                })
                .await
                .map_err(maho_ext_api::ExtensionFailure::new)?;
            self.assert_account_active()?;
            self.accounts_changed(provider);
            Ok(())
        })
    }
    fn reset_credential_account<'a>(
        &'a self,
        provider: &'a str,
        name: &'a str,
    ) -> maho_ext_api::ExtensionFuture<'a, ()> {
        Box::pin(async move {
            self.assert_account_active()?;
            self.registry
                .reset_credential_account_guarded(provider, name, &self.agent_dir, &|| {
                    self.assert_account_active().map_err(|error| error.message)
                })
                .await
                .map_err(maho_ext_api::ExtensionFailure::new)?;
            self.assert_account_active()?;
            self.accounts_changed(provider);
            Ok(())
        })
    }
    fn remove_credential_account<'a>(
        &'a self,
        provider: &'a str,
        name: &'a str,
    ) -> maho_ext_api::ExtensionFuture<'a, ()> {
        Box::pin(async move {
            self.assert_account_active()?;
            self.registry
                .remove_credential_account_guarded(provider, name, &self.agent_dir, &|| {
                    self.assert_account_active().map_err(|error| error.message)
                })
                .await
                .map_err(maho_ext_api::ExtensionFailure::new)?;
            self.assert_account_active()?;
            self.accounts_changed(provider);
            Ok(())
        })
    }
    fn rename_credential_account<'a>(
        &'a self,
        provider: &'a str,
        name: &'a str,
        display_name: Option<&'a str>,
    ) -> maho_ext_api::ExtensionFuture<'a, ()> {
        Box::pin(async move {
            self.assert_account_active()?;
            self.registry
                .rename_credential_account_guarded(provider, name, display_name, &|| {
                    self.assert_account_active().map_err(|error| error.message)
                })
                .await
                .map_err(maho_ext_api::ExtensionFailure::new)?;
            self.assert_account_active()?;
            self.accounts_changed(provider);
            Ok(())
        })
    }
    fn refresh<'a>(
        &'a self,
        options: maho_ai::models::ModelsRefreshOptions,
    ) -> maho_ext_api::ExtensionFuture<'a, maho_ai::models::ModelsRefreshResult> {
        // The core `ModelRegistry` shares its `ModelRuntime` state (config/extensions are `Arc`,
        // `models` is `Arc`-backed), so refreshing an owned clone updates the session's live catalog
        // exactly like pinned `modelRuntime.refresh(options)` mutating the single runtime.
        let mut registry = self.registry.clone();
        Box::pin(async move { Ok(registry.refresh(options).await) })
    }
}
impl maho_ext_api::ToolSessionManager for ExtensionSessionManagerView {
    fn session_id(&self) -> &str {
        &self.id
    }
    fn session_file(&self) -> Option<&std::path::Path> {
        self.file.as_deref()
    }
}
impl maho_ext_api::SessionManager for ExtensionSessionManagerView {
    fn get_usage_totals(&self) -> Option<Value> {
        self.session.upgrade().map(|inner| {
            AgentSession { inner }.with_session_manager(|manager| manager.extension_usage_totals())
        })
    }
    fn get_session_dir(&self) -> Option<std::path::PathBuf> {
        self.session.upgrade().map(|inner| {
            AgentSession { inner }.with_session_manager(|manager| manager.session_dir().into())
        })
    }
    fn get_entries(&self) -> Vec<maho_ext_api::SessionEntry> {
        self.session.upgrade().map_or_else(Vec::new, |inner| {
            AgentSession { inner }.with_session_manager(|manager| {
                manager
                    .entries()
                    .into_iter()
                    .map(session_entry_from_value)
                    .collect()
            })
        })
    }
    fn get_entries_of_kinds(&self, kinds: &[&str]) -> Vec<maho_ext_api::SessionEntry> {
        self.session.upgrade().map_or_else(Vec::new, |inner| {
            AgentSession { inner }.with_session_manager(|manager| {
                let mut entries = Vec::new();
                manager.for_each_entry(
                    &|entry| {
                        kinds.contains(
                            &entry
                                .get("type")
                                .and_then(Value::as_str)
                                .unwrap_or_default(),
                        )
                    },
                    &mut |entry| entries.push(session_entry_from_value(entry)),
                );
                entries
            })
        })
    }
    fn build_context_messages(&self) -> Option<Vec<Value>> {
        self.session.upgrade().map(|inner| {
            AgentSession { inner }
                .with_session_manager(|manager| manager.current_context().messages)
        })
    }
    fn for_each_entry(&self, visit: &mut dyn FnMut(maho_ext_api::SessionEntry)) {
        if let Some(inner) = self.session.upgrade() {
            AgentSession { inner }.with_session_manager(|manager| {
                manager.for_each_entry(&|_| true, &mut |entry| {
                    visit(session_entry_from_value(entry))
                });
            });
        }
    }
    fn get_branch(&self) -> Vec<maho_ext_api::SessionEntry> {
        self.session.upgrade().map_or_else(Vec::new, |inner| {
            AgentSession { inner }.with_session_manager(|manager| {
                manager
                    .current_branch()
                    .into_iter()
                    .map(session_entry_from_value)
                    .collect()
            })
        })
    }
    fn get_branch_tail(
        &self,
        min_entries: usize,
        satisfied: &mut dyn FnMut(&Value) -> bool,
    ) -> (usize, Vec<maho_ext_api::SessionEntry>) {
        self.session.upgrade().map_or_else(
            || (0, Vec::new()),
            |inner| {
                AgentSession { inner }.with_session_manager(|manager| {
                    let (len, tail) = manager.current_branch_tail(min_entries, satisfied);
                    (
                        len,
                        tail.into_iter().map(session_entry_from_value).collect(),
                    )
                })
            },
        )
    }
    fn get_leaf_id(&self) -> Option<String> {
        self.session.upgrade().and_then(|inner| {
            AgentSession { inner }
                .with_session_manager(|manager| manager.leaf_id().map(str::to_owned))
        })
    }
    fn get_session_name(&self) -> Option<String> {
        self.session
            .upgrade()
            .and_then(|inner| AgentSession { inner }.session_name())
    }
    fn is_child_session(&self) -> bool {
        self.session.upgrade().is_some_and(|inner| {
            AgentSession { inner }.with_session_manager(|manager| {
                manager.header().is_some_and(|header| {
                    header
                        .get("parentSession")
                        .and_then(serde_json::Value::as_str)
                        .is_some()
                })
            })
        })
    }
    fn fork_provenance(&self) -> Option<maho_ext_api::ForkProvenance> {
        self.session.upgrade().and_then(|inner| {
            AgentSession { inner }.with_session_manager(|manager| manager.fork_provenance())
        })
    }
}

struct AbortSignalBridge(tokio::task::JoinHandle<()>);

/// Two-way link between the compaction controller and the extension feedback signal (pinned
/// `_beginExtensionCompactionFeedback` hands the extension the controller's own signal). The
/// controller side is a synchronous abort listener; the extension signal is a watch flag with no
/// listener API, so its abort is forwarded by a task that the bridge cancels on drop.
struct CompactionSignalBridge {
    controller_signal: maho_ai::utils::abort::AbortSignal,
    controller_listener: maho_ai::utils::abort::ListenerId,
    extension_forward: Option<AbortSignalBridge>,
}
impl CompactionSignalBridge {
    fn new(
        controller: crate::compaction::lifecycle::CompactionAbortController,
        signal: maho_ext_api::AbortSignal,
    ) -> Self {
        let controller_signal = controller.signal();
        let extension_abort = signal.clone();
        let controller_listener =
            controller_signal.add_abort_listener(move |_| extension_abort.abort());
        let extension_forward = tokio::runtime::Handle::try_current().ok().map(|runtime| {
            AbortSignalBridge(runtime.spawn(async move {
                signal.cancelled().await;
                controller.abort();
            }))
        });
        Self {
            controller_signal,
            controller_listener,
            extension_forward,
        }
    }
}
impl Drop for CompactionSignalBridge {
    fn drop(&mut self) {
        self.controller_signal
            .remove_abort_listener(self.controller_listener);
        self.extension_forward.take();
    }
}

type BindingPromptReadiness = Arc<Mutex<Vec<tokio::sync::oneshot::Receiver<()>>>>;

struct ExtensionBindingReadiness<'a> {
    session: &'a AgentSession,
    pending: BindingPromptReadiness,
}

impl Drop for ExtensionBindingReadiness<'_> {
    fn drop(&mut self) {
        let mut active = lock(&self.session.binding_readiness);
        if active
            .as_ref()
            .is_some_and(|pending| Arc::ptr_eq(pending, &self.pending))
        {
            *active = None;
        }
    }
}

impl Drop for AbortSignalBridge {
    fn drop(&mut self) {
        self.0.abort();
    }
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
        self.session
            .state()
            .messages_awaiting_persistence
            .retain(|(id, _)| *id != self.id);
    }
}

impl Drop for PromptStartGuard<'_> {
    fn drop(&mut self) {
        self.0.state().prompt_start_pending = false;
    }
}

struct AutoCompactionOwnerGuard<'a>(&'a AgentSession);

impl Drop for AutoCompactionOwnerGuard<'_> {
    fn drop(&mut self) {
        self.0.state().auto_compaction_owned = false;
    }
}

struct PendingCompactionAdmissionGuard<'a> {
    session: &'a AgentSession,
    controller: crate::compaction::lifecycle::CompactionAbortController,
}

impl Drop for PendingCompactionAdmissionGuard<'_> {
    fn drop(&mut self) {
        let mut state = self.session.state();
        if state
            .pending_compaction_admission
            .as_ref()
            .is_some_and(|current| current.controller.same(&self.controller))
        {
            self.controller.abort();
            state.pending_compaction_admission = None;
        }
    }
}

impl SessionExtensionActions {
    fn session(&self) -> Result<AgentSession, maho_ext_api::ExtensionFailure> {
        self.0
            .upgrade()
            .map(|inner| AgentSession { inner })
            .ok_or_else(|| maho_ext_api::ExtensionFailure::new("Session disposed"))
    }

    fn update_retry(
        &self,
        update: impl FnOnce(&mut crate::settings_manager::SettingsManager) -> Result<(), String>,
    ) -> Result<(), maho_ext_api::ExtensionFailure> {
        let session = self.session()?;
        session
            .with_settings_manager_mut(update)
            .map_err(maho_ext_api::ExtensionFailure::new)
    }
}

impl maho_ext_api::ExtensionSessionSettings for SessionExtensionActions {
    fn get_retry_fallback_settings(&self) -> maho_ext_api::RetryFallbackSettings {
        let raw = self.session().ok().and_then(|session| {
            session.with_settings_manager(|manager| manager.get_value("retry").cloned())
        });
        let settings =
            crate::retry_fallback::settings::resolve_retry_fallback_settings(raw.as_ref());
        maho_ext_api::RetryFallbackSettings {
            model_fallback: settings.model_fallback,
            chains: settings.chains.into_iter().collect(),
            revert_policy: match settings.revert_policy {
                crate::retry_fallback::settings::FallbackRevertPolicy::Never => {
                    maho_ext_api::FallbackRevertPolicy::Never
                }
                crate::retry_fallback::settings::FallbackRevertPolicy::CooldownExpiry => {
                    maho_ext_api::FallbackRevertPolicy::CooldownExpiry
                }
            },
        }
    }
    fn set_fallback_chain<'a>(
        &'a self,
        key: &'a str,
        entries: &'a [String],
    ) -> maho_ext_api::ExtensionFuture<'a, ()> {
        Box::pin(
            async move { self.update_retry(|manager| manager.set_fallback_chain(key, entries)) },
        )
    }
    fn remove_fallback_chain<'a>(&'a self, key: &'a str) -> maho_ext_api::ExtensionFuture<'a, ()> {
        Box::pin(async move { self.update_retry(|manager| manager.remove_fallback_chain(key)) })
    }
    fn set_model_fallback_enabled(&self, enabled: bool) -> maho_ext_api::ExtensionFuture<'_, ()> {
        Box::pin(
            async move { self.update_retry(|manager| manager.set_model_fallback_enabled(enabled)) },
        )
    }
    fn set_fallback_revert_policy(
        &self,
        policy: maho_ext_api::FallbackRevertPolicy,
    ) -> maho_ext_api::ExtensionFuture<'_, ()> {
        Box::pin(async move {
            self.update_retry(|manager| {
                manager.set_fallback_revert_policy(match policy {
                    maho_ext_api::FallbackRevertPolicy::Never => "never",
                    maho_ext_api::FallbackRevertPolicy::CooldownExpiry => "cooldown-expiry",
                })
            })
        })
    }
    fn reload(&self) -> maho_ext_api::ExtensionFuture<'_, ()> {
        Box::pin(async move {
            self.session()?
                .with_settings_manager_mut(|manager| manager.reload());
            Ok(())
        })
    }
    fn get_fallback_status(&self) -> Option<maho_ext_api::RetryFallbackStatus> {
        let session = self.session().ok()?;
        let controller = session.retry_fallback.try_lock().ok()?;
        let state = controller.as_ref()?.state.as_ref()?;
        Some(maho_ext_api::RetryFallbackStatus {
            active: true,
            current_model: Some(format!(
                "{}/{}",
                session.model().provider,
                session.model().id
            )),
            original_selector: Some(state.original_selector.clone()),
            pinned: state.pinned,
        })
    }
}

impl maho_ext_api::ExtensionContextActions for SessionExtensionActions {
    fn assert_active(&self) -> Result<(), maho_ext_api::ExtensionFailure> {
        self.session().map(|_| ())
    }
    fn apply_fast_mode(
        &self,
        enabled: bool,
        context: maho_ext_api::ExtensionContext,
    ) -> maho_ext_api::ExtensionFuture<'static, maho_ext_api::FastModeResult> {
        let actions = SessionExtensionActions(self.0.clone());
        Box::pin(async move {
            let session = actions.session()?;
            let runner = session
                .extension_runner
                .lock()
                .await
                .clone()
                .ok_or_else(|| {
                    maho_ext_api::ExtensionFailure::new("Extension runner is not bound")
                })?;
            runner.apply_fast_mode(enabled, &context).await
        })
    }
    fn set_approved_monitor_parent(
        &self,
        id: &str,
        input: &Value,
        parent: &std::path::Path,
    ) -> Result<(), maho_ext_api::ExtensionFailure> {
        let session = self.session()?;
        monitor_invocation::CURRENT
            .try_with(|invocation| {
                if invocation.session_identity != Arc::as_ptr(&session.inner) as usize
                    || invocation.generation != session.monitor_generation.load(Ordering::SeqCst)
                    || session.state().disposed
                {
                    return Err("Monitor invocation belongs to another session".to_owned());
                }
                invocation.attach(
                    id,
                    input,
                    std::path::Path::new(&session.cwd()),
                    parent.to_path_buf(),
                )
            })
            .map_err(|_| maho_ext_api::ExtensionFailure::new("No active monitor invocation"))?
            .map_err(maho_ext_api::ExtensionFailure::new)
    }
    fn take_approved_monitor_parent(
        &self,
        id: &str,
        input: &Value,
    ) -> Result<Option<std::path::PathBuf>, maho_ext_api::ExtensionFailure> {
        let session = self.session()?;
        monitor_invocation::CURRENT
            .try_with(|invocation| {
                if invocation.session_identity != Arc::as_ptr(&session.inner) as usize
                    || invocation.generation != session.monitor_generation.load(Ordering::SeqCst)
                    || session.state().disposed
                {
                    return Err("Monitor invocation belongs to another session".to_owned());
                }
                invocation.take(id, input, std::path::Path::new(&session.cwd()))
            })
            .map_err(|_| maho_ext_api::ExtensionFailure::new("No active monitor invocation"))?
            .map_err(maho_ext_api::ExtensionFailure::new)
    }
    fn get_model(&self) -> Option<Model> {
        self.session().ok().map(|session| session.model())
    }
    fn get_service_tier(&self) -> Option<ServiceTier> {
        self.session()
            .ok()
            .and_then(|session| session.service_tier())
    }
    fn get_effective_service_tier(&self) -> Option<ServiceTier> {
        self.session()
            .ok()
            .and_then(|session| session.effective_service_tier())
    }
    fn get_scoped_models(&self) -> Vec<maho_ext_api::ScopedModel> {
        self.session().map_or_else(
            |_| Vec::new(),
            |session| {
                session
                    .scoped_models()
                    .into_iter()
                    .map(|model| maho_ext_api::ScopedModel {
                        model: model.model,
                        thinking_level: model.thinking_level,
                        service_tier: model.service_tier,
                    })
                    .collect()
            },
        )
    }
    fn get_agent_dir(&self) -> std::path::PathBuf {
        self.session()
            .map_or_else(|_| Default::default(), |session| session.agent_dir().into())
    }
    fn get_pause_state(&self) -> serde_json::Value {
        self.session().map_or_else(
            |_| serde_json::json!({"pauseSeq": 0, "holds": []}),
            |session| {
                let hold = lock(&session.engine_turn_hold);
                let holds = hold
                    .holds()
                    .into_iter()
                    .map(|record| {
                        serde_json::json!({
                            "kind": record.kind.as_str(), "key": record.key,
                            "since": record.since, "data": record.data,
                            "sourceCounts": record.source_counts,
                        })
                    })
                    .collect::<Vec<_>>();
                serde_json::json!({"pauseSeq": hold.current_pause_seq(), "holds": holds})
            },
        )
    }
    fn engine_turns_held(&self) -> bool {
        self.session()
            .is_ok_and(|session| lock(&session.engine_turn_hold).holds_source("goal"))
    }
    fn offload(
        &self,
        id: &str,
        args: Value,
        options: maho_ext_api::OffloadOptions,
    ) -> Option<maho_ext_api::ExtensionFuture<'_, Value>> {
        let session = self.session().ok()?;
        let id = id.to_owned();
        Some(Box::pin(async move {
            let modules = session
                .extension_runner_snapshot()
                .map(|runner| runner.get_all_child_modules())
                .unwrap_or_default();
            crate::tool_dispatch::offload::offload(
                session.tool_dispatcher.as_ref(),
                &modules,
                &id,
                args,
                Some(options),
            )
            .await
            .map_err(|error| {
                let mut failure = maho_ext_api::ExtensionFailure::new(error.to_string());
                failure.code = error
                    .downcast_ref::<crate::tool_dispatch::offload::OffloadError>()
                    .map(|offload| offload.code.as_str().to_owned());
                failure
            })
        }))
    }
    fn note_held_wake(&self, source: &str) {
        if let Ok(session) = self.session() {
            lock(&session.engine_turn_hold).note(source, None);
        }
    }
    fn question_input(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        let weak = self.0.clone();
        Some(Arc::new(move || {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            let session = AgentSession { inner };
            session.human_input_admitted(HumanInputKind::Answer);
            {
                let mut hold = lock(&session.engine_turn_hold);
                lock(&session.engine_turn_input).begin(&mut hold);
            }
            session.engine_resume_window.store(false, Ordering::SeqCst);
            // The answer's follow-on (if any) enters prompt() right after this; settle once it has.
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    session.settle_input_release().await;
                });
            }
        }))
    }
    fn is_idle(&self) -> bool {
        self.session().is_ok_and(|session| {
            !session.agent_run_active.load(Ordering::SeqCst)
                && !session.settling_with_background_work.load(Ordering::SeqCst)
        })
    }
    fn is_project_trusted(&self) -> bool {
        self.session().is_ok_and(|session| {
            session.with_settings_manager(|manager| manager.is_project_trusted())
        })
    }
    fn get_signal(&self) -> Option<maho_ext_api::AbortSignal> {
        EXTENSION_EVENT_SIGNAL
            .try_with(Clone::clone)
            .ok()
            .or_else(|| self.session().ok()?.state().extension_event_signal.clone())
    }
    fn get_compaction_preparation(&self) -> Option<maho_ext_api::CompactionPreparationDetails> {
        EXTENSION_COMPACTION_PREPARATION.try_with(Clone::clone).ok()
    }
    fn abort(&self, source: Option<maho_ext_api::AbortSource>) {
        if let Ok(session) = self.session() {
            if source == Some(maho_ext_api::AbortSource::System) {
                session.state().abort_source = source;
                let streaming = session.is_streaming();
                let joined = session
                    .state()
                    .abort_provenance
                    .join(maho_ext_api::AbortSource::System, streaming);
                if joined.abort_current_agent {
                    session.agent.abort(None);
                }
                session.abort_retry();
                session.abort_compaction();
            } else if let Some(pending) = session.begin_user_abort()
                && let Ok(runtime) = tokio::runtime::Handle::try_current()
            {
                runtime.spawn(async move {
                    session.finish_user_abort(pending).await;
                });
            }
        }
    }
    fn has_queued_person_input(&self) -> bool {
        self.session().is_ok_and(|session| {
            lock(&session.user_input_sources)
                .iter()
                .any(|(_, source)| *source != InputSource::Extension)
        })
    }
    fn todo_turn_end_backstop_enabled(&self) -> bool {
        self.session().is_ok_and(|session| {
            session.with_settings_manager(|manager| {
                manager
                    .get_value("todo")
                    .and_then(|todo| todo.get("turnEndBackstop"))
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true)
            })
        })
    }
    fn has_pending_messages(&self) -> bool {
        self.session()
            .is_ok_and(|session| session.pending_message_count() > 0)
    }
    fn request_reload(&self) -> maho_ext_api::ExtensionFuture<'_, ()> {
        Box::pin(async move {
            self.session()?
                .reload()
                .await
                .map(|_| ())
                .map_err(maho_ext_api::ExtensionFailure::new)
        })
    }
    fn is_compacting(&self) -> bool {
        self.session().is_ok_and(|session| session.is_compacting())
    }
    fn check_reload_veto(
        &self,
    ) -> maho_ext_api::ExtensionFuture<'_, maho_ext_api::ReloadVetoDecision> {
        Box::pin(async move {
            let result = self
                .session()?
                .session_before(maho_ext_api::ExtensionEvent::SessionBeforeReload)
                .await
                .map_err(maho_ext_api::ExtensionFailure::new)?;
            Ok(maho_ext_api::ReloadVetoDecision {
                cancelled: result.cancel == Some(true),
                reason: result.reason,
            })
        })
    }
    fn shutdown(&self) {
        if let Ok(session) = self.session() {
            // senpi `shutdown: () => this._extensionShutdownHandler?.()`: the bound mode owns shutdown.
            let handler = session.state().extension_shutdown_handler.clone();
            match handler {
                Some(handler) => handler(),
                None => {
                    tokio::spawn(async move {
                        session.dispose().await;
                    });
                }
            }
        }
    }
    fn get_context_usage(&self) -> Option<maho_ext_api::ContextUsage> {
        self.session()
            .ok()?
            .get_context_usage()
            .map(|usage| maho_ext_api::ContextUsage {
                tokens: usage.tokens,
                context_window: usage.context_window,
                percent: usage.percent,
            })
    }
    fn get_compaction_settings(&self) -> maho_ext_api::CompactionSettings {
        let resolved = self
            .get_resolved_compaction_settings()
            .expect("live compaction settings");
        maho_ext_api::CompactionSettings {
            enabled: resolved.enabled,
            reserve_tokens: resolved.reserve_tokens,
            keep_recent_tokens: resolved.keep_recent_tokens,
        }
    }
    fn get_compaction_model(&self) -> Option<Model> {
        let session = self.session().ok()?;
        // senpi `getCompactionModel: () => this._resolveCompactionModel(this.model)`: a spent account with no
        // usable alternate throws the AccountUsageLimitError to the extension, which fails its compaction.
        Some(
            session
                .resolve_compaction_model(&session.model())
                .unwrap_or_else(|error| {
                    std::panic::panic_any(maho_ext_api::ExtensionFailure::new(error))
                }),
        )
    }
    fn get_resolved_compaction_settings(&self) -> Option<maho_ext_api::ResolvedCompactionSettings> {
        let session = self
            .session()
            .unwrap_or_else(|error| std::panic::panic_any(error));
        let model = session.model();
        let raw = session.with_settings_manager(|manager| manager.get_value("compaction").cloned());
        let settings = raw
            .map(serde_json::from_value::<crate::compaction_settings_access::CompactionSettings>)
            .transpose()
            .unwrap_or_else(|error| {
                std::panic::panic_any(maho_ext_api::ExtensionFailure::new(error.to_string()))
            });
        let resolved = crate::compaction_settings_resolver::resolve_compaction_settings(
            settings.as_ref(),
            Some(crate::compaction_settings_access::CompactionModelSelector {
                provider: &model.provider,
                id: &model.id,
            }),
        )
        .unwrap_or_else(|error| std::panic::panic_any(maho_ext_api::ExtensionFailure::new(error)));
        Some(maho_ext_api::ResolvedCompactionSettings {
            enabled: session.auto_compaction_enabled(),
            reserve_tokens: u64::try_from(resolved.reserve_tokens).unwrap_or_else(|error| {
                std::panic::panic_any(maho_ext_api::ExtensionFailure::new(error.to_string()))
            }),
            keep_recent_tokens: u64::try_from(resolved.keep_recent_tokens).unwrap_or_else(
                |error| {
                    std::panic::panic_any(maho_ext_api::ExtensionFailure::new(error.to_string()))
                },
            ),
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
        let configured =
            session.with_settings_manager(|manager| manager.get_value("promptCache").cloned());
        crate::prompt_cache_budget::resolve_prompt_cache_safe_wait_seconds(
            Some(&session.model()),
            configured
                .as_ref()
                .and_then(|value| value.get("cacheAwareTimeouts"))
                .and_then(Value::as_bool)
                .unwrap_or(true),
            configured
                .as_ref()
                .and_then(|value| value.get("safetyBufferSeconds"))
                .and_then(Value::as_f64),
            &std::env::vars().collect(),
        )
        .map(|value| value as f64)
    }
    fn get_prompt_cache_goal_backstop_max_seconds(&self) -> f64 {
        self.session()
            .ok()
            .and_then(|session| {
                session.with_settings_manager(|manager| {
                    manager
                        .get_value("promptCache")
                        .and_then(|value| value.get("goalBackstopMaxSeconds"))
                        .and_then(Value::as_f64)
                })
            })
            .unwrap_or(270.0)
    }
    fn on_user_activity(
        &self,
        listener: maho_ext_api::UserActivityListener,
    ) -> Option<maho_ext_api::UserActivityUnsubscribe> {
        let session = self.session().ok()?;
        Some(Box::new(session.on_user_activity(listener)))
    }
    fn get_prompt_cache_prefix_request(
        &self,
        options: maho_ext_api::PromptCachePrefixRequestOptions,
    ) -> maho_ext_api::ExtensionFuture<'_, maho_ext_api::PromptCachePrefixResult> {
        Box::pin(async move {
            let session = self.session()?;
            Ok(session.prompt_cache_prefix_request(options).await)
        })
    }
    fn get_prompt_cache_keep_alive_settings(&self) -> maho_ext_api::PromptCacheKeepAliveSettings {
        let configured = self.session().ok().and_then(|session| {
            session.with_settings_manager(|manager| {
                manager
                    .get_value("promptCache")
                    .and_then(|value| value.get("keepAlive"))
                    .cloned()
            })
        });
        maho_ext_api::PromptCacheKeepAliveSettings {
            enabled: configured
                .as_ref()
                .and_then(|value| value.get("enabled"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
            max_requests_per_session: configured
                .as_ref()
                .and_then(|value| value.get("maxRequestsPerSession"))
                .and_then(Value::as_u64)
                .unwrap_or(3),
            max_cost_usd_per_session: configured
                .as_ref()
                .and_then(|value| value.get("maxCostUsdPerSession"))
                .and_then(Value::as_f64)
                .unwrap_or(0.05),
            margin_seconds: configured
                .as_ref()
                .and_then(|value| value.get("marginSeconds"))
                .and_then(Value::as_f64)
                .unwrap_or(60.0),
        }
    }
    fn get_look_at_settings(&self) -> maho_ext_api::LookAtSettings {
        let configured = self.session().ok().and_then(|session| {
            session.with_settings_manager(|manager| manager.get_value("lookAt").cloned())
        });
        maho_ext_api::LookAtSettings {
            enabled: configured
                .as_ref()
                .and_then(|value| value.get("enabled"))
                .and_then(Value::as_bool)
                .unwrap_or(true),
            models: configured
                .as_ref()
                .and_then(|value| value.get("models"))
                .and_then(Value::as_array)
                .map(|models| {
                    models
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                }),
        }
    }
    fn get_ask_user_settings(&self) -> maho_ext_api::AskUserSettings {
        let settings = self.session().ok().map(|session| {
            session.with_settings_manager(|manager| manager.get_ask_user_settings())
        });
        maho_ext_api::AskUserSettings {
            enabled: settings.is_none_or(|settings| settings.enabled),
            timeout_minutes: settings.map_or(
                crate::settings_manager::ASK_USER_DEFAULT_TIMEOUT_MINUTES,
                |settings| settings.timeout_minutes,
            ),
        }
    }
    fn get_image_settings(&self) -> maho_ext_api::ImageSettings {
        let configured = self.session().ok().and_then(|session| {
            session.with_settings_manager(|manager| manager.get_value("images").cloned())
        });
        maho_ext_api::ImageSettings {
            auto_resize: configured
                .as_ref()
                .and_then(|value| value.get("autoResize"))
                .and_then(Value::as_bool)
                .unwrap_or(true),
            block_images: configured
                .as_ref()
                .and_then(|value| value.get("blockImages"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
        }
    }
    fn session_settings(&self) -> &dyn maho_ext_api::ExtensionSessionSettings {
        self
    }
    fn compact(&self, options: maho_ext_api::CompactOptions) {
        if let Ok(session) = self.session() {
            tokio::spawn(async move {
                match session
                    .compact_from_extension(options.custom_instructions.as_deref())
                    .await
                {
                    Ok(result) => {
                        if let Some(callback) = options.on_complete {
                            callback(maho_ext_api::CompactionResult {
                                summary: result.summary,
                                first_kept_entry_id: result.first_kept_entry_id,
                                tokens_before: result.tokens_before as u64,
                                details: result.details,
                            });
                        }
                    }
                    Err(error) => {
                        if let Some(callback) = options.on_error {
                            callback(maho_ext_api::ExtensionFailure::new(error));
                        }
                    }
                }
            });
        }
    }
    fn prepare_provider_request(
        &self,
        messages: Vec<AgentMessage>,
    ) -> maho_ext_api::ExtensionFuture<'_, maho_ext_api::ProviderRequestPreparation> {
        Box::pin(async move {
            let session = self.session()?;
            let runner = session.extension_runner.lock().await.clone();
            if let Some(runner) = runner {
                return runner.prepare_provider_request(messages, None).await;
            }
            Ok(maho_ext_api::ProviderRequestPreparation {
                messages,
                transform_payload: Arc::new(|payload| Box::pin(async move { Ok(payload) })),
                transform_headers: Arc::new(|headers| Box::pin(async move { Ok(headers) })),
            })
        })
    }
    fn begin_compaction(
        &self,
        options: maho_ext_api::BeginCompactionOptions,
    ) -> Option<maho_ext_api::AbortSignal> {
        let session = self.session().ok()?;
        let signal = maho_ext_api::AbortSignal::default();
        let model = session.model();
        let request_id = uuid::Uuid::new_v4().to_string();
        {
            let mut state = session.state();
            if state.compaction_abort_controller.is_some() {
                return None;
            }
            let controller = crate::compaction::lifecycle::CompactionAbortController::new();
            let revision = state.message_revision as i64;
            state.compaction_lifecycle.begin(
                crate::compaction::lifecycle::BeginCompactionOperation {
                    operation_id: request_id.clone(),
                    stage: crate::compaction::lifecycle::CompactionStage::Feedback,
                    reason: format!("{:?}", options.reason),
                    model: Some(crate::compaction::lifecycle::CompactionModelRef {
                        provider: model.provider,
                        id: model.id,
                    }),
                    started_revision: revision,
                },
                controller.clone(),
            );
            state.compaction_abort_controller = Some(controller);
            state.compaction_extension_signal = Some(signal.clone());
            state.compaction_signal_bridge = state
                .compaction_abort_controller
                .clone()
                .map(|controller| CompactionSignalBridge::new(controller, signal.clone()));
        }
        // Each compaction abort controller is an isCompacting fact: setting one is a lifecycle edge (senpi#3980).
        session.refresh_lifecycle();
        session.emit(AgentSessionEvent::CompactionStart {
            reason: options.reason,
            request_id: Some(request_id),
        });
        Some(signal)
    }
    fn update_compaction(&self, options: maho_ext_api::UpdateCompactionOptions) {
        if let Ok(session) = self.session() {
            if !session.is_compacting()
                || options
                    .signal
                    .as_ref()
                    .is_some_and(maho_ext_api::AbortSignal::is_aborted)
            {
                return;
            }
            session.emit(AgentSessionEvent::CompactionProgress {
                reason: options.reason,
                delta: options.delta,
                text: options.text,
            });
        }
    }
    fn end_compaction(&self, options: maho_ext_api::EndCompactionOptions) {
        if let Ok(session) = self.session() {
            let lifecycle = session.compaction_state();
            let Some(operation) = lifecycle.operation() else {
                return;
            };
            if lifecycle.status() != "running"
                || operation.stage != crate::compaction::lifecycle::CompactionStage::Feedback
            {
                return;
            }
            let aborted = options.aborted.unwrap_or_else(|| {
                options
                    .signal
                    .as_ref()
                    .is_some_and(maho_ext_api::AbortSignal::is_aborted)
            });
            let revision = session.message_revision() as i64;
            session.state().compaction_lifecycle.finish(
                &crate::compaction::lifecycle::FinishCompactionOperation {
                    operation_id: operation.operation_id.clone(),
                    status: if aborted {
                        crate::compaction::lifecycle::CompactionFinishStatus::Aborted
                    } else {
                        crate::compaction::lifecycle::CompactionFinishStatus::Failed
                    },
                    ended_revision: revision,
                    rejection_cause: None,
                    error_message: options.error_message.clone(),
                },
            );
            session.set_compaction_abort_controller(None);
            session.state().compaction_extension_signal = None;
            session.state().compaction_signal_bridge = None;
            session.emit(AgentSessionEvent::CompactionEnd {
                reason: options.reason,
                result: None,
                aborted,
                will_retry: false,
                request_id: Some(operation.operation_id.clone()),
                accepted: None,
                rejection_cause: None,
                error_message: options.error_message,
            });
        }
    }
    fn get_message_revision(&self) -> u64 {
        self.session()
            .map_or(0, |session| session.message_revision())
    }
    fn apply_compaction(
        &self,
        result: maho_ext_api::CompactionResult,
        options: maho_ext_api::ApplyCompactionOptions,
    ) -> maho_ext_api::ExtensionFuture<'_, maho_ext_api::ApplyCompactionResult> {
        Box::pin(async move {
            let session = self.session()?;
            if options
                .expected_revision
                .is_some_and(|revision| revision != session.message_revision())
            {
                return Ok(maho_ext_api::ApplyCompactionResult::Stale);
            }
            // The extension treats `Rejected` as terminal and sends no feedback end of its own, so
            // a rejection before execution must close a running feedback operation here; otherwise
            // its `compaction_start` never gets a `compaction_end` and the TUI keeps showing
            // "Compacting...".
            let reject_before_execution = |error_message: &str| {
                maho_ext_api::ExtensionContextActions::end_compaction(
                    self,
                    maho_ext_api::EndCompactionOptions {
                        reason: options.reason,
                        signal: options.signal.clone(),
                        aborted: None,
                        error_message: Some(error_message.to_owned()),
                    },
                );
                Ok(maho_ext_api::ApplyCompactionResult::Rejected)
            };
            if options
                .signal
                .as_ref()
                .is_some_and(maho_ext_api::AbortSignal::is_aborted)
            {
                return reject_before_execution("Compaction cancelled");
            }
            if let Some(anchor) = options.expected_warm_anchor.clone() {
                let snapshot = crate::compaction::warm_anchor::WarmAnchorSnapshot {
                    first_kept_entry_id: anchor.first_kept_entry_id,
                    prefix_entry_ids: anchor.prefix_entry_ids,
                    latest_compaction_entry_id: anchor.latest_compaction_entry_id,
                };
                if !session.with_session_manager(|manager| {
                    crate::compaction::warm_anchor::is_warm_summary_anchor_valid(
                        &snapshot,
                        &manager.current_branch(),
                    )
                }) {
                    return Ok(maho_ext_api::ApplyCompactionResult::Stale);
                }
            }
            let Ok(_admission) = session.prompt_admission.try_lock() else {
                return reject_before_execution("Compaction did not apply: session busy");
            };
            if session.is_streaming() {
                return reject_before_execution("Compaction did not apply: session busy");
            }
            let _work = session.work_barrier.begin();
            let model = session.model();
            let (request_id, owns_controller) = {
                let mut state = session.state();
                let owns = state.compaction_abort_controller.is_none();
                let controller = state
                    .compaction_abort_controller
                    .clone()
                    .unwrap_or_default();
                let revision = state.message_revision as i64;
                let request_id = state.compaction_lifecycle.begin(
                    crate::compaction::lifecycle::BeginCompactionOperation {
                        operation_id: uuid::Uuid::new_v4().to_string(),
                        stage: crate::compaction::lifecycle::CompactionStage::Execution,
                        reason: format!("{:?}", options.reason),
                        model: Some(crate::compaction::lifecycle::CompactionModelRef {
                            provider: model.provider,
                            id: model.id,
                        }),
                        started_revision: revision,
                    },
                    controller.clone(),
                );
                state.compaction_abort_controller = Some(controller);
                (request_id, owns)
            };
            if owns_controller {
                session.refresh_lifecycle();
            }
            if owns_controller {
                session.emit(AgentSessionEvent::CompactionStart {
                    reason: options.reason,
                    request_id: Some(request_id.clone()),
                });
            }
            let applied = session.apply_compaction_internal(
                &crate::compaction::compaction::CompactionResult {
                    summary: result.summary.clone(),
                    first_kept_entry_id: result.first_kept_entry_id.clone(),
                    tokens_before: result.tokens_before as i64,
                    estimated_tokens_after: None,
                    usage: None,
                    details: result.details.clone(),
                },
                Some(true),
            );
            let revision = session.message_revision() as i64;
            session.state().compaction_lifecycle.finish(
                &crate::compaction::lifecycle::FinishCompactionOperation {
                    operation_id: request_id.clone(),
                    status: if applied.is_ok() {
                        crate::compaction::lifecycle::CompactionFinishStatus::Completed
                    } else {
                        crate::compaction::lifecycle::CompactionFinishStatus::Failed
                    },
                    ended_revision: revision,
                    rejection_cause: None,
                    error_message: applied.as_ref().err().cloned(),
                },
            );
            session.set_compaction_abort_controller(None);
            session.state().compaction_extension_signal = None;
            session.state().compaction_signal_bridge = None;
            let accepted_request_id = request_id.clone();
            session.emit(AgentSessionEvent::CompactionEnd {
                reason: options.reason,
                request_id: Some(request_id),
                result: applied.is_ok().then_some(result),
                aborted: false,
                will_retry: false,
                accepted: Some(applied.is_ok()),
                rejection_cause: None,
                error_message: applied
                    .as_ref()
                    .err()
                    .map(|error| format!("Compaction failed: {error}")),
            });
            let entry = match applied {
                Ok(entry) => entry,
                Err(_) => return Ok(maho_ext_api::ApplyCompactionResult::Rejected),
            };
            session.state().delegated_compaction_key = None;
            // senpi `_executeCompaction` emits `session_compact` for extension-applied compactions too.
            session
                .dispatch_extension_event(maho_ext_api::ExtensionEvent::SessionCompact(
                    maho_ext_api::SessionCompactEvent::Accepted {
                        reason: options.reason,
                        request_id: accepted_request_id,
                        compaction_entry: session_entry_from_value(entry),
                        from_extension: true,
                        will_retry: false,
                    },
                ))
                .await;
            // senpi `_onCompactionContextChanged`.
            let released = session
                .retry_fallback
                .lock()
                .await
                .as_mut()
                .is_some_and(|controller| controller.notify_compaction_applied());
            if released && !session.is_streaming() {
                session.restore_fallback_primary().await;
            }
            Ok(maho_ext_api::ApplyCompactionResult::Applied)
        })
    }
    fn get_system_prompt(&self) -> String {
        self.session()
            .map_or_else(|_| String::new(), |session| session.system_prompt())
    }
    fn get_browser_engine(&self) -> Option<&'static str> {
        match self.session().ok()?.browser_engine()?.as_str() {
            "connected" => Some("connected"),
            "builtin" => Some("builtin"),
            "none" => Some("none"),
            _ => None,
        }
    }
    fn get_system_prompt_options(&self) -> maho_ext_api::BuildSystemPromptOptions {
        let Ok(session) = self.session() else {
            return Default::default();
        };
        let skills = session
            .state()
            .skills
            .iter()
            .map(|skill| maho_ext_api::Skill {
                provenance: skill.provenance.as_ref().map(|provenance| {
                    maho_ext_api::SkillProvenance {
                        kind: match provenance.kind {
                            crate::skill_access::SkillProvenanceKind::User => "user",
                            crate::skill_access::SkillProvenanceKind::ProjectAuto => "project-auto",
                            crate::skill_access::SkillProvenanceKind::ProjectSelected => {
                                "project-selected"
                            }
                        }
                        .into(),
                        selected_root: provenance.selected_root.clone(),
                        project_root: provenance.project_root.clone(),
                    }
                }),
                allowed_set: skill.allowed_set.as_ref().map(|allowed| {
                    maho_ext_api::SkillAllowedSet {
                        roots: allowed.roots.clone(),
                        files: allowed.files.clone(),
                        digest: allowed.digest.clone(),
                    }
                }),
                name: skill.name.clone(),
                description: skill.description.clone(),
                file_path: skill.file_path.clone(),
                base_dir: skill.base_dir.clone(),
                source_info: maho_ext_api::SourceInfo {
                    path: skill.source_info.path.clone(),
                    source: skill.source_info.source.clone(),
                    scope: match skill.source_info.scope {
                        crate::source_info::SourceScope::User => maho_ext_api::SourceScope::User,
                        crate::source_info::SourceScope::Project => {
                            maho_ext_api::SourceScope::Project
                        }
                        crate::source_info::SourceScope::Temporary => {
                            maho_ext_api::SourceScope::Temporary
                        }
                        crate::source_info::SourceScope::System => {
                            maho_ext_api::SourceScope::System
                        }
                    },
                    origin: match skill.source_info.origin {
                        crate::source_info::SourceOrigin::Package => {
                            maho_ext_api::SourceOrigin::Package
                        }
                        crate::source_info::SourceOrigin::TopLevel => {
                            maho_ext_api::SourceOrigin::TopLevel
                        }
                    },
                    base_dir: skill.source_info.base_dir.clone(),
                },
                disable_model_invocation: skill.disable_model_invocation,
            })
            .collect();
        let (custom_prompt, append) = session.resolved_system_prompt();
        let tools = session.get_active_tool_names();
        let (tool_snippets, prompt_guidelines) = session.tool_prompt_contributions(&tools);
        maho_ext_api::BuildSystemPromptOptions {
            cwd: session.cwd().into(),
            tools,
            tool_snippets,
            prompt_guidelines,
            skills,
            surface: session.effective_prompt_surface(),
            custom_prompt,
            append_system_prompt: (!append.is_empty()).then(|| append.join("\n\n")),
            context_files: session
                .project_context_files()
                .into_iter()
                .map(|file| maho_ext_api::ContextFile {
                    path: file.path,
                    content: file.content,
                    scope: file.scope,
                })
                .collect(),
        }
    }
    fn get_loaded_hook_sources(&self) -> maho_ext_api::LoadedHookSources {
        if let Ok(session) = self.session() {
            let cwd = session.cwd();
            let state = session.state();
            if let Some(mut sources) = state.loaded_hook_sources.clone() {
                sources.cwd = cwd.into();
                sources.project_hooks_path = sources
                    .cwd
                    .join(crate::config::config_dir_name())
                    .join("hooks.json");
                // Package and auto-discovered hook files (senpi `globalHookSourcePaths` /
                // `projectHookSourcePaths` / `preSessionHookSourcePaths`) join the caller's record.
                let global_base = sources.agent_dir.to_string_lossy().into_owned();
                let project_base = sources
                    .cwd
                    .join(crate::config::config_dir_name())
                    .to_string_lossy()
                    .into_owned();
                let cwd_base = sources.cwd.to_string_lossy().into_owned();
                for (target, resolved, base) in [
                    (
                        &mut sources.global_hook_source_paths,
                        &state.global_hook_source_paths,
                        &global_base,
                    ),
                    (
                        &mut sources.project_hook_source_paths,
                        &state.project_hook_source_paths,
                        &project_base,
                    ),
                    (
                        &mut sources.pre_session_hook_source_paths,
                        &state.pre_session_hook_source_paths,
                        &cwd_base,
                    ),
                ] {
                    for path in resolved {
                        let wanted = crate::paths::canonicalize_path(&path.to_string_lossy());
                        let known = target.iter().any(|existing| {
                            crate::paths::canonicalize_path(&crate::paths::resolve_path(
                                &existing.to_string_lossy(),
                                base,
                                &Default::default(),
                            )) == wanted
                        });
                        if !known {
                            target.push(path.clone());
                        }
                    }
                }
                for entry in &state.discovered_resources.hook_paths {
                    let path = std::path::PathBuf::from(&entry.path);
                    if !sources.runtime_hook_source_paths.contains(&path) {
                        sources.runtime_hook_source_paths.push(path);
                    }
                }
                return sources;
            }
        }
        let session = self.session().ok();
        let cwd = session
            .as_ref()
            .map_or_else(std::path::PathBuf::new, |session| session.cwd().into());
        let dir = session
            .as_ref()
            .map_or_else(std::path::PathBuf::new, |session| {
                session.agent_dir().into()
            });
        let (global_settings_hooks, project_settings_hooks) =
            session.as_ref().map_or((None, None), |session| {
                session.with_settings_manager(|manager| {
                    (
                        manager.get_global().get("hooks").cloned(),
                        manager
                            .is_project_trusted()
                            .then(|| manager.get_project().get("hooks").cloned())
                            .flatten(),
                    )
                })
            });
        maho_ext_api::LoadedHookSources {
            global_hooks_path: dir.join("hooks.json"),
            project_hooks_path: cwd
                .join(crate::config::config_dir_name())
                .join("hooks.json"),
            cwd,
            agent_dir: dir,
            global_settings_hooks,
            project_settings_hooks,
            global_hook_source_paths: session
                .as_ref()
                .map(|session| session.state().global_hook_source_paths.clone())
                .unwrap_or_default(),
            project_hook_source_paths: session
                .as_ref()
                .map(|session| session.state().project_hook_source_paths.clone())
                .unwrap_or_default(),
            pre_session_hook_source_paths: session
                .as_ref()
                .map(|session| session.state().pre_session_hook_source_paths.clone())
                .unwrap_or_default(),
            runtime_hook_source_paths: session
                .as_ref()
                .map(|session| {
                    session
                        .state()
                        .discovered_resources
                        .hook_paths
                        .iter()
                        .map(|entry| std::path::PathBuf::from(&entry.path))
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
    fn kernel_tools(&self) -> Option<&dyn maho_ext_api::ExtensionKernelTools> {
        None
    }
    fn get_thinking_level(&self) -> Option<ThinkingLevel> {
        self.session()
            .ok()
            .and_then(|session| thinking_level_from_model_level(session.thinking_level()))
    }
}

fn to_ext_account_limits(
    limits: &crate::subscription_limits::limit_store::AccountLimits,
) -> maho_ext_api::AccountLimits {
    maho_ext_api::AccountLimits {
        provider: limits.provider.clone(),
        account: limits.account.clone(),
        windows: limits
            .windows
            .iter()
            .map(|window| maho_ext_api::LimitWindow {
                id: window.id.clone(),
                label: window.label.clone(),
                window_minutes: window.window_minutes,
                used_percent: window.used_percent,
                resets_at: window.resets_at.clone(),
            })
            .collect(),
        reported_at: limits.reported_at.clone(),
        plan: limits.plan.clone(),
        error: limits.error.clone(),
    }
}

impl SessionExtensionActions {
    /// The prompt path of `send_user_message`. `person`: the message is a person's input (a human question
    /// answer sent through the extension API), so it attends the session like any other person input.
    fn send_user_message_with(
        &self,
        content: maho_ext_api::UserMessageContent,
        options: maho_ext_api::SendUserMessageOptions,
        person: bool,
    ) -> Result<(), maho_ext_api::ExtensionFailure> {
        let session = self.session()?;
        let deferred_session = session.clone();
        let deferred_content = content.clone();
        let deferred_options = options.clone();
        let deferred_generation = session.monitor_generation.load(Ordering::SeqCst);
        let deferred_abort_generation = session.user_abort_generation.load(Ordering::SeqCst);
        let spawner = session.clone();
        if lock(&session.settled_delivery).defer_trigger_turn(move |claim| {
            let guard = deferred_session.work_barrier.begin();
            spawner.spawn_session_task(async move {
                if deferred_session.state().disposed || deferred_session.monitor_generation.load(Ordering::SeqCst) != deferred_generation
                    || deferred_session.user_abort_generation.load(Ordering::SeqCst) != deferred_abort_generation {
                    claim.resolve(crate::agent_settled_delivery::DeferredTurnDisposition::FinishedWithoutStart);
                    return;
                }
                claim.resolve(crate::agent_settled_delivery::DeferredTurnDisposition::Delegated);
                if let Err(error) = SessionExtensionActions(Arc::downgrade(&deferred_session.inner)).send_user_message_with(deferred_content, deferred_options, person) { deferred_session.emit(AgentSessionEvent::ContinuationError { error_message: error.message }); }
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
                let mut text = Vec::new();
                let mut images = Vec::new();
                for block in blocks {
                    match block {
                        maho_ext_api::ToolContent::Text { text: part, .. } => text.push(part),
                        maho_ext_api::ToolContent::Image { data, mime_type } => {
                            images.push(ImageContent { data, mime_type })
                        }
                    }
                }
                (text.join("\n"), images)
            }
        };
        let guard = session.work_barrier.begin();
        session.clone().spawn_session_task(async move {
            let mut readiness = readiness;
            if session.state().disposed
                || session.monitor_generation.load(Ordering::SeqCst) != deferred_generation
                || session.user_abort_generation.load(Ordering::SeqCst) != deferred_abort_generation
            {
                drop(readiness);
                drop(guard);
                return;
            }
            let mut disposition_reported = false;
            // senpi `sendUserMessage` -> `prompt()`: counted in flight before any await; a person's message attends the session.
            session.prompts_in_flight.fetch_add(1, Ordering::SeqCst);
            if person {
                session.begin_person_input();
            }
            let admission = Some(session.engine_admission_hook(None));
            let outcome = session
                .prompt_with_readiness(
                    &text,
                    PromptOptions {
                        images: Some(images.clone()),
                        source: Some(InputSource::Extension),
                        input_origin: person.then_some(InputOrigin::Person),
                        streaming_behavior: options.deliver_as,
                        expand_prompt_templates: Some(options.expand_prompt_templates),
                        prompt_admitted: admission,
                        ..Default::default()
                    },
                    &mut readiness,
                    &mut disposition_reported,
                )
                .await;
            session.prompts_in_flight.fetch_sub(1, Ordering::SeqCst);
            session.settle_input_release().await;
            if let Err(error) = outcome {
                if !disposition_reported
                    && !session.state().disposed
                    && session.monitor_generation.load(Ordering::SeqCst) == deferred_generation
                    && session.user_abort_generation.load(Ordering::SeqCst)
                        == deferred_abort_generation
                {
                    let mode = options.deliver_as.unwrap_or(StreamingBehavior::FollowUp);
                    session.queue_expanded_input(
                        text,
                        Some(images),
                        mode,
                        None,
                        Some(InputSource::Extension),
                    );
                }
                session.emit(AgentSessionEvent::ContinuationError {
                    error_message: error,
                });
            }
            drop(readiness);
            drop(guard);
        });
        Ok(())
    }
}

impl maho_ext_api::ExtensionActions for SessionExtensionActions {
    fn redact_for_upload(
        &self,
        text: &str,
        extra_literals: &[String],
    ) -> Option<maho_ext_api::UploadRedaction> {
        let redaction = crate::share::upload_redaction::redact_for_upload(text, extra_literals);
        Some(maho_ext_api::UploadRedaction {
            text: redaction.text,
            counts: redaction.counts,
        })
    }
    fn host_version(&self) -> Option<String> {
        Some(crate::engine_build_identity::ENGINE_VERSION.to_owned())
    }
    fn get_subscription_limits(&self) -> Vec<maho_ext_api::AccountLimits> {
        crate::subscription_limits::limit_store::subscription_limits()
            .snapshot()
            .iter()
            .map(to_ext_account_limits)
            .collect()
    }
    fn on_subscription_limits(
        &self,
        listener: maho_ext_api::SubscriptionLimitsListener,
    ) -> Option<maho_ext_api::UiUnsubscribe> {
        let store = crate::subscription_limits::limit_store::subscription_limits();
        let id = store.subscribe(Arc::new(move |limits| {
            listener(&to_ext_account_limits(limits))
        }));
        Some(Box::new(move || store.unsubscribe(id)))
    }
    fn send_message(
        &self,
        message: maho_ext_api::CustomMessage,
        options: maho_ext_api::SendMessageOptions,
    ) -> Result<(), maho_ext_api::ExtensionFailure> {
        let _ = self.send_message_with_receipt(message, options)?;
        Ok(())
    }
    fn relocation_offered(&self) -> Result<bool, maho_ext_api::ExtensionFailure> {
        Ok(self.session()?.relocation_offered())
    }
    fn relocation_targets(&self) -> maho_ext_api::ExtensionFuture<'_, Value> {
        let session = self.session();
        Box::pin(async move {
            let session = session?;
            serde_json::to_value(session.relocation_targets().await)
                .map_err(|e| maho_ext_api::ExtensionFailure::new(e.to_string()))
        })
    }
    fn request_relocation(&self, request: Value) -> maho_ext_api::ExtensionFuture<'_, Value> {
        let session = self.session();
        Box::pin(async move {
            let session = session?;
            let request = serde_json::from_value(request)
                .map_err(|e| maho_ext_api::ExtensionFailure::new(e.to_string()))?;
            serde_json::to_value(session.request_relocation(request).await)
                .map_err(|e| maho_ext_api::ExtensionFailure::new(e.to_string()))
        })
    }
    fn arm_tool_barrier(&self, spec: Value) -> Result<(), maho_ext_api::ExtensionFailure> {
        let spec = serde_json::from_value(spec)
            .map_err(|e| maho_ext_api::ExtensionFailure::new(e.to_string()))?;
        self.session()?
            .arm_tool_barrier(spec)
            .map_err(maho_ext_api::ExtensionFailure::new)
    }
    fn release_tool_barrier(&self, id: &str) -> Result<bool, maho_ext_api::ExtensionFailure> {
        Ok(self.session()?.release_tool_barrier(id))
    }
    fn send_itemized_message(
        &self,
        message: maho_ext_api::CustomMessage,
        options: maho_ext_api::SendMessageOptions,
        items: Vec<maho_ext_api::SendMessageItem>,
    ) -> maho_ext_api::ExtensionFuture<'_, Option<Vec<maho_ext_api::SendMessageReceipt>>> {
        let session = self.session();
        Box::pin(async move {
            let session = session?;
            let items = items
                .into_iter()
                .map(|i| {
                    Ok(crate::extensions::types::SendMessageItem {
                        item_id: i.item_id,
                        producer: i.producer,
                        run_ref: i.run_ref,
                        content: i
                            .content
                            .map(serde_json::to_value)
                            .transpose()
                            .map_err(|e| maho_ext_api::ExtensionFailure::new(e.to_string()))?,
                        details: i.details,
                    })
                })
                .collect::<Result<Vec<_>, maho_ext_api::ExtensionFailure>>()?;
            let receipts = session
                .send_itemized_message(message, options, items)
                .await
                .map_err(maho_ext_api::ExtensionFailure::new)?;
            Ok(Some(
                receipts
                    .into_iter()
                    .map(|r| maho_ext_api::SendMessageReceipt {
                        item_id: r.item_id,
                        disposition: r.disposition,
                        notice_id: r.notice_id,
                        entry_id: r.entry_id,
                    })
                    .collect(),
            ))
        })
    }
    fn send_message_with_receipt(
        &self,
        message: maho_ext_api::CustomMessage,
        options: maho_ext_api::SendMessageOptions,
    ) -> Result<maho_ext_api::MessageDeliveryReceipt, maho_ext_api::ExtensionFailure> {
        let custom: maho_agent::harness::messages::CustomMessage = serde_json::from_value(serde_json::json!({
            "role":"custom","customType":message.custom_type,"content":message.content,"display":message.display,
            "details":message.details,"timestamp":maho_ai::utils::diagnostics::now_ms(),
        })).map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?;
        let receipt = maho_ext_api::MessageDeliveryReceipt::pending();
        self.session()?.send_custom_message_content_with_receipt(
            custom,
            options,
            Some(receipt.clone()),
        )?;
        Ok(receipt)
    }
    fn send_user_message(
        &self,
        content: maho_ext_api::UserMessageContent,
        options: maho_ext_api::SendUserMessageOptions,
    ) -> Result<(), maho_ext_api::ExtensionFailure> {
        self.send_user_message_with(content, options, false)
    }
    fn send_user_message_with_origin(
        &self,
        content: maho_ext_api::UserMessageContent,
        options: maho_ext_api::SendUserMessageOptions,
        origin: maho_ext_api::InputOrigin,
    ) -> Result<(), maho_ext_api::ExtensionFailure> {
        let session = self.session()?;
        // senpi `sendUserMessage`: while engine turns are held, a non-person message is custody-noted like a wake.
        if origin != maho_ext_api::InputOrigin::Person && session.paused() {
            let content = match content {
                maho_ext_api::UserMessageContent::Text(text) => {
                    vec![maho_ext_api::ToolContent::Text {
                        text,
                        audience: None,
                    }]
                }
                maho_ext_api::UserMessageContent::Blocks(blocks) => blocks,
            };
            let deliver_as = options.deliver_as.map(|behavior| match behavior {
                StreamingBehavior::Steer => maho_ext_api::DeliverAs::Steer,
                StreamingBehavior::FollowUp => maho_ext_api::DeliverAs::FollowUp,
            });
            return maho_ext_api::ExtensionActions::send_message(
                self,
                maho_ext_api::CustomMessage {
                    custom_type: "extension-user-message".to_owned(),
                    content,
                    display: true,
                    details: None,
                },
                maho_ext_api::SendMessageOptions {
                    trigger_turn: true,
                    deliver_as,
                },
            );
        }
        self.send_user_message_with(
            content,
            options,
            origin == maho_ext_api::InputOrigin::Person,
        )
    }
    fn append_entry(
        &self,
        custom_type: &str,
        data: Option<Value>,
    ) -> Result<(), maho_ext_api::ExtensionFailure> {
        let session = self.session()?;
        let entry =
            session.with_session_manager_mut(|manager| manager.append_custom(custom_type, data));
        session.emit(AgentSessionEvent::EntryAppended {
            entry: session_entry_from_value(entry),
        });
        Ok(())
    }
    fn get_all_tools(&self) -> Result<Vec<maho_ext_api::ToolInfo>, maho_ext_api::ExtensionFailure> {
        Ok(self.session()?.get_all_tools())
    }
}

impl maho_ext_api::ExtensionSessionActions for SessionExtensionActions {
    fn set_session_name(&self, name: &str) -> Result<(), maho_ext_api::ExtensionFailure> {
        self.session()?.set_session_name(name);
        Ok(())
    }
    fn get_session_name(&self) -> Result<Option<String>, maho_ext_api::ExtensionFailure> {
        Ok(self.session()?.session_name())
    }
    fn set_label(
        &self,
        id: &str,
        label: Option<&str>,
    ) -> Result<(), maho_ext_api::ExtensionFailure> {
        self.session()?
            .with_session_manager_mut(|manager| manager.append_label(id, label))?;
        Ok(())
    }
    fn execute_tool<'a>(
        &'a self,
        name: &'a str,
        params: Value,
        options: maho_ext_api::ExecuteToolOptions,
    ) -> maho_ext_api::ExecuteToolFuture<'a> {
        Box::pin(async move {
            let session = self
                .session()
                .map_err(|error| maho_ext_api::ExecuteToolError {
                    code: maho_ext_api::ExecuteToolErrorCode::Blocked,
                    tool_name: name.to_owned(),
                    message: error.message,
                    active_tools: Vec::new(),
                })?;
            session
                .execute_tool_with_updates(
                    name,
                    params,
                    ExecuteToolOptions {
                        signal: options.signal,
                        activate_inactive_tool: options.activate_inactive_tool,
                    },
                    options.on_update,
                    None,
                    false,
                )
                .await
                .map_err(|error| maho_ext_api::ExecuteToolError {
                    code: match error.code.as_str() {
                        "unknown_tool" => maho_ext_api::ExecuteToolErrorCode::UnknownTool,
                        "inactive_tool" => maho_ext_api::ExecuteToolErrorCode::InactiveTool,
                        "invalid_params" => maho_ext_api::ExecuteToolErrorCode::InvalidParams,
                        _ => maho_ext_api::ExecuteToolErrorCode::Blocked,
                    },
                    tool_name: error.tool_name,
                    message: error.message,
                    active_tools: error.active_tools,
                })
        })
    }
    fn get_active_tools(&self) -> Result<Vec<String>, maho_ext_api::ExtensionFailure> {
        Ok(self.session()?.get_active_tool_names())
    }
    fn set_active_tools(&self, names: Vec<String>) -> Result<(), maho_ext_api::ExtensionFailure> {
        self.session()?.set_active_tools_by_name(names);
        Ok(())
    }
    fn refresh_tools(&self) -> Result<(), maho_ext_api::ExtensionFailure> {
        self.session()?.publish_eval_only_tool_hints();
        Ok(())
    }
    fn install_registered_tool(
        &self,
        registered: maho_ext_api::RegisteredTool,
    ) -> Result<(), maho_ext_api::ExtensionFailure> {
        let session = self.session()?;
        let binding = session.state().extension_tool_context.clone();
        if let Some((runtime, factory)) = binding {
            let definition = registered.definition.clone();
            let source = registered.source_info.clone();
            let tool = session.wrap_extension_tool(registered, runtime, factory);
            let mut active = session.get_active_tool_names();
            if normalize_tool_exposure(&definition, source.clone()).exposure != ToolExposure::Search
                && !active.contains(&definition.name)
            {
                active.push(definition.name.clone());
            }
            session.register_extension_tool(definition, source, tool);
            session.set_active_tools_by_name(active);
        }
        self.refresh_tools()
    }
    fn register_removed_tool_hint(
        &self,
        name: &str,
        hint: &str,
    ) -> Result<(), maho_ext_api::ExtensionFailure> {
        let session = self.session()?;
        let mut hints = session.agent.removed_tool_hints();
        session
            .state()
            .extension_hint_backups
            .entry(name.to_owned())
            .or_insert_with(|| hints.get(name).cloned());
        hints.insert(name.to_owned(), hint.to_owned());
        session.agent.set_removed_tool_hints(hints);
        Ok(())
    }
    fn register_lazy_tool_activator(
        &self,
        activator: maho_ext_api::LazyToolActivator,
    ) -> Result<(), maho_ext_api::ExtensionFailure> {
        let session = self.session()?;
        session
            .state()
            .extension_lazy_activators
            .push(activator.clone());
        session.add_lazy_tool_activator(activator);
        Ok(())
    }
    fn register_native_tool_search_injection_failure_source(
        &self,
        source: maho_ext_api::NativeToolSearchInjectionFailureSource,
    ) -> Result<(), maho_ext_api::ExtensionFailure> {
        self.session()?
            .state()
            .native_tool_search_injection_failure_source = Some(source);
        Ok(())
    }
    fn get_commands(
        &self,
    ) -> Result<Vec<maho_ext_api::SlashCommandInfo>, maho_ext_api::ExtensionFailure> {
        Ok(self.session()?.get_commands())
    }
    fn set_model(&self, model: Model) -> maho_ext_api::ExtensionFuture<'_, bool> {
        Box::pin(async move {
            let session = self.session()?;
            if !session.model_registry().has_configured_auth(&model) {
                return Ok(false);
            }
            session
                .set_model(model)
                .await
                .map(|_| true)
                .map_err(maho_ext_api::ExtensionFailure::new)
        })
    }
    fn get_thinking_level(&self) -> Result<ThinkingLevel, maho_ext_api::ExtensionFailure> {
        Ok(
            thinking_level_from_model_level(self.session()?.thinking_level())
                .unwrap_or(ThinkingLevel::Minimal),
        )
    }
    fn set_thinking_level(
        &self,
        level: ThinkingLevel,
    ) -> Result<(), maho_ext_api::ExtensionFailure> {
        self.session()?
            .set_thinking_level(extension_thinking_level(level));
        Ok(())
    }
    fn set_session_model(&self, model: Model) -> maho_ext_api::ExtensionFuture<'_, bool> {
        Box::pin(async move {
            let session = self.session()?;
            if !session.model_registry().has_configured_auth(&model) {
                return Ok(false);
            }
            session
                .set_session_model(model)
                .await
                .map(|_| true)
                .map_err(maho_ext_api::ExtensionFailure::new)
        })
    }
    fn set_session_thinking_level(
        &self,
        level: ThinkingLevel,
    ) -> Result<(), maho_ext_api::ExtensionFailure> {
        self.session()?
            .set_session_thinking_level(extension_thinking_level(level));
        Ok(())
    }
    fn get_model_thinking_level(
        &self,
    ) -> Result<ModelThinkingLevel, maho_ext_api::ExtensionFailure> {
        Ok(self.session()?.thinking_level())
    }
    fn set_model_thinking_level(
        &self,
        level: ModelThinkingLevel,
    ) -> Result<(), maho_ext_api::ExtensionFailure> {
        self.session()?.set_thinking_level(level);
        Ok(())
    }
    fn set_session_model_thinking_level(
        &self,
        level: ModelThinkingLevel,
    ) -> Result<(), maho_ext_api::ExtensionFailure> {
        self.session()?.set_session_thinking_level(level);
        Ok(())
    }
    fn set_session_fast_mode(&self, enabled: bool) -> Result<(), maho_ext_api::ExtensionFailure> {
        self.session()?.set_session_fast_mode(enabled);
        Ok(())
    }
    fn exec<'a>(
        &'a self,
        command: &'a str,
        args: &'a [String],
        cwd: &'a std::path::Path,
        options: maho_ext_api::ExecOptions,
    ) -> maho_ext_api::ExtensionFuture<'a, maho_ext_api::ExecResult> {
        Box::pin(async move {
            self.session()?;
            Ok(maho_ext_host::exec::exec_command(command, args, cwd, options).await)
        })
    }
}

impl maho_ext_api::ExtensionCommandContextActions for SessionExtensionActions {
    fn wait_for_idle(&self) -> maho_ext_api::ExtensionFuture<'_, ()> {
        Box::pin(async move {
            self.session()?.wait_for_idle().await;
            Ok(())
        })
    }
    fn new_session(
        &self,
        options: maho_ext_api::NewSessionOptions,
    ) -> maho_ext_api::ExtensionFuture<'_, maho_ext_api::SessionNavigationResult> {
        Box::pin(async move {
            let session = self.session()?;
            let changed = session
                .new_session(Some(crate::session_manager::NewSessionOptions {
                    parent_session: options.parent_session,
                    ..Default::default()
                }))
                .await
                .map_err(maho_ext_api::ExtensionFailure::new)?;
            if changed {
                session
                    .run_replacement_callbacks(options.setup, options.with_session)
                    .await?;
            }
            Ok(maho_ext_api::SessionNavigationResult {
                cancelled: !changed,
            })
        })
    }
    fn fork<'a>(
        &'a self,
        entry_id: &'a str,
        options: maho_ext_api::ForkOptions,
    ) -> maho_ext_api::ExtensionFuture<'a, maho_ext_api::SessionNavigationResult> {
        Box::pin(async move {
            let session = self.session()?;
            let result = session
                .fork(
                    entry_id,
                    options.position == Some(maho_ext_api::ForkPosition::At),
                )
                .await
                .map_err(maho_ext_api::ExtensionFailure::new)?;
            if !result.cancelled {
                session
                    .run_replacement_callbacks(None, options.with_session)
                    .await?;
            }
            Ok(maho_ext_api::SessionNavigationResult {
                cancelled: result.cancelled,
            })
        })
    }
    fn navigate_tree<'a>(
        &'a self,
        target_id: &'a str,
        options: maho_ext_api::ExtensionTreeNavigationOptions,
    ) -> maho_ext_api::ExtensionFuture<'a, maho_ext_api::SessionNavigationResult> {
        Box::pin(async move {
            let result = self
                .session()?
                .navigate_tree(
                    target_id,
                    TreeNavigationOptions {
                        summarize: options.summarize,
                        custom_instructions: options.custom_instructions,
                        replace_instructions: options.replace_instructions,
                        label: options.label,
                        expected_leaf_id: options.expected_leaf_id,
                        intent: None,
                    },
                )
                .await
                .map_err(maho_ext_api::ExtensionFailure::new)?;
            Ok(maho_ext_api::SessionNavigationResult {
                cancelled: result.cancelled,
            })
        })
    }
    fn edit_assistant_message<'a>(
        &'a self,
        entry_id: &'a str,
        text: &'a str,
        options: maho_ext_api::EditMessageOptions,
    ) -> maho_ext_api::ExtensionFuture<'a, maho_ext_api::EditMessageResult> {
        Box::pin(async move {
            let result = self
                .session()?
                .edit_assistant_message(
                    entry_id,
                    text,
                    TreeNavigationOptions {
                        summarize: options.summarize,
                        custom_instructions: options.custom_instructions,
                        expected_leaf_id: options.expected_leaf_id,
                        ..Default::default()
                    },
                )
                .await
                .map_err(maho_ext_api::ExtensionFailure::new)?;
            Ok(maho_ext_api::EditMessageResult {
                cancelled: result.cancelled,
                unchanged: result.unchanged,
                entry_id: result.entry_id,
            })
        })
    }
    fn edit_user_message<'a>(
        &'a self,
        entry_id: &'a str,
        text: &'a str,
        options: maho_ext_api::EditMessageOptions,
    ) -> maho_ext_api::ExtensionFuture<'a, maho_ext_api::EditMessageResult> {
        Box::pin(async move {
            let result = self
                .session()?
                .edit_user_message(
                    entry_id,
                    text,
                    TreeNavigationOptions {
                        summarize: options.summarize,
                        custom_instructions: options.custom_instructions,
                        expected_leaf_id: options.expected_leaf_id,
                        ..Default::default()
                    },
                )
                .await
                .map_err(maho_ext_api::ExtensionFailure::new)?;
            Ok(maho_ext_api::EditMessageResult {
                cancelled: result.cancelled,
                unchanged: result.unchanged,
                entry_id: result.entry_id,
            })
        })
    }
    fn switch_session<'a>(
        &'a self,
        path: &'a str,
        options: maho_ext_api::SwitchSessionOptions,
    ) -> maho_ext_api::ExtensionFuture<'a, maho_ext_api::SessionNavigationResult> {
        Box::pin(async move {
            let session = self.session()?;
            let changed = session
                .switch_session(path)
                .await
                .map_err(maho_ext_api::ExtensionFailure::new)?;
            if changed {
                session
                    .run_replacement_callbacks(None, options.with_session)
                    .await?;
            }
            Ok(maho_ext_api::SessionNavigationResult {
                cancelled: !changed,
            })
        })
    }
    fn reload(&self) -> maho_ext_api::ExtensionFuture<'_, ()> {
        Box::pin(async move {
            self.session()?
                .reload()
                .await
                .map(|_| ())
                .map_err(maho_ext_api::ExtensionFailure::new)
        })
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
        Self {
            session: session.clone(),
            id: session.session_id(),
            file: session.session_file().map(Into::into),
            actions: SessionExtensionActions(Arc::downgrade(&session.inner)),
        }
    }
}

#[cfg(test)]
impl maho_ext_api::ToolSessionManager for SessionContextManager {
    fn session_id(&self) -> &str {
        &self.id
    }
    fn session_file(&self) -> Option<&std::path::Path> {
        self.file.as_deref()
    }
}

#[cfg(test)]
impl maho_ext_api::SessionManager for SessionContextManager {
    fn get_usage_totals(&self) -> Option<Value> {
        Some(
            self.session
                .with_session_manager(|manager| manager.extension_usage_totals()),
        )
    }
    fn get_entries(&self) -> Vec<maho_ext_api::SessionEntry> {
        self.session
            .with_session_manager(|manager| manager.entries())
            .into_iter()
            .map(session_entry_from_value)
            .collect()
    }
    fn get_branch(&self) -> Vec<maho_ext_api::SessionEntry> {
        self.session
            .with_session_manager(|manager| manager.current_branch())
            .into_iter()
            .map(session_entry_from_value)
            .collect()
    }
    fn get_leaf_id(&self) -> Option<String> {
        self.session
            .with_session_manager(|manager| manager.leaf_id().map(str::to_owned))
    }
    fn get_session_name(&self) -> Option<String> {
        self.session.session_name()
    }
    fn is_child_session(&self) -> bool {
        self.session.with_session_manager(|manager| {
            manager.header().is_some_and(|header| {
                header
                    .get("parentSession")
                    .and_then(serde_json::Value::as_str)
                    .is_some()
            })
        })
    }
    fn fork_provenance(&self) -> Option<maho_ext_api::ForkProvenance> {
        self.session
            .with_session_manager(|manager| manager.fork_provenance())
    }
    fn extension_context_actions(&self) -> Option<&dyn maho_ext_api::ExtensionContextActions> {
        Some(&self.actions)
    }
}

impl crate::retry_fallback::controller::RetryFallbackDeps for SessionFallbackDeps {
    fn settings(&self) -> crate::retry_fallback::settings::ResolvedRetryFallbackSettings {
        let settings = self
            .0
            .upgrade()
            .map(|inner| AgentSession { inner })
            .and_then(|session| {
                session.with_settings_manager(|manager| manager.get_value("retry").cloned())
            });
        crate::retry_fallback::settings::resolve_retry_fallback_settings(settings.as_ref())
    }
    fn models(&self) -> Vec<Model> {
        self.0
            .upgrade()
            .map_or_else(Vec::new, |inner| inner.model_registry.get_all())
    }
    fn current(&self) -> Option<(Model, Option<ModelThinkingLevel>)> {
        self.0.upgrade().map(|inner| {
            let state = inner.agent.state();
            (state.model, Some(state.thinking_level))
        })
    }
    fn is_auth_available(&self, provider: &str) -> bool {
        self.0.upgrade().is_some_and(|inner| {
            inner.model_registry.get_all().iter().any(|model| {
                model.provider == provider && inner.model_registry.has_configured_auth(model)
            })
        })
    }
    fn is_using_oauth(&self, model: &Model) -> bool {
        self.0
            .upgrade()
            .is_some_and(|inner| inner.model_registry.is_using_oauth(model))
    }
    fn is_fallback_eligible(&self, model: &Model) -> bool {
        self.0
            .upgrade()
            .is_some_and(|inner| inner.model_registry.is_fallback_eligible(model))
    }
    fn has_configured_auth(&self, model: &Model) -> bool {
        self.0
            .upgrade()
            .is_some_and(|inner| inner.model_registry.has_configured_auth(model))
    }
    fn log(
        &mut self,
        level: crate::retry_fallback::controller::FallbackLogLevel,
        event: &str,
        data: &[(&str, Option<&str>)],
    ) {
        let data: Map<String, Value> = data
            .iter()
            .filter_map(|(key, value)| {
                value.map(|value| ((*key).to_owned(), Value::String(value.to_owned())))
            })
            .collect();
        match level {
            crate::retry_fallback::controller::FallbackLogLevel::Debug => {
                self.1.debug(event, Some(&data))
            }
            crate::retry_fallback::controller::FallbackLogLevel::Info => {
                self.1.info(event, Some(&data))
            }
        }
    }
    fn switch_model<'a>(
        &'a mut self,
        model: Model,
        thinking: ModelThinkingLevel,
        revert: bool,
    ) -> maho_ai::types::BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let inner = self.0.upgrade().ok_or("Session disposed")?;
            let session = AgentSession { inner };
            session
                .switch_model_with_thinking(
                    model,
                    false,
                    if revert {
                        maho_ext_api::ModelSelectSource::FallbackRevert
                    } else {
                        maho_ext_api::ModelSelectSource::Fallback
                    },
                    false,
                    Some(thinking),
                    None,
                )
                .await?;
            Ok(())
        })
    }
    fn emit(&mut self, event: crate::retry_fallback::controller::FallbackEvent) {
        if let Some(inner) = self.0.upgrade() {
            let session = AgentSession { inner };
            use crate::retry_fallback::controller::{FallbackEvent, FallbackReason};
            session.emit(match event {
                FallbackEvent::Applied {
                    from,
                    to,
                    chain_key,
                    reason,
                    limit,
                } => AgentSessionEvent::RetryFallbackApplied {
                    from,
                    to,
                    chain_key,
                    limit,
                    reason: match reason {
                        FallbackReason::Transient => "transient",
                        FallbackReason::Refusal => "refusal",
                        FallbackReason::HardError => "hard-error",
                        FallbackReason::Billing => "billing",
                    }
                    .to_owned(),
                },
                FallbackEvent::Reverted { from, to, stranded } => {
                    AgentSessionEvent::RetryFallbackReverted {
                        from,
                        to,
                        cause: stranded.then(|| "fallback-unusable".to_owned()),
                    }
                }
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

/// The host's project-trust inputs for [`AgentSession::switch_session_with_cwd`] into another project
/// (senpi `createServices` for the target cwd): the shared `projectTrustByCwd` cache, `--approve` /
/// `--no-approve`, the host's resolution (the extension `project_trust` event, then the saved decision and
/// `defaultProjectTrust`; the host never prompts in it), and whether an undecided project may be asked through
/// the session's UI (senpi `projectTrustContextFactory`, `hasUI: true` in the interactive mode).
#[derive(Clone, Default)]
pub struct SwitchProjectTrust {
    pub cache: Option<crate::agent_session_runtime::ProjectTrustCache>,
    pub trust_override: Option<bool>,
    pub resolver: Option<crate::agent_session_runtime::ProjectTrustResolver>,
    pub prompt: bool,
}

/// The tracker's owner: the session, held weakly so the tracker stored on it forms no cycle. A session
/// already dropped records nothing and counts as idle.
struct WeakDeadlineOwner(std::sync::Weak<AgentSessionInner>);

impl WeakDeadlineOwner {
    fn session(&self) -> Option<AgentSession> {
        self.0.upgrade().map(|inner| AgentSession { inner })
    }
}

impl crate::turn_deadline::DeadlineSessionOwner for WeakDeadlineOwner {
    fn record_deadline_stop(
        &self,
        deadline: &crate::turn_deadline::TurnDeadline,
    ) -> Result<(), String> {
        self.session()
            .map_or(Ok(()), |session| session.record_deadline_stop(deadline))
    }

    fn abort_turn_for_deadline(&self) -> crate::turn_deadline::DeadlineAbort {
        match self.session() {
            Some(session) => session.abort_turn_for_deadline(),
            None => Box::pin(async { Ok(()) }),
        }
    }

    fn release_held_extension_handlers(&self) -> usize {
        self.session()
            .map_or(0, |session| session.release_held_extension_handlers_now())
    }

    fn is_retry_in_flight(&self) -> bool {
        self.session().is_some_and(|session| session.is_retrying())
    }

    fn is_idle(&self) -> bool {
        self.session()
            .is_none_or(|session| !session.agent_run_active.load(Ordering::SeqCst))
    }

    fn warn_deadline(&self, event: crate::turn_deadline::TurnDeadlineWarning, detail: &str) {
        if let Some(session) = self.session() {
            session.warn_deadline(event, detail);
        }
    }
}

impl AgentSession {
    /// senpi `currentDeadline`: the deadline that stopped the current turn, when the lease fired (senpi#3227).
    pub fn current_deadline(&self) -> Option<crate::turn_deadline::TurnDeadline> {
        self.deadline_tracker
            .get()
            .and_then(crate::turn_deadline::AgentDeadlineTracker::current)
    }

    /// senpi `SessionLifecycleController.deadlineStopped`: captured for the active run only.
    fn lifecycle_deadline_stopped(&self, kind: crate::turn_deadline::TurnDeadlineKind, at_ms: i64) {
        use crate::session_lifecycle_types::LifecycleDeadlineKind;
        let ledger = lock(&self.lifecycle);
        let Some(run_id) = ledger.run_id.clone().filter(|_| ledger.run_active) else {
            return;
        };
        *lock(&self.lifecycle_deadline_stop) = Some(crate::lifecycle_stop::DeadlineStopCapture {
            run_id,
            kind: match kind {
                crate::turn_deadline::TurnDeadlineKind::Inactivity => {
                    LifecycleDeadlineKind::Inactivity
                }
                crate::turn_deadline::TurnDeadlineKind::MaxRuntime => {
                    LifecycleDeadlineKind::MaxRuntime
                }
            },
            at: chrono::DateTime::from_timestamp_millis(at_ms)
                .unwrap_or_default()
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            persisted: None,
        });
    }
}

/// senpi AgentSession's deadline owner adapters (`_recordDeadlineStop`, `abortTurnForDeadline`,
/// `releaseHeldExtensionHandlers`, `isRetryInFlight`, `isIdle`, `_warnDeadline`).
impl crate::turn_deadline::DeadlineSessionOwner for AgentSession {
    fn record_deadline_stop(
        &self,
        deadline: &crate::turn_deadline::TurnDeadline,
    ) -> Result<(), String> {
        let at = chrono::Utc::now().timestamp_millis();
        // The lifecycle stop is captured before the write, so a refused write keeps it for this run (senpi#3864).
        self.lifecycle_deadline_stopped(deadline.kind, at);
        let data = json!({
            "kind": deadline.kind,
            "limitMs": deadline.limit_ms,
            "elapsedMs": deadline.elapsed_ms,
            "at": at,
        });
        let (entry, write_error) = self.with_session_manager_mut(|manager| {
            let entry =
                manager.append_custom(crate::turn_deadline::TURN_DEADLINE_ENTRY_TYPE, Some(data));
            (entry, manager.last_persistence_error().map(str::to_owned))
        });
        if let Some(error) = write_error {
            // senpi `_lifecycleBinding.deadlineStopWriteFailed()`.
            if let Some(stop) = lock(&self.lifecycle_deadline_stop).as_mut() {
                stop.persisted = Some(false);
            }
            return Err(error);
        }
        if let Some(id) = entry.get("id").and_then(Value::as_str) {
            self.emit_entry_appended(id);
        }
        Ok(())
    }

    fn abort_turn_for_deadline(&self) -> crate::turn_deadline::DeadlineAbort {
        let session = self.clone();
        Box::pin(async move {
            session.abort_for_deadline().await;
            Ok(())
        })
    }

    fn release_held_extension_handlers(&self) -> usize {
        self.release_held_extension_handlers_now()
    }

    fn is_retry_in_flight(&self) -> bool {
        self.is_retrying()
    }

    fn is_idle(&self) -> bool {
        !self.agent_run_active.load(Ordering::SeqCst)
    }

    fn warn_deadline(&self, event: crate::turn_deadline::TurnDeadlineWarning, detail: &str) {
        let mut data = Map::new();
        data.insert("detail".to_owned(), Value::from(detail));
        self.session_logger.warn(event.as_str(), Some(&data));
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
    headers.map(|headers| {
        headers
            .into_iter()
            .filter_map(|(key, value)| value.map(|value| (key, value)))
            .collect()
    })
}

/// senpi `_mintQueuedId`: the next session-unique queued-input id, `q_<n>`.
fn mint_queued_id(state: &mut AgentSessionState) -> String {
    state.next_queued_id += 1;
    format!("q_{}", state.next_queued_id)
}

/// senpi `_changeQueuedInput`'s edit: the same user message with its text replaced, its images kept.
fn with_user_text(message: &AgentMessage, text: &str) -> AgentMessage {
    let Some(maho_ai::types::Message::User(user)) = message.try_as_llm() else {
        return message.clone();
    };
    let images: Vec<ImageContent> = match &user.content {
        maho_ai::types::UserContent::Text(_) => Vec::new(),
        maho_ai::types::UserContent::Blocks(blocks) => blocks
            .iter()
            .filter_map(|block| match block {
                maho_ai::types::ContentBlock::Image(image) => Some(image.clone()),
                _ => None,
            })
            .collect(),
    };
    let mut content = vec![maho_ai::types::ContentBlock::Text(
        maho_ai::types::TextContent {
            text: text.to_owned(),
            ..Default::default()
        },
    )];
    content.extend(images.into_iter().map(maho_ai::types::ContentBlock::Image));
    maho_ai::types::Message::User(maho_ai::types::UserMessage {
        content: maho_ai::types::UserContent::Blocks(content),
        timestamp: user.timestamp,
    })
    .into()
}

/// The session JSONL as the pending-input store's journal session (upstream passes `sessionManager`).
struct SessionJournal(std::sync::Weak<AgentSessionInner>);

impl SessionJournal {
    fn session(&self) -> Option<AgentSession> {
        self.0.upgrade().map(|inner| AgentSession { inner })
    }
}

#[async_trait::async_trait]
impl crate::pending_input_journal::JournalSession for SessionJournal {
    fn append_custom_entry(&self, custom_type: &str, data: Value) {
        if let Some(session) = self.session() {
            session.with_session_manager_mut(|manager| {
                manager.append_custom(custom_type, Some(data));
            });
        }
    }
    async fn persist_header_now(&self) -> Result<(), String> {
        match self.session() {
            Some(session) => {
                session.with_session_manager_mut(|manager| manager.persist_header_now())
            }
            None => Ok(()),
        }
    }
    fn entries(&self) -> Vec<Value> {
        self.session()
            .map(|session| session.with_session_manager(|manager| manager.entries()))
            .unwrap_or_default()
    }
    fn session_file(&self) -> Option<String> {
        self.session().and_then(|session| {
            session.with_session_manager(|manager| manager.session_file().map(str::to_owned))
        })
    }
    fn is_persisted(&self) -> bool {
        self.session()
            .is_some_and(|session| session.with_session_manager(|manager| manager.is_persisted()))
    }
    fn is_transcript_flushed(&self) -> bool {
        self.session()
            .is_some_and(|session| session.with_session_manager(|manager| manager.is_flushed()))
    }
}

/// senpi `registerMemoryReportSession(this, {...})`: read only when a report is requested; a dropped session
/// reports empty figures until its dispose unregisters it.
struct SessionMemoryReportSource(std::sync::Weak<AgentSessionInner>);

impl crate::memory_report::memory_report_registry::MemoryReportSessionSource
    for SessionMemoryReportSource
{
    fn session_id(&self) -> String {
        self.0
            .upgrade()
            .map(|inner| AgentSession { inner }.session_id())
            .unwrap_or_default()
    }
    fn session_file(&self) -> Option<String> {
        self.0
            .upgrade()
            .and_then(|inner| AgentSession { inner }.session_file())
    }
    fn resident_store(&self) -> crate::session_resident_store_size::ResidentStoreSize {
        self.0
            .upgrade()
            .map(|inner| {
                let stats = AgentSession { inner }
                    .with_session_manager(|manager| manager.resident_store_stats());
                crate::session_resident_store_size::ResidentStoreSize {
                    entries: stats.blob_count,
                    approx_bytes: stats.blob_bytes,
                }
            })
            .unwrap_or_default()
    }
    fn reporters(&self) -> Vec<maho_ext_api::NamedMemoryReporter> {
        self.0
            .upgrade()
            .map(|inner| AgentSession { inner }.extension_memory_reporters())
            .unwrap_or_default()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
thread_local! {
    /// Runs once inside the idle release, after it read the runtime messages and before it commits
    /// the tokenized copy, so a test can force a park into that window.
    static IDLE_RELEASE_BEFORE_COMMIT: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

type ToolChildHostTargets = Arc<
    Mutex<
        Vec<(
            crate::tool_child_host_calls::RegisteredHostCallKind,
            crate::tool_child_host_calls::HostCallTarget,
        )>,
    >,
>;

type SessionCell = Arc<std::sync::OnceLock<std::sync::Weak<AgentSessionInner>>>;

fn upgrade_session_cell(cell: &SessionCell) -> Option<AgentSession> {
    cell.get()
        .and_then(std::sync::Weak::upgrade)
        .map(|inner| AgentSession { inner })
}

/// Claims a released wake once its turn is admitted; `false` leaves the wake in custody.
type ReleasedWakeAdmit = Box<dyn Fn() -> bool + Send + Sync>;

/// The erased handler a native bus channel carries: a callback over the `Arc<dyn Any>` payload
/// that each subscriber downcasts to its own concrete snapshot type.
type ErasedHandler = Arc<dyn Fn(&Arc<dyn std::any::Any + Send + Sync>) + Send + Sync>;

/// Adapt a typed MCP wire-status handler to the erased `Arc<dyn Any>` the bus channel carries,
/// downcasting per emission so the channel needs no concrete type from maho-core.
fn downcast_handler<T: Send + Sync + 'static>(
    handler: Arc<dyn Fn(&T) + Send + Sync>,
) -> ErasedHandler {
    Arc::new(move |value| {
        if let Some(snapshot) = value.downcast_ref::<T>() {
            handler(snapshot);
        }
    })
}

impl AgentSession {
    /// Create a new agent session.
    pub fn new(config: AgentSessionConfig) -> Result<Self, MissingModelAccessError> {
        Self::new_with_tool_runtime(config, None)
    }

    /// senpi `new AgentSession({ ..., toolRuntime })`: `None` is the `in-process` runtime. Kept beside `new`
    /// so the many `AgentSessionConfig` literals that never choose a runtime stay valid.
    pub fn new_with_tool_runtime(
        config: AgentSessionConfig,
        tool_runtime: Option<crate::tool_dispatch::tool_dispatcher::ToolRuntimeKind>,
    ) -> Result<Self, MissingModelAccessError> {
        Self::new_with_internal_channel(config, tool_runtime, None)
    }

    /// senpi `new AgentSession({ ..., internalChannel })`: the channel sdk.ts created and handed to this session's
    /// Agent at construction; the structured lane reads it, nothing else does.
    pub fn new_with_internal_channel(
        mut config: AgentSessionConfig,
        tool_runtime: Option<crate::tool_dispatch::tool_dispatcher::ToolRuntimeKind>,
        internal_channel: Option<maho_agent::InternalAgentChannel>,
    ) -> Result<Self, MissingModelAccessError> {
        if config
            .flag_values
            .get("no-model-fallback")
            .is_some_and(|value| matches!(value, FlagValue::Boolean(true)))
            || std::env::var("NO_FALLBACK").as_deref() == Ok("1")
        {
            config.settings_manager.apply_overrides(&Map::from_iter([(
                "retry".to_owned(),
                serde_json::json!({"modelFallback":false}),
            )]));
        }
        if config
            .flag_values
            .get("no-ask-user")
            .is_some_and(|value| matches!(value, FlagValue::Boolean(true)))
        {
            config.settings_manager.apply_overrides(&Map::from_iter([(
                "askUser".to_owned(),
                serde_json::json!({"enabled":false}),
            )]));
        }
        let model_registry = match (config.model_runtime, config.model_registry) {
            (Some(runtime), _) => ModelRegistry::new(runtime),
            (None, Some(registry)) => registry,
            (None, None) => return Err(MissingModelAccessError),
        };
        let agent_dir = config
            .agent_dir
            .clone()
            .unwrap_or_else(crate::config::get_agent_dir);
        let fallback_logger_dir = agent_dir.clone();
        let fallback_validation_warnings =
            crate::retry_fallback::validate::validate_fallback_chains(
                config.settings_manager.get_raw_fallback_chains(),
                &model_registry.get_all(),
            );
        let fallback_chains_source = config
            .settings_manager
            .get_fallback_chains_scope()
            .map(|s| match s {
                crate::settings_manager::SettingsScope::Global => "global",
                crate::settings_manager::SettingsScope::Project => "project",
            })
            .unwrap_or("default");
        // senpi: one fallback logger serves startup validation and the controller's decision trail.
        let mut fallback_logger = crate::retry_fallback::log::FallbackLogger::new(
            std::path::Path::new(&fallback_logger_dir),
            None,
        );
        for warning in &fallback_validation_warnings {
            let mut data = Map::new();
            data.insert("warning".to_owned(), Value::String(warning.clone()));
            data.insert(
                "source".to_owned(),
                Value::String(fallback_chains_source.to_owned()),
            );
            fallback_logger.warn("validation_warning", Some(&data));
        }
        let agent = config.agent;
        let engine_hold_entries = config
            .session_manager
            .reduce_history(&crate::engine_turn_hold::ENGINE_TURN_HOLD_HISTORY)
            .entries;
        let engine_unattended = config
            .session_start_event
            .as_ref()
            .is_some_and(|event| event.attended == Some(false));
        let session_cell: Arc<std::sync::OnceLock<std::sync::Weak<AgentSessionInner>>> =
            Arc::default();
        let session_logger =
            SessionLogger::create(Some(&agent_dir), SessionLoggerOptions::default());
        let (tool_dispatcher, tool_child, tool_child_host_targets) = Self::build_tool_runtime(
            tool_runtime.unwrap_or_default(),
            &session_cell,
            &session_logger,
            &agent_dir,
            &config.cwd,
            config.session_manager.session_id(),
        );
        let engine_turn_hold = {
            let upgrade = |cell: &Arc<std::sync::OnceLock<std::sync::Weak<AgentSessionInner>>>| {
                cell.get()
                    .and_then(std::sync::Weak::upgrade)
                    .map(|inner| AgentSession { inner })
            };
            let append_cell = Arc::clone(&session_cell);
            let append: crate::engine_turn_hold::HoldAppend = Arc::new(move |kind, data| {
                let session = upgrade(&append_cell).ok_or_else(|| "session disposed".to_owned())?;
                let entry = session.with_session_manager_mut(|manager| {
                    let entry = manager.append_custom(kind, Some(data));
                    if let Some(error) = manager.last_persistence_error() {
                        return Err(error.to_owned());
                    }
                    Ok(entry)
                })?;
                if let Some(id) = entry.get("id").and_then(Value::as_str) {
                    session.emit_entry_appended(id);
                }
                Ok(())
            });
            let notify_cell = Arc::clone(&session_cell);
            let notify: crate::engine_turn_hold::HoldNotify = Arc::new(move |message| {
                if let Some(session) = upgrade(&notify_cell) {
                    session.notify_ui(message, maho_ext_api::NotificationType::Info);
                }
            });
            let emit_cell = Arc::clone(&session_cell);
            let emit: crate::engine_turn_hold::HoldEmit = Arc::new(move |event| {
                let Some(session) = upgrade(&emit_cell) else {
                    return;
                };
                // No `AgentSessionEvent` variant exists for the hold events yet, so they ride the session bus.
                let data = match &event {
                    crate::engine_turn_hold::SessionHoldEvent::SessionNotice {
                        notice_ids,
                        notice_count,
                        disposition,
                    } => {
                        serde_json::json!({ "type": "session_notice", "noticeIds": notice_ids, "noticeCount": notice_count, "disposition": disposition })
                    }
                    crate::engine_turn_hold::SessionHoldEvent::SessionPaused {
                        pause_seq,
                        kind,
                        at,
                        notice_count,
                        persisted,
                    } => serde_json::json!({
                        "type": "session_paused", "pauseSeq": pause_seq, "kind": kind.as_str(), "at": at, "noticeCount": notice_count, "persisted": persisted }),
                    crate::engine_turn_hold::SessionHoldEvent::SessionResumed {
                        pause_seq,
                        cause,
                        at,
                    } => serde_json::json!({
                        "type": "session_resumed", "pauseSeq": pause_seq, "cause": cause.as_str(), "at": at }),
                };
                session.emit_extension_event(SESSION_HOLD_CHANNEL, &data);
                let session_event = match &event {
                    crate::engine_turn_hold::SessionHoldEvent::SessionNotice { .. } => return,
                    crate::engine_turn_hold::SessionHoldEvent::SessionPaused {
                        pause_seq,
                        kind,
                        at,
                        notice_count,
                        persisted,
                    } => maho_ext_api::AgentSessionEvent::SessionPaused {
                        pause_seq: *pause_seq,
                        kind: kind.as_str().to_string(),
                        at: at.clone(),
                        notice_count: *notice_count,
                        persisted: *persisted,
                    },
                    crate::engine_turn_hold::SessionHoldEvent::SessionResumed {
                        pause_seq,
                        cause,
                        at,
                    } => maho_ext_api::AgentSessionEvent::SessionResumed {
                        pause_seq: *pause_seq,
                        cause: cause.as_str().to_string(),
                        at: at.clone(),
                    },
                };
                session.emit(session_event);
            });
            Arc::new(Mutex::new(crate::engine_turn_hold::EngineTurnHold::new(
                &engine_hold_entries,
                engine_unattended,
                append,
                notify,
                emit,
            )))
        };
        let state = AgentSessionState {
            scoped_models: config.scoped_models,
            favorite_models: config.favorite_models,
            cwd: config.cwd,
            agent_dir: agent_dir.clone(),
            custom_tools: config.custom_tools,
            initial_active_tool_names: config.initial_active_tool_names,
            default_tool_names: name_set(config.default_tool_names),
            eval_only_tool_names: Some(
                name_set(config.eval_only_tool_names.clone()).unwrap_or_else(|| {
                    EVAL_ONLY_TOOL_NAMES
                        .iter()
                        .map(|name| (*name).to_owned())
                        .collect()
                }),
            ),
            allowed_tool_names: name_set(config.allowed_tool_names),
            excluded_tool_names: name_set(config.excluded_tool_names),
            withheld_excluded_tool_names: BTreeSet::new(),
            extension_command_gate: None,
            user_bash_denied: false,
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
                sizes_only: false,
                reason: SessionReason::Startup,
                initial_model_provenance: None,
                previous_session_file: None,
                attended: None,
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
            loaded_context_files: None,
            resolved_system_prompt: None,
            system_prompt_override: None,
            prompt_surface: None,
            browser_engine: None,
            wake_sources: WakeSourceTracker::default(),
            security_notices: Default::default(),
            shown_high_reasoning_warning_keys: BTreeSet::new(),
            extension_mode: ExtensionMode::Print,
            extension_ui_context: None,
            extension_bound_ui: None,
            extension_abort_handler: None,
            extension_shutdown_handler: None,
            extension_error_listener: None,
            extension_lifecycle_started: false,
            message_revision: 0,
            assistant_generation: 0,
            last_persisted_assistant: None,
            post_compaction_assistant_generation: None,
            post_compaction_usage_exempt_entries: BTreeSet::new(),
            session_stats_cache: None,
            context_usage_cache: None,
            overflow_recovery_rungs: 0,
            compaction_skipped_too_small: false,
            messages_awaiting_persistence: Vec::new(),
            steering_messages: Vec::new(),
            follow_up_messages: Vec::new(),
            queued_input_order: Vec::new(),
            queued_input_ages: Default::default(),
            next_queued_input_order: 0,
            next_queued_id: 0,
            queued_messages: Vec::new(),
            delivered_queued_ids: std::collections::VecDeque::new(),
            post_compaction_deferred_steering_messages: Vec::new(),
            post_compaction_deferred_follow_up_messages: Vec::new(),
            prompt_start_pending: false,
            had_cleared_queued_messages: false,
            trigger_turn_admission_abort_generation: None,
            required_compaction_turn_error: None,
            required_compaction_admission_error: None,
            auto_compaction_session_override: None,
            turn_index: 0,
            message_replacements: Vec::new(),
            retry_attempt: 0,
            model_config_errors: Default::default(),
            retry_abort_controller: None,
            user_aborted: false,
            abort_source: None,
            abort_provenance: Default::default(),
            probe_phase: crate::retry_fallback::hint_policy::ProbePhase::Idle,
            hint_deadline_ms: None,
            cumulative_hinted_wait_ms: 0.0,
            pending_model_switch: None,
            compaction_model_fallback_notice: None,
            spent_accounts: Default::default(),
            compaction_abort_controller: None,
            auto_compaction_owned: false,
            compaction_signal_bridge: None,
            pending_compaction_admission: None,
            compaction_lifecycle: Default::default(),
            delegated_compaction_key: None,
            prompt_templates: Vec::new(),
            extension_commands: Vec::new(),
            extension_command_catalog: None,
            extension_runner_snapshot: None,
            extension_event_sender: None,
            extension_events: None,
            mcp_wire_status: None,
            extension_tool_context: None,
            extension_tool_backups: BTreeMap::new(),
            extension_lazy_activators: Vec::new(),
            native_tool_search_injection_failure_source: None,
            extension_hint_backups: BTreeMap::new(),
            skills: Vec::new(),
            skill_diagnostics: Vec::new(),
            prompt_diagnostics: Vec::new(),
            theme_diagnostics: Vec::new(),
            resource_loader_inputs: Default::default(),
            skills_loaded: false,
            discovered_resources: maho_ext_api::DiscoveredResources::default(),
            global_hook_source_paths: Vec::new(),
            project_hook_source_paths: Vec::new(),
            pre_session_hook_source_paths: Vec::new(),
            loaded_hook_sources: None,
            bash_abort_signals: BTreeMap::new(),
            pending_bash_messages: Vec::new(),
            pending_next_turn_messages: Vec::new(),
            pending_custom_messages: Vec::new(),
            extension_event_signal: None,
            compaction_extension_signal: None,
            branch_summary_abort_controller: None,
            session_title_abort_controller: None,
            active_compaction_log_attempt: None,
            superseded_compaction_log_attempt_ids: BTreeSet::new(),
            resume_compaction_requirement: None,
            standing_recovery: None,
            resume_slice: None,
            sizes_only_startup: false,
            prompt_size_inventory: None,
            disposed: false,
        };
        let lifecycle = crate::lifecycle_ledger::LifecycleLedger::new(
            config.session_manager.session_id(),
            config
                .session_manager
                .header()
                .as_ref()
                .and_then(|h| h.get("spawnedBy")),
            &config.session_manager.entries_of_types(&["custom"]),
        );
        let restored_run_id = lifecycle.run_id.clone();
        let held_inputs = crate::held_inputs::HeldInputs::new(
            &config.session_manager.entries_of_types(&["custom"]),
        );
        let session = Self {
            inner: Arc::new(AgentSessionInner {
                agent,
                lifecycle: Mutex::new(lifecycle),
                held_inputs: Mutex::new(held_inputs),
                pending_inputs: std::sync::OnceLock::new(),
                delivered_held_inputs: Mutex::new(Vec::new()),
                held_preparation: tokio::sync::Mutex::new(()),
                reserved_held_inputs: Mutex::new(BTreeSet::new()),
                observed_held_inputs: Mutex::new(BTreeSet::new()),
                external_admission: Default::default(),
                stop_interrupt: Mutex::new(None),
                lifecycle_human_pending: std::sync::atomic::AtomicBool::new(false),
                lifecycle_disposed: std::sync::atomic::AtomicBool::new(false),
                last_provider_failure: Mutex::new(None),
                lifecycle_goal: Mutex::new(None),
                lifecycle_turn_facts: Mutex::new(None),
                lifecycle_stop_facts: Mutex::new((None, restored_run_id)),
                lifecycle_activity: Mutex::new(Default::default()),
                structured_lane: crate::structured_session_lane::StructuredSessionLane::new(
                    internal_channel,
                ),
                memory_report_unregister: Mutex::new(None),
                recovery_identity: Mutex::new(
                    crate::engine_socket_flag::compact_updates_enabled()
                        .then(crate::session_recovery_identity::SessionRecoveryIdentity::default),
                ),
                session_manager: Mutex::new(config.session_manager),
                settings_manager: Arc::new(Mutex::new(config.settings_manager)),
                model_registry,
                state: Mutex::new(state),
                extension_runner: tokio::sync::Mutex::new(None),
                boundary_context_changed: AtomicBool::new(false),
                extension_runner_epoch: AtomicU64::new(0),
                handler_turn_scope: Arc::new(AtomicU64::new(0)),
                deadline_tracker: std::sync::OnceLock::new(),
                lifecycle_deadline_stop: Mutex::new(None),
                switch_project_trust: Mutex::new(None),
                event_bus: EventBus::new(),
                event_listeners: Arc::new(Mutex::new(Arc::new(Vec::new()))),
                next_listener_id: Arc::new(AtomicU64::new(0)),
                human_input_listeners: Arc::new(Mutex::new(Vec::new())),
                session_logger,
                fallback_now: config
                    .fallback_now
                    .unwrap_or_else(|| Arc::new(|| maho_ai::utils::diagnostics::now_ms() as f64)),
                retry_random: config.retry_random.unwrap_or_else(|| Arc::new(rand_unit)),
                fallback_validation_warnings,
                agent_subscription: Mutex::new(None),
                prompt_admission: tokio::sync::Mutex::new(()),
                nested_tool_queue: tokio::sync::Mutex::new(()),
                reload_admission: tokio::sync::Mutex::new(()),
                binding_readiness: Mutex::new(None),
                retry_fallback: tokio::sync::Mutex::new(None),
                // Work held by the barrier is a lifecycle fact: its release is an edge (senpi#3980).
                work_barrier: Arc::new(
                    crate::session_work_barrier::SessionWorkBarrier::with_on_active_change({
                        let session_cell = session_cell.clone();
                        Arc::new(move || {
                            if let Some(inner) =
                                session_cell.get().and_then(std::sync::Weak::upgrade)
                            {
                                AgentSession { inner }.refresh_lifecycle();
                            }
                        })
                    }),
                ),
                relocation_host: Mutex::new(None),
                control_endpoint_host: Mutex::new(None),
                session_control: Mutex::new(None),
                tool_barrier: crate::session_work_barrier::ToolBarrier::new({
                    let session_cell = session_cell.clone();
                    move |spec| {
                        if let Some(inner) = session_cell.get().and_then(std::sync::Weak::upgrade) {
                            AgentSession { inner }.emit_extension_event("tool_barrier_expired", &json!({"type":"tool_barrier_expired","id":spec.id,"deadlineMs":spec.deadline_ms}));
                        }
                    }
                }),
                session_thinking_memory: Mutex::new(
                    crate::session_thinking_memory::SessionThinkingMemory::new(),
                ),
                settling_with_background_work: std::sync::atomic::AtomicBool::new(false),
                agent_run_active: std::sync::atomic::AtomicBool::new(false),
                runtime_messages_tokenized: std::sync::atomic::AtomicBool::new(false),
                idle_park_generation: Mutex::new(0),
                wake_source_subscription: Mutex::new(None),
                lifecycle_fact_subscriptions: Mutex::new(Vec::new()),
                extension_rpc_event_forward: Mutex::new(None),
                settings_source_subscription: Mutex::new(None),
                before_session_invalidate: Mutex::new(None),
                before_session_replace: Mutex::new(Vec::new()),
                next_before_session_replace: AtomicU64::new(0),
                settled_delivery: Mutex::new(
                    crate::agent_settled_delivery::AgentSettledDelivery::new(),
                ),
                user_abort_generation: AtomicU64::new(0),
                monitor_generation: AtomicU64::new(0),
                settlement_epoch: AtomicU64::new(0),
                probe_scheduler: Mutex::new(
                    crate::retry_fallback::probe_scheduler::ProbeBackScheduler::default(),
                ),
                request_shape_recovery: Mutex::new(Default::default()),
                engine_turn_hold,
                engine_turn_release: crate::engine_turn_release::EngineTurnRelease::new(),
                engine_turn_input: Mutex::new(crate::engine_turn_input::EngineTurnInput::new()),
                engine_turn_starts: Mutex::new(Vec::new()),
                engine_resume_window: std::sync::atomic::AtomicBool::new(engine_unattended),
                engine_turn_pause_recorded: std::sync::atomic::AtomicBool::new(false),
                engine_turn_pause_notified: std::sync::atomic::AtomicBool::new(false),
                prompts_in_flight: std::sync::atomic::AtomicUsize::new(0),
                user_input_sources: Mutex::new(Vec::new()),
                user_activity_listeners: Mutex::new(Vec::new()),
                prompt_cache_prefix_builds: Default::default(),
                session_start_settled: tokio::sync::watch::Sender::new(true),
                probe_task: Mutex::new(None),
                user_abort_wait: Mutex::new(None),
                mcp_wire_status_order: tokio::sync::Mutex::new(()),
                mcp_wire_status_retention: Mutex::new(None),
                extension_rpc_event_forwarding: Mutex::new(None),
                extension_rpc_event_listeners: Mutex::new(Vec::new()),
                tool_context_disposers: Mutex::new(Vec::new()),
                tool_dispatcher,
                tool_child,
                tool_child_host_targets,
                tool_child_closed: Mutex::new(None),
                tool_placements: Mutex::new(BTreeMap::new()),
                nested_host_calls: Mutex::new(BTreeMap::new()),
                html_theme_resolver: Mutex::new(None),
                html_built_in_tool_renderers: Mutex::new(None),
                runtime_handle: tokio::runtime::Handle::try_current().ok(),
                transcript_write_failures: Mutex::new(Default::default()),
                work_obligations: Mutex::new(None),
                custody_run_id: Mutex::new(uuid::Uuid::new_v4().to_string()),
                queued_custom_receipts: Mutex::new(Vec::new()),
                pending_session_recoveries: Mutex::new(Vec::new()),
            }),
        };
        // The hold's callbacks reach the session through this cell; a session that never gets here has no hold users.
        let _ = session_cell.set(Arc::downgrade(&session.inner));
        // senpi agent-session.ts constructor: the turn deadline reads its limits from the live settings at
        // each arm. It holds the session weakly: the tracker lives on the session.
        let deadline_settings = Arc::clone(&session.settings_manager);
        let _ = session
            .deadline_tracker
            .set(crate::turn_deadline::AgentDeadlineTracker::new(
                Arc::new(WeakDeadlineOwner(Arc::downgrade(&session.inner))),
                Arc::new(move || {
                    let settings = lock(&deadline_settings);
                    crate::turn_deadline::resolve_turn_deadline_settings(
                        settings.get_value("turn_inactivity_ms"),
                        settings.get_value("turn_max_runtime_ms"),
                    )
                }),
                None,
            ));

        *lock(&session.work_obligations) =
            Some(crate::work_obligations::SessionWorkObligations::new(
                Box::new(SessionCustodyStore(Arc::downgrade(&session.inner))),
            ));
        session.restore_queued_external_deliveries();
        // A torn session tail is reported once and replayed to later subscribers until the next prompt (senpi#4295).
        let recovery_cell = Arc::downgrade(&session.inner);
        // The manager reports a repair inside its own append, under the session_manager lock: the listener only
        // queues it, and it is emitted once the lock is released.
        lock(&session.session_manager).on_recovery(crate::session_manager::RecoveryListener(
            Arc::new(move |recovery| {
                if let Some(inner) = recovery_cell.upgrade() {
                    lock(&inner.pending_session_recoveries).push(recovery.clone());
                }
            }),
        ));
        session.emit_pending_session_recoveries();
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
        session.agent.set_finish_turn(Some(Arc::new(move |turn, _signal| {
            let weak = weak.clone();
            Box::pin(async move {
                let session = AgentSession { inner: weak.upgrade()? };
                let event = maho_ext_api::ExtensionEvent::TurnEnd {
                    turn_index: session.state().turn_index,
                    message: AgentMessage::Llm(maho_ai::types::Message::Assistant(Arc::new(turn.message))),
                    tool_results: turn.tool_results,
                };
                match session.emit_boundary(event).await {
                    Ok(true) => Some(maho_agent::types::AgentTurnDecision::Continue),
                    Ok(false) => None,
                    Err(error_message) => {
                        session.emit(AgentSessionEvent::ContinuationError { error_message });
                        Some(maho_agent::types::AgentTurnDecision::End)
                    }
                }
            })
        })));

        let weak = Arc::downgrade(&session.inner);
        session
            .agent
            .set_resolve_unknown_tool_call(Some(Arc::new(move |requested, _context| {
                let weak = weak.clone();
                Box::pin(async move {
                    let inner = weak.upgrade()?;
                    let session = AgentSession { inner };
                    let mut callable = session.get_active_tool_names();
                    for (name, entry) in &session.state().tool_definitions {
                        let exposure =
                            normalize_tool_exposure(&entry.definition, entry.source_info.clone());
                        if exposure.exposure == ToolExposure::Search
                            && exposure.allow_lazy_activation
                        {
                            callable.push(name.clone());
                        }
                    }
                    let resolved =
                        resolve_tool_name_alias(&requested, callable).unwrap_or(requested);
                    if !session.activate_lazy_tool(&resolved) {
                        return None;
                    }
                    session
                        .agent
                        .state()
                        .tools()
                        .iter()
                        .find(|tool| tool.name() == resolved)
                        .cloned()
                })
            })));

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
        let cooldowns = crate::retry_fallback::cooldown::SelectorCooldowns::new(
            move || now(),
            move || random(),
        )
        .unwrap_or_else(|error| panic!("Invalid pinned cooldown pattern: {error}"));
        *session
            .retry_fallback
            .try_lock()
            .map_err(|_| MissingModelAccessError)? = Some(
            crate::retry_fallback::controller::RetryFallbackController::new(
                SessionFallbackDeps(Arc::downgrade(&session.inner), fallback_logger),
                cooldowns,
            ),
        );

        session.install_after_tool_call_hook();
        let weak = Arc::downgrade(&session.inner);
        session
            .external_admission
            .on_inputs_settled(Arc::new(move || {
                if let Some(inner) = weak.upgrade() {
                    AgentSession { inner }.refresh_lifecycle();
                }
            }));
        lock(&session.lifecycle).release_restore_barrier();
        session.install_agent_next_turn_refresh();
        session.install_pending_inputs();
        // A no-op unless SENPI_MEMORY_REPORT=1: the report reads these only when it is requested.
        *lock(&session.inner.memory_report_unregister) = Some(
            crate::memory_report::memory_report_registry::register_memory_report_session(
                session.memory_report_owner(),
                Arc::new(SessionMemoryReportSource(Arc::downgrade(&session.inner))),
                &crate::config::current_env(),
            ),
        );
        Ok(session)
    }

    /// The key this session's memory report source is registered under (senpi keys by object identity).
    pub fn memory_report_owner(&self) -> u64 {
        Arc::as_ptr(&self.inner) as usize as u64
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
        EXTENSION_EVENT_SIGNAL
            .scope(extension_signal, self.process_agent_event_inner(event))
            .await;
    }

    async fn process_agent_event_inner(&self, mut event: maho_agent::types::AgentEvent) {
        use maho_agent::types::AgentEvent;
        let mut recovery = self.observe_recovery(&event);
        let _persistence = if let AgentEvent::MessageEnd { message } = &event {
            let id = uuid::Uuid::new_v4();
            self.state()
                .messages_awaiting_persistence
                .push((id, message.clone()));
            Some(MessagePersistenceGuard { session: self, id })
        } else {
            None
        };
        {
            let mut state = self.state();
            match &mut event {
                AgentEvent::AgentStart => {
                    // senpi closes the agent_end boundary when a retry or continuation launches the
                    // next run. A boundary left open would make `begin_user_abort` treat a user
                    // abort of this run as a late join of the finished one and never abort it.
                    state.abort_provenance.close_agent_end_boundary();
                    *lock(&self.custody_run_id) = uuid::Uuid::new_v4().to_string();
                    state.queued_input_ages.note_run_started();
                    *lock(&self.stop_interrupt) = None;
                    *lock(&self.lifecycle_turn_facts) = None;
                    self.external_admission.begin_turn();
                    let mut ledger = lock(&self.lifecycle);
                    ledger.release_restore_barrier();
                    let origin = if self.lifecycle_human_pending.swap(false, Ordering::SeqCst) {
                        "human"
                    } else {
                        "automatic"
                    };
                    ledger.start_run(origin);
                    lock(&self.lifecycle_stop_facts).0 = None;
                    *lock(&self.lifecycle_deadline_stop) = None;
                    lock(&self.lifecycle_activity).clear();
                    // The run's opening record carries the admitted input and the epoch (senpi#3980).
                    ledger.publish_unwritten(|record| self.persist_lifecycle_record(record));
                    state.required_compaction_turn_error = None;
                    state.turn_index = 0;
                    state.message_replacements.clear();
                    state.abort_source = None;
                    state.abort_provenance = Default::default();
                }
                AgentEvent::TurnEnd { message, .. } => {
                    for (original, replacement) in &state.message_replacements {
                        if message == original {
                            *message = replacement.clone();
                        }
                    }
                }
                AgentEvent::AgentEnd { messages } => {
                    for message in messages {
                        for (original, replacement) in &state.message_replacements {
                            if message == original {
                                *message = replacement.clone();
                            }
                        }
                    }
                    // The run's last reader has applied them; holding every rewritten pair until
                    // the next run pins each full original message while the session idles.
                    state.message_replacements = Vec::new();
                }
                AgentEvent::MessageStart { message, .. } => {
                    if message.as_assistant().is_some() {
                        state.assistant_generation += 1;
                    }
                }
                // The agent appended this message to its transcript; not every role bumps the revision.
                AgentEvent::MessageEnd { .. } => state.context_usage_cache = None,
                AgentEvent::TurnStart
                | AgentEvent::MessageUpdate { .. }
                | AgentEvent::ToolExecutionStart { .. }
                | AgentEvent::ToolExecutionUpdate { .. }
                | AgentEvent::ToolExecutionEnd { .. } => {}
            }
        }
        self.observe_activity(&event);
        // senpi `_handleAgentEvent`: tool execution progress renews the inactivity lease; text deltas never do.
        if matches!(
            event,
            AgentEvent::ToolExecutionStart { .. }
                | AgentEvent::ToolExecutionUpdate { .. }
                | AgentEvent::ToolExecutionEnd { .. }
        ) && let Some(tracker) = self.deadline_tracker.get()
        {
            tracker.renew();
        }
        if let AgentEvent::MessageStart { message, .. } = &event
            && let Ok(value) = session_message_to_value(message)
        {
            if let Some(id) = value
                .pointer("/details/heldInputId")
                .and_then(Value::as_str)
            {
                lock(&self.observed_held_inputs).insert(id.to_owned());
            }

            let run_id = lock(&self.lifecycle).run_id.clone();
            let result = self.with_session_manager_mut(|manager| {
                let entries = manager.entries_of_types(&["custom"]);
                let mut latest = indexmap::IndexMap::<String, Value>::new();
                for entry in &entries {
                    if entry["type"] == "custom"
                        && entry["customType"] == "work-custody"
                        && let Some(id) = entry["data"]["id"].as_str()
                    {
                        latest.insert(id.into(), entry["data"].clone());
                    }
                }
                let active = latest
                    .into_values()
                    .filter(|r| {
                        ["open", "queued", "consumed"].contains(&r["state"].as_str().unwrap_or(""))
                    })
                    .collect::<Vec<_>>();
                let mut consumed_record = None;
                let result = crate::lifecycle_delivery::consume_lifecycle_delivery(
                    manager,
                    &active,
                    run_id.as_deref(),
                    &value,
                    recovery.as_ref(),
                    |ticket, run| {
                        let mut record = active
                            .iter()
                            .find(|r| r["ticketId"] == ticket)
                            .cloned()
                            .ok_or("missing custody")?;
                        record["state"] = json!("consumed");
                        record["runId"] = run["runId"].clone();
                        record["transcriptEntryId"] = run["transcriptEntryId"].clone();
                        consumed_record = Some(record);
                        Ok(())
                    },
                );
                if let Some(record) = consumed_record {
                    manager.append_custom("work-custody", Some(record));
                }
                if let Some(error) = manager.last_persistence_error() {
                    return Err(error.to_owned());
                }
                result
            });
            if let Err(error) = result {
                lock(&self.lifecycle).custody_write_failed();
                self.emit(AgentSessionEvent::ContinuationError {
                    error_message: error,
                });
            }
        }
        if let AgentEvent::MessageEnd { message } = &event
            && let Some(assistant) = message.as_assistant()
        {
            self.emit_server_fallback_aborted(assistant);
            // senpi agent-session.ts:4086: billing/quota exhaustion and account limits make the entry (or the
            // whole account) unusable for this session, whether or not the chain has a candidate left.
            let error_message = assistant.error_message.as_deref();
            if assistant.stop_reason == StopReason::Error
                && !Self::is_local_failure(assistant)
                && (crate::retry_fallback::circuit_probes::is_health_exhaustion_failure(
                    error_message,
                ) || crate::retry_fallback::usage_limit::usage_limit_scope_text(error_message)
                    == Some(maho_ai::types::UsageLimitScope::Account))
                && let Some(controller) = self.retry_fallback.lock().await.as_mut()
            {
                controller.note_unusable(
                    &self.model(),
                    crate::retry_fallback::cooldown::SelectorFailure {
                        retry_after_ms: None,
                        error_message,
                    },
                );
                self.state().spent_accounts = controller.spent_accounts();
            }
        }
        if let AgentEvent::MessageStart { message, .. } = &event
            && message.role() == "user"
        {
            self.state().overflow_recovery_rungs = 0;
            if let Some(controller) = self.retry_fallback.lock().await.as_mut() {
                controller.reset_turn();
            }
            let text = user_message_text(message);
            let removed = {
                let mut state = self.state();
                if let Some(index) = state
                    .steering_messages
                    .iter()
                    .position(|queued| queued == &text)
                {
                    state.steering_messages.remove(index);
                    Some(StreamingBehavior::Steer)
                } else if let Some(index) = state
                    .follow_up_messages
                    .iter()
                    .position(|queued| queued == &text)
                {
                    state.follow_up_messages.remove(index);
                    Some(StreamingBehavior::FollowUp)
                } else {
                    None
                }
            };
            if let Some(mode) = removed {
                if mode == StreamingBehavior::FollowUp {
                    self.clear_tool_barrier();
                }
                self.remove_queued_input(&text, mode);
                self.emit_queue_update();
            }
        }
        // senpi `_endSelectedModels`: frozen when the end arrives, before any await lets a queued run or a
        // fallback switch replace the selection the failing request was dispatched under.
        let selected_model = if let AgentEvent::AgentEnd { messages } = &event
            && messages
                .iter()
                .rev()
                .find_map(AgentMessage::as_assistant)
                .is_some_and(|message| message.stop_reason == StopReason::Error)
        {
            let original_selector = self
                .retry_fallback
                .lock()
                .await
                .as_ref()
                .and_then(|controller| controller.state.as_ref())
                .map(|state| state.original_selector.clone());
            crate::provider_failure::selected_model_of(
                original_selector.as_deref(),
                &self.model_registry.get_all(),
                Some(&self.model()),
            )
        } else {
            None
        };
        let will_retry = if let AgentEvent::AgentEnd { messages } = &event {
            self.will_retry(messages.iter().rev().find_map(AgentMessage::as_assistant))
                .await
        } else {
            false
        };
        if will_retry {
            self.agent.suppress_queued_message_drain();
        }
        let end_boundary = if let AgentEvent::AgentEnd { messages } = &event {
            let aborted = messages
                .iter()
                .rev()
                .find_map(AgentMessage::as_assistant)
                .is_some_and(|message| message.stop_reason == StopReason::Aborted);
            Some(self.state().abort_provenance.begin_agent_end(
                messages.clone(),
                will_retry,
                aborted,
            ))
        } else {
            None
        };
        if let AgentEvent::MessageEnd { message } = &mut event {
            let replacement = {
                // `emit_message_end` only runs handlers or reports a stale runtime; skip the clone otherwise.
                let runner = self
                    .extension_runner
                    .lock()
                    .await
                    .as_ref()
                    .filter(|runner| {
                        runner.has_handlers(maho_ext_api::EventKind::MessageEnd)
                            || runner.runtime.assert_active().is_err()
                    })
                    .cloned();
                match runner {
                    Some(mut runner) => runner.emit_message_end(message.clone()).await,
                    None => Ok(None),
                }
            };
            match replacement {
                Ok(Some(mut replacement)) => {
                    match (&*message, &mut replacement) {
                        (
                            AgentMessage::Llm(maho_ai::types::Message::Assistant(original)),
                            AgentMessage::Llm(maho_ai::types::Message::Assistant(rewritten)),
                        ) if original.error_cause.is_some() => {
                            std::sync::Arc::make_mut(rewritten).error_cause =
                                original.error_cause.clone();
                        }
                        (
                            AgentMessage::Llm(maho_ai::types::Message::ToolResult(original)),
                            AgentMessage::Llm(maho_ai::types::Message::ToolResult(rewritten)),
                        ) if original.nested_calls.is_some() => {
                            rewritten.nested_calls = original.nested_calls.clone();
                        }
                        _ => {}
                    }
                    if let Some(guard) = &_persistence
                        && let Some((_, pending)) = self
                            .state()
                            .messages_awaiting_persistence
                            .iter_mut()
                            .find(|(id, _)| *id == guard.id)
                    {
                        *pending = replacement.clone();
                    }
                    self.state()
                        .message_replacements
                        .push((message.clone(), replacement.clone()));
                    let mut messages = self.messages();
                    if let Some(original) = messages
                        .iter_mut()
                        .rev()
                        .find(|original| *original == message)
                    {
                        *original = replacement.clone();
                        self.agent.set_messages(messages);
                        self.state().context_usage_cache = None;
                    }
                    *message = replacement;
                }
                Ok(None) => {}
                Err(error) => self.emit(AgentSessionEvent::ContinuationError {
                    error_message: error.to_string(),
                }),
            }
        } else {
            // Bound at emission: the run this end belonged to, and the leaf before its handlers ran, so only a
            // guard that this end's handlers appended is attributed to it (senpi#3864).
            let bound_end = matches!(event, AgentEvent::AgentEnd { .. }).then(|| {
                (
                    lock(&self.lifecycle).run_id.clone(),
                    self.with_session_manager(|manager| manager.leaf_id().map(str::to_owned)),
                )
            });
            self.dispatch_agent_extension_event(&event, end_boundary.as_ref(), will_retry)
                .await;
            if let Some((run_id, guard_start_leaf)) = bound_end {
                let guard = self.with_session_manager(|manager| {
                    crate::lifecycle_stop::goal_guard_after(
                        &manager.branch(None),
                        guard_start_leaf.as_deref(),
                    )
                });
                lock(&self.lifecycle_stop_facts).0 = Some(crate::lifecycle_stop::RunStopFacts {
                    run_id,
                    guard,
                    todo: None,
                });
            }
        }
        if let Some(boundary) = &end_boundary {
            self.state().abort_provenance.end_agent_end(boundary);
            self.emit_late_user_abort().await;
        }
        if matches!(event, AgentEvent::TurnEnd { .. }) {
            self.state().turn_index += 1;
            let pending = std::mem::take(&mut self.state().pending_custom_messages);
            for message in pending {
                if let Err(error_message) = self.append_extension_custom_message(message) {
                    self.emit(AgentSessionEvent::ContinuationError { error_message });
                }
            }
        }
        if let Some(boundary) = &end_boundary {
            self.with_work_obligations(|registry| registry.settle_run(&lock(&self.custody_run_id)));
            self.external_admission.observe_run_settled();
            let generation = self.user_abort_generation.load(Ordering::SeqCst);
            let observed = std::mem::take(&mut *lock(&self.observed_held_inputs));
            let mut claimed = lock(&self.delivered_held_inputs);
            let mut ids = Vec::new();
            claimed.retain(|(id, admitted)| {
                if observed.contains(id) {
                    if *admitted == generation {
                        ids.push(id.clone());
                    }
                    false
                } else {
                    true
                }
            });
            drop(claimed);
            if let Err(error_message) = self.consume_held_inputs(&ids) {
                lock(&self.delivered_held_inputs)
                    .extend(ids.iter().cloned().map(|id| (id, generation)));
                lock(&self.observed_held_inputs).extend(ids);
                self.emit(AgentSessionEvent::ContinuationError { error_message });
            }

            lock(&self.lifecycle).run_active = false;
            lock(&self.lifecycle_activity).clear();
            let last = boundary
                .messages
                .iter()
                .filter_map(AgentMessage::as_assistant)
                .next_back();
            let ended_by = if last.is_some_and(|m| m.stop_reason == StopReason::Error) {
                "provider-error"
            } else if boundary.aborted {
                "unknown-abort"
            } else {
                "completed"
            };
            if lock(&self.lifecycle_turn_facts).is_none() {
                *lock(&self.lifecycle_turn_facts) = Some(
                    json!({"endedBy":ended_by,"willRetry":boundary.will_retry,"expectations":[]}),
                );
            }
            if !boundary.will_retry {
                self.clear_tool_barrier();
            }
            self.refresh_lifecycle();
            // senpi mutates the live agent_end event on a late User join (agent-abort-provenance.ts
            // `join`), so subscribers see the settled owner, not the snapshot taken at begin.
            let settled = self
                .state()
                .abort_provenance
                .settling_agent_end_event()
                .cloned();
            let (aborted, abort_source) = settled
                .map_or((boundary.aborted, boundary.abort_source), |event| {
                    (event.aborted, event.abort_source)
                });
            if !aborted
                && !boundary.will_retry
                && !boundary
                    .messages
                    .iter()
                    .filter_map(AgentMessage::as_assistant)
                    .any(|message| {
                        matches!(message.stop_reason, StopReason::Error | StopReason::Aborted)
                    })
            {
                let outlived = {
                    let mut state = self.state();
                    let ids = state
                        .queued_input_order
                        .iter()
                        .map(|input| input.queued_id.clone())
                        .collect::<Vec<_>>();
                    state
                        .queued_input_ages
                        .take_outlived(&ids, chrono::Utc::now().timestamp_millis())
                };
                for input in outlived {
                    self.emit_extension_event(
                        "queued_input_outlived",
                        &serde_json::json!({"queuedId":input.queued_id,"ageMs":input.age_ms}),
                    );
                }
            }
            *lock(&self.last_provider_failure) = crate::provider_failure::provider_failure_of(
                &boundary.messages,
                selected_model.as_ref(),
            );
            self.emit(AgentSessionEvent::AgentEnd {
                messages: boundary.messages.clone(),
                aborted,
                will_retry: boundary.will_retry,
                abort_source,
            });
        } else if let AgentEvent::MessageEnd { message } = &event {
            // A committed lifecycle delivery can predate its terminal record.
            if let Some(ticket) = message_ticket(message) {
                let durable = self.with_session_manager(|manager| {
                    crate::lifecycle_delivery::lifecycle_delivery_entries(&manager.entries())
                        .get(&ticket)
                        .cloned()
                });
                recovery = crate::session_recovery_identity::hint(recovery, durable.as_deref());
            }
            self.emit_carried(AgentSessionEvent::Agent(event.clone()), recovery.clone());
        } else {
            // Only MessageEnd is read below, so every other event is handed off by move.
            self.emit_carried(AgentSessionEvent::Agent(event), recovery);
            return;
        }
        if end_boundary.is_some() {
            self.emit_late_user_abort().await;
        }
        if let AgentEvent::MessageEnd { message } = &event {
            self.observe_engine_turn_message(message);
            if let AgentMessage::Custom(maho_agent::types::CustomAgentMessage::Custom(custom)) =
                message
            {
                match serde_json::to_value(&custom.content) {
                    Ok(content) => {
                        self.with_session_manager_mut(|manager| {
                            let already = custom
                                .details
                                .as_ref()
                                .and_then(|d| d["ticketId"].as_str())
                                .is_some_and(|ticket| {
                                    crate::lifecycle_delivery::lifecycle_delivery_entries(
                                        &manager.entries_of_types(&["custom_message"]),
                                    )
                                    .contains_key(ticket)
                                });
                            if already {
                                return Value::Null;
                            }
                            manager.append_custom_message_with_recovery(
                                &custom.custom_type,
                                content,
                                custom.display,
                                custom.details.clone(),
                                recovery.as_ref(),
                            )
                        });
                        self.state().message_revision += 1;
                        let outcome = self.with_session_manager(|manager| {
                            manager.last_persistence_error().map(str::to_owned)
                        });
                        let value = json!({"role":"custom","customType":custom.custom_type,"details":custom.details});
                        if let Some(error) = outcome {
                            self.external_admission
                                .observe_refused(&value, error.clone());
                            self.settle_queued_custom_receipt(custom, Err(error));
                        } else {
                            self.external_admission.observe_persisted(&value);
                            self.settle_queued_custom_receipt(custom, Ok(()));
                        }
                    }
                    Err(error) => {
                        let error_message = error.to_string();
                        self.settle_queued_custom_receipt(custom, Err(error_message.clone()));
                        self.emit(AgentSessionEvent::ContinuationError { error_message });
                    }
                }
                return;
            }
            if !matches!(message.role(), "user" | "assistant" | "toolResult") {
                return;
            }
            let input_source = if message.role() == "user" {
                self.take_input_source(message)
            } else {
                None
            };
            let refused_message = message.clone();
            match serde_json::to_value(message) {
                Ok(message) => {
                    let (entry, failure) = self.with_session_manager_mut(|manager| {
                        let entry = manager.append_owned_message_with_recovery(
                            message,
                            input_source,
                            recovery.as_ref(),
                        );
                        (entry, manager.take_write_error())
                    });
                    if let Some(error) = failure {
                        lock(&self.transcript_write_failures).record(refused_message, error);
                    }
                    if let Some(id) = entry.get("id").and_then(Value::as_str) {
                        if entry["message"]["role"] == "assistant" {
                            let mut state = self.state();
                            state.last_persisted_assistant =
                                Some((id.to_owned(), state.assistant_generation));
                        }
                        self.emit_entry_appended(id);
                    }
                    self.state().message_revision += 1;
                }
                Err(error) => self.emit(AgentSessionEvent::ContinuationError {
                    error_message: error.to_string(),
                }),
            }
            if let Some(assistant) = message.as_assistant()
                && assistant.error_message.is_none()
                && !matches!(
                    assistant.stop_reason,
                    StopReason::Error | StopReason::Aborted
                )
                && !maho_ai::utils::stop_details::is_classifier_refusal(assistant)
            {
                if assistant.stop_reason != StopReason::Length {
                    self.state().overflow_recovery_rungs = 0;
                }
                let attempt = self.state().retry_attempt;
                if attempt > 0 {
                    if let Some(fallback) = self
                        .retry_fallback
                        .lock()
                        .await
                        .as_ref()
                        .and_then(|controller| controller.state.clone())
                    {
                        self.emit(AgentSessionEvent::RetryFallbackSucceeded {
                            model: format!("{}/{}", self.model().provider, self.model().id),
                            chain_key: fallback.chain_key,
                        });
                    }
                    self.state().retry_attempt = 0;
                    self.reset_hint_tier_state();
                    self.emit(AgentSessionEvent::AutoRetryEnd {
                        success: true,
                        attempt,
                        final_error: None,
                    });
                }
            }
        }
    }

    /// Forwards a non-`MessageEnd` agent event to extensions. The extension event is built only
    /// when `emit` would do something, and lives in this helper rather than the caller's future.
    async fn dispatch_agent_extension_event(
        &self,
        event: &maho_agent::types::AgentEvent,
        end_boundary: Option<&crate::agent_abort_provenance::AgentEndEvent>,
        will_retry: bool,
    ) {
        use maho_agent::types::AgentEvent;
        use maho_ext_api::{EventKind, ExtensionEvent};
        let kind = match event {
            AgentEvent::AgentStart => EventKind::AgentStart,
            AgentEvent::AgentEnd { .. } => EventKind::AgentEnd,
            AgentEvent::TurnStart => EventKind::TurnStart,
            // Already dispatched by finish_turn, where the continuation decision is actionable.
            AgentEvent::TurnEnd { .. } => return,
            AgentEvent::MessageStart { .. } => EventKind::MessageStart,
            AgentEvent::MessageUpdate { .. } => EventKind::MessageUpdate,
            AgentEvent::MessageEnd { .. } => EventKind::MessageEnd,
            AgentEvent::ToolExecutionStart { .. } => EventKind::ToolExecutionStart,
            AgentEvent::ToolExecutionUpdate { .. } => EventKind::ToolExecutionUpdate,
            AgentEvent::ToolExecutionEnd { .. } => EventKind::ToolExecutionEnd,
        };
        let Some(mut runner) = self.extension_runner_for(kind).await else {
            return;
        };
        let extension_event = match event {
            AgentEvent::AgentStart => ExtensionEvent::AgentStart,
            AgentEvent::AgentEnd { messages } => ExtensionEvent::AgentEnd {
                messages: messages.clone(),
                aborted: end_boundary.map(|boundary| boundary.aborted),
                will_retry: Some(will_retry),
                abort_source: end_boundary.and_then(|boundary| boundary.abort_source),
            },
            AgentEvent::TurnStart => ExtensionEvent::TurnStart {
                turn_index: self.state().turn_index,
                timestamp: maho_ai::utils::diagnostics::now_ms() as u64,
            },
            AgentEvent::TurnEnd {
                message,
                tool_results,
            } => ExtensionEvent::TurnEnd {
                turn_index: self.state().turn_index,
                message: message.clone(),
                tool_results: tool_results.clone(),
            },
            AgentEvent::MessageStart { message, .. } => ExtensionEvent::MessageStart {
                message: message.clone(),
            },
            AgentEvent::MessageUpdate {
                message,
                assistant_message_event,
            } => ExtensionEvent::MessageUpdate {
                message: message.clone(),
                assistant_message_event: extension_assistant_message_event(assistant_message_event),
            },
            AgentEvent::MessageEnd { message } => ExtensionEvent::MessageEnd {
                message: message.clone(),
            },
            AgentEvent::ToolExecutionStart {
                tool_call_id,
                tool_name,
                args,
                parent_tool_call_id,
            } => ExtensionEvent::ToolExecutionStart {
                tool_call_id: tool_call_id.clone(),
                tool_name: tool_name.clone(),
                args: args.clone(),
                parent_tool_call_id: parent_tool_call_id.clone(),
            },
            AgentEvent::ToolExecutionUpdate {
                tool_call_id,
                tool_name,
                args,
                partial_result,
                parent_tool_call_id,
            } => ExtensionEvent::ToolExecutionUpdate {
                tool_call_id: tool_call_id.clone(),
                tool_name: tool_name.clone(),
                args: args.clone(),
                partial_result: partial_result.clone(),
                parent_tool_call_id: parent_tool_call_id.clone(),
            },
            AgentEvent::ToolExecutionEnd {
                tool_call_id,
                tool_name,
                result,
                is_error,
                parent_tool_call_id,
            } => ExtensionEvent::ToolExecutionEnd {
                tool_call_id: tool_call_id.clone(),
                tool_name: tool_name.clone(),
                result: result.clone(),
                is_error: *is_error,
                parent_tool_call_id: parent_tool_call_id.clone(),
            },
        };
        // Boxed so the runner's `emit` state machine is not inlined into every caller's future.
        if let Err(error) = Box::pin(runner.emit(extension_event)).await {
            self.emit(AgentSessionEvent::ContinuationError {
                error_message: error.to_string(),
            });
        }
    }

    /// A clone of the bound runner, only when `emit` would do something for `kind`: run a
    /// handler, or report a stale runtime.
    async fn extension_runner_for(&self, kind: maho_ext_api::EventKind) -> Option<ExtensionRunner> {
        self.extension_runner
            .lock()
            .await
            .as_ref()
            .filter(|runner| runner.has_handlers(kind) || runner.runtime.assert_active().is_err())
            .cloned()
    }

    async fn emit_boundary(&self, event: maho_ext_api::ExtensionEvent) -> Result<bool, String> {
        let Some(mut runner) = self.extension_runner_for(event.kind()).await else {
            return Ok(false);
        };
        let result = Box::pin(runner.emit(event)).await.map_err(|error| error.to_string())?;
        let maho_ext_api::EventResult::Boundary(result) = result else {
            return Ok(false);
        };
        let drafts = result.entries.unwrap_or_default();
        let changed = !drafts.is_empty();
        for draft in drafts {
            let mut entry = serde_json::to_value(draft).map_err(|error| error.to_string())?;
            let id = uuid::Uuid::new_v4().to_string();
            entry["id"] = json!(id);
            entry["parentId"] = self.with_session_manager(|manager| json!(manager.leaf_id()));
            entry["timestamp"] = json!(chrono::Utc::now().to_rfc3339());
            if entry["type"] == "compaction" && entry["firstKeptEntryId"].is_null() {
                entry["firstKeptEntryId"] = json!(id);
            }
            self.with_session_manager_mut(|manager| manager.append_entry_raw(entry));
            self.emit_entry_appended(&id);
        }
        if changed {
            let messages = self.with_session_manager(|manager| manager.current_context().messages)
                .into_iter().map(session_message_from_value).collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())?;
            self.agent.set_messages(messages);
            self.state().context_usage_cache = None;
            self.state().message_revision += 1;
            self.boundary_context_changed.store(true, Ordering::SeqCst);
        }
        Ok(result.should_continue == Some(true))
    }

    async fn dispatch_extension_event(&self, event: maho_ext_api::ExtensionEvent) {
        let Some(mut runner) = self.extension_runner_for(event.kind()).await else {
            return;
        };
        // Boxed so the runner's `emit` state machine is not inlined into every caller's future.
        if let Err(error) = Box::pin(runner.emit(event)).await {
            self.emit(AgentSessionEvent::ContinuationError {
                error_message: error.to_string(),
            });
        }
    }

    /// Start a prompt, or explicitly queue input when a provider turn is active.
    /// Register a listener told once per person input, classified by entry point (never by the
    /// connection's `interactive_user` role). A throwing listener never blocks the input.
    /// Returns an unsubscribe closure.
    pub fn on_human_input_admitted(
        &self,
        listener: HumanInputListener,
    ) -> impl FnOnce() + Send + 'static {
        let id = self.next_listener_id.fetch_add(1, Ordering::SeqCst);
        self.human_input_listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((id, listener));
        let registry = Arc::clone(&self.human_input_listeners);
        move || {
            registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .retain(|(existing, _)| *existing != id);
        }
    }

    pub fn admit_external_message(&self, input: &Value, editor: &Value) -> Result<Value, String> {
        self.external_admission
            .admit(input, editor, self.is_streaming(), |message, lane| {
                let custom = serde_json::from_value(message)
                    .map_err(|e| format!("invalid external message: {e}"))?;
                self.send_custom_message_content_inner(
                    custom,
                    maho_ext_api::SendMessageOptions {
                        trigger_turn: lane == "start",
                        deliver_as: Some(if lane == "steer" {
                            maho_ext_api::DeliverAs::Steer
                        } else {
                            maho_ext_api::DeliverAs::FollowUp
                        }),
                    },
                )
                .map_err(|e| e.message)
            })
    }

    pub fn external_admission(&self) -> &crate::external_admission::ExternalAdmission {
        &self.external_admission
    }

    /// The provider failure of the last agent_end (senpi agent_end `failure`). The maho-ext-api agent_end
    /// envelope has no failure field, so subscribers read it here once the end is delivered.
    pub fn last_provider_failure(&self) -> Option<crate::provider_failure::ProviderFailure> {
        lock(&self.last_provider_failure).clone()
    }

    /// Reads the lifecycle; every fact that feeds it raises its own change edge, so a read never writes
    /// (senpi#3980).
    pub fn lifecycle_snapshot(&self) -> Value {
        lock(&self.lifecycle).snapshot().clone()
    }

    pub fn update_lifecycle_facts(
        &self,
        human: Vec<Value>,
        background: Vec<Value>,
        goal: Option<&Value>,
        turn_end: Option<&Value>,
    ) {
        *lock(&self.lifecycle_goal) = goal.cloned();
        if let Some(input) = turn_end {
            *lock(&self.lifecycle_turn_facts) = Some(input.clone());
        }
        let mut ledger = lock(&self.lifecycle);
        if let Some(goal) = goal.filter(|g| g["status"] == "complete")
            && let Some(id) = goal["id"].as_str()
        {
            ledger.mark_goal_completed(id, |record| self.persist_lifecycle_record(record));
        }
        if let Some(end) = turn_end {
            ledger.last_turn_end = Some(end.clone());
        }
        let mut synthetic = Vec::new();
        if let Some(goal) = goal {
            if goal["status"] == "blocked" {
                synthetic.push((
                    format!("goal:{}", goal["id"].as_str().unwrap_or_default()),
                    "goal_blocked".into(),
                ));
            } else if goal["status"] == "paused" || goal.get("continuationStoppedAt").is_some() {
                synthetic.push((
                    format!("goal:{}", goal["id"].as_str().unwrap_or_default()),
                    "goal_paused".into(),
                ));
            } else if goal["status"] == "active" && !ledger.run_active && background.is_empty() {
                synthetic.push((
                    format!("stall:{}", ledger.current["epoch"]),
                    "stalled".into(),
                ));
            }
        }
        if ledger.interrupted {
            synthetic.push((
                format!("interrupted:{}", ledger.current["epoch"]),
                "interrupted_run".into(),
            ));
        }
        let mut waits = ledger
            .current
            .get("waitingOn")
            .cloned()
            .unwrap_or(json!({"children":0}));
        let mut human = human;
        human.extend(ledger.attention.reconcile(&synthetic));
        waits["human"] = json!(human);
        waits["background"] = json!(background);
        ledger.current["waitingOn"] = waits;
        ledger.current["persisted"] = json!(false);
        drop(ledger);
        self.refresh_lifecycle();
    }

    pub fn report_child_lifecycle(&self, report: &Value) -> Result<bool, String> {
        let entries = self.with_session_manager(|manager| manager.entries_of_types(&["custom"]));
        let mut children = crate::lifecycle_children::LifecycleChildren::new(&entries);
        let changed = children.report(report, |accepted| {
            self.with_session_manager_mut(|manager| {
                manager.append_custom("child-lifecycle", Some(accepted.clone()));
                manager
                    .last_persistence_error()
                    .map_or(Ok(()), |e| Err(e.into()))
            })
        })?;
        if changed {
            let mut ledger = lock(&self.lifecycle);
            let mut waits = ledger
                .current
                .get("waitingOn")
                .cloned()
                .unwrap_or(json!({"children":0,"background":[],"human":[]}));
            waits["children"] = json!(children.active_count());
            ledger.current["waitingOn"] = waits;
            ledger.current["persisted"] = json!(false);
        }
        self.refresh_lifecycle();
        Ok(changed)
    }

    pub async fn emit_session_move(
        &self,
        move_id: String,
        dir: std::path::PathBuf,
        redelivered: Option<bool>,
    ) -> Result<(), String> {
        let Some(mut runner) = self.extension_runner.lock().await.clone() else {
            return Ok(());
        };
        let signal = maho_ext_api::AbortSignal::default();
        let event = match redelivered {
            Some(redelivered) => maho_ext_api::ExtensionEvent::SessionMoveImport {
                move_id,
                dir,
                redelivered,
                signal,
            },
            None => maho_ext_api::ExtensionEvent::SessionMoveExport {
                move_id,
                dir,
                signal,
            },
        };
        let failures = runner
            .emit_session_move(event)
            .await
            .map_err(|e| e.to_string())?;
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures
                .into_iter()
                .map(|error| format!("{}: {}", error.extension_path, error.error))
                .collect::<Vec<_>>()
                .join("; "))
        }
    }

    pub fn request_self_settle(&self) -> Value {
        if self.external_admission.has_input_work() {
            let snapshot = lock(&self.lifecycle).snapshot().clone();
            return json!({"recorded":false,"reason":"not_eligible","epoch":snapshot["epoch"],"humanInputRevision":snapshot["humanInputRevision"]});
        }
        lock(&self.lifecycle).request_self_settle(|record| self.persist_lifecycle_record(record))
    }

    fn refresh_lifecycle(&self) {
        // Work aborted by dispose() still unwinds afterwards; its edges must not write to the released session file.
        if self.lifecycle_disposed.load(Ordering::SeqCst) {
            return;
        }
        let activity = self.activity_snapshot();
        let entries = self.with_session_manager(|manager| {
            manager.reduce_history(&crate::lifecycle_history::LIFECYCLE_HISTORY)
        });
        let children = crate::lifecycle_children::LifecycleChildren::new(&entries);
        let pending = self.pending_message_count();
        let holds = crate::lifecycle_runtime_facts::lifecycle_holds(&self.holds());
        let (active, custody_persisted) = {
            let registry = lock(&self.work_obligations);
            let Some(registry) = registry.as_ref() else {
                return;
            };
            (
                registry.active().into_iter().cloned().collect::<Vec<_>>(),
                registry.persisted(),
            )
        };
        let mut ledger = lock(&self.lifecycle);
        let mut waits = ledger
            .current
            .get("waitingOn")
            .cloned()
            .unwrap_or(json!({"children":0,"background":[],"human":[]}));
        waits["children"] = json!(
            children
                .reports()
                .iter()
                .filter(|r| r["childSessionId"] != self.session_id() && r.get("terminal").is_none())
                .count()
        );
        let child_obligations = active
            .iter()
            .filter(|r| r.kind == crate::work_custody_record::ObligationKind::Children)
            .map(|r| r.active_count.unwrap_or(1.))
            .sum::<f64>() as u64;
        waits["children"] = json!(
            waits["children"]
                .as_u64()
                .unwrap_or(0)
                .max(child_obligations)
        );
        // Each wait source names the items it waits on (senpi#4106).
        let project = |r: &crate::work_custody_record::WorkCustodyRecord| {
            let mut wait = json!({"source":r.source.as_deref().unwrap_or(&r.key),"activeCount":r.active_count.unwrap_or(1.) as u64});
            let named = crate::lifecycle_wait_items::project_wait_items(
                &crate::lifecycle_wait_items::WaitSourceItems {
                    items: r.items.clone(),
                    omitted_items: r.omitted_items,
                },
            );
            if let Some(items) = named.items {
                wait["items"] = json!(items);
            }
            if let Some(omitted) = named.omitted_items {
                wait["omittedItems"] = json!(omitted);
            }
            wait
        };
        waits["background"] = json!(
            active
                .iter()
                .filter(|r| r.kind == crate::work_custody_record::ObligationKind::Background)
                .map(project)
                .collect::<Vec<_>>()
        );
        if !custody_persisted {
            ledger.custody_write_failed();
        }
        let goal = lock(&self.lifecycle_goal).clone();
        let input = lock(&self.lifecycle_turn_facts).clone();
        let facts = crate::session_lifecycle::evaluate_turn_facts(
            input.as_ref(),
            goal.as_ref(),
            waits["background"].as_array().cloned().unwrap_or_default(),
            (pending, waits["children"].as_u64().unwrap_or(0) as usize),
            (ledger.run_active, ledger.interrupted),
            &ledger.current,
            activity.is_streaming
                || activity.is_compacting
                || activity.is_bash_running
                || activity.has_session_work,
        );
        waits["background"] = json!(facts.background);
        let previous_synthetic = ledger.attention.synthetic();
        let mut human = waits["human"].as_array().cloned().unwrap_or_default();
        human.retain(|h| {
            !previous_synthetic
                .as_array()
                .expect("synthetic waits")
                .iter()
                .any(|s| s["wait"]["requestId"] == h["requestId"])
        });
        human.extend(ledger.attention.reconcile(&facts.synthetic));
        if ledger.current["role"] == "child" {
            human.clear();
        }
        waits["human"] = json!(human);
        let running = facts.running || self.external_admission.has_input_work();
        let own = children.reports().into_iter().find(|r| {
            r["childSessionId"] == self.session_id() && ledger.current["taskId"] == r["taskId"]
        });
        let terminal = own.as_ref().and_then(|r| r["terminal"].as_str());
        let failure = facts.failure.or_else(|| {
            own.as_ref().and_then(|r| {
                let reason = match r["terminal"].as_str()? {
                    "error" => "task_error",
                    "lost" => "lost",
                    "interrupted" => "interrupted",
                    _ => return None,
                };
                Some(json!({"reason":reason,"taskId":r["taskId"],"runEpoch":r["runEpoch"]}))
            })
        });
        let state = crate::lifecycle_runtime_facts::lifecycle_state(
            ledger.current["role"].as_str().unwrap_or("root"),
            terminal,
            &holds,
            &waits,
            running,
            failure.is_some(),
        );
        if terminal.is_none()
            && ["done", "failed", "cancelled"].contains(&state.as_str())
            && !active.is_empty()
        {
            return;
        }
        let last_stop = {
            let stop_facts = lock(&self.lifecycle_stop_facts);
            let mut facts = stop_facts.0.clone();
            if let Some(facts) = facts.as_mut() {
                facts.todo = input
                    .as_ref()
                    .and_then(|input| input["todoOutcome"].as_str())
                    .map(str::to_owned);
            }
            let deadline = lock(&self.lifecycle_deadline_stop).clone();
            lifecycle_last_stop(
                &ledger,
                stop_facts.1.as_deref(),
                facts.as_ref(),
                deadline.as_ref(),
                input.as_ref(),
                ledger
                    .attention
                    .synthetic()
                    .as_array()
                    .is_some_and(|waits| {
                        waits.iter().any(|wait| wait["wait"]["kind"] == "stalled")
                    }),
            )
        };
        let changed = ledger.update_with(
            crate::lifecycle_ledger::LedgerUpdate {
                state: state.clone(),
                waits,
                holds,
                failure,
                last_stop,
                activity: lock(&self.lifecycle_activity).project(&state),
            },
            |record| self.persist_lifecycle_record(record),
        );
        let snapshot = ledger.snapshot().clone();
        drop(ledger);
        if changed {
            self.event_bus.emit("session_lifecycle", &json!({"type":"session_lifecycle","sessionId":self.session_id(),"lifecycle":snapshot}));
        }
    }

    /// senpi `SessionLifecycleController.observeActivity`: a run's tool and model edges are live lifecycle activity.
    /// An activity-only change emits `session_lifecycle` with a new activity revision and writes nothing.
    fn observe_activity(&self, event: &maho_agent::types::AgentEvent) {
        if !lock(&self.lifecycle).run_active {
            return;
        }
        if lock(&self.lifecycle_activity).observe(event) {
            self.refresh_lifecycle();
        }
    }

    /// Emits the repairs the session manager queued during an append; callers hold no session_manager lock.
    fn emit_pending_session_recoveries(&self) {
        let recoveries = std::mem::take(&mut *lock(&self.pending_session_recoveries));
        for recovery in &recoveries {
            self.report_session_recovered(recovery);
        }
    }

    /// senpi `sessionManager.onRecovery` listener: emits `session_recovered` and keeps it standing for replay.
    fn report_session_recovered(&self, recovery: &crate::session_write_recovery::SessionRecovery) {
        let event = AgentSessionEvent::SessionRecovered {
            cause: recovery.cause.as_str().to_owned(),
            backup_path: recovery.backup_path.clone(),
            dropped_bytes: recovery.dropped_bytes as u64,
            notice: recovery.notice.clone(),
        };
        self.state().standing_recovery = Some(event.clone());
        self.emit(event);
    }

    fn persist_lifecycle_record(&self, record: &Value) -> Result<(), String> {
        // Writes and emits nothing once disposed: the session file is no longer this session's (senpi#4149).
        if self.lifecycle_disposed.load(Ordering::SeqCst) {
            return Ok(());
        }
        self.with_session_manager_mut(|manager| {
            manager.append_custom("session-lifecycle", Some(record.clone()));
            manager
                .last_persistence_error()
                .map_or(Ok(()), |e| Err(e.to_owned()))
        })
    }

    /// Held commands leave custody before invocation; commands run once, in input order.
    async fn take_held_input_messages(&self) -> Result<(Vec<AgentMessage>, Vec<String>), String> {
        if HELD_COMMAND_SESSION
            .try_with(|id| id == &self.session_id())
            .unwrap_or(false)
        {
            return Ok((Vec::new(), Vec::new()));
        }
        let _preparation = self.held_preparation.lock().await;
        let pending = self.held_inputs();
        let mut commands = Vec::new();
        for input in &pending {
            if input["prepare"] != true {
                continue;
            }
            let text = input["text"].as_str().expect("validated held text");
            if let Some(command) = text.strip_prefix('/') {
                let name = command.split_once(' ').map_or(command, |(name, _)| name);
                let is_command = self
                    .extension_runner
                    .lock()
                    .await
                    .as_ref()
                    .is_some_and(|r| r.get_command(name).is_some());
                if is_command {
                    commands.push(input.clone());
                }
            }
        }
        self.consume_held_inputs(
            &commands
                .iter()
                .map(|i| i["inputId"].as_str().expect("input id").to_owned())
                .collect::<Vec<_>>(),
        )?;
        for input in commands {
            if let Err(error) = Box::pin(HELD_COMMAND_SESSION.scope(
                self.session_id(),
                self.try_execute_extension_command(input["text"].as_str().expect("held command")),
            ))
            .await
            {
                self.session_logger.warn(
                    "held_command_failed",
                    Some(&serde_json::Map::from_iter([(
                        "error".into(),
                        json!(error),
                    )])),
                );
            }
        }
        let mut messages = Vec::new();
        let mut ids = Vec::new();
        let claimed = lock(&self.delivered_held_inputs)
            .iter()
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for input in self.held_inputs() {
            let id = input["inputId"].as_str().expect("input id").to_owned();
            if claimed.contains(&id) || lock(&self.reserved_held_inputs).contains(&id) {
                continue;
            }
            ids.push(id);
            let text = input["text"].as_str().expect("held text");
            let has_images = input
                .get("images")
                .and_then(Value::as_array)
                .is_some_and(|i| !i.is_empty());
            if input["prepare"] == true
                && crate::manual_continue::is_manual_continue_submission(
                    text,
                    self.agent.has_messages(),
                    has_images,
                )
            {
                continue;
            }
            let text = self.expand_input(text, input["prepare"] == true)?;
            let content = if has_images {
                let mut blocks = vec![json!({"type":"text","text":text})];
                blocks.extend(input["images"].as_array().expect("images").iter().cloned());
                json!(blocks)
            } else {
                json!(text)
            };
            messages.push(session_message_from_value(json!({"role":"custom","customType":crate::held_inputs::HELD_INPUT_ENTRY,"content":content,"display":true,"details":{"heldInputId":input["inputId"]},"timestamp":maho_ai::utils::diagnostics::now_ms()})).map_err(|error| error.to_string())?);
        }
        let held_message_ids = messages
            .iter()
            .filter_map(|m| {
                session_message_to_value(m).ok().and_then(|v| {
                    v.pointer("/details/heldInputId")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
            })
            .collect::<BTreeSet<_>>();
        for id in ids.iter().filter(|id| !held_message_ids.contains(*id)) {
            messages.push(session_message_from_value(json!({"role":"custom","customType":"held-input-continue","content":"","display":false,"details":{"heldInputId":id},"timestamp":maho_ai::utils::diagnostics::now_ms()})).map_err(|error| error.to_string())?);
        }
        lock(&self.reserved_held_inputs).extend(ids.iter().cloned());
        Ok((messages, ids))
    }

    pub async fn pause_for_run(
        &self,
        parent: &str,
        pause_seq: u64,
        run_ref: crate::run_holds::RunRef,
    ) -> Result<crate::run_holds::InheritResult, String> {
        let result = crate::run_holds::RunHolds::new(&mut lock(&self.engine_turn_hold))
            .inherit(parent, pause_seq, run_ref)?;
        if matches!(result, crate::run_holds::InheritResult::Inherited { .. }) {
            self.abort().await;
        }
        self.refresh_lifecycle();
        Ok(result)
    }

    pub fn release_run_hold(
        &self,
        parent: &str,
        pause_seq: u64,
        child_seq_cutoff: u64,
    ) -> Result<crate::run_holds::ReleaseInheritedResult, String> {
        let result = crate::run_holds::RunHolds::new(&mut lock(&self.engine_turn_hold))
            .release_inherited(parent, pause_seq, child_seq_cutoff)?;
        self.refresh_lifecycle();
        Ok(result)
    }

    pub async fn resume_for_run(
        &self,
        parent: &str,
        pause_seq: u64,
        child_seq_cutoff: u64,
    ) -> Result<crate::run_holds::ReleaseInheritedResult, String> {
        let result = crate::run_holds::RunHolds::new(&mut lock(&self.engine_turn_hold))
            .release_inherited(parent, pause_seq, child_seq_cutoff)?;
        if matches!(
            result,
            crate::run_holds::ReleaseInheritedResult::Released {
                other_hold_live: false
            }
        ) && !self.paused()
        {
            self.deliver_held_inputs().await?;
        }
        self.refresh_lifecycle();
        Ok(result)
    }

    pub async fn deliver_held_inputs(&self) -> Result<(), String> {
        if self.held_inputs().is_empty() {
            return Ok(());
        }
        Box::pin(self.send_custom_message_content(
            crate::manual_continue::MANUAL_CONTINUE_CUSTOM_TYPE.into(),
            maho_agent::harness::messages::CustomMessageContent::Text(
                crate::manual_continue::MANUAL_CONTINUE_DIRECTIVE.into(),
            ),
            false,
            None,
            maho_ext_api::SendMessageOptions {
                trigger_turn: true,
                deliver_as: None,
            },
        ))
        .await
    }

    pub fn hold_input(&self, input: Value) -> Result<Value, String> {
        lock(&self.held_inputs).hold(input, |kind, data| {
            self.with_session_manager_mut(|manager| {
                let entry = manager.append_custom(kind, Some(data.clone()));
                if let Some(error) = manager.last_persistence_error() {
                    return Err(error.to_owned());
                }
                entry["id"]
                    .as_str()
                    .map(str::to_owned)
                    .ok_or("held input entry has no id".into())
            })
        })
    }

    pub fn consume_held_inputs(&self, input_ids: &[String]) -> Result<(), String> {
        lock(&self.held_inputs).consume(input_ids, |kind, data| {
            self.with_session_manager_mut(|manager| {
                let entry = manager.append_custom(kind, Some(data.clone()));
                if let Some(error) = manager.last_persistence_error() {
                    return Err(error.to_owned());
                }
                entry["id"]
                    .as_str()
                    .map(str::to_owned)
                    .ok_or("held input entry has no id".into())
            })
        })
    }

    pub fn held_inputs(&self) -> Vec<Value> {
        lock(&self.held_inputs).pending()
    }

    pub fn withdraw_held_input(&self, input_id: &str) -> Result<bool, String> {
        lock(&self.held_inputs).withdraw(input_id, |kind, data| {
            self.with_session_manager_mut(|manager| {
                let entry = manager.append_custom(kind, Some(data.clone()));
                if let Some(error) = manager.last_persistence_error() {
                    return Err(error.to_owned());
                }
                entry["id"]
                    .as_str()
                    .map(str::to_owned)
                    .ok_or("held input entry has no id".into())
            })
        })
    }

    fn human_input_admitted(&self, kind: HumanInputKind) {
        self.lifecycle_human_pending.store(true, Ordering::SeqCst);
        let now = chrono::Utc::now().to_rfc3339();
        lock(&self.lifecycle).human_input(&now, |record| self.persist_lifecycle_record(record));
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        let event = HumanInputAdmittedEvent { kind, at };
        self.event_bus.emit(
            "human_input_admitted",
            &serde_json::json!({"at":at,"kind":format!("{kind:?}")}),
        );
        let listeners: Vec<HumanInputListener> = self
            .human_input_listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|(_, l)| Arc::clone(l))
            .collect();
        for listener in listeners {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| listener(&event))).is_err()
            {
                self.session_logger
                    .warn("human_input_listener_failed", None);
            }
        }
    }

    /// Internal adapter boundary for a validated reply that resumes the current turn.
    pub fn admit_human_answer(&self) {
        self.human_input_admitted(HumanInputKind::Answer);
    }

    /// Explicit user Resume; attachment alone must never call this. Reports the human input even when
    /// nothing is paused, then releases every held wake one at a time through full agent_end handling.
    pub fn paused_held_input_count(&self) -> usize {
        self.held_inputs().len()
    }
    pub fn security_notices(&self) -> Value {
        serde_json::to_value(self.security_notice_state()).expect("security state serializes")
    }
    pub fn stop_for_run(&self) {
        crate::run_holds::RunHolds::new(&mut lock(&self.engine_turn_hold)).stamp();
    }
    pub fn note_automatic_input(&self, text: &str) -> bool {
        let mut hold = lock(&self.engine_turn_hold);
        if !hold.holds().iter().any(|record| {
            (record.kind == crate::engine_turn_hold::HoldKind::SessionHold
                && record.key != crate::engine_turn_hold::INTERRUPTED_RUN_HOLD_KEY)
                || record.kind == crate::engine_turn_hold::HoldKind::InheritedHold
        }) {
            return false;
        }
        hold.note(&format!("rpc-input:{}",uuid::Uuid::new_v4()),Some(serde_json::json!({"role":"custom","customType":"rpc-automatic-input","content":text,"display":true,"timestamp":maho_ai::utils::diagnostics::now_ms()})));
        true
    }
    pub async fn begin_resume(&self, request: &Value) -> Result<Value, String> {
        let seq = self.pause_seq();
        if let Some(effect) = request["effectId"].as_str()
            && let Some(mut answer) = self.with_session_manager(|manager| {
                manager
                    .entries_of_types(&["custom"])
                    .iter()
                    .rev()
                    .find(|entry| {
                        entry["customType"] == "session-resume-effect"
                            && entry["data"]["effectId"] == effect
                    })
                    .map(|entry| entry["data"]["answer"].clone())
            })
        {
            answer["replayed"] = true.into();
            return Ok(answer);
        }
        let idle = serde_json::json!({"resumed":false,"pauseSeq":seq,"turnStarted":false,"deliveredNotices":0,"deliveredHeldInputs":0});
        if !self.paused() {
            let mut answer = idle;
            answer["reason"] = "not_paused".into();
            return Ok(answer);
        }
        if request["expectedSeq"]
            .as_u64()
            .is_some_and(|expected| expected != seq)
        {
            let mut answer = idle;
            answer["reason"] = "stale".into();
            return Ok(answer);
        }
        let inherited =
            crate::run_holds::RunHolds::new(&mut lock(&self.engine_turn_hold)).inherited_owner();
        if let Some(owner) = inherited.as_ref()
            && self
                .holds()
                .iter()
                .all(|hold| hold.kind == crate::engine_turn_hold::HoldKind::InheritedHold)
        {
            let mut answer = idle;
            answer["reason"] = "resume_through_parent".into();
            answer["parentSessionId"] = owner.parent_session_id.clone().into();
            return Ok(answer);
        }
        if request["mode"] == "release" || inherited.is_some() {
            let mut hold = lock(&self.engine_turn_hold);
            hold.attend();
            let keys = hold
                .holds()
                .iter()
                .filter(|hold| {
                    matches!(
                        hold.kind,
                        crate::engine_turn_hold::HoldKind::SessionHold
                            | crate::engine_turn_hold::HoldKind::Unattended
                    )
                })
                .map(|hold| hold.key.clone())
                .collect::<Vec<_>>();
            hold.release(
                &keys,
                crate::engine_turn_hold::HoldReleaseCause::ResumeSession,
            )?;
            let mut answer = idle;
            answer["resumed"] = true.into();
            return Ok(answer);
        }
        if let Some(message) = request["message"].as_str() {
            self.hold_input(serde_json::json!({"inputId":format!("resume-{}",request["effectId"].as_str().map(str::to_owned).unwrap_or_else(||uuid::Uuid::new_v4().to_string())),"revision":0,"text":message}))?;
        }
        let notices = self.paused_notice_count();
        let inputs = self.paused_held_input_count();
        self.resume_session().await?;
        let answer = serde_json::json!({"resumed":true,"pauseSeq":seq,"turnStarted":notices>0||inputs>0,"deliveredNotices":notices,"deliveredHeldInputs":inputs});
        if let Some(effect) = request["effectId"].as_str() {
            self.with_session_manager_mut(|manager| {
                manager.append_custom(
                    "session-resume-effect",
                    Some(serde_json::json!({"effectId":effect,"answer":answer})),
                )
            });
        }
        Ok(answer)
    }
    /// senpi `_resumeCause`: only the interruption gate held (no user Stop) resumes as "interrupted".
    pub fn resume_cause(&self) -> crate::stop_resume_directive::ResumeCause {
        let holds = self.holds();
        let session_hold = |interrupted: bool| {
            holds.iter().any(|hold| {
                hold.kind == crate::engine_turn_hold::HoldKind::SessionHold
                    && (hold.key == crate::engine_turn_hold::INTERRUPTED_RUN_HOLD_KEY)
                        == interrupted
            })
        };
        if session_hold(true) && !session_hold(false) {
            crate::stop_resume_directive::ResumeCause::Interrupted
        } else {
            crate::stop_resume_directive::ResumeCause::Stop
        }
    }
    pub async fn resume_session(&self) -> Result<(), String> {
        self.human_input_admitted(HumanInputKind::Resume);
        let mut hold =
            crate::engine_turn_hold::SharedEngineTurnHold(Arc::clone(&self.engine_turn_hold));
        let sender = self.clone();
        let announcer = self.clone();
        self.engine_turn_release
            .resume(
                &mut hold,
                move |message, source, notice_ids| {
                    let session = sender.clone();
                    async move {
                        session
                            .send_released_wake(message, source, notice_ids)
                            .await
                    }
                },
                move |sources| async move { announcer.emit_held_wakes_released(sources).await },
            )
            .await
            .map_err(|error| error.to_string())?;
        self.dispatch_hold_changed("resumed").await;
        self.deliver_held_inputs().await?;
        self.refresh_lifecycle();
        Ok(())
    }

    /// One held wake delivered as a turn. `Ok(true)` once the run settled; `Ok(false)` when it was not
    /// delivered or a user abort landed meanwhile (the hold then keeps custody).
    async fn send_released_wake(
        &self,
        message: Value,
        source: String,
        notice_ids: Vec<String>,
    ) -> Result<bool, String> {
        let generation = self.user_abort_generation.load(Ordering::SeqCst);
        self.wait_for_idle().await;
        let custom: maho_agent::harness::messages::CustomMessage =
            serde_json::from_value(message).map_err(|error| error.to_string())?;
        // A released wake never rides the native queues: a busy session keeps custody instead.
        if self.is_streaming() || self.is_compacting() {
            return Ok(false);
        }
        let claimer = self.clone();
        let admit: ReleasedWakeAdmit = Box::new(move || {
            lock(&claimer.engine_turn_hold).consume(&source, Some(notice_ids.as_slice()))
        });
        if !self
            .trigger_custom_message_turn_with(custom, None, generation, Some(admit))
            .await?
        {
            return Ok(false);
        }
        self.wait_for_idle().await;
        if generation != self.user_abort_generation.load(Ordering::SeqCst) {
            let mut hold = lock(&self.engine_turn_hold);
            hold.invalidate();
            hold.reject_input();
            return Ok(false);
        }
        Ok(true)
    }

    /// senpi `_recordEngineTurnStart`. The bound's bookkeeping must never stop a turn.
    fn record_engine_turn_start(&self, custom_type: &str) {
        self.engine_turn_pause_recorded
            .store(false, Ordering::SeqCst);
        self.engine_turn_pause_notified
            .store(false, Ordering::SeqCst);
        lock(&self.engine_turn_starts).push(crate::engine_turn_limit::EngineTurnStartRecord {
            at: maho_ai::utils::diagnostics::now_ms(),
            tool_used: false,
        });
        let entry = self.with_session_manager_mut(|manager| {
            manager.append_custom(
                crate::engine_turn_limit::ENGINE_TURN_START_ENTRY_TYPE,
                Some(serde_json::json!({"customType": custom_type})),
            )
        });
        if let Some(id) = entry.get("id").and_then(Value::as_str) {
            self.emit_entry_appended(id);
        }
    }

    /// senpi `_observeEngineTurnMessage`: a user message clears this process's record, a tool call marks the
    /// running engine turn.
    fn observe_engine_turn_message(&self, message: &AgentMessage) {
        let user_directed = matches!(message, AgentMessage::Custom(maho_agent::types::CustomAgentMessage::Custom(custom))
            if crate::engine_turn_limit::resets_engine_turn_history(&custom.custom_type, custom.details.as_ref()));
        if message.role() == "user" || user_directed {
            lock(&self.engine_turn_starts).clear();
            self.engine_resume_window.store(false, Ordering::SeqCst);
            self.engine_turn_pause_recorded
                .store(false, Ordering::SeqCst);
            self.engine_turn_pause_notified
                .store(false, Ordering::SeqCst);
            return;
        }
        if message.as_assistant().is_some_and(|assistant| {
            assistant
                .content
                .iter()
                .any(|block| matches!(block, maho_ai::types::ContentBlock::ToolCall(_)))
        }) && let Some(running) = lock(&self.engine_turn_starts).last_mut()
        {
            running.tool_used = true;
        }
    }

    /// senpi `_stopEngineTurn`: an engine-originated turn that would exceed the engine-wide bound is not
    /// started; the stop is recorded (best effort), emitted and shown (senpi#2967).
    fn stop_engine_turn(&self, custom_type: &str) -> bool {
        use crate::engine_turn_limit::{
            ENGINE_TURN_LIMIT_ENTRY_TYPE, ENGINE_TURN_LIMIT_EVENT, EngineTurnStopReason,
        };
        if crate::engine_turn_limit::is_user_directed_turn(custom_type) {
            return false;
        }
        let history = self.with_session_manager(|manager| {
            manager.reduce_history(&crate::engine_turn_limit::ENGINE_TURN_HISTORY)
        });
        let settings = self.with_settings_manager(|manager| manager.get_engine_turn_settings());
        let bound = |value: i64| usize::try_from(value).unwrap_or(0);
        let limits = crate::engine_turn_limit::EngineTurnLimits {
            max_per_user_input: bound(settings.max_per_user_input),
            max_tool_free_per_minute: bound(settings.max_tool_free_per_minute),
            max_per_resume_window: self
                .engine_resume_window
                .load(Ordering::SeqCst)
                .then(|| bound(settings.max_per_resume_window)),
        };
        let now = maho_ai::utils::diagnostics::now_ms();
        let starts = lock(&self.engine_turn_starts).clone();
        let Some(stop) =
            crate::engine_turn_limit::engine_turn_stop(&history, now, &limits, &starts)
        else {
            return false;
        };
        let details = serde_json::json!({
            "customType": custom_type, "reason": stop.reason.as_str(), "sinceUserInput": stop.since_user_input,
            "toolFreeInWindow": stop.tool_free_in_window, "at": now,
        });
        if !self.engine_turn_pause_recorded.load(Ordering::SeqCst)
            && !crate::engine_paused::engine_pause_since_last_turn(
                &self.with_session_manager(|manager| manager.current_branch()),
            )
        {
            self.engine_turn_pause_recorded
                .store(true, Ordering::SeqCst);
            let (reason, count) = match stop.reason {
                EngineTurnStopReason::ResumeWindow => {
                    ("cap-per-resume-window", stop.since_user_input)
                }
                EngineTurnStopReason::PerUserInput => ("cap-per-message", stop.since_user_input),
                EngineTurnStopReason::ToolFreeRate => ("cap-per-minute", stop.tool_free_in_window),
            };
            let entry = self.with_session_manager_mut(|manager| manager.append_custom(
                crate::engine_paused::ENGINE_PAUSED_ENTRY_TYPE,
                Some(serde_json::json!({"reason": reason, "customType": custom_type, "count": count, "at": now}))));
            if let Some(id) = entry.get("id").and_then(Value::as_str) {
                self.emit_entry_appended(id);
            }
        }
        let entry = self.with_session_manager_mut(|manager| {
            manager.append_custom(ENGINE_TURN_LIMIT_ENTRY_TYPE, Some(details.clone()))
        });
        if let Some(id) = entry.get("id").and_then(Value::as_str) {
            self.emit_entry_appended(id);
        }
        self.emit_extension_event(ENGINE_TURN_LIMIT_EVENT, &details);
        if !self.engine_turn_pause_notified.swap(true, Ordering::SeqCst) {
            self.notify_ui(
                &crate::engine_turn_limit::engine_turn_limit_notice(&stop),
                maho_ext_api::NotificationType::Warning,
            );
        }
        true
    }

    /// senpi `_recordRefusedEngineTurnMessage`: a refused engine turn still records its message.
    fn record_refused_engine_turn_message(
        &self,
        message: maho_agent::harness::messages::CustomMessage,
    ) {
        if let Err(error) = self.append_extension_custom_message(message) {
            self.session_logger.warn(
                "engine_turn_record_write_failed",
                Some(&serde_json::Map::from_iter([(
                    "error".to_owned(),
                    Value::String(error),
                )])),
            );
        }
    }

    /// senpi `_userInputSources.set`: remembered until the message persists (bounded).
    fn remember_input_source(&self, message: &AgentMessage, source: Option<InputSource>) {
        let Some(source) = source else {
            return;
        };
        let mut sources = lock(&self.user_input_sources);
        if sources.len() >= 64 {
            sources.remove(0);
        }
        sources.push((message.clone(), source));
    }

    fn take_input_source(&self, message: &AgentMessage) -> Option<InputSource> {
        let mut sources = lock(&self.user_input_sources);
        let index = sources
            .iter()
            .position(|(candidate, _)| candidate == message)?;
        Some(sources.remove(index).1)
    }

    pub async fn prompt(
        &self,
        text: &str,
        mut options: PromptOptions,
    ) -> Result<PromptDisposition, String> {
        self.state().standing_recovery = None;
        options
            .observed_pause_seq
            .get_or_insert_with(|| lock(&self.engine_turn_hold).current_pause_seq());
        options.hold_after_preparation = options
            .input_origin
            .unwrap_or_else(|| input_origin_of(options.source))
            == InputOrigin::Person
            && self
                .holds()
                .iter()
                .any(|h| h.kind == crate::engine_turn_hold::HoldKind::RunFence);
        let input_hold = Arc::new(self.external_admission.begin_input(
            text.starts_with('/'),
            options.source == Some(InputSource::Extension),
        ));
        if is_human_input(options.source) && !options.human_input_reported {
            self.human_input_admitted(prompt_kind(&options));
        }
        options.human_input_reported = true;
        if options
            .input_origin
            .unwrap_or_else(|| input_origin_of(options.source))
            == InputOrigin::Person
            && self
                .holds()
                .iter()
                .any(|h| h.kind == crate::engine_turn_hold::HoldKind::InheritedHold)
        {
            let mut input = json!({"inputId":options.client_message_id.clone().unwrap_or_else(||format!("prompt-{}",uuid::Uuid::new_v4())),"revision":0,"text":text});
            if let Some(images) = &options.images
                && !images.is_empty()
            {
                input["images"] = serde_json::to_value(images).map_err(|e| e.to_string())?;
            }
            if options.expand_prompt_templates.unwrap_or(true) {
                input["prepare"] = json!(true);
            }
            self.hold_input(input)?;
            let mut hold = lock(&self.engine_turn_hold);
            let keys = hold
                .holds()
                .into_iter()
                .filter(|h| h.kind == crate::engine_turn_hold::HoldKind::SessionHold)
                .map(|h| h.key)
                .collect::<Vec<_>>();
            hold.release(&keys, crate::engine_turn_hold::HoldReleaseCause::Input)?;
            return Ok(PromptDisposition::Queued);
        }
        // Preparation runs before prompt admission locks; a held command may itself submit input.
        let (held_messages, held_ids) = if options.hold_after_preparation {
            (Vec::new(), Vec::new())
        } else {
            heap_future(|| self.take_held_input_messages()).await?
        };
        let _held_reservation = HeldInputReservation {
            session: self.clone(),
            ids: held_ids.clone(),
        };
        options.held_messages = held_messages;
        options.held_input_ids = held_ids;

        let origin = options
            .input_origin
            .unwrap_or_else(|| input_origin_of(options.source));
        // Counted before any await: an external delivery never overtakes this input.
        self.prompts_in_flight.fetch_add(1, Ordering::SeqCst);
        if origin == InputOrigin::Person && !options.hold_after_preparation {
            self.begin_person_input();
        }
        let admission = self.engine_admission_hook(options.prompt_admitted.take());
        let accepted_hold = input_hold.clone();
        let queued_held_messages = options.held_messages.clone();
        let queued_held_ids = options.held_input_ids.clone();
        let queued_session = self.clone();
        options.prompt_admitted = Some(Arc::new(move |disposition| {
            if disposition == PromptDisposition::Queued {
                let generation = queued_session.user_abort_generation.load(Ordering::SeqCst);
                lock(&queued_session.delivered_held_inputs)
                    .extend(queued_held_ids.iter().cloned().map(|id| (id, generation)));
                for message in &queued_held_messages {
                    queued_session.agent.steer(message.clone());
                }
            }
            accepted_hold.accepted();
            admission(disposition);
        }));
        let admitted = options.prompt_admitted.clone();
        let mut reported = false;
        let mut readiness = None;
        let result = {
            let (readiness, reported) = (&mut readiness, &mut reported);
            heap_future(move || self.prompt_with_readiness(text, options, readiness, reported))
        }
        .await;
        if !reported
            && let Ok(disposition) = &result
            && let Some(admitted) = admitted
        {
            admitted(*disposition);
        }
        self.prompts_in_flight.fetch_sub(1, Ordering::SeqCst);
        // No await unless a release is pending: settling must not delay a prompt's completion.
        self.settle_input_release().await;
        input_hold.end();
        result
    }

    /// Wraps a prompt's admission callback: a started or queued input claims the wakes its release holds.
    fn engine_admission_hook(
        &self,
        inner: Option<Arc<dyn Fn(PromptDisposition) + Send + Sync>>,
    ) -> Arc<dyn Fn(PromptDisposition) + Send + Sync> {
        let session = self.clone();
        Arc::new(move |disposition| {
            if matches!(
                disposition,
                PromptDisposition::Started | PromptDisposition::Queued
            ) {
                session.admit_engine_turn_input(disposition == PromptDisposition::Queued);
            }
            if let Some(inner) = &inner {
                inner(disposition);
            }
        })
    }

    /// senpi `messages.push(...this._engineTurnInput.messages())`: wakes the release owns ride with the input.
    fn engine_turn_input_messages(&self) -> Vec<AgentMessage> {
        lock(&self.engine_turn_input)
            .messages()
            .into_iter()
            .filter_map(|message| session_message_from_value(message).ok())
            .collect()
    }

    fn admit_engine_turn_input(&self, queued: bool) {
        let mut to_enqueue = Vec::new();
        {
            let mut hold = lock(&self.engine_turn_hold);
            lock(&self.engine_turn_input)
                .admit(&mut hold, queued, |message| to_enqueue.push(message));
        }
        for message in to_enqueue {
            if let Ok(message) = session_message_from_value(message) {
                self.agent.steer(message);
            }
        }
    }

    /// A person's input (prompt, steer, follow-up, answer) attends the session and releases what it holds.
    fn begin_person_input(&self) {
        self.record_user_activity(UserActivityKind::Prompt);
        if let Some(tracker) = self.deadline_tracker.get() {
            tracker.clear_hold();
        }
        self.engine_resume_window.store(false, Ordering::SeqCst);
        let mut hold = lock(&self.engine_turn_hold);
        lock(&self.engine_turn_input).begin(&mut hold);
    }

    /// Every input release settles here, once nothing could still admit it: no prompt in flight and no run
    /// active. Called from every prompt's end, from run settlement, and after an answer is dispatched.
    pub(crate) async fn settle_input_release(&self) {
        if !lock(&self.engine_turn_input).has_pending() {
            return;
        }
        if self.prompts_in_flight.load(Ordering::SeqCst) > 0
            || self.agent_run_active.load(Ordering::SeqCst)
            || self.agent.is_streaming_or_has_pending_tool_calls()
        {
            return;
        }
        // Turns deferred by the agent_settled window or the session_start gate still enter prompt() later.
        if lock(&self.settled_delivery).active() {
            return;
        }
        if self
            .extension_runner
            .lock()
            .await
            .as_ref()
            .is_some_and(|runner| runner.session_start_turns_held())
        {
            return;
        }
        let sources = {
            let mut hold = lock(&self.engine_turn_hold);
            lock(&self.engine_turn_input).take_settlement(&mut hold)
        };
        if let Some(sources) = sources
            && let Err(error) = self.emit_held_wakes_released(sources).await
        {
            self.session_logger.warn(
                "input_release_settle_failed",
                Some(&serde_json::Map::from_iter([(
                    "error".to_owned(),
                    Value::String(error),
                )])),
            );
        }
    }

    async fn emit_held_wakes_released(
        &self,
        sources: BTreeMap<String, usize>,
    ) -> Result<(), String> {
        let mut runner = self.extension_runner.lock().await.clone();
        let Some(runner) = runner.as_mut() else {
            return Ok(());
        };
        let sources = sources
            .into_iter()
            .map(|(source, count)| (source, u32::try_from(count).unwrap_or(u32::MAX)))
            .collect();
        runner
            .emit_held_wakes_released(sources)
            .await
            .map_err(|failure| failure.message)
    }

    /// Host input entry point; not exposed by the extension API or its event bus.
    pub fn record_user_activity(&self, kind: UserActivityKind) {
        let listeners: Vec<UserActivityListener> = lock(&self.user_activity_listeners)
            .iter()
            .map(|(_, listener)| Arc::clone(listener))
            .collect();
        for listener in listeners {
            listener(kind);
        }
    }

    /// Subscribe to user activity (a draft keystroke or a submitted prompt). Returns the unsubscribe closure.
    pub fn on_user_activity(
        &self,
        listener: UserActivityListener,
    ) -> impl FnOnce() + Send + 'static {
        let id = self.next_listener_id.fetch_add(1, Ordering::SeqCst);
        lock(&self.user_activity_listeners).push((id, listener));
        let session = self.clone();
        move || lock(&session.user_activity_listeners).retain(|(existing, _)| *existing != id)
    }

    fn notify_ui(&self, message: &str, kind: maho_ext_api::NotificationType) {
        let ui = self.state().extension_ui_context.clone();
        if let Some(ui) = ui {
            ui.notify(message, kind);
        }
    }

    /// Whether engine turns are held back (the session is paused).
    pub fn paused(&self) -> bool {
        lock(&self.engine_turn_hold).is_held()
    }

    pub fn wake_source_counts(&self) -> crate::session_activity::ActiveCounts {
        self.state().wake_sources.active_counts()
    }
    /// senpi `activeWakeSources`: the sources that currently publish live work.
    pub fn active_wake_sources(&self) -> Vec<String> {
        self.state()
            .wake_sources
            .active_sources()
            .into_iter()
            .map(str::to_owned)
            .collect()
    }
    pub fn paused_notice_count(&self) -> usize {
        lock(&self.engine_turn_hold).notice_count()
    }

    pub fn pause_seq(&self) -> u64 {
        lock(&self.engine_turn_hold).current_pause_seq()
    }

    pub fn run_paused(&self) -> bool {
        lock(&self.engine_turn_hold)
            .holds()
            .iter()
            .any(|hold| hold.kind == crate::engine_turn_hold::HoldKind::RunFence)
    }

    pub fn holds(&self) -> Vec<crate::engine_turn_hold::SessionHold> {
        lock(&self.engine_turn_hold).holds()
    }
    pub async fn prompt_size_prefix_request(
        &self,
        signal: maho_ai::utils::abort::AbortSignal,
    ) -> crate::prompt_cache_prefix_request::PromptCachePrefixResult {
        use crate::prompt_cache_prefix_request::{
            PromptCachePrefixRequest, PromptCachePrefixResult,
        };
        let work = async {
            self.wait_for_idle().await;
            let mut guard = self.extension_runner.lock().await;
            let paths = guard
                .as_ref()
                .map_or_else(Vec::new, |runner| runner.extension_paths());
            let names =
                crate::prompt_size_extension_names::prompt_size_extension_names(&paths, |_| None);
            let unsafe_paths = guard.as_ref().map_or_else(Vec::new, |runner| {
                runner.preview_unsafe_before_agent_start_paths()
            });
            if !unsafe_paths.is_empty() {
                return PromptCachePrefixResult::Skipped(format!(
                    "before_agent_start handlers not registered previewSafe: {}",
                    unsafe_paths.join(", ")
                ));
            }
            let mut accounting = None;
            let mut system_prompt = String::new();
            for _ in 0..2 {
                let base = self.state().base_system_prompt.clone();
                let skill_count = self
                    .state()
                    .skills
                    .iter()
                    .filter(|skill| !skill.disable_model_invocation)
                    .count();
                let current =
                    std::sync::Mutex::new(crate::prompt_size_report::PromptSizeAccounting::new(
                        &base,
                        skill_count,
                        &[],
                        names.clone(),
                    ));
                system_prompt = base.clone();
                if let Some(runner) = guard.as_mut() {
                    let observe = |path: &str, before: &str, after: &str, parts: Option<&[maho_ext_api::DynamicSystemPromptPart]>| {
                        let parts = parts.map(|parts| parts.iter().map(|part| crate::prompt_size_report::DynamicSystemPromptPart { name: part.name.clone(), source: part.source.clone(), text: part.text.clone() }).collect::<Vec<_>>());
                        current.lock().unwrap_or_else(std::sync::PoisonError::into_inner).observe(path,before,after,parts.as_deref());
                    };
                    let event = maho_ext_api::BeforeAgentStartEvent {
                        prompt: String::new(),
                        trigger: "prompt".into(),
                        preview: true,
                        images: None,
                        system_prompt: base.clone(),
                        system_prompt_options: self.extension_system_prompt_options(),
                    };
                    match runner
                        .emit_before_agent_start_observed(event, Some(&observe))
                        .await
                    {
                        Ok(Some(result)) => {
                            if let Some(prompt) = result.system_prompt {
                                system_prompt = prompt;
                            }
                        }
                        Ok(None) => {}
                        Err(error) => return PromptCachePrefixResult::Skipped(error.message),
                    }
                }
                accounting = Some(
                    current
                        .into_inner()
                        .unwrap_or_else(std::sync::PoisonError::into_inner),
                );
                if self.state().base_system_prompt == base {
                    break;
                }
            }
            let third_party = guard
                .as_ref()
                .map_or_else(Vec::new, |runner| runner.third_party_session_start_paths());
            let state = self.agent.state();
            let tools = state
                .declared_tools
                .clone()
                .unwrap_or_else(|| state.tools().to_vec())
                .into_iter()
                .map(|tool| tool.tool)
                .collect::<Vec<_>>();
            let report = accounting.map(|accounting| accounting.report(&tools));
            let options = serde_json::json!({"thinkingLevel":self.thinking_level(),"thinkingSelection":self.thinking_selection(),"sessionId":self.session_id(),"serviceTier":self.service_tier()});
            PromptCachePrefixResult::Ready(Box::new(PromptCachePrefixRequest {
                model: self.model(),
                system_prompt,
                tools,
                options,
                size_report: report,
                startup_notes: Some(crate::prompt_size_startup::prompt_size_startup_notes(
                    &third_party,
                    &names,
                    None,
                    false,
                )),
            }))
        };
        tokio::select! { biased; () = signal.cancelled() => crate::prompt_cache_prefix_request::PromptCachePrefixResult::Skipped(crate::prompt_cache_prefix_request::cancelled_reason(&signal)), result = work => result }
    }
    /// senpi `getPromptCachePrefixRequest`: the preview-composed prefix of the next user turn
    /// (`prompt_size_prefix_request`) as a ready `{ model, context, options }` request. Unless
    /// `sizes_only`, auth, headers (through `before_provider_headers`), extra body and env resolve
    /// the way the turn resolves them; an auth failure skips with its message.
    pub async fn prompt_cache_prefix_request(
        &self,
        options: maho_ext_api::PromptCachePrefixRequestOptions,
    ) -> maho_ext_api::PromptCachePrefixResult {
        let sizes_only = options.sizes_only;
        self.prompt_cache_prefix_builds
            .run(options.signal, |signal| {
                self.build_prompt_cache_prefix_request(sizes_only, signal)
            })
            .await
    }
    /// Resolves once the session start the prefix depends on has settled (senpi `ready`).
    async fn session_start_settled(&self) {
        let mut settled = self.session_start_settled.subscribe();
        // The sender lives as long as the session, so the wait ends only by settling; the
        // borrow guard is dropped within the statement.
        let _settled: bool = settled.wait_for(|settled| *settled).await.is_ok();
    }
    /// senpi `_beginSessionStartSettlement`: unsettled until the returned guard drops.
    fn begin_session_start_settlement(&self) -> SessionStartSettlement {
        self.session_start_settled.send_replace(false);
        SessionStartSettlement(self.session_start_settled.clone())
    }
    async fn build_prompt_cache_prefix_request(
        &self,
        sizes_only: bool,
        signal: maho_ai::utils::abort::AbortSignal,
    ) -> maho_ext_api::PromptCachePrefixResult {
        use crate::prompt_cache_prefix_request::{
            PromptCachePrefixResult as Built, cancelled_reason,
        };
        use maho_ext_api::PromptCachePrefixResult;
        tokio::select! {
            biased;
            () = signal.cancelled() => return PromptCachePrefixResult::Skipped(cancelled_reason(&signal)),
            () = self.session_start_settled() => {}
        }
        let request = match self.prompt_size_prefix_request(signal.clone()).await {
            Built::Skipped(reason) => return PromptCachePrefixResult::Skipped(reason),
            Built::Ready(request) => *request,
        };
        let mut stream = maho_ai::types::SimpleStreamOptions {
            reasoning: thinking_level_from_model_level(self.thinking_level()),
            thinking_selection: self.thinking_selection(),
            thinking_budgets: self.agent.thinking_budgets(),
            service_tier: self.resolve_request_service_tier(&request.model),
            ..Default::default()
        };
        stream.stream.session_id = Some(self.session_id());
        stream.stream.request.on_payload = self.agent.on_payload();
        let mut model = request.model;
        if !sizes_only {
            let auth = match self.get_required_request_auth(&model).await {
                Ok(auth) => auth,
                Err(error) => return PromptCachePrefixResult::Skipped(error),
            };
            let mut headers: ProviderHeaders = auth
                .headers
                .unwrap_or_default()
                .into_iter()
                .map(|(name, value)| (name, Some(value)))
                .collect();
            let runner = self.extension_runner.lock().await.clone();
            if let Some(mut runner) = runner
                && runner.has_handlers(maho_ext_api::EventKind::BeforeProviderHeaders)
            {
                headers = match runner.emit_before_provider_headers(headers).await {
                    Ok(headers) => headers,
                    Err(error) => return PromptCachePrefixResult::Skipped(error.message),
                };
            }
            model = auth.model;
            stream.stream.request.api_key = auth.api_key;
            stream.stream.request.headers = Some(headers);
            stream.stream.request.env = auth.env;
            stream.stream.extra_body = auth.extra_body;
        }
        if signal.aborted() {
            return PromptCachePrefixResult::Skipped(cancelled_reason(&signal));
        }
        PromptCachePrefixResult::Ready(Box::new(maho_ext_api::PromptCachePrefixRequest {
            model,
            context: maho_ai::types::Context {
                system_prompt: Some(request.system_prompt),
                messages: Vec::new(),
                active_tool_names: Some(
                    request.tools.iter().map(|tool| tool.name.clone()).collect(),
                ),
                tools: Some(request.tools),
            },
            options: stream,
        }))
    }
    pub async fn start_run_resume_turn(
        &self,
        ports: Arc<dyn crate::run_resume_turn::RunResumePorts>,
    ) -> crate::run_resume_turn::RunResumeTurn {
        crate::run_resume_turn::start_run_resume_turn(ports).await
    }
    /// senpi `SessionLifecycleController.requiresHumanContinuation`: the last run was interrupted and no human input
    /// has been admitted since, so automatic continuations wait for a person or Resume.
    pub fn requires_human_continuation(&self) -> bool {
        lock(&self.lifecycle).interrupted && !self.lifecycle_human_pending.load(Ordering::SeqCst)
    }
    /// The hold key to install when this session is opened, if its run must wait for a person (senpi#4204).
    pub fn opening_hold_key(&self) -> Option<&'static str> {
        self.requires_human_continuation()
            .then_some(crate::engine_turn_hold::INTERRUPTED_RUN_HOLD_KEY)
    }
    /// senpi `LifecycleSessionBinding` restore -> `controller.markInterruptedFromCustody`: consumed custody, or a
    /// ticket whose delivery the transcript already holds, marks the run interrupted before session_start runs.
    pub fn mark_interrupted_from_custody(&self, delivered: &dyn Fn(&str) -> bool) {
        let consumed = lock(&self.work_obligations)
            .as_ref()
            .is_some_and(|registry| {
                registry.active().into_iter().any(|record| {
                    record.ticket_id.as_deref().is_some_and(|ticket| {
                        record.state == crate::work_custody_record::CustodyState::Consumed
                            || delivered(ticket)
                    })
                })
            });
        if consumed {
            lock(&self.lifecycle).interrupted = true;
        }
    }
    /// The production binding: the session's own ledger is the lifecycle controller, so restore's
    /// `mark_interrupted_from_custody` reaches it before any queued delivery is re-sent (senpi#4204).
    pub fn bind_session_lifecycle(
        &self,
    ) -> crate::lifecycle_session_binding::LifecycleSessionBinding {
        self.bind_lifecycle(Arc::new(SessionLifecycleHost(Arc::downgrade(&self.inner))))
    }
    pub fn bind_lifecycle(
        &self,
        host: Arc<dyn crate::lifecycle_session_binding::LifecycleBindingHost>,
    ) -> crate::lifecycle_session_binding::LifecycleSessionBinding {
        crate::lifecycle_session_binding::LifecycleSessionBinding::new(Arc::new(self.clone()), host)
    }
    /// Connection owners retain this attachment only for handshakes advertising grant_select.
    pub fn attach_mcp_grant_presentation(
        &self,
        select: crate::mcp_grant_presentations::Select,
    ) -> crate::mcp_grant_presentations::McpGrantAttachment {
        crate::mcp_grant_presentations::attach_mcp_grant_presentation(&self.session_id(), select)
    }
    pub async fn select_mcp_grant(
        &self,
        title: String,
        signal: Option<maho_ai::utils::abort::AbortSignal>,
    ) -> bool {
        crate::mcp_grant_presentations::select_mcp_grant(
            crate::mcp_grant_presentations::get_mcp_grant_presentation(Some(&self.session_id())),
            title,
            signal,
        )
        .await
    }
    pub fn with_run_holds<T>(&self, f: impl FnOnce(&mut crate::run_holds::RunHolds<'_>) -> T) -> T {
        let mut ledger = lock(&self.engine_turn_hold);
        f(&mut crate::run_holds::RunHolds::new(&mut ledger))
    }

    async fn prompt_with_readiness(
        &self,
        text: &str,
        options: PromptOptions,
        readiness: &mut Option<tokio::sync::oneshot::Sender<()>>,
        disposition_reported: &mut bool,
    ) -> Result<PromptDisposition, String> {
        if options
            .signal
            .as_ref()
            .is_some_and(|signal| signal.aborted())
        {
            return Err("Prompt cancelled".to_owned());
        }
        // senpi `prompt()`: an in-flight user abort quiesces before this prompt is considered.
        let user_abort_wait = lock(&self.user_abort_wait).clone();
        if let Some(wait) = user_abort_wait {
            wait.await;
            if options
                .signal
                .as_ref()
                .is_some_and(|signal| signal.aborted())
            {
                return Err("Prompt cancelled".to_owned());
            }
        }
        if self.state().sizes_only_startup {
            if let Some(readiness) = readiness.take() {
                let _ = readiness.send(());
            }
            self.queue_expanded_input(
                text.to_owned(),
                options.images,
                StreamingBehavior::FollowUp,
                None,
                options.source,
            );
            return Ok(PromptDisposition::Handled);
        }
        if text.starts_with('/') && heap_future(|| self.try_execute_extension_command(text)).await?
        {
            return Ok(PromptDisposition::Handled);
        }
        // senpi `prompt()`: manual compaction owns admission; only extension prompts may wait on it.
        let manual_compaction_active = {
            let state = self.state();
            state.pending_compaction_admission.is_some()
                || (state.compaction_abort_controller.is_some() && !state.auto_compaction_owned)
        };
        if options.source != Some(InputSource::Extension) && manual_compaction_active {
            return Err("Cannot submit a prompt while compaction is in progress. Wait for compaction to finish and retry.".to_owned());
        }
        // Bare "." on a session that already has messages is the manual-continue shortcut: it
        // never becomes a visible user turn. Route it through the hidden custom-message path
        // before extensions see an input event, so a "." cannot be echoed, transformed, or
        // persisted as literal text.
        if crate::manual_continue::is_manual_continue_submission(
            text,
            self.agent.has_messages(),
            options
                .images
                .as_ref()
                .is_some_and(|images| !images.is_empty()),
        ) {
            heap_future(|| {
                self.send_custom_message_content(
                    crate::manual_continue::MANUAL_CONTINUE_CUSTOM_TYPE.to_owned(),
                    maho_agent::harness::messages::CustomMessageContent::Text(
                        crate::manual_continue::MANUAL_CONTINUE_DIRECTIVE.to_owned(),
                    ),
                    false,
                    None,
                    maho_ext_api::SendMessageOptions {
                        trigger_turn: true,
                        deliver_as: Some(
                            if options.streaming_behavior == Some(StreamingBehavior::FollowUp) {
                                maho_ext_api::DeliverAs::FollowUp
                            } else {
                                maho_ext_api::DeliverAs::Steer
                            },
                        ),
                    },
                )
            })
            .await?;
            return Ok(PromptDisposition::Handled);
        }
        let (pending_compaction, compaction_generation) =
            if options.source == Some(InputSource::Extension) {
                let state = self.state();
                (
                    state.pending_compaction_admission.clone(),
                    (state.compaction_lifecycle.state().status() == "running"
                        && state.compaction_lifecycle.state().operation().is_some_and(
                            |operation| {
                                operation.stage
                                    == crate::compaction::lifecycle::CompactionStage::Execution
                                    && operation.reason == "manual"
                            },
                        ))
                    .then(|| state.compaction_lifecycle.state().generation()),
                )
            } else {
                (None, None)
            };
        let waits_for_manual_compaction =
            pending_compaction.is_some() || compaction_generation.is_some();
        let streaming = self.is_streaming();
        let _prompt_start = {
            let mut state = self.state();
            if !streaming && !state.prompt_start_pending && options.streaming_behavior.is_none() {
                state.prompt_start_pending = true;
                Some(PromptStartGuard(self))
            } else {
                None
            }
        };
        // senpi `prompt()`: a prompt that cannot queue waits for the running turn, auto-compaction and
        // other settled session work instead of failing. Native extension prompts are delivered by a
        // task that already holds a work guard, so they keep the queue route below.
        if !waits_for_manual_compaction
            && options.streaming_behavior.is_none()
            && options.source != Some(InputSource::Extension)
            && (self.is_streaming() || self.is_compacting() || self.work_barrier.has_active_work())
        {
            self.work_barrier
                .wait_for_settled(|| self.agent.wait_for_idle())
                .await;
            if options
                .signal
                .as_ref()
                .is_some_and(|signal| signal.aborted())
            {
                return Err("Prompt cancelled".to_owned());
            }
        }
        let queues_behind_pending_prompt =
            self.state().prompt_start_pending && options.streaming_behavior.is_some();
        if !waits_for_manual_compaction
            && (self.is_streaming() || self.is_compacting() || queues_behind_pending_prompt)
        {
            if options.thinking_level.is_some() {
                return Err("Cannot set thinkingLevel on a queued prompt; set it after the current turn completes.".to_owned());
            }
            let mode = options.streaming_behavior.ok_or_else(||
                "Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message.".to_owned())?;
            heap_future(|| {
                self.queue_user_input(
                    text,
                    options.images,
                    mode,
                    QueuedInputOptions {
                        source: options.source,
                        from_prompt: true,
                        ..Default::default()
                    },
                )
            })
            .await?;
            return Ok(PromptDisposition::Queued);
        }
        let _admission = if let Some(signal) = &options.signal {
            tokio::select! {
                biased;
                _ = signal.cancelled() => return Err("Prompt cancelled".to_owned()),
                admission = self.prompt_admission.lock() => admission,
            }
        } else {
            self.prompt_admission.lock().await
        };
        let weak = Arc::downgrade(&self.inner);
        let _prompt_signal_bridge = options.signal.map(|signal| {
            AbortSignalBridge(tokio::spawn(async move {
                signal.cancelled().await;
                if let Some(inner) = weak.upgrade() {
                    AgentSession { inner }.abort().await;
                }
            }))
        });
        let _work = self.work_barrier.begin();
        self.state().user_aborted = false;
        // senpi `prompt()` turn boundary: restore the primary model if the fallback cooldown expired.
        if !self.is_streaming() {
            self.restore_fallback_primary().await;
        }
        let Some((text, images, input_id)) =
            heap_future(|| self.run_input_handlers(text, options.images, options.source, None))
                .await?
        else {
            return Ok(PromptDisposition::Handled);
        };
        let text = self.expand_input(&text, options.expand_prompt_templates.unwrap_or(true))?;
        let compaction_failed = {
            let state = self.state();
            let lifecycle = state.compaction_lifecycle.state();
            pending_compaction
                .as_ref()
                .is_some_and(|pending| !pending.completed.load(Ordering::SeqCst))
                || (compaction_generation
                    .is_some_and(|generation| generation == lifecycle.generation())
                    && matches!(lifecycle.status(), "failed" | "aborted"))
        };
        if compaction_failed {
            let mode = options
                .streaming_behavior
                .unwrap_or(StreamingBehavior::Steer);
            self.queue_expanded_input(text, images, mode, None, options.source);
            heap_future(|| {
                self.dispatch_extension_event(maho_ext_api::ExtensionEvent::InputDisposition {
                    input_id,
                    disposition: maho_ext_api::InputDisposition::Queued,
                })
            })
            .await;
            *disposition_reported = true;
            drop(readiness.take());
            if let Some(admitted) = &options.prompt_admitted {
                admitted(PromptDisposition::Queued);
            }
            return Ok(PromptDisposition::Queued);
        }
        // senpi `prompt()`: refuse before any provider work when the model's provider has no auth.
        let model = self.model();
        let has_configured_auth = self.model_runtime().has_configured_auth(&model.provider)
            || match self.model_runtime().check_auth(&model.provider, None).await {
                Ok(check) => check.is_some(),
                Err(error) => {
                    heap_future(|| {
                        self.dispatch_extension_event(
                            maho_ext_api::ExtensionEvent::InputDisposition {
                                input_id,
                                disposition: maho_ext_api::InputDisposition::Rejected,
                            },
                        )
                    })
                    .await;
                    return Err(error.message);
                }
            };
        if !has_configured_auth {
            heap_future(|| {
                self.dispatch_extension_event(maho_ext_api::ExtensionEvent::InputDisposition {
                    input_id,
                    disposition: maho_ext_api::InputDisposition::Rejected,
                })
            })
            .await;
            if self.model_runtime().is_using_oauth(&model.provider) {
                return Err(format!(
                    "Authentication failed for \"{}\". Credentials may have expired or network is unavailable. Run '/login {}' to re-authenticate.",
                    model.provider, model.provider
                ));
            }
            return Err(crate::auth_guidance::format_no_api_key_found_message(
                &model.provider,
            ));
        }
        if !self.is_compaction_delegated()
            && self.pending_model_switch().is_none()
            && heap_future(|| self.check_pre_prompt_overflow()).await?
        {
            // senpi `_checkCompaction` (inline): the overflow route already settled admission.
        } else if self.auto_compaction_enabled()
            && !self.is_compaction_delegated()
            && self.pending_model_switch().is_none()
        {
            let model = self.model();
            let resolved = self.get_compaction_settings(Some(&model))?;
            if self
                .get_context_usage()
                .and_then(|usage| usage.tokens)
                .is_some_and(|tokens| {
                    tokens.saturating_add(text.len().div_ceil(4) as u64)
                        > model
                            .context_window
                            .saturating_sub(if resolved.reserve_scaling_enabled {
                                crate::compaction::compaction::resolve_reserve_tokens(
                                    model.context_window as f64,
                                    resolved.reserve_tokens as f64,
                                ) as u64
                            } else {
                                resolved.reserve_tokens as u64
                            })
                })
            {
                let result =
                    heap_future(|| self.compact_for_model_splitting_turn(&model, "pre-prompt"))
                        .await;
                if !self.is_compaction_delegated() {
                    result?;
                }
            }
        }
        if let Some(pending) = self.pending_model_switch() {
            heap_future(|| self.compact_for_model(None, &pending.model, "pre-prompt")).await?;
            let measured = self
                .with_session_manager(|manager| manager.current_context())
                .messages
                .iter()
                .map(crate::compaction::compaction::estimate_tokens)
                .sum();
            let admitted = self.reduce_for_switch_target(&pending.model, measured)?;
            self.state().pending_model_switch = None;
            self.assert_model_usable(&pending.model, admitted)?;
            heap_future(|| {
                self.set_model_internal(
                    pending.model,
                    pending.persist_default,
                    maho_ext_api::ModelSelectSource::Set,
                    false,
                )
            })
            .await?;
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
        // senpi#2115: a real turn's before_agent_start never runs beside a preview pass.
        self.prompt_cache_prefix_builds.cancel_all();
        let before = {
            let mut runner = self.extension_runner.lock().await.clone();
            match runner.as_mut() {
                Some(runner) => runner
                    .emit_before_agent_start(maho_ext_api::BeforeAgentStartEvent {
                        prompt: text.clone(),
                        images: images.clone(),
                        system_prompt: base_system_prompt,
                        trigger: "prompt".into(),
                        preview: false,
                        system_prompt_options: self.extension_system_prompt_options(),
                    })
                    .await
                    .map_err(|error| error.to_string()),
                None => Ok(None),
            }
        };
        let before = match before {
            Ok(before) => before,
            Err(error) => {
                self.state()
                    .pending_next_turn_messages
                    .splice(0..0, consumed);
                heap_future(|| {
                    self.dispatch_extension_event(maho_ext_api::ExtensionEvent::InputDisposition {
                        input_id,
                        disposition: maho_ext_api::InputDisposition::Rejected,
                    })
                })
                .await;
                return Err(error);
            }
        };
        let prepared_images = images.clone();
        let user_message = make_user_message(&text, images);
        self.remember_input_source(
            &user_message,
            Some(options.source.unwrap_or(InputSource::Interactive)),
        );
        let mut messages = self.engine_turn_input_messages();
        messages.extend(options.held_messages.clone());
        let prompt_index = messages.len();
        messages.push(user_message);
        messages.extend(consumed.clone());
        self.apply_before_agent_start_result(&mut messages, before, prompt_index)?;
        if let Err(error) = heap_future(|| self.enforce_final_provider_admission(&messages)).await {
            self.state()
                .pending_next_turn_messages
                .splice(0..0, consumed);
            heap_future(|| {
                self.dispatch_extension_event(maho_ext_api::ExtensionEvent::InputDisposition {
                    input_id,
                    disposition: maho_ext_api::InputDisposition::Rejected,
                })
            })
            .await;
            return Err(error);
        }
        if options.hold_after_preparation
            || options.observed_pause_seq.is_some_and(|observed| {
                self.holds().iter().any(|h| {
                    h.kind != crate::engine_turn_hold::HoldKind::Unattended
                        && lock(&self.engine_turn_hold).current_pause_seq() > observed
                })
            })
            || (options
                .input_origin
                .unwrap_or_else(|| input_origin_of(options.source))
                == InputOrigin::Person
                && self.holds().iter().any(|h| {
                    matches!(
                        h.kind,
                        crate::engine_turn_hold::HoldKind::RunFence
                            | crate::engine_turn_hold::HoldKind::InheritedHold
                    )
                }))
        {
            let mut held = json!({"inputId":options.client_message_id.clone().unwrap_or_else(||format!("prompt-{}",uuid::Uuid::new_v4())),"revision":0,"text":text});
            if let Some(images) = prepared_images
                && !images.is_empty()
            {
                held["images"] = serde_json::to_value(images).map_err(|e| e.to_string())?;
            }
            self.hold_input(held)?;
            self.state()
                .pending_next_turn_messages
                .splice(0..0, consumed);
            *disposition_reported = true;
            if let Some(admitted) = &options.prompt_admitted {
                admitted(PromptDisposition::Handled);
            }
            heap_future(|| {
                self.dispatch_extension_event(maho_ext_api::ExtensionEvent::InputDisposition {
                    input_id,
                    disposition: maho_ext_api::InputDisposition::Handled,
                })
            })
            .await;
            return Ok(PromptDisposition::Handled);
        }
        heap_future(|| {
            self.dispatch_extension_event(maho_ext_api::ExtensionEvent::InputDisposition {
                input_id,
                disposition: maho_ext_api::InputDisposition::Started,
            })
        })
        .await;
        *disposition_reported = true;
        drop(readiness.take());
        if let Some(admitted) = &options.prompt_admitted {
            admitted(PromptDisposition::Started);
        }
        let generation = self.user_abort_generation.load(Ordering::SeqCst);
        lock(&self.delivered_held_inputs).extend(
            options
                .held_input_ids
                .iter()
                .cloned()
                .map(|id| (id, generation)),
        );
        if let Some(spec) = options.tool_barrier.clone() {
            self.arm_tool_barrier(spec)?;
        }
        // senpi `_promptAgent`: an interrupted run admits no automatic turn. A turn the engine refused never ran, so
        // it releases the barrier it armed (senpi#4106).
        if self.requires_human_continuation() {
            if let Some(spec) = &options.tool_barrier {
                self.release_tool_barrier(&spec.id);
            }
            return Err(INTERRUPTED_RUN_REFUSAL.to_owned());
        }
        // senpi `_promptAgent`: a new turn admitted arms the inactivity lease and the hard max-runtime cap,
        // and owns the held handler waits it starts.
        if let Some(tracker) = self.deadline_tracker.get() {
            tracker.arm();
        }
        self.handler_turn_scope.fetch_add(1, Ordering::SeqCst);
        lock(&self.transcript_write_failures).start_run();
        if let Some(kept) = lock(&self.transcript_write_failures).take_refused_out(&self.messages())
        {
            self.agent.set_messages(kept);
        }
        self.agent_run_active.store(true, Ordering::SeqCst);
        self.hydrate_runtime_messages_for_run();
        // senpi `_promptAgent`: abort on a provider-side model substitution only when the setting
        // allows it and a configured fallback chain can take over.
        self.agent
            .set_abort_server_side_fallback(Some(self.abort_server_side_fallback_enabled()));
        self.state().required_compaction_admission_error = None;
        // Every large callee future in this function is built on the heap (`heap_future`): built
        // inline, each one takes its own slot in this frame in unoptimized builds, and the frames of
        // the prompt -> agent run -> retry recovery chain then overflow a 2 MiB thread stack.
        heap_future(|| {
            self.agent
                .prompt(maho_agent::agent::AgentPromptInput::Messages(messages))
        })
        .await;
        if let Err(error) = heap_future(|| self.finish_provider_turn()).await {
            // A turn the engine refused never ran, so it releases the barrier it armed (senpi#4106).
            if let Some(spec) = &options.tool_barrier
                && self.agent.signal().is_none()
            {
                self.release_tool_barrier(&spec.id);
            }
            return Err(error);
        }
        let transcript_failure = lock(&self.transcript_write_failures).take_report();
        self.flush_pending_bash_messages();
        let required_compaction_error = self.state().required_compaction_admission_error.take();
        let title_prompt = match options.session_title_prompt {
            Some(SessionTitlePrompt::Disabled) => None,
            Some(SessionTitlePrompt::Text(prompt)) => Some(prompt),
            None => Some(text),
        };
        if self.state().auto_title_sessions
            && let Some(text) = title_prompt
        {
            let session = self.clone();
            tokio::spawn(async move {
                session.generate_session_title_if_needed(&text).await;
            });
        }
        drop(_work);
        drop(_admission);
        heap_future(|| self.emit_agent_settled()).await;
        // senpi `_promptAgent`: the stored admission rejection surfaces as the prompt rejecting.
        if let Some(error) = required_compaction_error {
            return Err(error);
        }
        if let Some(failure) = transcript_failure {
            return Err(failure.error);
        }
        Ok(PromptDisposition::Started)
    }

    /// senpi `_applyBeforeAgentStartResult`.
    fn apply_before_agent_start_result(
        &self,
        messages: &mut Vec<AgentMessage>,
        result: Option<maho_ext_api::BeforeAgentStartCombinedResult>,
        prompt_index: usize,
    ) -> Result<(), String> {
        if let Some(before) = result {
            // `placement: "before-prompt"` messages land just before the triggering prompt, in handler order.
            for (offset, message) in before.before_prompt_messages.into_iter().enumerate() {
                messages.insert(
                    prompt_index + offset,
                    session_message_from_value(
                        serde_json::json!({"role":"custom", "customType":message.custom_type,
                    "content":message.content, "display":message.display, "details":message.details,
                    "timestamp":maho_ai::utils::diagnostics::now_ms()}),
                    )
                    .map_err(|error| error.to_string())?,
                );
            }
            for message in before.messages {
                messages.push(
                    session_message_from_value(
                        serde_json::json!({"role":"custom", "customType":message.custom_type,
                    "content":message.content, "display":message.display, "details":message.details,
                    "timestamp":maho_ai::utils::diagnostics::now_ms()}),
                    )
                    .map_err(|error| error.to_string())?,
                );
            }
            self.state().system_prompt_override = before.system_prompt.clone();
            self.agent.set_system_prompt(
                before
                    .system_prompt
                    .unwrap_or_else(|| self.state().base_system_prompt.clone()),
            );
        } else {
            // Ensure the base prompt is used (in case a previous turn had modifications).
            self.state().system_prompt_override = None;
            self.agent
                .set_system_prompt(self.state().base_system_prompt.clone());
        }
        Ok(())
    }

    async fn enforce_final_provider_admission(
        &self,
        additions: &[AgentMessage],
    ) -> Result<(), String> {
        let pending_queued_messages = || {
            let state = self.state();
            state
                .steering_messages
                .iter()
                .chain(&state.follow_up_messages)
                .map(|text| make_user_message(text, None))
                .collect::<Vec<_>>()
        };
        if !additions.iter().any(|message| message.role() == "custom")
            && pending_queued_messages().is_empty()
        {
            return Ok(());
        }
        let model = self.model();
        // senpi `if (!model) return;`: the unset agent model carries a zero context window.
        if model.context_window == 0 {
            return Ok(());
        }
        let raw = self.with_settings_manager(|manager| manager.get_value("compaction").cloned());
        let settings = raw
            .map(serde_json::from_value::<crate::compaction_settings_access::CompactionSettings>)
            .transpose()
            .map_err(|error| error.to_string())?;
        let resolved = crate::compaction_settings_resolver::resolve_compaction_settings(
            settings.as_ref(),
            Some(crate::compaction_settings_access::CompactionModelSelector {
                provider: &model.provider,
                id: &model.id,
            }),
        )?;
        if !self.auto_compaction_enabled() || self.is_compaction_delegated() {
            return Ok(());
        }
        let reserve = if resolved.reserve_scaling_enabled {
            crate::compaction::compaction::resolve_reserve_tokens(
                model.context_window as f64,
                resolved.reserve_tokens as f64,
            ) as u64
        } else {
            resolved.reserve_tokens as u64
        };
        let oversized = || -> Result<bool, String> {
            let messages = self
                .messages()
                .into_iter()
                .chain(additions.iter().cloned())
                .chain(pending_queued_messages())
                .map(|message| session_message_to_value(&message))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())?;
            let messages = crate::messages::filter_context_excluded_messages(messages);
            let estimate = crate::compaction::estimate_context_tokens(&messages);
            let compacted = self.with_session_manager(|manager| {
                manager
                    .latest_current_branch_entry_id("compaction", None)
                    .is_some()
            });
            let tokens = if compacted {
                messages
                    .iter()
                    .map(crate::compaction::estimate_tokens)
                    .sum()
            } else {
                estimate.tokens
            };
            Ok(tokens > model.context_window.saturating_sub(reserve))
        };
        if !oversized()? {
            return Ok(());
        }
        if !self
            .messages()
            .iter()
            .any(|message| message.role() == "assistant")
        {
            return Err("Compaction required before provider request".to_owned());
        }
        let result = self
            .compact_for_model_splitting_turn(&model, "pre-prompt")
            .await;
        if self.is_compaction_delegated() {
            return Ok(());
        }
        result?;
        if oversized()? {
            return Err("Compaction required before provider request".to_owned());
        }
        Ok(())
    }

    async fn run_input_handlers(
        &self,
        text: &str,
        images: Option<Vec<ImageContent>>,
        source: Option<InputSource>,
        streaming_behavior: Option<StreamingBehavior>,
    ) -> Result<Option<(String, Option<Vec<ImageContent>>, String)>, String> {
        let input_id = format!(
            "{}:{}",
            self.session_id(),
            self.reserve_queued_input_order()
        );
        let mut runner = self.extension_runner.lock().await.clone();
        let Some(runner) = runner.as_mut() else {
            return Ok(Some((text.to_owned(), images, input_id)));
        };
        let result = Box::pin(runner.emit_input(maho_ext_api::InputEvent {
            input_id: input_id.clone(),
            text: text.to_owned(),
            images: images.clone(),
            source: source.unwrap_or(InputSource::Interactive),
            streaming_behavior,
        }))
        .await
        .map_err(|error| error.to_string())?;
        match result {
            maho_ext_api::InputEventResult::Continue => {
                Ok(Some((text.to_owned(), images, input_id)))
            }
            maho_ext_api::InputEventResult::Transform { text, images } => {
                Ok(Some((text, images, input_id)))
            }
            maho_ext_api::InputEventResult::Handled => {
                Box::pin(runner.emit(maho_ext_api::ExtensionEvent::InputDisposition {
                    input_id,
                    disposition: maho_ext_api::InputDisposition::Handled,
                }))
                .await
                .map_err(|error| error.to_string())?;
                Ok(None)
            }
        }
    }

    async fn queue_user_input(
        &self,
        text: &str,
        images: Option<Vec<ImageContent>>,
        mode: StreamingBehavior,
        options: QueuedInputOptions,
    ) -> Result<(), String> {
        let origin = input_origin_of(options.source);
        if origin == InputOrigin::Person
            && !options.from_prompt
            && lock(&self.engine_turn_hold).held()
        {
            // A held session releases through prompt(), which admits the input and its held wakes together.
            Box::pin(self.prompt(
                text,
                PromptOptions {
                    images,
                    streaming_behavior: Some(mode),
                    source: options.source,
                    human_input_reported: true,
                    ..Default::default()
                },
            ))
            .await?;
            return Ok(());
        }
        if origin == InputOrigin::Person && !options.from_prompt {
            self.begin_person_input();
        }
        if let Some(command_text) = text.strip_prefix('/') {
            let name = command_text
                .split_once(' ')
                .map_or(command_text, |(name, _)| name);
            let runner = self.extension_runner.lock().await;
            if runner
                .as_ref()
                .is_some_and(|runner| runner.get_command(name).is_some())
            {
                return Err(format!(
                    "Extension command \"/{name}\" cannot be queued. Use prompt() or execute the command when not streaming."
                ));
            }
        }
        let Some((text, images, input_id)) = self
            .run_input_handlers(
                text,
                images,
                options.source,
                self.is_streaming().then_some(mode),
            )
            .await?
        else {
            return Ok(());
        };
        let text = self.expand_input(&text, true)?;
        self.queue_expanded_input(text, images, mode, options.enqueue_order, options.source);
        self.dispatch_extension_event(maho_ext_api::ExtensionEvent::InputDisposition {
            input_id,
            disposition: maho_ext_api::InputDisposition::Queued,
        })
        .await;
        Ok(())
    }

    fn queue_expanded_input(
        &self,
        text: String,
        images: Option<Vec<ImageContent>>,
        mode: StreamingBehavior,
        enqueue_order: Option<u64>,
        source: Option<InputSource>,
    ) {
        let queued_id = self.record_queued_input(&text, mode, enqueue_order, None, None);
        let message = make_user_message(&text, images);
        self.remember_input_source(&message, Some(source.unwrap_or(InputSource::Interactive)));
        self.state()
            .queued_messages
            .push((queued_id, message.clone()));
        let defer = {
            let state = self.state();
            state.prompt_start_pending && state.post_compaction_assistant_generation.is_some()
        };
        match mode {
            StreamingBehavior::Steer => {
                self.state().steering_messages.push(text);
                if defer {
                    self.state()
                        .post_compaction_deferred_steering_messages
                        .push(message);
                } else {
                    self.agent.steer(message);
                }
            }
            StreamingBehavior::FollowUp => {
                self.state().follow_up_messages.push(text);
                if defer {
                    self.state()
                        .post_compaction_deferred_follow_up_messages
                        .push(message);
                } else {
                    self.agent.follow_up(message);
                }
            }
        }
        self.emit_queue_update();
    }

    pub async fn steer(
        &self,
        text: &str,
        images: Option<Vec<ImageContent>>,
        options: QueuedInputOptions,
    ) -> Result<(), String> {
        if is_human_input(options.source) {
            self.human_input_admitted(HumanInputKind::Steer);
        }
        Box::pin(self.queue_user_input(text, images, StreamingBehavior::Steer, options)).await
    }

    pub async fn follow_up(
        &self,
        text: &str,
        images: Option<Vec<ImageContent>>,
        options: QueuedInputOptions,
    ) -> Result<(), String> {
        if is_human_input(options.source) {
            self.human_input_admitted(HumanInputKind::FollowUp);
        }
        Box::pin(self.queue_user_input(text, images, StreamingBehavior::FollowUp, options)).await
    }

    pub fn fence_run(&self, run_ref: Value) -> Result<(), String> {
        let kind = run_ref["kind"]
            .as_str()
            .ok_or("run reference has no kind")?;
        let id = run_ref["id"].as_str().ok_or("run reference has no id")?;
        if !["dag", "task", "workpool", "team"].contains(&kind) {
            return Err("invalid run reference kind".into());
        }
        let key = format!("run:{kind}:{id}");
        lock(&self.engine_turn_hold).add_hold(
            crate::engine_turn_hold::HoldKind::RunFence,
            Some(&key),
            Some(run_ref),
        );
        self.refresh_lifecycle();
        Ok(())
    }

    pub async fn stop(&self) {
        lock(&self.engine_turn_hold).add_hold(
            crate::engine_turn_hold::HoldKind::SessionHold,
            Some("stop"),
            None,
        );
        self.abort().await;
        self.dispatch_hold_changed("paused").await;
        self.refresh_lifecycle();
    }

    async fn dispatch_hold_changed(&self, change: &str) {
        let runner = self.extension_runner.lock().await.clone();
        if let Some(mut runner) = runner {
            let holds = self
                .holds()
                .into_iter()
                .map(|h| json!({"kind":h.kind.as_str(),"key":h.key,"data":h.data}))
                .collect();
            if let Err(error) = runner
                .emit(maho_ext_api::ExtensionEvent::SessionHoldChanged {
                    change: change.into(),
                    pause_seq: self.user_abort_generation.load(Ordering::SeqCst),
                    holds,
                })
                .await
            {
                self.emit(AgentSessionEvent::ContinuationError {
                    error_message: error.to_string(),
                });
            }
        }
    }

    /// senpi `releaseHeldExtensionHandlers`: stop waiting on the current run's extension handlers that
    /// still hold the turn (the turn deadline and an unhonoured abort call this, senpi#3650). Returns how
    /// many waits were released.
    pub async fn release_held_extension_handlers(&self) -> usize {
        let scope = self.handler_turn_scope.load(Ordering::SeqCst);
        self.extension_runner
            .lock()
            .await
            .as_ref()
            .map_or(0, |runner| runner.release_held_handlers(scope))
    }

    /// senpi `releaseHeldExtensionHandlers` for a caller that cannot await: the bound runner snapshot
    /// shares its held-wait registry with the installed runner.
    fn release_held_extension_handlers_now(&self) -> usize {
        let scope = self.handler_turn_scope.load(Ordering::SeqCst);
        self.extension_runner_snapshot()
            .map_or(0, |runner| runner.release_held_handlers(scope))
    }

    /// senpi `_waitForIdleAfterAbort`: an abort (Esc, or the turn deadline) the run does not honour within
    /// the grace is held by an extension handler no abort signal reaches, so the turn's held handler waits
    /// are released and the run can settle (senpi#3650). The run's agent listeners (the extension event
    /// handlers) are part of its settlement, so the agent's idle is the run's. Holds the session weakly:
    /// the in-flight abort wait is stored on the session.
    fn wait_for_idle_after_abort(
        &self,
    ) -> impl std::future::Future<Output = ()> + Send + 'static + use<> {
        let agent = self.agent.clone();
        let session = Arc::downgrade(&self.inner);
        async move {
            let idle = agent.wait_for_idle();
            tokio::pin!(idle);
            let grace =
                std::time::Duration::from_millis(crate::turn_deadline::TURN_DEADLINE_GRACE_MS);
            if tokio::time::timeout(grace, &mut idle).await.is_err() {
                if let Some(inner) = session.upgrade() {
                    AgentSession { inner }.release_held_extension_handlers_now();
                }
                idle.await;
            }
        }
    }

    pub async fn abort(&self) {
        if let Some(pending) = self.begin_user_abort() {
            self.finish_user_abort(pending).await;
        } else if self.agent_run_active.load(Ordering::SeqCst) {
            // senpi abort(): an Esc that joined an open agent_end boundary returns at once; a boundary
            // handler that holds the run is still released after the grace, without abort() waiting for it.
            tokio::spawn(self.wait_for_idle_after_abort());
        }
    }

    /// senpi `abortTurnForDeadline`: end the turn through the normal system abort, then wait out the
    /// grace, releasing the turn's held extension handlers when the run does not settle in it.
    async fn abort_for_deadline(&self) {
        use maho_ext_api::AbortSource;
        self.state().abort_source = Some(AbortSource::System);
        // senpi `_abortActiveAgentAndRetry("system")`.
        self.abort_retry();
        self.abort_branch_summary();
        let wait = {
            let mut in_flight = lock(&self.user_abort_wait);
            let streaming = self.is_streaming();
            if let Some(wait) = in_flight.clone() {
                let joined = self
                    .state()
                    .abort_provenance
                    .join(AbortSource::System, streaming);
                if joined.abort_current_agent {
                    self.agent.abort(None);
                }
                Some(wait)
            } else if self
                .state()
                .abort_provenance
                .join_open_boundary(AbortSource::System)
                .is_some()
            {
                None
            } else {
                if streaming {
                    let _recorded = self.state().abort_provenance.begin(AbortSource::System);
                }
                self.agent.abort(None);
                let wait = futures::FutureExt::shared(Box::pin(self.wait_for_idle_after_abort())
                    as futures::future::BoxFuture<'static, ()>);
                *in_flight = Some(wait.clone());
                Some(wait)
            }
        };
        if let Some(wait) = wait {
            wait.clone().await;
            let mut in_flight = lock(&self.user_abort_wait);
            if in_flight
                .as_ref()
                .is_some_and(|current| current.ptr_eq(&wait))
            {
                *in_flight = None;
            }
        }
        // An abort that joined an open agent_end boundary returns at once; the run is still held until it
        // settles.
        if self.agent_run_active.load(Ordering::SeqCst) {
            self.wait_for_idle_after_abort().await;
        }
    }

    fn begin_user_abort(&self) -> Option<bool> {
        *lock(&self.stop_interrupt) = Some(
            json!({"pauseSeq":self.user_abort_generation.load(Ordering::SeqCst)+1,"at":chrono::Utc::now().to_rfc3339()}),
        );
        // senpi abort(): streaming aborts are carried by agent_end provenance; only gaps need session_abort.
        let streaming = self.is_streaming();
        let had_retry_backoff = self.state().retry_abort_controller.is_some();
        let was_mid_run = streaming && !had_retry_backoff;
        let has_pending_trigger_turn_admission =
            self.state().trigger_turn_admission_abort_generation
                == Some(self.user_abort_generation.load(Ordering::SeqCst));
        let had_compaction_or_pending = !streaming
            && (self.is_compacting()
                || self.pending_message_count() > 0
                || has_pending_trigger_turn_admission);
        let had_cleared_queues = std::mem::take(&mut self.state().had_cleared_queued_messages);
        let joined_agent_end_boundary = self.state().abort_provenance.has_open_agent_end_boundary();
        let should_emit_abort = !joined_agent_end_boundary
            && !was_mid_run
            && (had_retry_backoff || had_compaction_or_pending || had_cleared_queues);
        self.user_abort_generation.fetch_add(1, Ordering::SeqCst);
        lock(&self.settled_delivery).cancel();
        self.state().user_aborted = true;
        self.state().abort_source = Some(maho_ext_api::AbortSource::User);
        if let Some(signal) = self.state().extension_event_signal.as_ref() {
            signal.abort();
        }
        self.abort_compaction();
        // senpi _abortActiveAgentAndRetry("user").
        self.abort_retry();
        self.abort_branch_summary();
        let mut in_flight = lock(&self.user_abort_wait);
        if in_flight.is_none()
            && self
                .state()
                .abort_provenance
                .join_open_boundary(maho_ext_api::AbortSource::User)
                .is_some()
        {
            return None;
        }
        if in_flight.is_some() {
            let joined = self
                .state()
                .abort_provenance
                .join(maho_ext_api::AbortSource::User, streaming);
            if joined.abort_current_agent {
                self.agent.abort(None);
            }
            return Some(should_emit_abort);
        }
        if streaming {
            self.state()
                .abort_provenance
                .begin(maho_ext_api::AbortSource::User);
        }
        self.agent.suppress_queued_message_drain();
        self.agent.abort(None);
        // senpi `_abortActiveAgentAndRetry`: the abort waits for idle through `_waitForIdleAfterAbort`, so a
        // run held by an extension handler is released after the grace.
        let wait: futures::future::BoxFuture<'static, ()> =
            Box::pin(self.wait_for_idle_after_abort());
        *in_flight = Some(futures::FutureExt::shared(wait));
        Some(should_emit_abort)
    }

    async fn finish_user_abort(&self, should_emit_abort: bool) {
        let wait = lock(&self.user_abort_wait).clone();
        if let Some(wait) = wait {
            wait.clone().await;
            let mut in_flight = lock(&self.user_abort_wait);
            if in_flight
                .as_ref()
                .is_some_and(|current| current.ptr_eq(&wait))
            {
                *in_flight = None;
            }
        }
        if should_emit_abort {
            self.dispatch_extension_event(maho_ext_api::ExtensionEvent::SessionAbort)
                .await;
            self.emit(AgentSessionEvent::SessionAbort);
        }
        self.release_queued_deliveries(&self.queued_delivery_tickets(), "aborted");
    }

    /// senpi `_maybeRestoreFallbackPrimary`: a failed revert clears the fallback state instead of
    /// failing the turn.
    async fn restore_fallback_primary(&self) {
        let policy = self.with_settings_manager(|manager| {
            crate::retry_fallback::settings::resolve_retry_fallback_settings(
                manager.get_value("retry"),
            )
            .revert_policy
        });
        let mut guard = self.retry_fallback.lock().await;
        let Some(controller) = guard.as_mut() else {
            return;
        };
        if let Err(error) = controller.maybe_restore_primary(policy).await {
            controller.clear();
            crate::output_guard::maho_write_stderr(&format!(
                "fallback revert failed; cleared fallback state {error}\n"
            ));
        }
    }

    /// senpi `_claimCompactionController(controller, "auto")`: the post-turn compaction is owned by
    /// `_autoCompactionAbortController`, which prompt admission never rejects on.
    fn begin_auto_compaction(&self) -> AutoCompactionOwnerGuard<'_> {
        self.state().auto_compaction_owned = true;
        AutoCompactionOwnerGuard(self)
    }

    /// senpi `set _compactionAbortController`: an abort controller is an isCompacting fact, so setting or clearing
    /// one is a lifecycle edge (senpi#3980).
    fn set_compaction_abort_controller(
        &self,
        controller: Option<crate::compaction::lifecycle::CompactionAbortController>,
    ) {
        let changed = {
            let mut state = self.state();
            let same = match (&state.compaction_abort_controller, &controller) {
                (Some(current), Some(next)) => current.same(next),
                (None, None) => true,
                _ => false,
            };
            state.compaction_abort_controller = controller;
            !same
        };
        if changed {
            self.refresh_lifecycle();
        }
    }

    /// senpi `set _branchSummaryAbortController`: only the end of a tree navigation is a lifecycle edge; a record
    /// written while it runs would become the leaf under it (senpi#4120).
    fn set_branch_summary_abort_controller(
        &self,
        controller: Option<maho_ai::utils::abort::AbortController>,
    ) {
        let cleared = controller.is_none();
        let changed = {
            let mut state = self.state();
            let changed = state.branch_summary_abort_controller.is_some() || !cleared;
            state.branch_summary_abort_controller = controller;
            changed
        };
        if cleared && changed {
            self.refresh_lifecycle();
        }
    }

    pub fn is_retrying(&self) -> bool {
        self.state().retry_attempt > 0
    }

    pub fn is_compacting(&self) -> bool {
        let state = self.state();
        state.compaction_lifecycle.state().status() == "running"
            || state.compaction_abort_controller.is_some()
            || state.pending_compaction_admission.is_some()
            || state.branch_summary_abort_controller.is_some()
    }

    fn is_compaction_delegated(&self) -> bool {
        let model = self.model();
        self.state()
            .delegated_compaction_key
            .as_ref()
            .is_some_and(|(provider, id)| *provider == model.provider && *id == model.id)
    }

    pub fn compaction_state(&self) -> crate::compaction::lifecycle::CompactionLifecycleState {
        self.state().compaction_lifecycle.state().clone()
    }

    pub fn abort_compaction(&self) {
        if let Some(pending) = self.state().pending_compaction_admission.as_ref() {
            pending.controller.abort();
        }
        if let Some(controller) = self.state().compaction_abort_controller.as_ref() {
            controller.abort();
        }
        if let Some(signal) = self.state().compaction_extension_signal.as_ref() {
            signal.abort();
        }
    }

    /// The user's explicit entry point (`/compact`, RPC): aborts the active turn and never retries.
    pub async fn compact(
        &self,
        instructions: Option<&str>,
    ) -> Result<crate::compaction::compaction::CompactionResult, String> {
        self.compact_with_reason(instructions, "manual").await
    }

    /// An extension's `ctx.compact` (senpi's `compaction` context action): same admission, but labeled
    /// `extension`, so the log names the real trigger and the circuit breaker keeps governing it.
    pub async fn compact_from_extension(
        &self,
        instructions: Option<&str>,
    ) -> Result<crate::compaction::compaction::CompactionResult, String> {
        self.compact_with_reason(instructions, "extension").await
    }

    async fn compact_with_reason(
        &self,
        instructions: Option<&str>,
        reason: &str,
    ) -> Result<crate::compaction::compaction::CompactionResult, String> {
        let pending = crate::compaction::lifecycle::CompactionAbortController::new();
        let pending_outcome = Arc::new(PendingCompactionAdmission {
            controller: pending.clone(),
            completed: std::sync::atomic::AtomicBool::new(false),
        });
        let _work = self.work_barrier.begin();
        {
            let mut state = self.state();
            if let Some(prior) = state
                .pending_compaction_admission
                .replace(pending_outcome.clone())
            {
                prior.controller.abort();
            }
            if let Some(prior) = &state.compaction_abort_controller {
                prior.abort();
            }
            if let Some(signal) = &state.compaction_extension_signal {
                signal.abort();
            }
        }
        let pending_guard = PendingCompactionAdmissionGuard {
            session: self,
            controller: pending.clone(),
        };
        // senpi `_abortActiveAgentAndRetry("system")`: the cut-off turn is owned by the system, so it
        // renders as "System operation aborted" instead of a provider failure.
        let streaming = self.is_streaming();
        if lock(&self.user_abort_wait).is_some() {
            self.state()
                .abort_provenance
                .join(maho_ext_api::AbortSource::System, streaming);
        } else if streaming {
            self.state()
                .abort_provenance
                .begin(maho_ext_api::AbortSource::System);
        }
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
            if state
                .pending_compaction_admission
                .as_ref()
                .is_some_and(|current| current.controller.same(&pending))
            {
                state.pending_compaction_admission = None;
            }
        }
        drop(pending_guard);
        let _admission = admission;
        let result = if pending.aborted() {
            Err("Compaction cancelled".to_owned())
        } else {
            self.compact_for_model(instructions, &self.model(), reason)
                .await
        };
        pending_outcome
            .completed
            .store(result.is_ok(), Ordering::SeqCst);
        if result.is_ok() && self.agent.has_queued_messages() {
            let session = self.clone();
            let guard = self.work_barrier.begin();
            let generation = self.user_abort_generation.load(Ordering::SeqCst);
            tokio::spawn(async move {
                if let Err(error) = session.continue_session_internal(Some(generation)).await {
                    session.emit(AgentSessionEvent::ContinuationError {
                        error_message: error,
                    });
                }
                drop(guard);
            });
        }
        result
    }

    async fn compact_for_model(
        &self,
        instructions: Option<&str>,
        budget_model: &Model,
        reason: &str,
    ) -> Result<crate::compaction::compaction::CompactionResult, String> {
        self.compact_for_model_with_retry(
            instructions,
            budget_model,
            reason,
            !matches!(reason, "manual" | "extension"),
            false,
            None,
        )
        .await
    }

    async fn compact_for_model_with_retry(
        &self,
        instructions: Option<&str>,
        budget_model: &Model,
        reason: &str,
        will_retry: bool,
        allow_summary_only: bool,
        keep_recent_tokens_override: Option<i64>,
    ) -> Result<crate::compaction::compaction::CompactionResult, String> {
        use crate::compaction::compaction::{CompactionResult, prepare_compaction};
        let mut resolved = self.get_compaction_settings(Some(budget_model))?;
        if self
            .pending_model_switch()
            .is_some_and(|pending| models_are_equal(Some(&pending.model), Some(budget_model)))
        {
            let window = budget_model.context_window;
            let ratio = match window {
                0..=16_000 => 0.45,
                16_001..=32_000 => 0.5,
                32_001..=64_000 => 0.55,
                64_001..=128_000 => 0.6,
                128_001..=512_000 => 0.7,
                _ => 0.8,
            };
            let keep = if window > 409_600 && resolved.keep_recent_tokens >= 10_000 {
                resolved
                    .keep_recent_tokens
                    .max((window / 20).min(60_000) as i64)
            } else {
                resolved.keep_recent_tokens
            };
            resolved.keep_recent_tokens = keep
                .min(((window as f64 * (1.0 - ratio - 0.05)).floor() as i64).max(1_024))
                .max(1);
        }
        if let Some(keep_recent_tokens) = keep_recent_tokens_override {
            resolved.keep_recent_tokens = keep_recent_tokens;
        }
        let entries = self.with_session_manager(|manager| manager.current_branch());
        let settings = maho_ai::utils::overflow::cursor_overflow_compaction_settings(
            crate::compaction::settings::CompactionSettings {
                enabled: resolved.enabled,
                reserve_tokens: resolved.reserve_tokens,
                keep_recent_tokens: resolved.keep_recent_tokens,
                restoration_enabled: Some(resolved.restoration_enabled),
                summarization_max_duration_ms: resolved.summarization_max_duration_ms,
                ..crate::compaction::settings::default_compaction_settings()
            },
            Some(self.model().provider.as_str()),
            Some(reason),
        );
        let preparation = prepare_compaction(
            &entries,
            &settings,
            reason == "overflow",
            allow_summary_only,
        );
        let request_id = uuid::Uuid::new_v4().to_string();
        let Some(preparation) = preparation else {
            let error = if entries
                .last()
                .is_some_and(|entry| entry["type"] == "compaction")
            {
                "Already compacted"
            } else {
                "Nothing to compact (session too small)"
            };
            let compact_reason = compaction_reason_from_str(reason);
            self.emit(AgentSessionEvent::CompactionStart {
                reason: compact_reason,
                request_id: Some(request_id.clone()),
            });
            self.emit(AgentSessionEvent::CompactionEnd {
                reason: compact_reason,
                request_id: Some(request_id),
                aborted: false,
                result: None,
                rejection_cause: None,
                error_message: Some(format!("Compaction failed: {error}")),
                accepted: Some(false),
                will_retry: false,
            });
            self.dispatch_extension_event(maho_ext_api::ExtensionEvent::SessionCompactFailed {
                reason: compact_reason,
                error_message: Some(format!("Compaction failed: {error}")),
                aborted: false,
                will_retry: false,
                from_extension: false,
            })
            .await;
            return Err(error.to_owned());
        };
        let controller = crate::compaction::lifecycle::CompactionAbortController::new();
        let signal = controller.signal();
        let extension_signal = maho_ext_api::AbortSignal::default();
        self.state().compaction_extension_signal = Some(extension_signal.clone());
        self.set_compaction_abort_controller(Some(controller.clone()));
        let compact_reason = compaction_reason_from_str(reason);
        self.emit(AgentSessionEvent::CompactionStart {
            reason: compact_reason,
            request_id: Some(request_id.clone()),
        });
        let revision = self.message_revision();
        let request_id = self.state().compaction_lifecycle.begin(
            crate::compaction::lifecycle::BeginCompactionOperation {
                operation_id: request_id,
                stage: crate::compaction::lifecycle::CompactionStage::Execution,
                reason: reason.to_owned(),
                model: Some(crate::compaction::lifecycle::CompactionModelRef {
                    provider: budget_model.provider.clone(),
                    id: budget_model.id.clone(),
                }),
                started_revision: revision as i64,
            },
            controller.clone(),
        );
        let _work = self.work_barrier.begin();
        let mut rejection = None;
        let mut rejection_aborted = false;
        let mut rejection_reason = None;
        let mut accepted_entry = None;
        let execution = async {
            // senpi agent-session.ts:10160: resolved before session_before_compact, so a spent account with
            // no usable alternate fails compaction before any hook or summary request.
            let compaction_model = self.resolve_compaction_model(&self.model())?;
            let messages_before_extension = self.messages();
            let before = {
                let runner = self.extension_runner.lock().await.clone();
                if let Some(mut runner) = runner {
                    let event = maho_ext_api::SessionBeforeCompactEvent {
                        reason: compact_reason,
                        will_retry,
                        request_id: request_id.clone(),
                        preparation: maho_ext_api::CompactionPreparation {
                            settings: maho_ext_api::CompactionSettings {
                                enabled: self.get_compaction_settings(Some(budget_model))?.enabled,
                                reserve_tokens: resolved.reserve_tokens as u64,
                                keep_recent_tokens: resolved.keep_recent_tokens as u64,
                            },
                            messages_to_summarize: preparation
                                .messages_to_summarize
                                .iter()
                                .cloned()
                                .map(session_message_from_value)
                                .collect::<Result<_, _>>()
                                .map_err(|error| error.to_string())?,
                            turn_prefix_messages: preparation
                                .turn_prefix_messages
                                .iter()
                                .cloned()
                                .map(session_message_from_value)
                                .collect::<Result<_, _>>()
                                .map_err(|error| error.to_string())?,
                            tokens_before: preparation.tokens_before as u64,
                            first_kept_entry_id: preparation.first_kept_entry_id.clone(),
                            previous_summary: preparation.previous_summary.clone(),
                        },
                        branch_entries: entries
                            .iter()
                            .cloned()
                            .map(session_entry_from_value)
                            .collect(),
                        custom_instructions: instructions.map(str::to_owned),
                        signal: extension_signal.clone(),
                    };
                    let details = maho_ext_api::CompactionPreparationDetails {
                        preparation: event.preparation.clone(),
                        source_messages: Some(
                            preparation
                                .source_messages
                                .iter()
                                .cloned()
                                .map(session_message_from_value)
                                .collect::<Result<_, _>>()
                                .map_err(|error| error.to_string())?,
                        ),
                        turn_prefix_source_messages: Some(
                            preparation
                                .turn_prefix_source_messages
                                .iter()
                                .cloned()
                                .map(session_message_from_value)
                                .collect::<Result<_, _>>()
                                .map_err(|error| error.to_string())?,
                        ),
                        is_split_turn: preparation.is_split_turn,
                        file_ops: maho_ext_api::CompactionFileOperations {
                            read: preparation.file_ops.read.iter().cloned().collect(),
                            written: preparation.file_ops.written.iter().cloned().collect(),
                            edited: preparation.file_ops.edited.iter().cloned().collect(),
                        },
                    };
                    EXTENSION_COMPACTION_PREPARATION
                        .scope(
                            details,
                            runner.emit(maho_ext_api::ExtensionEvent::SessionBeforeCompact(event)),
                        )
                        .await
                        .map_err(|error| error.to_string())?
                } else {
                    maho_ext_api::EventResult::None
                }
            };
            // senpi hands session_before_compact the controller's own signal (agent-session.ts:6234), so an
            // extension-side abort is a controller abort and isCurrent() then cancels (lifecycle.ts:139-145).
            if extension_signal.is_aborted() {
                controller.abort();
            }
            let messages_after_extension = self.messages();
            let diagnostic_messages =
                if messages_after_extension.starts_with(&messages_before_extension) {
                    messages_after_extension[messages_before_extension.len()..]
                        .iter()
                        .filter(|message| {
                            let AgentMessage::Custom(
                                maho_agent::types::CustomAgentMessage::Custom(custom),
                            ) = message
                            else {
                                return false;
                            };
                            custom.custom_type == "senpi.hook"
                                && custom.details.as_ref().is_some_and(|details| {
                                    details["event"] == "PreCompact"
                                        && details["compactionRequestId"] == request_id
                                })
                        })
                        .cloned()
                        .collect::<Vec<_>>()
                } else {
                    Vec::new()
                };
            let (mut result, from_extension) = match before {
                maho_ext_api::EventResult::SessionBefore(
                    maho_ext_api::SessionBeforeEventResult {
                        cancel: Some(true),
                        rejection_cause,
                        reason,
                        ..
                    },
                ) => {
                    rejection =
                        Some(rejection_cause.unwrap_or(
                            maho_ext_api::CompactionRejectionCause::CancelledByExtension,
                        ));
                    rejection_aborted = true;
                    rejection_reason = reason
                        .map(|reason| reason.trim().to_owned())
                        .filter(|reason| !reason.is_empty());
                    return Err(rejection_reason.as_ref().map_or_else(
                        || describe_compaction_rejection(rejection.expect("cause")).to_owned(),
                        |reason| format!("Compaction rejected: {reason}"),
                    ));
                }
                maho_ext_api::EventResult::SessionBefore(
                    maho_ext_api::SessionBeforeEventResult {
                        compaction: Some(result),
                        ..
                    },
                ) => (
                    CompactionResult {
                        summary: result.summary,
                        first_kept_entry_id: result.first_kept_entry_id,
                        tokens_before: result.tokens_before as i64,
                        details: result.details,
                        usage: None,
                        estimated_tokens_after: None,
                    },
                    true,
                ),
                _ => {
                    let model = compaction_model;
                    signal
                        .throw_if_aborted()
                        .map_err(|error| error.to_string())?;
                    if !self
                        .state()
                        .compaction_lifecycle
                        .is_current(&request_id, &controller)
                    {
                        return Err("Compaction cancelled".to_owned());
                    }
                    let auth = self.get_compaction_request_auth(&model).await?;
                    (
                        self.run_default_compaction(
                            &preparation,
                            auth,
                            instructions,
                            signal.clone(),
                            compact_reason,
                        )
                        .await?,
                        false,
                    )
                }
            };
            result.details = Some(structural_yield_details(
                result.details.take(),
                &preparation,
                &result.summary,
                result.tokens_before as f64,
            ));
            signal
                .throw_if_aborted()
                .map_err(|error| error.to_string())?;
            if !self
                .state()
                .compaction_lifecycle
                .is_current(&request_id, &controller)
            {
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
            let entry = self
                .apply_compaction_internal(&result, Some(from_extension))
                .map_err(|error| {
                    if error == "Compaction rejected: summary-would-overflow" {
                        rejection = Some(maho_ext_api::CompactionRejectionCause::WouldOverflow);
                    }
                    error
                })?;
            result.estimated_tokens_after = Some(
                self.with_session_manager(|manager| manager.current_context())
                    .messages
                    .iter()
                    .map(crate::compaction::compaction::estimate_tokens)
                    .sum::<u64>() as i64,
            );
            let mut messages = self.messages();
            if !diagnostic_messages.is_empty() {
                messages.retain(|message| {
                        let AgentMessage::Custom(maho_agent::types::CustomAgentMessage::Custom(
                            custom,
                        )) = message
                        else {
                            return true;
                        };
                        !(custom.custom_type == "senpi.hook"
                            && custom.details.as_ref().is_some_and(|details| {
                                details["event"] == "PreCompact"
                                    && details["compactionRequestId"] == request_id
                            }))
                    });
                messages.extend(diagnostic_messages);
            }
            self.agent.set_messages(messages);
            self.state().context_usage_cache = None;
            accepted_entry = Some((entry, from_extension));
            Ok(result)
        }
        .await;
        let ended_revision = self.message_revision() as i64;
        let owns_terminal = self.state().compaction_lifecycle.finish(
            &crate::compaction::lifecycle::FinishCompactionOperation {
                operation_id: request_id.clone(),
                status: if signal.aborted() {
                    crate::compaction::lifecycle::CompactionFinishStatus::Aborted
                } else if execution.is_ok() {
                    crate::compaction::lifecycle::CompactionFinishStatus::Completed
                } else {
                    crate::compaction::lifecycle::CompactionFinishStatus::Failed
                },
                ended_revision,
                rejection_cause: rejection.map(|cause| {
                    match cause {
                        CompactionRejectionCause::CancelledByExtension => "cancelled-by-extension",
                        CompactionRejectionCause::ExternalOwner => "external-owner",
                        CompactionRejectionCause::WouldOverflow => "would-overflow",
                        CompactionRejectionCause::CircuitBreaker => "circuit-breaker",
                        CompactionRejectionCause::PerTurnCap => "per-turn-cap",
                        CompactionRejectionCause::StaleRevision => "stale-revision",
                    }
                    .to_owned()
                }),
                error_message: execution.as_ref().err().cloned(),
            },
        );
        if !owns_terminal {
            return execution;
        }
        self.set_compaction_abort_controller(None);
        self.state().compaction_extension_signal = None;
        self.state().compaction_signal_bridge = None;
        match &execution {
            Ok(result) => {
                self.state().delegated_compaction_key = None;
                let value = maho_ext_api::CompactionResult {
                    summary: result.summary.clone(),
                    first_kept_entry_id: result.first_kept_entry_id.clone(),
                    tokens_before: result.tokens_before as u64,
                    details: result.details.clone(),
                };
                self.emit(AgentSessionEvent::CompactionEnd {
                    reason: compact_reason,
                    request_id: Some(request_id.clone()),
                    aborted: false,
                    result: Some(value),
                    rejection_cause: None,
                    error_message: None,
                    accepted: Some(true),
                    will_retry,
                });
                if let Some((entry, from_extension)) = accepted_entry {
                    self.dispatch_extension_event(maho_ext_api::ExtensionEvent::SessionCompact(
                        maho_ext_api::SessionCompactEvent::Accepted {
                            reason: compact_reason,
                            request_id,
                            compaction_entry: session_entry_from_value(entry),
                            from_extension,
                            will_retry,
                        },
                    ))
                    .await;
                }
                // senpi `_onCompactionContextChanged`.
                let released = self
                    .retry_fallback
                    .lock()
                    .await
                    .as_mut()
                    .is_some_and(|controller| controller.notify_compaction_applied());
                if released && !self.is_streaming() {
                    self.restore_fallback_primary().await;
                }
            }
            Err(error) => {
                let error_message = if let Some(cause) = rejection {
                    if rejection_aborted && rejection_reason.is_none() {
                        None
                    } else {
                        Some(rejection_reason.as_ref().map_or_else(
                            || describe_compaction_rejection(cause).to_owned(),
                            |reason| format!("Compaction rejected: {reason}"),
                        ))
                    }
                } else {
                    (!signal.aborted()).then(|| format!("Compaction failed: {error}"))
                };
                self.emit(AgentSessionEvent::CompactionEnd {
                    reason: compact_reason,
                    request_id: Some(request_id.clone()),
                    aborted: signal.aborted() || rejection_aborted,
                    result: None,
                    rejection_cause: rejection,
                    error_message: error_message.clone(),
                    accepted: Some(false),
                    will_retry: false,
                });
                if let Some(rejection_cause) = rejection {
                    if rejection_cause == maho_ext_api::CompactionRejectionCause::ExternalOwner {
                        let model = self.model();
                        self.state().delegated_compaction_key = Some((model.provider, model.id));
                    }
                    self.dispatch_extension_event(maho_ext_api::ExtensionEvent::SessionCompact(
                        maho_ext_api::SessionCompactEvent::Rejected {
                            reason: compact_reason,
                            request_id,
                            rejection_cause,
                        },
                    ))
                    .await;
                } else {
                    self.dispatch_extension_event(
                        maho_ext_api::ExtensionEvent::SessionCompactFailed {
                            reason: compact_reason,
                            error_message,
                            aborted: signal.aborted(),
                            will_retry: false,
                            from_extension: false,
                        },
                    )
                    .await;
                }
            }
        }
        execution
    }

    pub fn apply_compaction(
        &self,
        result: &crate::compaction::compaction::CompactionResult,
    ) -> Result<Value, String> {
        self.apply_compaction_internal(result, None)
    }

    fn apply_compaction_internal(
        &self,
        result: &crate::compaction::compaction::CompactionResult,
        from_hook: Option<bool>,
    ) -> Result<Value, String> {
        let mut branch = self.with_session_manager(|manager| manager.current_branch());
        if !branch.iter().any(|entry| {
            entry.get("id").and_then(Value::as_str) == Some(result.first_kept_entry_id.as_str())
        }) {
            return Err("Compaction first kept entry is not on the current branch".to_owned());
        }
        let model = self.model();
        let parent = branch
            .last()
            .and_then(|entry| entry.get("id"))
            .cloned()
            .unwrap_or(Value::Null);
        let simulated_id = format!("simulated-{}", uuid::Uuid::new_v4());
        branch.push(serde_json::json!({"type":"compaction","id":simulated_id,"parentId":parent,
            "timestamp":"1970-01-01T00:00:00.000Z","summary":result.summary,
            "firstKeptEntryId":result.first_kept_entry_id,"tokensBefore":result.tokens_before,"details":result.details}));
        let context = crate::session_manager::build_session_context(&branch, Some(&simulated_id));
        let tokens = context
            .messages
            .iter()
            .map(crate::compaction::compaction::estimate_tokens)
            .sum::<u64>() as i64;
        let raw = self.with_settings_manager(|manager| manager.get_value("compaction").cloned());
        let settings = raw
            .map(serde_json::from_value::<crate::compaction_settings_access::CompactionSettings>)
            .transpose()
            .map_err(|error| error.to_string())?;
        let settings = crate::compaction_settings_resolver::resolve_compaction_settings(
            settings.as_ref(),
            Some(crate::compaction_settings_access::CompactionModelSelector {
                provider: &model.provider,
                id: &model.id,
            }),
        )?;
        let reserve = if settings.reserve_scaling_enabled {
            crate::compaction::compaction::resolve_reserve_tokens(
                model.context_window as f64,
                settings.reserve_tokens as f64,
            ) as i64
        } else {
            settings.reserve_tokens
        };
        if tokens > (model.context_window as i64).saturating_sub(reserve) {
            return Err("Compaction rejected: summary-would-overflow".to_owned());
        }
        let usage = result
            .usage
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(|error| error.to_string())?;
        let latest_configuration_effort = branch
            .iter()
            .rev()
            .find(|entry| entry.get("type").and_then(Value::as_str) == Some("configuration_update"))
            .and_then(|entry| {
                entry
                    .get("reasoning")?
                    .get("effort")?
                    .as_str()
                    .map(str::to_owned)
            });
        let entry = self.with_session_manager_mut(|manager| {
            manager.append_compaction(
                &result.summary,
                &result.first_kept_entry_id,
                result.tokens_before,
                result.details.clone(),
                usage,
                from_hook,
            )
        });
        if let Some(effort) = latest_configuration_effort
            && is_reasoning_configuration_model(&self.model())
        {
            self.with_session_manager_mut(|manager| manager.append_configuration_update(&effort));
        }
        let context = self.with_session_manager(|manager| manager.current_context());
        let mut messages = context
            .messages
            .into_iter()
            .map(session_message_from_value)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        messages.extend(
            self.state()
                .messages_awaiting_persistence
                .iter()
                .map(|(_, message)| message.clone()),
        );
        self.agent.set_messages(messages);
        let mut state = self.state();
        state.message_revision += 1;
        state.post_compaction_assistant_generation = Some(state.assistant_generation);
        Ok(entry)
    }

    pub fn retry_attempt(&self) -> u32 {
        self.state().retry_attempt
    }

    /// senpi `currentAbortSource`: the abort owner for the current turn boundary, used by renderers.
    pub fn current_abort_source(&self) -> Option<maho_ext_api::AbortSource> {
        self.state().abort_provenance.current_source()
    }

    pub fn abort_retry(&self) {
        if let Some(controller) = self.state().retry_abort_controller.as_ref() {
            controller.abort(None);
        }
    }

    pub fn auto_retry_enabled(&self) -> bool {
        self.with_settings_manager(|manager| {
            manager
                .get_value("retry")
                .and_then(|settings| settings.get("enabled"))
                .and_then(Value::as_bool)
                .unwrap_or(true)
        })
    }

    fn reset_hint_tier_state(&self) {
        let mut state = self.state();
        state.probe_phase = crate::retry_fallback::hint_policy::ProbePhase::Idle;
        state.hint_deadline_ms = None;
        state.cumulative_hinted_wait_ms = 0.0;
    }

    pub fn fallback_validation_warnings(&self) -> &[String] {
        &self.inner.fallback_validation_warnings
    }

    pub fn set_auto_retry_enabled(&self, enabled: bool) -> Result<(), String> {
        let mut retry = self
            .with_settings_manager(|manager| manager.get_value("retry").cloned())
            .and_then(|retry| retry.as_object().cloned())
            .unwrap_or_default();
        retry.insert("enabled".to_owned(), Value::Bool(enabled));
        self.with_settings_manager_mut(|manager| {
            manager.set(
                crate::settings_manager::SettingsScope::Global,
                &Map::from_iter([("retry".to_owned(), Value::Object(retry))]),
            )
        })
    }

    pub async fn wait_for_retry(&self) {
        let _admission = self.prompt_admission.lock().await;
    }

    fn is_subscription_same_model_remint_error(
        &self,
        message: &maho_ai::types::AssistantMessage,
    ) -> bool {
        self.model().provider == "anthropic-subscription"
            && message.error_message.as_deref().is_some_and(|error| {
                error.contains("Lock file is already being held") || error == "invalid_request"
            })
    }

    /// `_willRetryAfterAgentEnd`: whether the agent_end handler will start another attempt.
    async fn will_retry(&self, message: Option<&maho_ai::types::AssistantMessage>) -> bool {
        let Some(message) = message else {
            return false;
        };
        if self
            .required_compaction_still_needed(message)
            .unwrap_or(false)
        {
            return true;
        }
        let profile = self.resolve_retry_profile();
        if !profile.turn.enabled {
            return false;
        }
        if self.state().user_aborted
            || !self.with_settings_manager(|manager| {
                manager
                    .get_value("retry")
                    .and_then(|settings| settings.get("enabled"))
                    .and_then(Value::as_bool)
                    .unwrap_or(true)
            })
        {
            return false;
        }
        let turn_max_retries = profile.turn.max_retries;
        let retryable_error = self.is_retryable_error(message);
        let probe = maho_ai::utils::overflow::CursorExhaustionProbe::from_message(message);
        if maho_ai::utils::overflow::is_cursor_zero_token_resource_exhausted(&probe) {
            return true;
        }
        if maho_ai::utils::overflow::is_cursor_quota_resource_exhausted(
            &probe,
            self.model().context_window,
        ) {
            return self.can_try_fallback().await;
        }
        if self.is_subscription_same_model_remint_error(message) {
            return true;
        }
        if !retryable_error && self.is_hard_error_fallback_eligible(message).await {
            return true;
        }
        if !retryable_error {
            return false;
        }
        let retry_attempt = self.state().retry_attempt;
        if maho_ai::utils::stop_details::is_classifier_refusal(message) {
            return retry_attempt.saturating_add(1) <= turn_max_retries
                && self.can_try_fallback().await;
        }
        if retry_attempt.saturating_add(1) > turn_max_retries {
            return self.can_try_fallback().await;
        }
        let error_message = message
            .error_message
            .as_deref()
            .filter(|error| !error.is_empty())
            .unwrap_or("Unknown error");
        let Some(provider_delay_ms) = self.get_provider_retry_delay_ms(error_message) else {
            return true;
        };
        if provider_delay_ms
            <= self.with_settings_manager(|manager| {
                manager.get_provider_retry_settings().max_retry_delay_ms
            })
        {
            return true;
        }
        self.can_try_fallback().await
    }

    /// `retryCanAdmitProvider` in the agent_end handler: whether the retry handler runs at all.
    /// Unlike `will_retry` it ignores the budget, so an exhausted turn still reaches `auto_retry_end`.
    async fn retry_can_admit_provider(&self, message: &maho_ai::types::AssistantMessage) -> bool {
        if !self.resolve_retry_profile().turn.enabled {
            return false;
        }
        if self.state().user_aborted
            || !self.with_settings_manager(|manager| {
                manager
                    .get_value("retry")
                    .and_then(|settings| settings.get("enabled"))
                    .and_then(Value::as_bool)
                    .unwrap_or(true)
            })
        {
            return false;
        }
        let probe = maho_ai::utils::overflow::CursorExhaustionProbe::from_message(message);
        (self.request_shape_decision(message)
            == crate::retry_fallback::request_shape::RequestShapeDecision::NotRequestShape
            && self.is_retryable_error(message))
            || maho_ai::utils::overflow::is_cursor_zero_token_resource_exhausted(&probe)
            || maho_ai::utils::overflow::is_cursor_quota_resource_exhausted(
                &probe,
                self.model().context_window,
            )
            || self.is_subscription_same_model_remint_error(message)
            || self.is_hard_error_fallback_eligible(message).await
    }

    async fn can_try_fallback(&self) -> bool {
        self.retry_fallback
            .lock()
            .await
            .as_mut()
            .is_some_and(|controller| controller.can_try_fallback())
    }

    /// `_isLocalFailure`: a failure the agent marked local (the session's journal or file, senpi#3866). The
    /// mark is typed and set by where the error came from, never read from its text: no model produced it, so
    /// it never retries, walks the fallback chain, or marks a model unhealthy.
    fn is_local_failure(message: &maho_ai::types::AssistantMessage) -> bool {
        message.failure_source == Some(maho_ai::types::AssistantFailureSource::Local)
    }

    /// `_requestShapeRecovery.decide` against the current model's window.
    fn request_shape_decision(
        &self,
        message: &maho_ai::types::AssistantMessage,
    ) -> crate::retry_fallback::request_shape::RequestShapeDecision {
        lock(&self.request_shape_recovery).decide(message, self.model().context_window)
    }

    /// `_isRetryableError`.
    fn is_retryable_error(&self, message: &maho_ai::types::AssistantMessage) -> bool {
        if Self::is_local_failure(message) {
            return false;
        }
        // A route that does not serve the model fails the same way on every attempt, whatever status wraps it.
        if crate::retry_fallback::model_config_error::is_model_config_failure(message) {
            return false;
        }
        if message.error_message.as_deref().is_some_and(|error| {
            error.starts_with(
                maho_ai::utils::provider_failure_description::TURN_RETRY_SUPPRESSION_PREFIX,
            )
        }) {
            return false;
        }
        if maho_ai::utils::overflow::is_context_overflow(message, Some(self.model().context_window))
        {
            return false;
        }
        if maho_ai::utils::stop_details::is_classifier_refusal(message) {
            return true;
        }
        if message.error_message.as_deref().is_none_or(str::is_empty) {
            return false;
        }
        if message.stop_reason == StopReason::Aborted {
            return maho_ai::utils::retry::is_provider_timeout_error(message);
        }
        maho_ai::utils::retry::is_retryable_assistant_error(message)
    }

    /// `_isHardErrorFallbackEligible`.
    async fn is_hard_error_fallback_eligible(
        &self,
        message: &maho_ai::types::AssistantMessage,
    ) -> bool {
        let probe = maho_ai::utils::overflow::CursorExhaustionProbe::from_message(message);
        let suppressed = message.error_message.as_deref().is_some_and(|error| {
            error.starts_with(
                maho_ai::utils::provider_failure_description::TURN_RETRY_SUPPRESSION_PREFIX,
            )
        });
        let subscription_unconfigured = self.model().provider == "anthropic-subscription"
            && message.error_message.as_deref()
                == Some(
                    maho_ai::auth::resolve::provider_not_configured_message(
                        "anthropic-subscription",
                    )
                    .as_str(),
                );
        if suppressed
            || subscription_unconfigured
            || self.is_subscription_same_model_remint_error(message)
        {
            return false;
        }
        if Self::is_local_failure(message)
            || crate::retry_fallback::model_config_error::is_model_config_failure(message)
        {
            return false;
        }
        message.stop_reason == StopReason::Error
            && self.request_shape_decision(message)
                == crate::retry_fallback::request_shape::RequestShapeDecision::NotRequestShape
            && !maho_ai::utils::overflow::is_context_overflow(
                message,
                Some(self.model().context_window),
            )
            && !maho_ai::utils::overflow::is_cursor_payload_resource_exhausted(&probe, 0)
            && !maho_ai::utils::overflow::is_cursor_zero_token_resource_exhausted(&probe)
            && !maho_ai::utils::stop_details::is_classifier_refusal(message)
            && !message
                .content
                .iter()
                .any(|content| matches!(content, maho_ai::types::ContentBlock::ToolCall(_)))
            && self
                .can_try_fallback_refused(
                    crate::provider_failure::is_request_level_refusal(message)
                        .then_some(message.provider.as_str()),
                )
                .await
    }

    /// senpi `canTryFallback(refusedProvider)` (senpi#2491).
    async fn can_try_fallback_refused(&self, refused_provider: Option<&str>) -> bool {
        self.retry_fallback
            .lock()
            .await
            .as_mut()
            .is_some_and(|controller| controller.can_try_fallback_refused(refused_provider))
    }

    /// `_takeNativeToolSearchInjectionFailure`: consumes the tool-search service's pending flag.
    fn take_native_tool_search_injection_failure(&self) -> Option<String> {
        let source = self
            .state()
            .native_tool_search_injection_failure_source
            .clone()?;
        source()
    }

    /// `_getProviderRetryDelayMs`.
    fn get_provider_retry_delay_ms(&self, error_message: &str) -> Option<u64> {
        maho_ai::utils::retry_hint::parse_retry_after_ms_marker(error_message).or_else(|| {
            maho_ai::utils::retry_hint::extract_429_retry_after_ms(
                &maho_ai::utils::retry_hint::RetryHintInput {
                    body_text: error_message,
                    ..Default::default()
                },
                Some(self.fallback_now() as i64),
            )
        })
    }

    /// `_terminalFailureText`: user-facing text for a turn that is really over.
    async fn terminal_failure_text(
        &self,
        message: &maho_ai::types::AssistantMessage,
        attempts: u32,
    ) -> Option<String> {
        if crate::retry_fallback::usage_limit::usage_limit_scope(message).is_some() {
            return crate::retry_fallback::usage_limit::describe_usage_limit_message(message)
                .error_message;
        }
        let model = self.model();
        let has_configured_chain = self
            .retry_fallback
            .lock()
            .await
            .as_mut()
            .is_some_and(|controller| controller.has_configured_chain());
        maho_ai::utils::provider_failure_description::describe_provider_failure_for_user(
            message.error_message.as_deref(),
            &maho_ai::utils::retry::ProviderStallDescriptionOptions {
                attempts: Some(attempts),
                model: Some(format!("{}/{}", model.provider, model.id)),
                recovery: Some(if has_configured_chain {
                    maho_ai::utils::retry::StallRecovery::ChainExhausted
                } else {
                    maho_ai::utils::retry::StallRecovery::NoFallbackConfigured
                }),
            },
        )
        .or_else(|| {
            message.error_message.as_deref().map(
                maho_ai::utils::provider_failure_description::strip_turn_retry_suppression_prefix,
            )
        })
    }

    fn retire_failed_retry_assistant(
        &self,
        failed: &maho_ai::types::AssistantMessage,
    ) -> Result<(), String> {
        let mut messages = self.messages();
        if messages.last().and_then(AgentMessage::as_assistant) != Some(failed) {
            return Ok(());
        }
        let failed_value = serde_json::to_value(messages.last().expect("assistant tail"))
            .map_err(|error| error.to_string())?;
        self.with_session_manager_mut(|manager| {
            let branch = manager.current_branch();
            if let Some(entry) = branch
                .last()
                .filter(|entry| entry.get("message") == Some(&failed_value))
            {
                return manager.set_leaf(entry.get("parentId").and_then(Value::as_str));
            }
            Ok(())
        })?;
        messages.pop();
        self.agent.set_messages(messages);
        self.state().message_revision += 1;
        Ok(())
    }

    fn requeue_at_original_place(&self, parked: &[AgentMessage], mode: StreamingBehavior) {
        if parked.is_empty() {
            return;
        }
        let get_text = |msg: &AgentMessage| -> Option<String> {
            match msg {
                AgentMessage::Llm(maho_ai::types::Message::User(u)) => match &u.content {
                    maho_ai::types::UserContent::Text(t) => Some(t.clone()),
                    maho_ai::types::UserContent::Blocks(b) => {
                        b.iter().find_map(|block| match block {
                            maho_ai::types::ContentBlock::Text(t) => Some(t.text.clone()),
                            _ => None,
                        })
                    }
                },
                _ => None,
            }
        };
        let order_of = |text: &str| -> u64 {
            let state = self.state();
            state
                .queued_input_order
                .iter()
                .find(|input| input.mode == mode && input.text == text)
                .map(|input| input.enqueue_order)
                .unwrap_or(u64::MAX)
        };
        let oldest_parked = parked
            .iter()
            .filter_map(get_text)
            .map(|t| order_of(&t))
            .min()
            .unwrap_or(u64::MAX);

        let mut to_requeue = parked.to_vec();
        let newer_texts: Vec<String> = {
            let state = self.state();
            state
                .queued_input_order
                .iter()
                .filter(|input| input.mode == mode && input.enqueue_order > oldest_parked)
                .map(|input| input.text.clone())
                .collect()
        };
        for text in newer_texts {
            let msg = make_user_message(&text, None);
            if !parked.contains(&msg) && self.agent.remove_queued_message(&msg) {
                to_requeue.push(msg);
            }
        }
        to_requeue.sort_by_key(|msg| get_text(msg).map(|t| order_of(&t)).unwrap_or(u64::MAX));
        for msg in to_requeue {
            match mode {
                StreamingBehavior::Steer => self.agent.steer(msg),
                StreamingBehavior::FollowUp => self.agent.follow_up(msg),
            }
        }
    }

    fn flush_post_compaction_deferred_messages(&self) -> bool {
        let (steering, follow_up) = {
            let mut state = self.state();
            (
                std::mem::take(&mut state.post_compaction_deferred_steering_messages),
                std::mem::take(&mut state.post_compaction_deferred_follow_up_messages),
            )
        };
        let has_deferred = !steering.is_empty() || !follow_up.is_empty();
        self.requeue_at_original_place(&steering, StreamingBehavior::Steer);
        self.requeue_at_original_place(&follow_up, StreamingBehavior::FollowUp);
        has_deferred
    }

    fn consume_post_compaction_usage_exemption(&self) -> bool {
        let latest_entry = self.latest_current_branch_entry_id("message", Some("assistant"));
        let mut state = self.state();
        latest_entry.is_some_and(|id| {
            if state.post_compaction_usage_exempt_entries.contains(&id) {
                return true;
            }
            let is_new =
                state
                    .last_persisted_assistant
                    .as_ref()
                    .is_some_and(|(entry, generation)| {
                        entry == &id
                            && state
                                .post_compaction_assistant_generation
                                .is_some_and(|boundary| *generation > boundary)
                    });
            if is_new {
                state.post_compaction_assistant_generation = None;
                state.post_compaction_usage_exempt_entries.insert(id);
            }
            is_new
        })
    }

    /// senpi `_isCursorPayloadOverflow`.
    fn is_cursor_payload_overflow(message: &maho_ai::types::AssistantMessage) -> bool {
        maho_ai::utils::overflow::is_cursor_payload_resource_exhausted(
            &maho_ai::utils::overflow::CursorExhaustionProbe::from_message(message),
            0,
        )
    }

    /// The overflow predicate of senpi `_checkCompaction`: a confirmed context overflow from the same
    /// source (or while the current context is also at the limit), a recoverable `length` stop from the
    /// same model, or a Cursor zero-token payload `resource_exhausted`.
    fn is_overflow_for_compaction(
        &self,
        message: &maho_ai::types::AssistantMessage,
        same_model: bool,
        current_context_needs_compaction: bool,
    ) -> bool {
        let model = self.model();
        let recoverable_length = same_model
            && maho_ai::utils::overflow::is_recoverable_length(message, model.max_tokens);
        (maho_ai::utils::overflow::is_context_overflow(message, Some(model.context_window))
            && (same_model || current_context_needs_compaction))
            || recoverable_length
            || Self::is_cursor_payload_overflow(message)
    }

    /// senpi `_truncateAgentMessagesToLastUserTurn`.
    fn truncate_agent_messages_to_last_user_turn(&self) -> bool {
        let messages = self.messages();
        let Some(last_user) = messages
            .iter()
            .rposition(|message| message.role() == "user")
        else {
            return false;
        };
        if last_user == 0 {
            return false;
        }
        self.agent.set_messages(messages[last_user..].to_vec());
        self.state().message_revision += 1;
        true
    }

    /// senpi `_isAssistantFromBeforeLatestCompaction`.
    fn is_assistant_from_before_latest_compaction(
        &self,
        assistant: &maho_ai::types::AssistantMessage,
    ) -> bool {
        // An agent message_end still awaiting persistence is necessarily a post-boundary message.
        if self
            .state()
            .messages_awaiting_persistence
            .iter()
            .any(|(_, pending)| pending.as_assistant() == Some(assistant))
        {
            return false;
        }
        self.with_session_manager(|manager| {
            let Some(compaction_id) = manager.latest_current_branch_entry_id("compaction", None)
            else {
                return false;
            };
            // PERF-DIVERGENCE(senpi agent-session.ts): upstream uses an identity map; we serialize the assistant only after the cheap pending/compaction checks pass instead of on every call. Same result, including false on serialization failure.
            let Ok(value) = serde_json::to_value(assistant) else {
                return false;
            };
            let compaction_order = manager.entry_order(&compaction_id);
            if let (Some((_, order)), Some(compaction_order)) =
                (manager.message_entry_position(&value), compaction_order)
            {
                return order <= compaction_order;
            }
            // Reloaded/reconstructed messages have no append position: keep the timestamp heuristic.
            manager
                .entry(&compaction_id)
                .as_ref()
                .and_then(|compaction| compaction.get("timestamp"))
                .and_then(Value::as_str)
                .and_then(|timestamp| chrono::DateTime::parse_from_rfc3339(timestamp).ok())
                .is_some_and(|timestamp| assistant.timestamp <= timestamp.timestamp_millis())
        })
    }

    /// The runtime context, filtered of context-excluded messages, as compaction values.
    fn context_message_values(&self) -> Result<Vec<Value>, String> {
        let messages = self
            .messages()
            .iter()
            .map(session_message_to_value)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        Ok(crate::messages::filter_context_excluded_messages(messages))
    }

    /// senpi `_exceedsPolicyByContentEstimate`: content measured by bytes, ignoring provider usage.
    fn exceeds_policy_by_content_estimate(&self) -> Result<bool, String> {
        let model = self.model();
        let settings = self.threshold_compaction_settings(&model)?;
        let tokens = estimate_messages_tokens(&self.context_message_values()?);
        Ok(crate::compaction::should_compact(
            tokens as f64,
            model.context_window as f64,
            &settings,
        ))
    }

    fn threshold_compaction_settings(
        &self,
        model: &Model,
    ) -> Result<crate::compaction::settings::CompactionSettings, String> {
        let resolved = self.get_compaction_settings(Some(model))?;
        Ok(crate::compaction::settings::CompactionSettings {
            enabled: resolved.enabled,
            reserve_tokens: resolved.reserve_tokens,
            keep_recent_tokens: resolved.keep_recent_tokens,
            ideal: crate::compaction::ideal_settings::IdealCompactionSettings {
                reserve_scaling_enabled: Some(resolved.reserve_scaling_enabled),
                ..crate::compaction::ideal_settings::default_ideal_compaction_settings()
            },
            ..crate::compaction::settings::default_compaction_settings()
        })
    }

    /// The context size the threshold branch of senpi `_checkCompaction` measures: direct usage resolved
    /// against the local transcript (`_resolveThresholdContextTokens`), else the trailing estimate, where
    /// a stale pre-compaction usage number is dropped in favor of the content estimate.
    fn threshold_context_tokens(
        &self,
        message: &maho_ai::types::AssistantMessage,
    ) -> Result<u64, String> {
        let messages = self.context_message_values()?;
        let direct = crate::compaction::calculate_context_tokens(&message.usage);
        if message.stop_reason != StopReason::Error && direct != 0 {
            let estimate = estimate_messages_tokens(&messages);
            return Ok(crate::compaction::resolve_threshold_context_tokens(
                direct as f64,
                estimate as f64,
            ) as u64);
        }
        let estimate = crate::compaction::estimate_context_tokens(&messages);
        if let Some(index) = estimate.last_usage_index
            && let Ok(usage_message) =
                serde_json::from_value::<maho_ai::types::AssistantMessage>(messages[index].clone())
            && self.is_assistant_from_before_latest_compaction(&usage_message)
        {
            // Drop only the stale usage number; the messages themselves still count.
            return Ok(estimate_messages_tokens(&messages));
        }
        Ok(estimate.tokens)
    }

    /// senpi `_modelConfigErrors.report`: logs `model_config_error` once per model and session (senpi#4282).
    fn report_model_config_error(&self, message: &maho_ai::types::AssistantMessage) {
        let session_id = self.session_id();
        let Some(event) = self
            .state()
            .model_config_errors
            .report(message, &session_id)
        else {
            return;
        };
        let mut data = Map::new();
        data.insert("model".to_owned(), Value::String(event.model.clone()));
        data.insert("provider".to_owned(), Value::String(event.provider.clone()));
        data.insert(
            "errorMessage".to_owned(),
            Value::String(message.error_message.clone().unwrap_or_default()),
        );
        crate::retry_fallback::log::FallbackLogger::new(
            std::path::Path::new(&self.agent_dir()),
            None,
        )
        .warn("model_config_error", Some(&data));
        self.emit(AgentSessionEvent::ModelConfigError {
            model: event.model,
            provider: event.provider,
            error_excerpt: event.error_excerpt,
        });
    }

    /// senpi `agent_end`: an unhandled Cursor quota `resource_exhausted` names the likely cause.
    fn append_cursor_quota_exhaustion_hint(
        &self,
        failed: &maho_ai::types::AssistantMessage,
        error: String,
    ) -> String {
        if failed.error_message.is_none() {
            return error;
        }
        let hinted = format!(
            "{error} (likely provider usage/quota exhaustion: conversation is well below the model context window)"
        );
        let mut messages = self.messages();
        let Some(index) = messages
            .iter()
            .rposition(|message| message.as_assistant() == Some(failed))
        else {
            return hinted;
        };
        if let AgentMessage::Llm(maho_ai::types::Message::Assistant(assistant)) =
            &mut messages[index]
        {
            std::sync::Arc::make_mut(assistant).error_message = Some(hinted.clone());
        }
        self.agent.set_messages(messages);
        self.state().message_revision += 1;
        hinted
    }

    /// senpi `_checkCompaction` rung step: spend one overflow recovery rung and return its
    /// `keepRecentTokensOverride`. The second rung keeps nothing but the summary and the turn being
    /// answered, because a re-send still too long after the configured tail needs a smaller one.
    fn begin_overflow_recovery_rung(&self) -> Option<i64> {
        let mut state = self.state();
        state.overflow_recovery_rungs = state.overflow_recovery_rungs.saturating_add(1);
        (state.overflow_recovery_rungs >= 2).then_some(0)
    }

    /// senpi `_checkCompaction` inline threshold route: when the required compaction finds nothing
    /// older than the recent window, split the current turn by compacting again with
    /// `keepRecentTokens` 0 (senpi#2925).
    async fn compact_for_model_splitting_turn(
        &self,
        budget_model: &Model,
        reason: &str,
    ) -> Result<crate::compaction::compaction::CompactionResult, String> {
        match self.compact_for_model(None, budget_model, reason).await {
            Err(error)
                if !self.is_compaction_delegated()
                    && maho_ai::utils::overflow::should_retry_overflow_without_compact(
                        false, &error,
                    ) =>
            {
                self.compact_for_model_with_retry(
                    None,
                    budget_model,
                    reason,
                    reason != "manual",
                    false,
                    Some(0),
                )
                .await
            }
            result => result,
        }
    }

    /// The overflow case of senpi `_checkCompaction` on the inline `pre_prompt` route
    /// (`_enforceCompactionBeforeProvider`): the latest assistant still overflowed, so compact once
    /// before admitting the prompt. Returns whether the overflow route handled admission.
    async fn check_pre_prompt_overflow(&self) -> Result<bool, String> {
        let Some(message) = self
            .messages()
            .iter()
            .rev()
            .find_map(AgentMessage::as_assistant)
            .cloned()
        else {
            return Ok(false);
        };
        let model = self.model();
        let upstream_model_id = self
            .model_runtime()
            .get_compatibility_request_config(&model)
            .upstream_model_id;
        let same_model = message.provider == model.provider
            && (message.model == model.id
                || upstream_model_id.as_deref() == Some(message.model.as_str()));
        let settings = self.threshold_compaction_settings(&model)?;
        let current_context_needs_compaction = self
            .get_context_usage()
            .and_then(|usage| usage.tokens)
            .is_some_and(|tokens| {
                crate::compaction::should_compact(
                    tokens as f64,
                    model.context_window as f64,
                    &settings,
                )
            });
        if message.stop_reason == StopReason::Stop
            || !self.is_overflow_for_compaction(
                &message,
                same_model,
                current_context_needs_compaction,
            )
            || (!settings.enabled
                && !crate::compaction::is_turn_stuck_on_context_overflow(
                    &message,
                    model.context_window,
                ))
            || self.is_assistant_from_before_latest_compaction(&message)
        {
            return Ok(false);
        }
        self.flush_post_compaction_deferred_messages();
        // A new turn admission is a fresh overflow episode, whatever an earlier turn spent: the
        // prompt it admits must reach a compaction, not a stale latch.
        self.state().overflow_recovery_rungs = 0;
        let keep_recent_tokens_override = self.begin_overflow_recovery_rung();
        let compacted = self
            .compact_for_model_with_retry(
                None,
                &model,
                "overflow",
                true,
                false,
                keep_recent_tokens_override,
            )
            .await;
        if self.is_compaction_delegated() {
            return Ok(true);
        }
        match compacted {
            Ok(_) => {
                // The failed assistant stays in session history but leaves the retry context.
                let mut messages = self.messages();
                if messages.last().and_then(AgentMessage::as_assistant) == Some(&message) {
                    messages.pop();
                    self.agent.set_messages(messages);
                    self.state().message_revision += 1;
                }
                Ok(true)
            }
            Err(error) => {
                if maho_ai::utils::overflow::is_context_overflow(
                    &message,
                    Some(model.context_window),
                ) {
                    self.state().overflow_recovery_rungs = 0;
                }
                self.state().compaction_skipped_too_small =
                    maho_ai::utils::overflow::should_retry_overflow_without_compact(false, &error);
                if self.state().compaction_skipped_too_small {
                    self.state().compaction_skipped_too_small = false;
                    if matches!(model.provider.as_str(), "cursor" | "cursor-cli-oauth") {
                        self.truncate_agent_messages_to_last_user_turn();
                    }
                    return Ok(true);
                }
                Err(RequiredCompactionError.to_string())
            }
        }
    }

    /// senpi `_continueAgentAfterCurrentRun`: a continuation the agent refuses (another run took
    /// over, or the transcript tail cannot be continued) ends the retry loop instead of the process.
    fn settle_refused_continuation(&self, error: maho_agent::AgentContinueError) {
        if error != maho_agent::AgentContinueError::AlreadyProcessing {
            crate::output_guard::maho_write_stderr(&format!("continuation skipped: {error}\n"));
        }
        if self.agent.signal().is_none() {
            self.clear_tool_barrier();
        }
    }

    async fn finish_provider_turn(&self) -> Result<(), String> {
        loop {
            let mut retry_owned_deferred_queue = false;
            let outcome = {
                let deferred = &mut retry_owned_deferred_queue;
                heap_future(move || self.finish_provider_turn_retrying(deferred))
            }
            .await;
            self.end_unhandled_retry();
            outcome?;
            if self.agent.signal().is_none() {
                let messages = self.messages();
                let outcome = if self.state().user_aborted {
                    "aborted"
                } else if messages.iter().rev().find_map(AgentMessage::as_assistant)
                    .is_some_and(|message| matches!(message.stop_reason, StopReason::Error | StopReason::Aborted)) {
                    "error"
                } else {
                    "completed"
                };
                let can_continue = outcome == "completed";
                let should_continue = self.emit_boundary(maho_ext_api::ExtensionEvent::AgentBeforeSettle {
                    entries: Vec::new(),
                    should_continue: false,
                    context: json!({
                        "outcome": outcome,
                        "canContinue": can_continue,
                        "contextEntries": self.with_session_manager(|manager| manager.branch(None)),
                        "contextMessages": messages,
                        "pendingMessages": [],
                    }),
                }).await?;
                if should_continue && can_continue && !self.state().user_aborted {
                    if let Err(error) = heap_future(|| self.agent.continue_boundary()).await {
                        self.settle_refused_continuation(error);
                        return Ok(());
                    }
                    continue;
                }
            }
            // senpi `retryExhaustionAllowsQueuedContinuation`: provider-timeout retries skip their
            // first queue poll so steering cannot be consumed by another doomed retry request. Once
            // the retry owner stops retrying, hand that retained queue back to the continuation
            // path instead of parking it until an unrelated later prompt arrives.
            let allows_queued_continuation = retry_owned_deferred_queue
                && !self.state().user_aborted
                && self
                    .messages()
                    .last()
                    .and_then(AgentMessage::as_assistant)
                    .is_some_and(|tail| {
                        matches!(tail.stop_reason, StopReason::Error | StopReason::Aborted)
                    })
                && self.agent.has_queued_messages();
            // A concurrent prompt already owns the agent: a benign takeover (senpi "taken-over").
            if !allows_queued_continuation || self.agent.signal().is_some() {
                if self.agent.signal().is_none() {
                    self.clear_tool_barrier();
                }
                return Ok(());
            }
            if let Err(error) =
                heap_future(|| self.agent.continue_with_queued_messages(Default::default())).await
            {
                self.settle_refused_continuation(error);
                return Ok(());
            }
        }
    }

    /// senpi `retryOutcome === "not-handled" && _retryAttempt > 0`: a retried attempt that failed
    /// with an error the retry loop did not take over (no longer retryable, overflow, user abort)
    /// still closes the retry episode; without it the TUI keeps showing "Retrying (n/m)".
    fn end_unhandled_retry(&self) {
        let attempt = self.state().retry_attempt;
        if attempt == 0 {
            return;
        }
        let Some(final_error) = self
            .messages()
            .iter()
            .rev()
            .find_map(AgentMessage::as_assistant)
            .and_then(|message| message.error_message.clone())
        else {
            return;
        };
        self.state().retry_attempt = 0;
        self.reset_hint_tier_state();
        self.emit(AgentSessionEvent::AutoRetryEnd {
            success: false,
            attempt,
            final_error: Some(final_error),
        });
    }

    /// The retry loop of senpi's `agent_end` handling. `retry_owned_deferred_queue` reports whether
    /// the run that produced the final tail was a retry continuation that deferred queued messages
    /// (senpi `DEFERRED_RETRY_QUEUE_OWNERS`).
    async fn finish_provider_turn_retrying(
        &self,
        retry_owned_deferred_queue: &mut bool,
    ) -> Result<(), String> {
        use crate::retry_fallback::controller::FallbackReason;
        use maho_ai::utils::retry_hint::parse_retry_after_ms_marker;
        let mut overflow_compacted = false;
        let mut deferred_by_retry = false;
        loop {
            self.agent.wait_for_idle().await;
            *retry_owned_deferred_queue = std::mem::take(&mut deferred_by_retry);
            let Some(message) = self
                .messages()
                .iter()
                .rev()
                .find_map(AgentMessage::as_assistant)
                .cloned()
            else {
                return Ok(());
            };
            let assistant_pending_persistence = {
                let state = self.state();
                state.assistant_generation
                    > state
                        .last_persisted_assistant
                        .as_ref()
                        .map_or(0, |(_, generation)| *generation)
            };
            let assistant_before_compaction = !assistant_pending_persistence
                && self.is_assistant_from_before_latest_compaction(&message);
            if assistant_before_compaction && !self.exceeds_policy_by_content_estimate()? {
                return Ok(());
            }
            if message.stop_reason == StopReason::Stop && !overflow_compacted {
                let exempt = self.consume_post_compaction_usage_exemption();
                if exempt {
                    let has_deferred = self.flush_post_compaction_deferred_messages();
                    if has_deferred && !self.state().user_aborted {
                        if let Err(error) = heap_future(|| {
                            self.agent.continue_with_queued_messages(Default::default())
                        })
                        .await
                        {
                            self.settle_refused_continuation(error);
                            return Ok(());
                        }
                        continue;
                    }
                    return Ok(());
                }
            }
            let model = self.model();
            let upstream_model_id = self
                .model_runtime()
                .get_compatibility_request_config(&model)
                .upstream_model_id;
            let same_overflow_source = message.provider == model.provider
                && (message.model == model.id
                    || upstream_model_id.as_deref() == Some(message.model.as_str()));
            let current_context_needs_compaction = if same_overflow_source
                || !self.auto_compaction_enabled()
            {
                false
            } else {
                let raw =
                    self.with_settings_manager(|manager| manager.get_value("compaction").cloned());
                let configured = raw
                    .map(
                        serde_json::from_value::<
                            crate::compaction_settings_access::CompactionSettings,
                        >,
                    )
                    .transpose()
                    .map_err(|error| error.to_string())?;
                let resolved = crate::compaction_settings_resolver::resolve_compaction_settings(
                    configured.as_ref(),
                    Some(crate::compaction_settings_access::CompactionModelSelector {
                        provider: &model.provider,
                        id: &model.id,
                    }),
                )?;
                let reserve = if resolved.reserve_scaling_enabled {
                    crate::compaction::compaction::resolve_reserve_tokens(
                        model.context_window as f64,
                        resolved.reserve_tokens as f64,
                    ) as u64
                } else {
                    resolved.reserve_tokens as u64
                };
                self.get_context_usage()
                    .and_then(|usage| usage.tokens)
                    .is_some_and(|tokens| tokens > model.context_window.saturating_sub(reserve))
            };
            if (self.auto_compaction_enabled()
                || crate::compaction::is_turn_stuck_on_context_overflow(
                    &message,
                    self.model().context_window,
                ))
                && !self.is_compaction_delegated()
                && !self.state().user_aborted
                && self.is_overflow_for_compaction(
                    &message,
                    same_overflow_source,
                    current_context_needs_compaction,
                )
            {
                self.flush_post_compaction_deferred_messages();
                let will_retry = message.stop_reason != StopReason::Stop;
                if will_retry && self.state().overflow_recovery_rungs >= OVERFLOW_RECOVERY_RUNGS {
                    self.emit(AgentSessionEvent::CompactionEnd {
                        reason: maho_ext_api::CompactionReason::Overflow,
                        request_id: None,
                        result: None,
                        aborted: false,
                        will_retry: false,
                        accepted: None,
                        rejection_cause: None,
                        error_message: Some(OVERFLOW_RECOVERY_FAILED_MESSAGE.to_owned()),
                    });
                    return Ok(());
                }
                overflow_compacted |= will_retry;
                let keep_recent_tokens_override = if will_retry {
                    self.begin_overflow_recovery_rung()
                } else {
                    None
                };
                let execution = {
                    let _auto = self.begin_auto_compaction();
                    let model = self.model();
                    heap_future(|| {
                        self.compact_for_model_with_retry(
                            None,
                            &model,
                            "overflow",
                            will_retry,
                            false,
                            keep_recent_tokens_override,
                        )
                    })
                    .await
                };
                if self.is_compaction_delegated() {
                    return Ok(());
                }
                if let Err(error) = execution {
                    // senpi blocks the retry when required overflow compaction fails; the too-small flag is
                    // consumed by the next inline (pre-prompt) overflow check.
                    self.state().overflow_recovery_rungs = 0;
                    self.state().compaction_skipped_too_small =
                        maho_ai::utils::overflow::should_retry_overflow_without_compact(
                            false, &error,
                        );
                    return Err(error);
                }
                if !will_retry {
                    return Ok(());
                }
                let mut messages = self.messages();
                if messages
                    .last()
                    .and_then(AgentMessage::as_assistant)
                    .is_some_and(|tail| {
                        matches!(tail.stop_reason, StopReason::Error | StopReason::Length)
                    })
                {
                    messages.pop();
                    self.agent.set_messages(messages);
                    self.state().message_revision += 1;
                }
                if let Err(error) = heap_future(|| {
                    self.agent
                        .continue_run(maho_agent::agent::AgentContinuationOptions {
                            defer_queued_messages: Some(true),
                            ..Default::default()
                        })
                })
                .await
                {
                    self.settle_refused_continuation(error);
                    return Ok(());
                }
                continue;
            }
            // senpi `retryAfterRequiredCompaction`: a run ended by a failed required compaction runs
            // threshold recovery with retry; if it is still required and input is queued, the
            // originating prompt rejects with the stored admission error.
            if !self.state().user_aborted && self.required_compaction_still_needed(&message)? {
                let compacted = {
                    let _auto = self.begin_auto_compaction();
                    let model = self.model();
                    heap_future(|| {
                        self.compact_for_model_with_retry(
                            None,
                            &model,
                            "threshold",
                            true,
                            false,
                            None,
                        )
                    })
                    .await
                    .is_ok()
                };
                if self.is_compaction_delegated() {
                    return Ok(());
                }
                if compacted {
                    self.state().required_compaction_admission_error = None;
                    let mut messages = self.messages();
                    if messages
                        .last()
                        .and_then(AgentMessage::as_assistant)
                        .is_some_and(|tail| tail.stop_reason == StopReason::Error)
                    {
                        messages.pop();
                        self.agent.set_messages(messages);
                        self.state().message_revision += 1;
                    }
                    if let Err(error) =
                        heap_future(|| self.agent.continue_with_queued_messages(Default::default()))
                            .await
                    {
                        self.settle_refused_continuation(error);
                        return Ok(());
                    }
                    continue;
                }
                if self.agent.has_queued_messages()
                    && self.required_compaction_still_needed(&message)?
                {
                    self.state().required_compaction_admission_error =
                        Some(RequiredCompactionError.to_string());
                }
                return Ok(());
            }
            // senpi `_handleRequestShapeRejection`: a request-shape 400 is ours to fix, not the model's. It is
            // recovered once on the same model and never walks the fallback chain (senpi#3727).
            let request_shape = self.request_shape_decision(&message);
            if request_shape
                == crate::retry_fallback::request_shape::RequestShapeDecision::NotRequestShape
            {
                lock(&self.request_shape_recovery).end_streak();
            }
            let mut request_shape_recover = false;
            if request_shape
                != crate::retry_fallback::request_shape::RequestShapeDecision::NotRequestShape
                && !self.state().user_aborted
                && self.with_settings_manager(|manager| {
                    manager
                        .get_value("retry")
                        .and_then(|settings| settings.get("enabled"))
                        .and_then(Value::as_bool)
                        .unwrap_or(true)
                })
            {
                if request_shape
                    == crate::retry_fallback::request_shape::RequestShapeDecision::GiveUp
                {
                    lock(&self.request_shape_recovery).end_streak();
                    let attempt = self.state().retry_attempt.max(1);
                    self.state().retry_attempt = 0;
                    self.emit(AgentSessionEvent::AutoRetryEnd {
                        success: false,
                        attempt,
                        final_error: Some(
                            crate::retry_fallback::request_shape::REQUEST_SHAPE_FAILURE_TEXT
                                .to_owned(),
                        ),
                    });
                } else {
                    lock(&self.request_shape_recovery).begin(message.timestamp);
                    let entry = self.with_session_manager_mut(|manager| manager.append_custom(
                        crate::retry_fallback::request_shape::REQUEST_SHAPE_RECOVERY_ENTRY_TYPE,
                        Some(serde_json::json!({
                            "note": crate::retry_fallback::request_shape::REQUEST_SHAPE_RECOVERY_NOTE,
                            "model": message.model,
                        }))));
                    if let Some(id) = entry.get("id").and_then(Value::as_str) {
                        self.emit_entry_appended(id);
                    }
                    self.session_logger.info(
                        "request_shape_recovery",
                        Some(
                            &[
                                (
                                    "provider".to_owned(),
                                    Value::String(message.provider.clone()),
                                ),
                                ("model".to_owned(), Value::String(message.model.clone())),
                            ]
                            .into_iter()
                            .collect(),
                        ),
                    );
                    self.state().retry_attempt = 0;
                    request_shape_recover = true;
                }
            }
            if !request_shape_recover
                && !heap_future(|| self.retry_can_admit_provider(&message)).await
            {
                if crate::retry_fallback::model_config_error::is_model_config_failure(&message) {
                    self.report_model_config_error(&message);
                }
                if let Some(error) = message.error_message.clone()
                    && maho_ai::utils::overflow::is_cursor_quota_resource_exhausted(
                        &maho_ai::utils::overflow::CursorExhaustionProbe::from_message(&message),
                        model.context_window,
                    )
                {
                    self.append_cursor_quota_exhaustion_hint(&message, error);
                }
                if self.auto_compaction_enabled()
                    && !self.state().user_aborted
                    && !overflow_compacted
                    && message.stop_reason != StopReason::Aborted
                    && !maho_ai::utils::overflow::is_context_overflow(
                        &message,
                        Some(self.model().context_window),
                    )
                    && self.consume_post_compaction_usage_exemption()
                {
                    if self.flush_post_compaction_deferred_messages() {
                        if let Err(error) = heap_future(|| {
                            self.agent.continue_with_queued_messages(Default::default())
                        })
                        .await
                        {
                            self.settle_refused_continuation(error);
                            return Ok(());
                        }
                        continue;
                    }
                    return Ok(());
                }
                if self.auto_compaction_enabled()
                    && !self.is_compaction_delegated()
                    && !self.state().user_aborted
                    && !matches!(message.stop_reason, StopReason::Error | StopReason::Aborted)
                {
                    let model = self.model();
                    let raw = self
                        .with_settings_manager(|manager| manager.get_value("compaction").cloned());
                    let configured = raw
                        .map(
                            serde_json::from_value::<
                                crate::compaction_settings_access::CompactionSettings,
                            >,
                        )
                        .transpose()
                        .map_err(|error| error.to_string())?;
                    let resolved =
                        crate::compaction_settings_resolver::resolve_compaction_settings(
                            configured.as_ref(),
                            Some(crate::compaction_settings_access::CompactionModelSelector {
                                provider: &model.provider,
                                id: &model.id,
                            }),
                        )?;
                    let reserve = if resolved.reserve_scaling_enabled {
                        crate::compaction::compaction::resolve_reserve_tokens(
                            model.context_window as f64,
                            resolved.reserve_tokens as f64,
                        ) as u64
                    } else {
                        resolved.reserve_tokens as u64
                    };
                    let threshold = model.context_window.saturating_sub(reserve);
                    if self.threshold_context_tokens(&message)? > threshold {
                        let result = {
                            let _auto = self.begin_auto_compaction();
                            heap_future(|| self.compact_for_model(None, &model, "threshold")).await
                        };
                        if !self.is_compaction_delegated() {
                            result?;
                        }
                    }
                }
                return Ok(());
            }
            if self.auto_compaction_enabled() && !self.is_compaction_delegated() {
                let raw =
                    self.with_settings_manager(|manager| manager.get_value("compaction").cloned());
                let configured = raw
                    .map(
                        serde_json::from_value::<
                            crate::compaction_settings_access::CompactionSettings,
                        >,
                    )
                    .transpose()
                    .map_err(|error| error.to_string())?;
                let resolved = crate::compaction_settings_resolver::resolve_compaction_settings(
                    configured.as_ref(),
                    Some(crate::compaction_settings_access::CompactionModelSelector {
                        provider: &model.provider,
                        id: &model.id,
                    }),
                )?;
                let reserve = if resolved.reserve_scaling_enabled {
                    crate::compaction::compaction::resolve_reserve_tokens(
                        model.context_window as f64,
                        resolved.reserve_tokens as f64,
                    ) as u64
                } else {
                    resolved.reserve_tokens as u64
                };
                if self.threshold_context_tokens(&message)?
                    > model.context_window.saturating_sub(reserve)
                {
                    self.retire_failed_retry_assistant(&message)?;
                    if let Err(error) =
                        heap_future(|| self.revalidate_continuation_admission(false)).await
                    {
                        let attempt = self.state().retry_attempt;
                        self.state().retry_attempt = 0;
                        self.reset_hint_tier_state();
                        self.emit(AgentSessionEvent::AutoRetryEnd {
                            success: false,
                            attempt,
                            final_error: Some(error),
                        });
                        return Ok(());
                    }
                }
            }
            let settings = self
                .with_settings_manager(|manager| manager.get_value("retry").cloned())
                .unwrap_or(Value::Null);
            let profile = self.resolve_retry_profile();
            let max_attempts = profile.turn.max_retries;
            let base_delay = profile.turn.backoff.base_delay_ms;
            let profile_cap = match &profile.turn.server_hint {
                maho_ai::utils::retry_profile::types::RetryServerHintPolicy::Override {
                    ceiling,
                    ..
                } => ceiling.max_delay_ms,
                maho_ai::utils::retry_profile::types::RetryServerHintPolicy::Tiered { .. } => {
                    Some(60_000)
                }
            };
            // The over-ceiling gate keys off `retry.provider.maxRetryDelayMs`; a profile's override-mode
            // ceiling owns it instead (`null` bypasses the gate). `cap` stays the final per-wait clamp.
            let max_retry_delay_ms = self.with_settings_manager(|manager| {
                manager.get_provider_retry_settings().max_retry_delay_ms
            });
            let effective_max_retry_delay_ms = match &profile.turn.server_hint {
                maho_ai::utils::retry_profile::types::RetryServerHintPolicy::Override {
                    ceiling,
                    ..
                } => ceiling.max_delay_ms,
                maho_ai::utils::retry_profile::types::RetryServerHintPolicy::Tiered { .. } => {
                    Some(max_retry_delay_ms)
                }
            }
            .unwrap_or(u64::MAX);
            let cap = settings
                .get("maxAgentDelayMs")
                .and_then(Value::as_u64)
                .or(profile_cap)
                .unwrap_or(u64::MAX);
            let error = if request_shape_recover {
                crate::retry_fallback::request_shape::REQUEST_SHAPE_RECOVERY_NOTE.to_owned()
            } else {
                message
                    .error_message
                    .clone()
                    .unwrap_or_else(|| "Unknown error".to_owned())
            };
            let hint = parse_retry_after_ms_marker(&error).or_else(|| {
                maho_ai::utils::retry_hint::extract_429_retry_after_ms(
                    &maho_ai::utils::retry_hint::RetryHintInput {
                        body_text: &error,
                        ..Default::default()
                    },
                    Some(self.fallback_now() as i64),
                )
            });
            let refusal = maho_ai::utils::stop_details::is_classifier_refusal(&message);
            let same_model_remint =
                maho_ai::utils::overflow::is_cursor_zero_token_resource_exhausted(
                    &maho_ai::utils::overflow::CursorExhaustionProbe::from_message(&message),
                ) || self.is_subscription_same_model_remint_error(&message)
                    || request_shape_recover;
            let quota_exhausted = maho_ai::utils::overflow::is_cursor_quota_resource_exhausted(
                &maho_ai::utils::overflow::CursorExhaustionProbe::from_message(&message),
                model.context_window,
            );
            if quota_exhausted {
                self.retire_failed_retry_assistant(&message)?;
            }
            let transient = !quota_exhausted
                && (maho_ai::utils::retry::is_retryable_assistant_error(&message)
                    || maho_ai::utils::retry::is_provider_timeout_error(&message));
            let attempt = self.state().retry_attempt.saturating_add(1);
            if same_model_remint && attempt > max_attempts {
                self.state().retry_attempt = 0;
                self.reset_hint_tier_state();
                if attempt > 1 {
                    self.emit(AgentSessionEvent::AutoRetryEnd {
                        success: false,
                        attempt,
                        final_error: Some(error),
                    });
                }
                return Ok(());
            }
            let rate_limited = !same_model_remint
                && !quota_exhausted
                && !maho_ai::utils::retry::is_provider_timeout_error(&message)
                && RATE_LIMIT_PATTERN.is_match(&error);
            let tier_routed = rate_limited
                && profile.fallback.rate_limited
                    == maho_ai::utils::retry_profile::types::FallbackRateLimited::Tiered;
            let hint_settings =
                crate::retry_fallback::settings::resolve_hint_policy_settings(Some(&settings));
            let tier = crate::retry_fallback::hint_policy::classify_rate_limited_wait(
                hint.map(|hint| hint as f64),
                hint_settings,
            );
            let mut hint_delay = None;
            if tier_routed && tier == crate::retry_fallback::hint_policy::HintTier::Tier1InTurn {
                let mut state = self.state();
                let result = crate::retry_fallback::hint_policy::next_in_turn_delay_ms(
                    crate::retry_fallback::hint_policy::InTurnState {
                        probe_phase: state.probe_phase,
                        hint_deadline_ms: state.hint_deadline_ms,
                        attempt,
                        cumulative_hinted_wait_ms: state.cumulative_hinted_wait_ms,
                    },
                    hint.map(|hint| hint as f64),
                    base_delay,
                    hint_settings.hinted_wait_cap_ms,
                    self.fallback_now(),
                );
                state.probe_phase = result.probe_phase;
                state.hint_deadline_ms = result.hint_deadline_ms;
                state.cumulative_hinted_wait_ms = result.cumulative_hinted_wait_ms;
                if !result.demote_to_probe_back {
                    hint_delay = Some(result.delay_ms as u64);
                }
            }
            let needs_fallback = !same_model_remint
                && (refusal
                    || !transient
                    || attempt > max_attempts
                    || if tier_routed {
                        hint_delay.is_none()
                    } else {
                        hint.is_some_and(|hint| hint > effective_max_retry_delay_ms)
                    });
            let mut switched = false;
            let mut same_model_native_recovery = false;
            let mut probe_selector = None;
            // A rejected native tool-search request is recoverable in place: the adapter is disabled for
            // the session on the 400, so the SAME model retries once instead of demoting down the chain.
            if needs_fallback
                && !refusal
                && !transient
                && self.take_native_tool_search_injection_failure().is_some()
            {
                same_model_native_recovery = true;
            }
            if needs_fallback && !same_model_native_recovery {
                let reason = if refusal {
                    FallbackReason::Refusal
                } else if transient {
                    FallbackReason::Transient
                } else if crate::retry_fallback::billing::is_billing_error_message(Some(&error)) {
                    FallbackReason::Billing
                } else {
                    FallbackReason::HardError
                };
                let mut controller = self.retry_fallback.lock().await;
                if let Some(controller) = controller.as_mut() {
                    if crate::provider_failure::is_request_level_refusal(&message) {
                        controller.note_refused_provider(&message.provider);
                    }
                    switched = controller
                        .try_fallback(
                            reason,
                            crate::retry_fallback::cooldown::SelectorFailure {
                                error_message: Some(&error),
                                retry_after_ms: hint.map(|hint| hint as f64),
                            },
                        )
                        .await?;
                    if switched
                        && tier_routed
                        && tier
                            == crate::retry_fallback::hint_policy::HintTier::Tier2FallbackProbeBack
                    {
                        probe_selector = controller
                            .state
                            .as_ref()
                            .map(|state| state.original_selector.clone());
                    }
                    if !switched && let Some(chain_key) = controller.exhausted_chain_key.clone() {
                        self.emit(AgentSessionEvent::RetryFallbackExhausted {
                            chain_key,
                            last_error: error.clone(),
                        });
                    }
                }
                drop(controller);
                let mut degraded_fail_hint_ms = None;
                if !switched && tier_routed && attempt <= max_attempts && !refusal {
                    match crate::retry_fallback::hint_policy::degrade_without_fallback(
                        tier,
                        hint.map(|hint| hint as f64),
                        attempt,
                        base_delay,
                        hint_settings.hinted_wait_cap_ms,
                    ) {
                        crate::retry_fallback::hint_policy::DegradedRateLimitAction::InTurn {
                            delay_ms,
                        } => hint_delay = Some(delay_ms as u64),
                        crate::retry_fallback::hint_policy::DegradedRateLimitAction::Fail {
                            hint_ms,
                        } => degraded_fail_hint_ms = Some(hint_ms),
                    }
                }
                if !switched && hint_delay.is_none() {
                    let error = if quota_exhausted {
                        self.append_cursor_quota_exhaustion_hint(&message, error)
                    } else {
                        error
                    };
                    let previous_attempt = self.state().retry_attempt;
                    let (attempt, final_error) = if let Some(hint_ms) = degraded_fail_hint_ms {
                        // `_degradeRateLimitedWithoutFallback`: tier3 waits stay terminal, naming the wait.
                        (
                            previous_attempt,
                            Some(format!(
                                "Provider requested a {}s wait before retrying and no usable fallback model is available. {}",
                                (hint_ms / 1000.0).ceil() as u64,
                                message.error_message.as_deref().unwrap_or("")
                            )),
                        )
                    } else if transient && !refusal && !tier_routed && attempt > max_attempts {
                        (
                            previous_attempt,
                            self.terminal_failure_text(&message, previous_attempt).await,
                        )
                    } else if transient
                        && !refusal
                        && !tier_routed
                        && let Some(provider_delay_ms) =
                            hint.filter(|hint| *hint > effective_max_retry_delay_ms)
                    {
                        (
                            attempt,
                            Some(format!(
                                "Provider requested retry delay {provider_delay_ms}ms, exceeding configured maximum {max_retry_delay_ms}ms"
                            )),
                        )
                    } else {
                        (previous_attempt, Some(error))
                    };
                    self.state().retry_attempt = 0;
                    self.reset_hint_tier_state();
                    self.emit(AgentSessionEvent::AutoRetryEnd {
                        success: false,
                        attempt,
                        final_error,
                    });
                    return Ok(());
                }
            }
            if let Some(selector) = probe_selector {
                self.arm_probe_back(&selector, hint.unwrap_or(0) as f64);
            }
            let attempt = if switched || same_model_native_recovery {
                1
            } else {
                attempt
            };
            self.state().retry_attempt = attempt;
            let local_delay = maho_ai::utils::retry_profile::backoff::retry_backoff_delay_ms(
                &profile.turn.backoff,
                attempt,
                (self.retry_random)(),
            );
            let delay_ms = if switched || same_model_native_recovery {
                0
            } else {
                hint_delay
                    .or(hint.filter(|hint| *hint > 0))
                    .unwrap_or(local_delay as u64)
                    .min(cap)
            };
            let abort = maho_ai::utils::abort::AbortController::new();
            self.state().retry_abort_controller = Some(abort.clone());
            self.emit(AgentSessionEvent::AutoRetryStart {
                attempt,
                max_attempts,
                delay_ms,
                error_message: error,
            });
            self.retire_failed_retry_assistant(&message)?;
            let signal = abort.signal();
            tokio::select! {
                _ = signal.cancelled() => {
                    self.state().retry_abort_controller = None;
                    self.state().retry_attempt = 0;
                    self.reset_hint_tier_state();
                    // senpi: abort during the retry sleep settles the run before auto_retry_end.
                    heap_future(|| self.emit_agent_settled()).await;
                    self.emit(AgentSessionEvent::AutoRetryEnd { success: false, attempt, final_error: Some("Retry cancelled".to_owned()) });
                    return Ok(());
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(delay_ms)) => {}
            }
            self.state().retry_abort_controller = None;
            self.restore_fallback_primary().await;
            // Model fallback can select a smaller window after the earlier checks: re-check the canonical
            // context right before the continuation so a failed compaction never admits that model.
            let model = self.model();
            let resolved = self.get_compaction_settings(Some(&model))?;
            let compaction_settings = crate::compaction::settings::CompactionSettings {
                enabled: resolved.enabled,
                reserve_tokens: resolved.reserve_tokens,
                keep_recent_tokens: resolved.keep_recent_tokens,
                ideal: crate::compaction::ideal_settings::IdealCompactionSettings {
                    reserve_scaling_enabled: Some(resolved.reserve_scaling_enabled),
                    ..crate::compaction::ideal_settings::default_ideal_compaction_settings()
                },
                ..crate::compaction::settings::default_compaction_settings()
            };
            let context_tokens = crate::compaction::estimate_context_tokens(
                &crate::messages::filter_context_excluded_messages(
                    self.with_session_manager(|manager| manager.current_context().messages),
                ),
            )
            .tokens;
            if compaction_settings.enabled
                && crate::compaction::should_compact(
                    context_tokens as f64,
                    model.context_window as f64,
                    &compaction_settings,
                )
            {
                let pre_retry_compaction = if self.is_compaction_delegated() {
                    None
                } else {
                    Some(
                        heap_future(|| {
                            self.compact_for_model_with_retry(
                                None,
                                &model,
                                "threshold",
                                true,
                                true,
                                None,
                            )
                        })
                        .await,
                    )
                };
                let on_cooldown = matches!(self.compaction_state(), crate::compaction::lifecycle::CompactionLifecycleState::Failed(_, _, Some(cause), _)
                    if cause == "circuit-breaker");
                if !pre_retry_compaction.as_ref().is_some_and(Result::is_ok)
                    && !on_cooldown
                    && !self.is_compaction_delegated()
                {
                    self.state().retry_attempt = 0;
                    self.reset_hint_tier_state();
                    self.emit(AgentSessionEvent::AutoRetryEnd {
                        success: false,
                        attempt,
                        final_error: Some(RequiredCompactionError.to_string()),
                    });
                    return Ok(());
                }
            }
            if let Err(error) = heap_future(|| self.revalidate_continuation_admission(false)).await
            {
                self.state().retry_attempt = 0;
                self.reset_hint_tier_state();
                self.emit(AgentSessionEvent::AutoRetryEnd {
                    success: false,
                    attempt,
                    final_error: Some(error),
                });
                return Ok(());
            }
            let plan = crate::provider_timeout_retry::create_provider_timeout_retry_plan(
                &message,
                crate::provider_timeout_retry::ProviderTimeoutRetryPlanInput {
                    stream_retry_timeout_ms: settings
                        .get("provider")
                        .and_then(|provider| provider.get("streamRetryTimeoutMs"))
                        .and_then(Value::as_u64)
                        .or(Some(30_000)),
                    timeout_ms: self.agent.timeout_ms(),
                    stream_start_timeout_ms: self.agent.stream_start_timeout_ms(),
                },
            );
            deferred_by_retry = plan.options.defer_queued_messages == Some(true);
            // Match the primary prompt's heap boundary: retry continuations also embed the
            // large agent loop future. Keep ownership here so the watchdog still aborts and
            // awaits this same run rather than dropping it or detaching its completion.
            let mut continuation = heap_future(|| self.agent.continue_run(plan.options));
            let started = if let Some(timeout) = plan.watchdog_timeout_ms {
                tokio::select! {
                    started = &mut continuation => started,
                    _ = tokio::time::sleep(std::time::Duration::from_millis(timeout)) => {
                        self.agent.abort(Some(maho_agent::ProviderRetryWatchdogAbortError::new(
                            provider_retry_watchdog_abort_message(Some(timeout), self.agent.stream_start_timeout_ms()),
                        ).into_abort_reason()));
                        continuation.await
                    }
                }
            } else {
                continuation.await
            };
            if let Err(error) = started {
                self.settle_refused_continuation(error);
                return Ok(());
            }
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

    /// senpi: `settingsManager.getAbortServerSideFallback() && _retryFallback.hasConfiguredChain()`.
    /// Resolved without the controller lock because fallback switches call
    /// `switch_model_with_thinking` while the controller is held.
    fn abort_server_side_fallback_enabled(&self) -> bool {
        let settings = self.with_settings_manager(|manager| manager.get_value("retry").cloned());
        if !crate::retry_fallback::settings::resolve_abort_server_side_fallback(settings.as_ref()) {
            return false;
        }
        let chains =
            crate::retry_fallback::settings::resolve_retry_fallback_settings(settings.as_ref())
                .chains;
        let models = self.model_registry.get_all();
        let oauth = |model: &Model| self.model_registry.is_using_oauth(model);
        let eligible = |model: &Model| self.model_registry.is_fallback_eligible(model);
        let configured = |model: &Model| self.model_registry.has_configured_auth(model);
        let canonical = crate::retry_fallback::chains::canonicalize_fallback_chains(
            &chains,
            &models,
            &crate::retry_fallback::expansion::FallbackAuthTiers {
                is_using_oauth: &oauth,
                has_configured_auth: Some(&configured),
                is_fallback_eligible: Some(&eligible),
            },
        );
        crate::retry_fallback::chains::resolve_chain_key(
            &self.model(),
            Some(self.thinking_level()),
            &canonical,
        )
        .is_some()
    }

    fn cancel_probe_back(&self) {
        lock(&self.probe_scheduler).cancel();
        if let Some(task) = lock(&self.probe_task).take() {
            task.abort();
        }
    }

    fn arm_probe_back(&self, selector: &str, hint_ms: f64) {
        let Some((provider, id)) = selector.split_once('/') else {
            return;
        };
        if selector == format!("{}/{}", self.model().provider, self.model().id) {
            return;
        }
        let Some(model) = self.model_runtime().get_model(provider, id) else {
            return;
        };
        if !self.model_registry.has_configured_auth(&model) {
            return;
        }
        self.cancel_probe_back();
        let schedule =
            crate::retry_fallback::hint_policy::probe_back_schedule(hint_ms, self.fallback_now());
        let events = lock(&self.probe_scheduler).arm(
            crate::retry_fallback::probe_scheduler::ProbeBackPlan {
                selector: selector.to_owned(),
                first_at_ms: schedule.first_at_ms,
                deadline_ms: schedule.deadline_ms,
            },
        );
        self.emit_probe_events(events);
        let weak = Arc::downgrade(&self.inner);
        *lock(&self.probe_task) = Some(tokio::spawn(async move {
            loop {
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                let session = AgentSession { inner };
                let Some(deadline) = lock(&session.probe_scheduler).next_deadline() else {
                    return;
                };
                let delay = (deadline - session.fallback_now()).max(0.0);
                tokio::time::sleep(std::time::Duration::from_secs_f64(delay / 1000.0)).await;
                let (ticket, events) = lock(&session.probe_scheduler).begin_due_probe(
                    session.fallback_now().max(deadline),
                    session.model_registry.has_configured_auth(&model),
                );
                session.emit_probe_events(events);
                let Some(ticket) = ticket else {
                    return;
                };
                let context = maho_ai::types::Context {
                    active_tool_names: None,
                    system_prompt: Some("Reply with OK.".to_owned()),
                    messages: vec![maho_ai::types::Message::User(maho_ai::types::UserMessage {
                        content: maho_ai::types::UserContent::Text("OK".to_owned()),
                        timestamp: session.fallback_now() as i64,
                    })],
                    tools: None,
                };
                let request = session.model_runtime().stream_simple(
                    &model,
                    &context,
                    Some(maho_ai::types::SimpleStreamOptions {
                        stream: maho_ai::types::StreamOptions {
                            max_tokens: Some(1),
                            request: maho_ai::types::ProviderRequestOptions {
                                signal: Some(ticket.signal.clone()),
                                cwd: Some(session.cwd()),
                                ..Default::default()
                            },
                            ..Default::default()
                        },
                        ..Default::default()
                    }),
                );
                let remaining = (schedule.deadline_ms - session.fallback_now()).max(0.0);
                // Pop events so their partial-message clones are freed instead of queued until drop.
                let settled = async {
                    while let Ok(Some(_)) = request.next().await {}
                    request.result().await
                };
                let result = if ticket.index == 1 {
                    match tokio::time::timeout(
                        std::time::Duration::from_secs_f64(remaining / 1000.0),
                        settled,
                    )
                    .await
                    {
                        Ok(result) => result.ok(),
                        Err(_) => {
                            if let Err(error) = request.cancel().await {
                                session.emit_probe_events(vec![crate::retry_fallback::probe_scheduler::ProbeBackEvent::Result {
                                    selector: format!("{}/{}", model.provider, model.id), ok: false, error_message: Some(error.message),
                                }]);
                            }
                            continue;
                        }
                    }
                } else {
                    settled.await.ok()
                };
                let ok = result.is_some_and(|message| {
                    !matches!(message.stop_reason, StopReason::Error | StopReason::Aborted)
                });
                let events = lock(&session.probe_scheduler).finish_probe(&ticket, ok);
                if ok && let Some(controller) = session.retry_fallback.lock().await.as_mut() {
                    controller
                        .cooldowns
                        .clear(&format!("{}/{}", model.provider, model.id));
                }
                session.emit_probe_events(events);
            }
        }));
    }

    fn emit_probe_events(
        &self,
        events: Vec<crate::retry_fallback::probe_scheduler::ProbeBackEvent>,
    ) {
        for event in events {
            match event {
                crate::retry_fallback::probe_scheduler::ProbeBackEvent::Scheduled {
                    selector,
                    at_ms,
                    probe_index,
                } => self.emit(AgentSessionEvent::RetryProbeScheduled {
                    selector,
                    at_ms: at_ms as u64,
                    probe_index,
                }),
                crate::retry_fallback::probe_scheduler::ProbeBackEvent::Result {
                    selector,
                    ok,
                    error_message,
                } => self.emit(AgentSessionEvent::RetryProbeResult {
                    selector,
                    ok,
                    error_message,
                }),
            }
        }
    }

    pub fn pending_model_switch(&self) -> Option<PendingModelSwitch> {
        self.state().pending_model_switch.clone()
    }

    fn model_budget(
        &self,
        model: &Model,
        live_context_tokens: u64,
        speculation: bool,
    ) -> Result<(maho_ext_api::ModelBudget, bool), String> {
        let raw = self.with_settings_manager(|manager| manager.get_value("compaction").cloned());
        let settings = raw
            .map(serde_json::from_value::<crate::compaction_settings_access::CompactionSettings>)
            .transpose()
            .map_err(|error| error.to_string())?;
        let settings = crate::compaction_settings_resolver::resolve_compaction_settings(
            settings.as_ref(),
            Some(crate::compaction_settings_access::CompactionModelSelector {
                provider: &model.provider,
                id: &model.id,
            }),
        )?;
        let window = model.context_window;
        let ratio = match window {
            0..=16_000 => 0.45,
            16_001..=32_000 => 0.5,
            32_001..=64_000 => 0.55,
            64_001..=128_000 => 0.6,
            128_001..=512_000 => 0.7,
            _ => 0.8,
        };
        let mut context = maho_ai::types::Context {
            active_tool_names: None,
            system_prompt: Some(self.system_prompt()),
            messages: Vec::new(),
            tools: None,
        };
        let prompt_tokens = maho_ai::utils::estimate::estimate_context_tokens(&context).tokens;
        context.tools = Some(
            self.agent
                .state()
                .tools()
                .iter()
                .map(|tool| tool.tool.clone())
                .collect(),
        );
        let tools = maho_ai::utils::estimate::estimate_context_tokens(&context)
            .tokens
            .saturating_sub(prompt_tokens);
        let model_id = model.id.to_lowercase();
        let family = |marker: &str| {
            MODEL_FAMILY_PATTERNS
                .iter()
                .any(|(candidate, pattern)| *candidate == marker && pattern.is_match(&model_id))
        };
        let (margin, profile) = if model.provider == "anthropic" || family("claude") {
            (16_384, "anthropic")
        } else if model.provider == "openai"
            || ["gpt-5", "o1", "o3", "o4"]
                .iter()
                .any(|marker| family(marker))
        {
            (16_384, "openai-reasoning")
        } else if model.provider == "google" || family("gemini") {
            (12_288, "google")
        } else if model.provider == "deepseek" || family("deepseek") {
            (12_288, "deepseek")
        } else {
            (8_192, "default")
        };
        let reserve = if settings.enabled {
            let configured =
                u64::try_from(settings.reserve_tokens).map_err(|error| error.to_string())?;
            if settings.reserve_scaling_enabled {
                crate::compaction::compaction::resolve_reserve_tokens(
                    window as f64,
                    configured as f64,
                ) as u64
            } else {
                configured
            }
        } else {
            0
        };
        let lead = if speculation && settings.enabled && settings.speculative_enabled {
            settings
                .speculative_lead_tokens
                .unwrap_or((window as f64 * ratio * 0.125).floor())
                .clamp(8_192.0, 32_768.0) as u64
        } else {
            0
        };
        let overhead = prompt_tokens
            .saturating_add(tools)
            .saturating_add(model.max_tokens.min(window / 2))
            .saturating_add(reserve)
            .saturating_add(lead)
            .saturating_add(margin);
        let required_tokens = overhead.saturating_add(live_context_tokens);
        let keep = u64::try_from(settings.keep_recent_tokens).map_err(|error| error.to_string())?;
        let keep = if window > 409_600 && keep >= 10_000 {
            keep.max((window / 20).min(60_000))
        } else {
            keep
        };
        let keep = keep.min(((window as f64 * (1.0 - ratio - 0.05)).floor() as u64).max(1_024));
        Ok((
            maho_ext_api::ModelBudget {
                context_window: window,
                live_context_tokens,
                required_tokens,
                shortfall_tokens: required_tokens.saturating_sub(window),
                safety_margin_profile: Some(profile.to_owned()),
            },
            settings.enabled && overhead.saturating_add(keep) <= window,
        ))
    }

    pub fn assert_model_usable(
        &self,
        model: &Model,
        live_context_tokens: u64,
    ) -> Result<(), String> {
        if model.context_window == 0 {
            return Ok(());
        }
        let (budget, _) =
            self.model_budget(model, live_context_tokens, live_context_tokens == 0)?;
        if budget.shortfall_tokens == 0 {
            Ok(())
        } else {
            Err(format!(
                "Model \"{}/{}\" cannot {}: context window {} tokens is {} tokens short of the {}-token requirement.",
                model.provider,
                model.id,
                if live_context_tokens == 0 {
                    "start"
                } else {
                    "switch"
                },
                budget.context_window,
                budget.shortfall_tokens,
                budget.required_tokens,
            ))
        }
    }

    pub async fn set_model(&self, model: Model) -> Result<Option<SystemPromptChangeEvent>, String> {
        self.cancel_probe_back();
        self.set_model_internal(model, true, maho_ext_api::ModelSelectSource::Set, true)
            .await
    }

    pub async fn set_session_model(
        &self,
        model: Model,
    ) -> Result<Option<SystemPromptChangeEvent>, String> {
        self.cancel_probe_back();
        self.set_model_internal(model, false, maho_ext_api::ModelSelectSource::Set, true)
            .await
    }

    fn reduce_for_switch_target(&self, model: &Model, live: u64) -> Result<u64, String> {
        let mut entries = self.with_session_manager(|manager| manager.current_branch());
        let measured = self
            .with_session_manager(|manager| manager.current_context())
            .messages
            .iter()
            .map(crate::compaction::compaction::estimate_tokens)
            .sum();
        let (budget, repairable) = self.model_budget(model, measured, false)?;
        if budget.shortfall_tokens == 0 || !repairable {
            return Ok(live);
        }
        let overhead = budget.required_tokens.saturating_sub(measured);
        let previous = entries
            .iter()
            .rposition(|entry| entry["type"] == "compaction");
        let start = previous.map_or(0, |index| {
            entries
                .iter()
                .position(|entry| entry["id"] == entries[index]["firstKeptEntryId"])
                .unwrap_or(index + 1)
        });
        let mut summary = "[Resume recovery checkpoint]\nThe restored conversation was larger than this model's context window, so older context was reduced without any provider request.\nThe complete transcript is still recorded in the session file. Continue from the retained messages and treat omitted details as unknown.".to_owned();
        if let Some(carried) = previous
            .and_then(|index| entries[index]["summary"].as_str())
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            let note = "\n[Earlier checkpoint truncated]";
            let carried = if carried.encode_utf16().count() <= 8_000 {
                carried.to_owned()
            } else {
                let prefix: Vec<_> = carried.encode_utf16().take(8_000 - note.len()).collect();
                format!("{}{note}", String::from_utf16_lossy(&prefix))
            };
            summary.push_str(&format!("\n\nEarlier checkpoint:\n{carried}"));
        }
        let mut target = model.context_window.saturating_sub(overhead);
        let mut last_cut = None;
        while target >= 1_024 {
            let cut = crate::compaction::compaction::find_cut_point(
                &entries,
                start,
                entries.len(),
                target as i64,
            )
            .first_kept_entry_index;
            if last_cut != Some(cut) {
                last_cut = Some(cut);
                if let Some(id) = entries
                    .get(cut)
                    .and_then(|entry| entry["id"].as_str())
                    .map(str::to_owned)
                {
                    let id = id.as_str();
                    // PERF-DIVERGENCE(senpi agent-session.ts): upstream builds a synthetic preview entry per cut; the port deep-cloned the whole entry list for each cut, so we push the preview onto the local list and pop it after. The estimate is identical.
                    let parent_id = entries.last().map(|entry| entry["id"].clone());
                    entries.push(serde_json::json!({"type":"compaction","id":"__senpi_resume_slice_preview__",
                        "parentId":parent_id,"timestamp":"1970-01-01T00:00:00.000Z",
                        "summary":summary,"firstKeptEntryId":id,"tokensBefore":measured,"fromHook":false}));
                    let after: u64 = crate::session_manager::build_session_context(
                        &entries,
                        Some("__senpi_resume_slice_preview__"),
                    )
                    .messages
                    .iter()
                    .map(crate::compaction::compaction::estimate_tokens)
                    .sum();
                    entries.pop();
                    if after.saturating_add(overhead) <= model.context_window {
                        self.with_session_manager_mut(|manager| manager.append_compaction(&summary, id, measured as i64,
                            Some(serde_json::json!({"schema":"senpi.compaction.resume-slice.v1","origin":"resume-admission"})), None, None));
                        let context =
                            self.with_session_manager(|manager| manager.current_context());
                        self.agent.set_messages(
                            context
                                .messages
                                .into_iter()
                                .map(session_message_from_value)
                                .collect::<Result<_, _>>()
                                .map_err(|error| error.to_string())?,
                        );
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

    /// senpi `_recordRejectedModelChange` (#1526): persist the refusal, then emit it.
    fn record_rejected_model_change(
        &self,
        model: Model,
        detail: String,
        budget: Option<maho_ext_api::ModelBudget>,
    ) {
        let reason = if budget.is_some() {
            "context-budget"
        } else {
            "auth"
        };
        let numbers = budget.as_ref().map(|budget| {
            let mut numbers = Map::new();
            numbers.insert(
                "contextWindow".to_owned(),
                Value::from(budget.context_window),
            );
            numbers.insert(
                "liveContextTokens".to_owned(),
                Value::from(budget.live_context_tokens),
            );
            numbers.insert(
                "requiredTokens".to_owned(),
                Value::from(budget.required_tokens),
            );
            numbers.insert(
                "shortfallTokens".to_owned(),
                Value::from(budget.shortfall_tokens),
            );
            if let Some(profile) = &budget.safety_margin_profile {
                numbers.insert(
                    "safetyMarginProfile".to_owned(),
                    Value::from(profile.clone()),
                );
            }
            numbers
        });
        self.with_session_manager_mut(|manager| {
            manager.append_model_change_rejected(
                &model.provider,
                &model.id,
                reason,
                &detail,
                numbers,
            )
        });
        self.emit(AgentSessionEvent::ModelChangeRejected {
            model,
            reason: reason.to_owned(),
            detail,
            budget,
        });
    }

    async fn set_model_internal(
        &self,
        model: Model,
        persist_default: bool,
        source: maho_ext_api::ModelSelectSource,
        allow_deferral: bool,
    ) -> Result<Option<SystemPromptChangeEvent>, String> {
        self.switch_model_with_thinking(model, persist_default, source, allow_deferral, None, None)
            .await
    }

    /// `thinking` is the ephemeral (fallback) level; `favorite_thinking` is a favorite entry's own
    /// level and selection, applied through `_getThinkingForModelSwitch` like upstream's cycle.
    async fn switch_model_with_thinking(
        &self,
        model: Model,
        persist_default: bool,
        source: maho_ext_api::ModelSelectSource,
        allow_deferral: bool,
        thinking: Option<ModelThinkingLevel>,
        favorite_thinking: Option<(Option<ModelThinkingLevel>, Option<ThinkingSelection>)>,
    ) -> Result<Option<SystemPromptChangeEvent>, String> {
        let previous = self.model();
        let context_changed = !models_are_equal(Some(&previous), Some(&model))
            || previous.context_window != model.context_window
            || previous.api != model.api;
        let (current_budget, _) = self.model_budget(&previous, 0, false)?;
        let (target_budget, _) = self.model_budget(&model, 0, false)?;
        let live = if model
            .context_window
            .saturating_sub(target_budget.required_tokens)
            < previous
                .context_window
                .saturating_sub(current_budget.required_tokens)
        {
            let prefix =
                maho_ai::utils::estimate::estimate_context_tokens(&maho_ai::types::Context {
                    active_tool_names: None,
                    system_prompt: Some(self.system_prompt()),
                    messages: Vec::new(),
                    tools: Some(
                        self.agent
                            .state()
                            .tools()
                            .iter()
                            .map(|tool| tool.tool.clone())
                            .collect(),
                    ),
                })
                .tokens;
            self.get_context_usage()
                .and_then(|usage| usage.tokens)
                .unwrap_or(0)
                .saturating_sub(prefix)
        } else {
            0
        };
        let (budget, repairable) = self.model_budget(&model, live, self.messages().is_empty())?;
        let admission = if model.context_window > 0 && budget.shortfall_tokens > 0 && !repairable {
            self.assert_model_usable(&model, live)
        } else {
            Ok(())
        };
        if let Err(detail) = admission {
            self.record_rejected_model_change(model, detail.clone(), Some(budget));
            return Err(detail);
        }
        // senpi `_setModel` (agent-session.ts:5148): refuse only when `checkAuth` finds no auth.
        if !matches!(
            self.model_runtime().check_auth(&model.provider, None).await,
            Ok(Some(_))
        ) {
            let detail = format!("No API key for {}/{}", model.provider, model.id);
            self.record_rejected_model_change(model, detail.clone(), None);
            return Err(detail);
        }
        if allow_deferral && budget.shortfall_tokens > 0 && repairable {
            let notice = format!(
                "{} needs {} fewer tokens than this conversation holds. It is compacted on your next message, and the switch applies after that.",
                model.id, budget.shortfall_tokens
            );
            self.state().pending_model_switch = Some(PendingModelSwitch {
                model: model.clone(),
                budget: budget.clone(),
                persist_default,
                notice: notice.clone(),
            });
            self.emit(AgentSessionEvent::ModelChangePending {
                model,
                budget,
                notice,
            });
            return Ok(None);
        }
        if context_changed {
            self.abort_compaction();
            self.abort_branch_summary();
            self.state().delegated_compaction_key = None;
            self.state().message_revision += 1;
        }
        if matches!(
            source,
            maho_ext_api::ModelSelectSource::Set | maho_ext_api::ModelSelectSource::Cycle
        ) && let Some(controller) = self.retry_fallback.lock().await.as_mut()
        {
            let had_active_fallback = controller.state.is_some();
            controller.clear_for_manual_model_change(&model);
            self.state().spent_accounts = controller.spent_accounts();
            if had_active_fallback {
                self.abort_retry();
            }
        }
        let old_prompt = self.system_prompt();
        let old_thinking = self.thinking_level();
        let old_selection = self.thinking_selection();
        let old_baseline = self.agent.reasoning_baseline();
        let old_tier = self.service_tier();
        let (explicit_level, explicit_selection) = favorite_thinking.unwrap_or((thinking, None));
        let (next_level, next_selection) =
            self.get_thinking_for_model_switch(&model, explicit_level, explicit_selection);
        let previous_abort_server_side_fallback = self.agent.abort_server_side_fallback();
        self.agent.set_model(model.clone());
        self.agent
            .set_abort_server_side_fallback(Some(self.abort_server_side_fallback_enabled()));
        if !(model.id == "gpt-6-astra"
            && matches!(model.provider.as_str(), "openai" | "chatgpt-subscription"))
        {
            self.agent.set_reasoning_baseline(None);
        }
        if thinking.is_some() {
            // `_applyEphemeralThinkingLevel`.
            self.agent.set_thinking_level(next_level);
            self.agent.set_thinking_selection(None);
            if next_level != old_thinking
                && let Some(level) = thinking_level_from_model_level(next_level)
            {
                self.emit(AgentSessionEvent::ThinkingLevelChanged {
                    level,
                    thinking_selection: None,
                });
            }
        } else {
            self.apply_thinking_level(next_level, false, next_selection);
        }
        let restore = |session: &Self, previous: Model, old_prompt: String| {
            session.agent.set_model(previous);
            session.agent.set_system_prompt(old_prompt);
            session.agent.set_thinking_level(old_thinking);
            session.agent.set_thinking_selection(old_selection.clone());
            session.agent.set_reasoning_baseline(old_baseline.clone());
            session
                .agent
                .set_abort_server_side_fallback(previous_abort_server_side_fallback);
        };
        let result = {
            let runner = self.extension_runner.lock().await.clone();
            match runner {
                Some(mut runner) if context_changed => runner
                    .emit_model_select(maho_ext_api::ModelSelectEvent {
                        model: model.clone(),
                        previous_model: Some(previous.clone()),
                        source,
                        system_prompt: old_prompt.clone(),
                        system_prompt_options: self.extension_system_prompt_options(),
                    })
                    .await
                    .map_err(|error| error.to_string()),
                _ => Ok(None),
            }
        };
        let change = match result {
            Ok(result) => {
                let prompt = result
                    .as_ref()
                    .and_then(|result| result.system_prompt.clone())
                    .unwrap_or_else(|| Some(old_prompt.clone()))
                    .unwrap_or_else(|| self.state().base_system_prompt.clone());
                self.agent.set_system_prompt(prompt.clone());
                if prompt != old_prompt {
                    let system_prompt_name = result
                        .as_ref()
                        .and_then(|result| result.system_prompt_name.clone());
                    self.dispatch_extension_event(
                        maho_ext_api::ExtensionEvent::SystemPromptChange {
                            system_prompt: prompt.clone(),
                            previous_system_prompt: old_prompt.clone(),
                            system_prompt_name: system_prompt_name.clone(),
                            model: model.clone(),
                            previous_model: Some(previous.clone()),
                        },
                    )
                    .await;
                    self.emit(AgentSessionEvent::SystemPromptChange {
                        system_prompt: prompt.clone(),
                        previous_system_prompt: old_prompt.clone(),
                        system_prompt_name,
                        model: model.clone(),
                        previous_model: Some(previous.clone()),
                    });
                }
                let (post_hook_budget, post_hook_repairable) =
                    self.model_budget(&model, live, self.messages().is_empty())?;
                if allow_deferral && post_hook_budget.shortfall_tokens > 0 && post_hook_repairable {
                    restore(self, previous, old_prompt);
                    let notice = format!(
                        "{} needs {} fewer tokens than this conversation holds. It is compacted on your next message, and the switch applies after that.",
                        model.id, post_hook_budget.shortfall_tokens
                    );
                    self.state().pending_model_switch = Some(PendingModelSwitch {
                        model: model.clone(),
                        budget: post_hook_budget.clone(),
                        persist_default,
                        notice: notice.clone(),
                    });
                    self.emit(AgentSessionEvent::ModelChangePending {
                        model,
                        budget: post_hook_budget,
                        notice,
                    });
                    return Ok(None);
                }
                let admitted_live = if matches!(
                    source,
                    maho_ext_api::ModelSelectSource::Fallback
                        | maho_ext_api::ModelSelectSource::FallbackRevert
                ) {
                    self.reduce_for_switch_target(&model, live)?
                } else {
                    live
                };
                let admission = self.assert_model_usable(&model, admitted_live);
                if let Err(error) = admission {
                    restore(self, previous, old_prompt);
                    return Err(error);
                }
                (prompt != old_prompt).then(|| SystemPromptChangeEvent {
                    system_prompt: prompt,
                    previous_system_prompt: old_prompt,
                    system_prompt_name: result.and_then(|result| result.system_prompt_name),
                    model: model.clone(),
                    previous_model: Some(previous.clone()),
                })
            }
            Err(error) => {
                restore(self, previous, old_prompt);
                return Err(error);
            }
        };
        let reason = match source {
            maho_ext_api::ModelSelectSource::Fallback => Some("fallback"),
            maho_ext_api::ModelSelectSource::FallbackRevert => Some("fallback-revert"),
            _ => None,
        };
        self.with_session_manager_mut(|manager| {
            manager.append_model_change(
                &model.provider,
                &model.id,
                reason,
                reason.map(|_| (previous.provider.as_str(), previous.id.as_str())),
            )
        });
        if persist_default {
            self.with_settings_manager_mut(|manager| {
                manager.set(
                    crate::settings_manager::SettingsScope::Global,
                    &Map::from_iter([
                        (
                            "defaultProvider".to_owned(),
                            Value::String(model.provider.clone()),
                        ),
                        ("defaultModel".to_owned(), Value::String(model.id.clone())),
                    ]),
                )
            })?;
        }
        self.state().pending_model_switch = None;
        self.state().current_service_tier = resolve_service_tier(
            &model,
            self.scoped_models()
                .iter()
                .find(|entry| models_are_equal(Some(&entry.model), Some(&model)))
                .and_then(|entry| entry.service_tier),
        );
        self.emit(AgentSessionEvent::ModelChanged {
            model: model.clone(),
            thinking_level: thinking_level_from_model_level(self.thinking_level())
                .unwrap_or(ThinkingLevel::Minimal),
            source,
        });
        if old_tier != self.service_tier() {
            self.emit(AgentSessionEvent::ServiceTierChanged {
                tier: self.service_tier(),
                fast_mode: self.is_fast_mode_active(),
            });
        }
        Ok(change)
    }

    pub async fn cycle_model(&self, forward: bool) -> Result<Option<ModelCycleResult>, String> {
        self.cancel_probe_back();
        let models = self.get_current_favorite_models();
        if models.len() <= 1 {
            return Ok(None);
        }
        let current = self.model();
        let index = models
            .iter()
            .position(|entry| models_are_equal(Some(&entry.model), Some(&current)));
        let direction = if forward { "forward" } else { "backward" };
        // `_getDownswitchLiveContextTokens`: only a target with less usable room is charged the live transcript.
        let downswitch_live = |model: &Model| -> Result<u64, String> {
            let (current_budget, _) = self.model_budget(&current, 0, false)?;
            let (target_budget, _) = self.model_budget(model, 0, false)?;
            if model
                .context_window
                .saturating_sub(target_budget.required_tokens)
                >= current
                    .context_window
                    .saturating_sub(current_budget.required_tokens)
            {
                return Ok(0);
            }
            let prefix =
                maho_ai::utils::estimate::estimate_context_tokens(&maho_ai::types::Context {
                    active_tool_names: None,
                    system_prompt: Some(self.system_prompt()),
                    messages: Vec::new(),
                    tools: Some(
                        self.agent
                            .state()
                            .tools()
                            .iter()
                            .map(|tool| tool.tool.clone())
                            .collect(),
                    ),
                })
                .tokens;
            Ok(self
                .get_context_usage()
                .and_then(|usage| usage.tokens)
                .unwrap_or(0)
                .saturating_sub(prefix))
        };
        let stay = |skipped_models: Vec<Model>| ModelCycleResult {
            model: current.clone(),
            thinking_level: thinking_level_from_model_level(self.thinking_level())
                .unwrap_or(ThinkingLevel::Minimal),
            is_scoped: true,
            skipped_models,
            system_prompt_change: None,
        };
        let mut skipped_models = Vec::new();
        for offset in 1..=models.len() {
            let next = match (index, forward) {
                (Some(index), true) => (index + offset) % models.len(),
                (Some(index), false) => {
                    (index + models.len() - offset % models.len()) % models.len()
                }
                (None, true) => offset - 1,
                (None, false) => models.len() - offset,
            };
            let entry = &models[next];
            let model = &entry.model;
            if models_are_equal(Some(model), Some(&current)) {
                continue;
            }
            let live = downswitch_live(model)?;
            let (budget, repairable) = self.model_budget(model, live, false)?;
            if model.context_window > 0 && budget.shortfall_tokens > 0 && !repairable {
                self.emit(AgentSessionEvent::ModelChangeSkipped {
                    model: model.clone(),
                    budget,
                    direction: direction.to_owned(),
                });
                skipped_models.push(model.clone());
                continue;
            }
            // The favorite projection's own level and selection win over remembered preferences.
            let favorite_thinking = (
                entry.thinking_level.map(extension_thinking_level),
                entry.thinking_selection.clone(),
            );
            let system_prompt_change = self
                .switch_model_with_thinking(
                    model.clone(),
                    true,
                    maho_ext_api::ModelSelectSource::Cycle,
                    true,
                    None,
                    Some(favorite_thinking),
                )
                .await?;
            return Ok(Some(ModelCycleResult {
                model: self.model(),
                thinking_level: thinking_level_from_model_level(self.thinking_level())
                    .unwrap_or(ThinkingLevel::Minimal),
                is_scoped: true,
                skipped_models,
                system_prompt_change,
            }));
        }
        // Every candidate was inadmissible. With a single alternative the user asked for THAT model,
        // so the usability error still surfaces (or the switch is held for compaction) instead of a
        // silent no-op; with more alternatives the cycle reports the skipped list and stays put.
        let alternatives: Vec<_> = models
            .iter()
            .filter(|entry| !models_are_equal(Some(&entry.model), Some(&current)))
            .collect();
        if let [only] = alternatives.as_slice() {
            let model = only.model.clone();
            let live = downswitch_live(&model)?;
            let (budget, repairable) = self.model_budget(&model, live, false)?;
            let has_context =
                self.with_session_manager(|manager| !manager.current_context().messages.is_empty());
            if model.context_window > 0 && has_context && budget.shortfall_tokens > 0 && repairable
            {
                let notice = format!(
                    "{} needs {} fewer tokens than this conversation holds. It is compacted on your next message, and the switch applies after that.",
                    model.id, budget.shortfall_tokens
                );
                self.state().pending_model_switch = Some(PendingModelSwitch {
                    model: model.clone(),
                    budget: budget.clone(),
                    persist_default: true,
                    notice: notice.clone(),
                });
                self.emit(AgentSessionEvent::ModelChangePending {
                    model,
                    budget,
                    notice,
                });
            } else if let Err(detail) = self.assert_model_usable(&model, live) {
                self.emit(AgentSessionEvent::ModelChangeRejected {
                    model,
                    reason: "context-budget".to_owned(),
                    detail: detail.clone(),
                    budget: Some(budget),
                });
                return Err(detail);
            }
        }
        Ok(Some(stay(skipped_models)))
    }

    async fn session_before(
        &self,
        event: maho_ext_api::ExtensionEvent,
    ) -> Result<maho_ext_api::SessionBeforeEventResult, String> {
        let mut runner = self.extension_runner.lock().await.clone();
        match runner.as_mut() {
            Some(runner) => match runner
                .emit(event)
                .await
                .map_err(|error| error.to_string())?
            {
                maho_ext_api::EventResult::SessionBefore(result) => Ok(result),
                _ => Ok(Default::default()),
            },
            None => Ok(Default::default()),
        }
    }

    pub(crate) async fn runtime_before_switch(
        &self,
        reason: maho_ext_api::SessionReason,
        path: Option<String>,
    ) -> Result<bool, String> {
        // senpi `emitBeforeSwitch` / `emitBeforeFork`: no handler, no emission. The runner of a session a failed
        // replacement disposed has none left, so the next replacement still proceeds.
        if !self
            .has_extension_handlers(maho_ext_api::EventKind::SessionBeforeSwitch)
            .await
        {
            return Ok(false);
        }
        Ok(self
            .session_before(maho_ext_api::ExtensionEvent::SessionBeforeSwitch {
                reason,
                target_session_file: path,
            })
            .await?
            .cancel
            == Some(true))
    }

    pub async fn runtime_shutdown(&self, reason: maho_ext_api::SessionReason) {
        self.emit_session_shutdown(reason).await;
    }

    pub(crate) async fn runtime_shutdown_to(
        &self,
        reason: maho_ext_api::SessionReason,
        target_session_file: Option<String>,
    ) {
        let mut guard = self.extension_runner.lock().await.clone();
        if let Some(runner) = guard.as_mut() {
            let event = maho_ext_api::SessionShutdownEvent {
                reason,
                target_session_file,
                signal: Some(maho_ext_api::AbortSignal::default()),
            };
            if let Err(error) = Box::pin(runner.emit_session_shutdown_event(event, false)).await {
                self.emit(AgentSessionEvent::ContinuationError {
                    error_message: error.to_string(),
                });
            }
        }
    }

    /// senpi `AgentSessionRuntime.emitAttachmentEvent`: `session_parked` / `session_resumed` reach
    /// only the extensions that registered for them.
    pub(crate) async fn runtime_emit_attachment(&self, event: maho_ext_api::ExtensionEvent) {
        if self.has_extension_handlers(event.kind()).await {
            self.dispatch_extension_event(event).await;
        }
    }

    pub(crate) async fn runtime_before_fork(
        &self,
        entry_id: &str,
        include_entry: bool,
    ) -> Result<bool, String> {
        // senpi `emitBeforeSwitch` / `emitBeforeFork`: no handler, no emission. The runner of a session a failed
        // replacement disposed has none left, so the next replacement still proceeds.
        if !self
            .has_extension_handlers(maho_ext_api::EventKind::SessionBeforeFork)
            .await
        {
            return Ok(false);
        }
        Ok(self
            .session_before(maho_ext_api::ExtensionEvent::SessionBeforeFork {
                entry_id: entry_id.to_owned(),
                position: if include_entry {
                    maho_ext_api::ForkPosition::At
                } else {
                    maho_ext_api::ForkPosition::Before
                },
            })
            .await?
            .cancel
            == Some(true))
    }

    fn rebuild_session_context(&self) -> Result<(), String> {
        let context = self.with_session_manager(|manager| manager.current_context());
        let messages = context
            .messages
            .into_iter()
            .map(session_message_from_value)
            .collect::<Result<_, _>>()
            .map_err(|error| error.to_string())?;
        if let Some((provider, id)) = context.model
            && let Some(model) = self.model_registry.find(&provider, &id)
        {
            self.agent.set_model(model);
        }
        if let Some(level) = ModelThinkingLevel::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == context.thinking_level)
        {
            // Restore the persisted selection with the level so a restore never reads as a new choice.
            let selection: Option<ThinkingSelection> = context
                .thinking_selection
                .and_then(|selection| serde_json::from_value(selection).ok());
            if level == self.thinking_level() {
                self.agent.set_thinking_selection(selection.clone());
            }
            self.apply_thinking_level(level, false, selection);
        }
        self.agent.set_messages(messages);
        self.with_session_manager(|manager| manager.drop_materialized_cache());
        self.release_idle_runtime_messages();
        self.state().message_revision += 1;
        Ok(())
    }
    pub fn admit_sdk_model(&self, resumed: bool) -> Result<(), String> {
        let model = self.model();
        if model.context_window == 0 {
            return Ok(());
        }
        let live = if resumed {
            self.with_session_manager(|manager| manager.current_context())
                .messages
                .iter()
                .map(crate::compaction::compaction::estimate_tokens)
                .sum()
        } else {
            0
        };
        let (budget, reducible) = self.model_budget(&model, live, !resumed)?;
        let admission = if resumed { "resume" } else { "start" };
        if budget.required_tokens <= budget.context_window {
            return Ok(());
        }
        if !resumed || !reducible {
            return Err(format!(
                "Model \"{}/{}\" cannot {}: context window {} tokens is {} tokens short of the {}-token requirement.",
                model.provider,
                model.id,
                admission,
                budget.context_window,
                budget.shortfall_tokens,
                budget.required_tokens,
            ));
        }
        let after = self.reduce_for_switch_target(&model, live)?;
        if after == live {
            self.state().pending_model_switch = Some(PendingModelSwitch {
                model,
                budget,
                persist_default: false,
                notice: "Resume compaction required before the first provider request".into(),
            });
        }
        Ok(())
    }

    async fn finish_session_replacement(
        &self,
        reason: maho_ext_api::SessionReason,
        previous: Option<String>,
    ) -> Result<(), String> {
        self.clear_queue(false);
        *lock(&self.work_obligations) = Some(crate::work_obligations::SessionWorkObligations::new(
            Box::new(SessionCustodyStore(Arc::downgrade(&self.inner))),
        ));
        *lock(&self.transcript_write_failures) = Default::default();
        self.state().pending_model_switch = None;
        self.state().retry_attempt = 0;
        self.reset_hint_tier_state();
        self.rebuild_session_context()?;
        self.renew_extension_runtime(reason).await?;
        self.dispatch_extension_event(maho_ext_api::ExtensionEvent::SessionStart(
            maho_ext_api::SessionStartEvent {
                sizes_only: false,
                reason,
                initial_model_provenance: None,
                previous_session_file: previous,
                attended: None,
            },
        ))
        .await;
        let reason = match reason {
            maho_ext_api::SessionReason::Fork => "fork",
            maho_ext_api::SessionReason::New => "new",
            _ => "resume",
        };
        self.event_bus.emit(
            SESSION_REBIND_CHANNEL,
            &serde_json::json!({"reason": reason}),
        );
        Ok(())
    }

    pub async fn new_session(
        &self,
        options: Option<crate::session_manager::NewSessionOptions>,
    ) -> Result<bool, String> {
        if self
            .session_before(maho_ext_api::ExtensionEvent::SessionBeforeSwitch {
                reason: maho_ext_api::SessionReason::New,
                target_session_file: None,
            })
            .await?
            .cancel
            == Some(true)
        {
            return Ok(false);
        }
        self.ensure_extension_recreation().await?;
        self.abort().await;
        let previous = self.session_file();
        self.emit_session_shutdown(maho_ext_api::SessionReason::New)
            .await;
        self.run_before_session_replace();
        self.invalidate_extension_runtime().await;
        self.with_session_manager_mut(|manager| manager.new_session(options));
        self.finish_session_replacement(maho_ext_api::SessionReason::New, previous)
            .await?;
        Ok(true)
    }

    pub async fn switch_session(&self, path: &str) -> Result<bool, String> {
        self.switch_session_with_cwd(path, None).await
    }

    pub async fn switch_session_with_cwd(
        &self,
        path: &str,
        cwd_override: Option<&str>,
    ) -> Result<bool, String> {
        self.switch_session_with_cwd_coded(path, cwd_override)
            .await
            .map_err(|error| error.message)
    }

    /// `switch_session_with_cwd` keeping senpi's typed `MissingSessionCwdError`.
    pub async fn switch_session_with_cwd_coded(
        &self,
        path: &str,
        cwd_override: Option<&str>,
    ) -> Result<bool, SessionSwitchFailure> {
        let entries = crate::session_manager::load_entries_from_file(path);
        if entries
            .first()
            .and_then(|entry| entry.get("type"))
            .and_then(Value::as_str)
            != Some("session")
        {
            return Err(format!("Invalid session file: {path}").into());
        }
        if self
            .session_before(maho_ext_api::ExtensionEvent::SessionBeforeSwitch {
                reason: maho_ext_api::SessionReason::Resume,
                target_session_file: Some(path.to_owned()),
            })
            .await?
            .cancel
            == Some(true)
        {
            return Ok(false);
        }
        // senpi opens the target for its stored (or overridden) cwd and refuses one that is gone
        // before tearing anything down.
        let session_dir = Path::new(path)
            .parent()
            .map(|parent| parent.to_string_lossy().into_owned());
        let target = crate::session_manager::SessionManager::open(
            path,
            session_dir.as_deref(),
            cwd_override,
            None,
        )?;
        crate::session_cwd::assert_session_cwd_exists(&target, &self.cwd())?;
        self.ensure_extension_recreation().await?;
        self.abort().await;
        let previous = self.session_file();
        let previous_cwd = self.cwd();
        self.emit_session_shutdown(maho_ext_api::SessionReason::Resume)
            .await;
        self.run_before_session_replace();
        self.invalidate_extension_runtime().await;
        // Nothing writes to the replaced manager any more, so it releases its writer claim.
        self.with_session_manager_mut(|manager| std::mem::replace(manager, target).dispose());
        self.state().cwd = self.with_session_manager(|manager| manager.cwd().to_owned());
        let cwd_changed = self.cwd() != previous_cwd;
        if cwd_changed {
            // senpi `createServices` for the target cwd: its project trust is decided before its settings and
            // resources load, so project code loads only when that project is trusted. A session no host
            // installed trust inputs on (an SDK embedding) keeps the trust it was created with.
            let host = lock(&self.switch_project_trust).clone();
            if let Some(host) = host {
                let trusted = self.switch_project_trusted(&previous_cwd, host).await;
                self.with_settings_manager_mut(|manager| {
                    manager.set_trust_from_full_access(trusted.1);
                    manager.set_project_trusted(trusted.0);
                });
            }
            self.rebind_cwd_services();
            // senpi `createRuntime` re-runs the full `DefaultResourceLoader.reload` with the same
            // resource loader options (package, CLI and auto-discovered resources) for the new cwd.
            if self.state().skills_loaded {
                self.state().discovered_resources = maho_ext_api::DiscoveredResources::default();
                self.load_resources().await?;
            }
        }
        self.finish_session_replacement(maho_ext_api::SessionReason::Resume, previous)
            .await?;
        if cwd_changed {
            self.extend_resources_from_extensions(maho_ext_api::SessionReason::Resume)
                .await;
        }
        Ok(true)
    }

    /// Installs the host's project-trust inputs a switch into another project uses.
    pub fn set_switch_project_trust(&self, trust: Option<SwitchProjectTrust>) {
        *lock(&self.switch_project_trust) = trust;
    }

    /// The project trust a switch from `previous_cwd` into the session's (already rebound) cwd starts with,
    /// as `(trusted, trust_from_full_access)`. Same order as `AgentSessionRuntime::replacement_project_trust`
    /// (senpi `createServices`): the same project keeps the live trust (a Full-access-only trust yields to a
    /// saved decision); another project takes the host cache, then `--approve` / `--no-approve`, then Full
    /// access unless saved "do not trust", then no trust-requiring resources or a saved decision, then the
    /// host resolver; still undecided with a UI, it asks with the launch prompt's choices (senpi
    /// `resolveProjectTrusted` `selectProjectTrustOption`). A trust store that cannot be read fails closed.
    async fn switch_project_trusted(
        &self,
        previous_cwd: &str,
        host: SwitchProjectTrust,
    ) -> (bool, bool) {
        use crate::trust_manager::normalize_cwd;
        let cwd = self.cwd();
        let (current_trusted, current_from_full_access, global_preset, default_trust) = self
            .with_settings_manager(|current| {
                (
                    current.is_project_trusted(),
                    current.is_trust_from_full_access() || current.can_restore_full_access_trust(),
                    current.get_global().get("permissionPreset").cloned(),
                    current
                        .get_global()
                        .get("defaultProjectTrust")
                        .and_then(Value::as_str)
                        .and_then(crate::project_trust::DefaultProjectTrust::parse),
                )
            });
        let store = crate::trust_manager::ProjectTrustStore::new(&self.agent_dir());
        let read_saved = || match store.get(&cwd) {
            Ok(saved) => Some(saved),
            Err(error) => {
                self.session_logger.warn(
                    "project_trust_store_unreadable",
                    Some(&Map::from_iter([
                        ("cwd".to_owned(), Value::from(cwd.clone())),
                        ("error".to_owned(), Value::from(error.clone())),
                    ])),
                );
                self.notify_ui(
                    &format!("Project trust store unreadable; not trusting {cwd}: {error}"),
                    maho_ext_api::NotificationType::Warning,
                );
                None
            }
        };
        if normalize_cwd(&cwd) == normalize_cwd(previous_cwd) {
            if !current_from_full_access {
                return (current_trusted, false);
            }
            return match read_saved() {
                Some(Some(false)) | None => (false, false),
                Some(Some(true)) => (current_trusted, false),
                Some(None) => (current_trusted, true),
            };
        }
        let cached = host
            .cache
            .as_ref()
            .and_then(|cache| lock(cache).get(&cwd).copied());
        if let Some(trusted) = cached {
            return (trusted, false);
        }
        if let Some(trusted) = host.trust_override {
            return (trusted, false);
        }
        let preset_flag = self.flag_value("permission-preset");
        let full_access = crate::project_trust::preset_trusts_project(
            preset_flag.as_ref(),
            global_preset.as_ref(),
        );
        let has_resources = crate::trust_manager::has_trust_requiring_project_resources(&cwd, None);
        if !has_resources && !full_access {
            return (true, false);
        }
        let Some(saved) = read_saved() else {
            return (false, false);
        };
        let full_access_trust = full_access && saved != Some(false);
        if full_access_trust || !has_resources {
            return (
                true,
                full_access_trust && has_resources && saved != Some(true),
            );
        }
        // senpi `shouldResolveProjectTrust` does not look at the saved decision: the resolution runs the extension
        // `project_trust` event first, then reads the saved decision, then `defaultProjectTrust`.
        let decided_by_host = match &host.resolver {
            Some(resolver) => {
                let preset = match &preset_flag {
                    Some(FlagValue::String(preset)) => Some(preset.clone()),
                    _ => None,
                };
                // senpi interactive `projectTrustContextFactory`: the handlers get the session's UI and `hasUI: true`.
                let ui = host
                    .prompt
                    .then(|| self.state().extension_ui_context.clone())
                    .flatten();
                let resolved = resolver(cwd.clone(), preset, ui).await;
                for warning in &resolved.warnings {
                    self.notify_ui(warning, maho_ext_api::NotificationType::Warning);
                }
                resolved.decided.then_some(resolved.trusted)
            }
            None => saved.or(match default_trust {
                Some(crate::project_trust::DefaultProjectTrust::Always) => Some(true),
                Some(crate::project_trust::DefaultProjectTrust::Never) => Some(false),
                _ => None,
            }),
        };
        let asked = match decided_by_host {
            Some(trusted) => trusted,
            None if host.prompt => self.ask_project_trust(&cwd, &store).await,
            None => false,
        };
        if let Some(cache) = &host.cache {
            lock(cache).insert(cwd.clone(), asked);
        }
        (asked, false)
    }

    /// senpi `selectProjectTrustOption` + `saveProjectTrustPromptResult` through the session's UI: the launch
    /// prompt's text and choices; a saved choice is written to trust.json, a session-only one only to the
    /// caller's cache. No UI, or a dismissed prompt, is untrusted.
    async fn ask_project_trust(
        &self,
        cwd: &str,
        store: &crate::trust_manager::ProjectTrustStore,
    ) -> bool {
        let Some(ui) = self.state().extension_ui_context.clone() else {
            return false;
        };
        let options = crate::trust_manager::get_project_trust_options(cwd, true);
        let labels: Vec<String> = options.iter().map(|option| option.label.clone()).collect();
        let selected = ui
            .select(
                &crate::project_trust::format_project_trust_prompt(cwd),
                &labels,
                maho_ext_api::ExtensionUiDialogOptions::default(),
            )
            .await;
        let Some(option) =
            selected.and_then(|label| options.into_iter().find(|option| option.label == label))
        else {
            return false;
        };
        if !option.updates.is_empty()
            && let Err(error) = store.set_many(&option.updates)
        {
            self.notify_ui(
                &format!("Saving the project trust decision failed: {error}"),
                maho_ext_api::NotificationType::Warning,
            );
        }
        option.trusted
    }

    /// senpi recreates the cwd-bound services for the target cwd on a switch (agent-session-runtime.ts
    /// `createRuntime({ cwd: sessionManager.getCwd() })`): settings, built-in tools, and the prompt and
    /// skill resources follow the resumed session's cwd (reloaded by the caller through
    /// [`Self::load_resources`]). Extension discovery follows through the runner renewal, whose
    /// context reads the new cwd.
    fn rebind_cwd_services(&self) {
        let cwd = self.cwd();
        // senpi `createRuntime` builds a fresh resource loader for the new cwd.
        {
            let mut state = self.state();
            state.loaded_context_files = None;
            state.resolved_system_prompt = None;
        }
        self.with_settings_manager_mut(|manager| manager.rebind_cwd(&cwd));
        let weak = self.weak_accessor();
        let context: maho_tools::tool_definition_wrapper::ContextFactory = Arc::new(move |_, _| {
            let session = weak().expect("Session retained during tool execution");
            Arc::new(crate::sdk::extension_context::create(&session))
        });
        // senpi `createRuntime` -> `_buildRuntime`: the rebuilt built-ins keep the composed options.
        let options = self.with_settings_manager(|settings| {
            crate::sdk::built_in_tools_options(settings, self.weak_accessor())
        });
        for (name, definition) in
            maho_tools::index::create_all_tool_definitions(Path::new(&cwd), options)
        {
            let tool = maho_tools::tool_definition_wrapper::wrap_tool_definition(
                definition.clone(),
                Some(context.clone()),
                Some(crate::tool_dispatch::tool_dispatcher::tool_run_dispatcher(
                    self.tool_dispatcher(),
                )),
            );
            let mut state = self.state();
            if state.custom_tools.iter().any(|custom| custom.name == name) {
                continue;
            }
            // A built-in an extension currently shadows is restored from its backup on renewal.
            if let Some(Some((entry, backup))) = state.extension_tool_backups.get_mut(&name) {
                entry.definition = definition;
                *backup = tool;
                continue;
            }
            let Some(source_info) = state
                .tool_definitions
                .get(&name)
                .filter(|_| state.base_tool_definitions.contains_key(&name))
                .map(|entry| entry.source_info.clone())
            else {
                continue;
            };
            drop(state);
            self.register_tool_definition(definition, source_info, tool);
        }
        self.set_active_tools_by_name(self.get_active_tool_names());
    }

    pub async fn import_from_jsonl(
        &self,
        input_path: &str,
        cwd_override: Option<&str>,
    ) -> Result<bool, String> {
        self.import_from_jsonl_coded(input_path, cwd_override)
            .await
            .map_err(|error| error.message)
    }

    /// `import_from_jsonl` keeping senpi's typed `MissingSessionCwdError`.
    pub async fn import_from_jsonl_coded(
        &self,
        input_path: &str,
        cwd_override: Option<&str>,
    ) -> Result<bool, SessionSwitchFailure> {
        let base = std::env::current_dir().map_err(|error| error.to_string())?;
        let resolved = crate::paths::resolve_path(
            input_path,
            &base.to_string_lossy(),
            &crate::paths::PathInputOptions::expanding_tilde(crate::config::home_dir()),
        );
        let source = std::path::Path::new(&resolved);
        if !source.exists() {
            return Err(format!("File not found: {resolved}").into());
        }
        let directory = self.with_session_manager(|manager| manager.session_dir().to_owned());
        std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
        let filename = source
            .file_name()
            .ok_or_else(|| "Invalid import filename".to_owned())?;
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
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        }
        self.switch_session_with_cwd_coded(&destination, cwd_override)
            .await
    }

    pub async fn fork(
        &self,
        entry_id: &str,
        include_entry: bool,
    ) -> Result<AssistantEditResult, String> {
        let entry = self
            .with_session_manager(|manager| manager.entry(entry_id))
            .ok_or_else(|| "Invalid entry ID for forking".to_owned())?;
        let position = if include_entry {
            maho_ext_api::ForkPosition::At
        } else {
            maho_ext_api::ForkPosition::Before
        };
        if self
            .session_before(maho_ext_api::ExtensionEvent::SessionBeforeFork {
                entry_id: entry_id.to_owned(),
                position,
            })
            .await?
            .cancel
            == Some(true)
        {
            return Ok(AssistantEditResult {
                cancelled: true,
                ..Default::default()
            });
        }
        self.ensure_extension_recreation().await?;
        self.abort().await;
        let leaf = if include_entry {
            Some(entry_id)
        } else {
            entry.get("parentId").and_then(Value::as_str)
        };
        let entries = self.with_session_manager(|manager| {
            if leaf.is_some() {
                manager.branch(leaf)
            } else {
                Vec::new()
            }
        });
        let previous = self.session_file();
        let forked_from_leaf_id = leaf.map(str::to_owned).or_else(|| {
            entries
                .last()
                .and_then(|e| e.get("id"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
        self.emit_session_shutdown(maho_ext_api::SessionReason::Fork)
            .await;
        self.run_before_session_replace();
        self.invalidate_extension_runtime().await;
        self.with_session_manager_mut(|manager| {
            manager.new_session(Some(crate::session_manager::NewSessionOptions {
                parent_session: previous.clone(),
                forked_from_leaf_id,
                ..Default::default()
            }));
            for entry in entries {
                manager.append_entry_raw(entry);
            }
        });
        self.finish_session_replacement(maho_ext_api::SessionReason::Fork, previous)
            .await?;
        Ok(AssistantEditResult {
            editor_text: (!include_entry)
                .then(|| {
                    entry
                        .get("message")
                        .and_then(|message| {
                            serde_json::from_value::<AgentMessage>(message.clone()).ok()
                        })
                        .map(|message| user_message_text(&message))
                })
                .flatten(),
            ..Default::default()
        })
    }

    pub async fn navigate_tree(
        &self,
        target_id: &str,
        options: TreeNavigationOptions,
    ) -> Result<AssistantEditResult, String> {
        self.navigate_tree_coded(target_id, options)
            .await
            .map_err(|error| error.message)
    }

    /// `navigate_tree` keeping senpi's `SessionStreamingError` / `AssistantEditError` codes
    /// (`streaming`, `stale_leaf`, `not_found`), checked in `_navigateTree` order.
    pub async fn navigate_tree_coded(
        &self,
        target_id: &str,
        options: TreeNavigationOptions,
    ) -> Result<AssistantEditResult, MessageEditFailure> {
        if self.is_streaming() {
            return Err(MessageEditFailure {
                message: crate::edited_assistant_message::SessionStreamingError.to_string(),
                code: Some("streaming"),
            });
        }
        if self.is_compacting() {
            return Err(NAVIGATE_WHILE_COMPACTING.to_owned().into());
        }
        self.with_session_manager(|manager| {
            crate::edited_assistant_message::assert_expected_leaf(
                options.expected_leaf_id.as_deref(),
                crate::lifecycle_edit_leaf::lifecycle_edit_leaf(
                    manager,
                    options.expected_leaf_id.as_deref(),
                )
                .as_deref(),
            )
        })
        .map_err(|error| MessageEditFailure {
            code: Some(error.reason.code()),
            message: error.to_string(),
        })?;
        if !self.with_session_manager(|manager| manager.entry(target_id).is_some()) {
            return Err(MessageEditFailure {
                message: format!("Entry {target_id} not found"),
                code: Some("not_found"),
            });
        }
        self.navigate_tree_internal(target_id, options, None)
            .await
            .map_err(Into::into)
    }

    pub async fn edit_assistant_message(
        &self,
        entry_id: &str,
        text: &str,
        options: TreeNavigationOptions,
    ) -> Result<AssistantEditResult, String> {
        self.edit_assistant_message_coded(entry_id, text, options)
            .await
            .map_err(|error| error.message)
    }

    pub async fn edit_assistant_message_coded(
        &self,
        entry_id: &str,
        text: &str,
        options: TreeNavigationOptions,
    ) -> Result<AssistantEditResult, MessageEditFailure> {
        if self.is_streaming() {
            return Err(MessageEditFailure {
                message: crate::edited_assistant_message::SessionStreamingError.to_string(),
                code: Some("streaming"),
            });
        }
        self.with_session_manager(|manager| {
            crate::edited_assistant_message::assert_expected_leaf(
                options.expected_leaf_id.as_deref(),
                crate::lifecycle_edit_leaf::lifecycle_edit_leaf(
                    manager,
                    options.expected_leaf_id.as_deref(),
                )
                .as_deref(),
            )
        })
        .map_err(|error| MessageEditFailure {
            code: Some(error.reason.code()),
            message: error.to_string(),
        })?;
        let entry = self
            .with_session_manager(|manager| manager.entry(entry_id))
            .ok_or_else(|| MessageEditFailure {
                message: format!("Entry {entry_id} not found"),
                code: Some("not_found"),
            })?;
        if entry["type"] != "message" || entry["message"]["role"] != "assistant" {
            return Err(MessageEditFailure {
                message: format!("Entry {entry_id} is not an assistant message"),
                code: Some("not_assistant"),
            });
        }
        let message: maho_ai::types::AssistantMessage =
            serde_json::from_value(entry["message"].clone()).map_err(|error| error.to_string())?;
        let replacement =
            crate::edited_assistant_message::build_edited_assistant_message(&message, text)
                .map_err(|error| MessageEditFailure {
                    code: Some(error.reason.code()),
                    message: error.to_string(),
                })?;
        if crate::edited_assistant_message::assistant_text_equals(&message, text) {
            return Ok(AssistantEditResult {
                unchanged: Some(true),
                ..Default::default()
            });
        }
        self.navigate_tree_internal(
            entry_id,
            options,
            Some(AgentMessage::Llm(maho_ai::types::Message::Assistant(
                std::sync::Arc::new(replacement),
            ))),
        )
        .await
        .map_err(Into::into)
    }

    pub async fn edit_user_message(
        &self,
        entry_id: &str,
        text: &str,
        options: TreeNavigationOptions,
    ) -> Result<UserEditResult, String> {
        self.edit_user_message_coded(entry_id, text, options)
            .await
            .map_err(|error| error.message)
    }

    pub async fn edit_user_message_coded(
        &self,
        entry_id: &str,
        text: &str,
        options: TreeNavigationOptions,
    ) -> Result<UserEditResult, MessageEditFailure> {
        if self.is_streaming() {
            return Err(MessageEditFailure {
                message: crate::edited_assistant_message::SessionStreamingError.to_string(),
                code: Some("streaming"),
            });
        }
        self.with_session_manager(|manager| {
            crate::edited_user_message::assert_expected_user_leaf(
                options.expected_leaf_id.as_deref(),
                crate::lifecycle_edit_leaf::lifecycle_edit_leaf(
                    manager,
                    options.expected_leaf_id.as_deref(),
                )
                .as_deref(),
            )
        })
        .map_err(|error| MessageEditFailure {
            code: Some(error.reason.code()),
            message: error.to_string(),
        })?;
        let entry = self
            .with_session_manager(|manager| manager.entry(entry_id))
            .ok_or_else(|| MessageEditFailure {
                message: format!("Entry {entry_id} not found"),
                code: Some("not_found"),
            })?;
        if entry["type"] != "message" || entry["message"]["role"] != "user" {
            return Err(MessageEditFailure {
                message: format!("Entry {entry_id} is not a user message"),
                code: Some("not_user"),
            });
        }
        let message: maho_ai::types::UserMessage =
            serde_json::from_value(entry["message"].clone()).map_err(|error| error.to_string())?;
        let replacement = crate::edited_user_message::build_edited_user_message(&message, text)
            .map_err(|error| MessageEditFailure {
                code: Some(error.reason.code()),
                message: error.to_string(),
            })?;
        if crate::edited_user_message::user_text_equals(&message, text) {
            return Ok(UserEditResult {
                unchanged: Some(true),
                ..Default::default()
            });
        }
        self.navigate_tree_internal(
            entry_id,
            options,
            Some(AgentMessage::Llm(maho_ai::types::Message::User(
                replacement,
            ))),
        )
        .await
        .map_err(Into::into)
    }

    async fn navigate_tree_internal(
        &self,
        target_id: &str,
        mut options: TreeNavigationOptions,
        replacement: Option<AgentMessage>,
    ) -> Result<AssistantEditResult, String> {
        if self.is_streaming() {
            return Err(crate::edited_assistant_message::SessionStreamingError.to_string());
        }
        if self.is_compacting() {
            return Err(NAVIGATE_WHILE_COMPACTING.to_owned());
        }
        let old_leaf = self.with_session_manager(|manager| {
            crate::lifecycle_edit_leaf::lifecycle_edit_leaf(
                manager,
                options.expected_leaf_id.as_deref(),
            )
        });
        // Stale tokens fail even for a would-be no-op, before any extension hears about the navigation.
        crate::edited_assistant_message::assert_expected_leaf(
            options.expected_leaf_id.as_deref(),
            old_leaf.as_deref(),
        )
        .map_err(|error| error.to_string())?;
        let entry = self
            .with_session_manager(|manager| manager.entry(target_id))
            .ok_or_else(|| format!("Entry {target_id} not found"))?;
        let collected = self.with_session_manager(|manager| {
            crate::compaction::branch_summarization::collect_entries_for_branch_summary(
                manager,
                old_leaf.as_deref(),
                target_id,
            )
        });
        let controller = maho_ai::utils::abort::AbortController::new();
        let navigation_signal = controller.signal();
        // Only the end of a tree navigation is a lifecycle edge: a record written while it runs would become the leaf
        // under it (senpi#4120). It is session work: a prompt, tick or delivery arriving meanwhile waits for it
        // instead of starting a run on the leaf it is moving (senpi#4154).
        self.set_branch_summary_abort_controller(Some(controller));
        let session_work = self.work_barrier.begin();
        let signal = maho_ext_api::AbortSignal::default();
        let hook_signal = signal.clone();
        let abort_signal = navigation_signal.clone();
        let _bridge = AbortSignalBridge(tokio::spawn(async move {
            abort_signal.cancelled().await;
            hook_signal.abort();
        }));
        let result = async {
            let before = self
                .session_before(maho_ext_api::ExtensionEvent::SessionBeforeTree {
                    preparation: maho_ext_api::TreePreparation {
                        target_id: target_id.to_owned(),
                        old_leaf_id: old_leaf.clone(),
                        common_ancestor_id: collected.common_ancestor_id,
                        entries_to_summarize: collected
                            .entries
                            .iter()
                            .cloned()
                            .map(session_entry_from_value)
                            .collect(),
                        user_wants_summary: options.summarize.unwrap_or(false),
                        custom_instructions: options.custom_instructions.clone(),
                        replace_instructions: options.replace_instructions,
                        label: options.label.clone(),
                    },
                    signal,
                })
                .await?;
            if before.cancel == Some(true) {
                return Ok(AssistantEditResult {
                    cancelled: true,
                    ..Default::default()
                });
            }
            if let Some(instructions) = before.custom_instructions {
                options.custom_instructions = Some(instructions);
            }
            if let Some(replace) = before.replace_instructions {
                options.replace_instructions = Some(replace);
            }
            if let Some(label) = before.label {
                options.label = Some(label);
            }
            let mut summary = if options.summarize == Some(true) {
                before.summary.clone()
            } else {
                None
            };
            let from_extension = summary.is_some();
            if options.summarize == Some(true) && !collected.entries.is_empty() && summary.is_none()
            {
                summary = Some(
                    self.generate_branch_summary_with_signal(
                        &collected.entries,
                        options.custom_instructions.as_deref(),
                        options.replace_instructions.unwrap_or(false),
                        navigation_signal.clone(),
                    )
                    .await?,
                );
                if navigation_signal.aborted()
                    || summary
                        .as_ref()
                        .is_some_and(|summary| summary["aborted"] == true)
                {
                    return Ok(AssistantEditResult {
                        cancelled: true,
                        aborted: Some(true),
                        ..Default::default()
                    });
                }
            }
            if self.with_session_manager(|manager| {
                crate::lifecycle_edit_leaf::lifecycle_edit_leaf(manager, old_leaf.as_deref())
            }) != old_leaf
            {
                return Err("Session leaf changed during tree navigation".to_owned());
            }
            let mut editor_text = None;
            let leaf = if replacement.is_some() {
                entry
                    .get("parentId")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            } else if options.intent == Some(TreeNavigationIntent::Resume) {
                Some(target_id.to_owned())
            } else if entry
                .get("message")
                .and_then(|message| message.get("role"))
                .and_then(Value::as_str)
                == Some("user")
            {
                editor_text = Some(user_message_text(
                    &session_message_from_value(entry["message"].clone())
                        .map_err(|error| error.to_string())?,
                ));
                entry
                    .get("parentId")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            } else {
                Some(target_id.to_owned())
            };
            let summary_text = summary
                .as_ref()
                .and_then(|summary| summary.get("summary").and_then(Value::as_str))
                .filter(|text| !text.is_empty());
            let summary_entry = match summary_text {
                Some(text) => Some(self.with_session_manager_mut(|manager| {
                    manager.branch_with_summary(
                        leaf.as_deref(),
                        text,
                        summary
                            .as_ref()
                            .and_then(|summary| summary.get("details"))
                            .cloned(),
                        summary
                            .as_ref()
                            .and_then(|summary| summary.get("usage"))
                            .cloned(),
                        Some(from_extension),
                    )
                })?),
                None => {
                    self.with_session_manager_mut(|manager| manager.set_leaf(leaf.as_deref()))?;
                    None
                }
            };
            let replacement_entry = replacement
                .map(|message| {
                    serde_json::to_value(message).map(|message| {
                        self.with_session_manager_mut(|manager| manager.append_message(message))
                    })
                })
                .transpose()
                .map_err(|error| error.to_string())?;
            if let Some(label) = options.label.as_deref() {
                let labelled = summary_entry
                    .as_ref()
                    .or(replacement_entry.as_ref())
                    .and_then(|entry| entry.get("id"))
                    .and_then(Value::as_str)
                    .unwrap_or(target_id);
                self.with_session_manager_mut(|manager| {
                    manager.append_label(labelled, Some(label))
                })?;
            }
            if options.intent == Some(TreeNavigationIntent::Resume)
                && replacement_entry.is_none()
                && (summary_entry.is_some() || options.label.is_some())
            {
                self.with_session_manager_mut(|manager| manager.set_leaf(Some(target_id)))?;
            }
            self.rebuild_session_context()?;
            self.dispatch_extension_event(maho_ext_api::ExtensionEvent::SessionTree {
                new_leaf_id: self
                    .with_session_manager(|manager| manager.leaf_id().map(str::to_owned)),
                old_leaf_id: old_leaf,
                summary_entry: summary_entry.clone().map(session_entry_from_value),
                from_extension: Some(from_extension),
            })
            .await;
            if options.intent == Some(TreeNavigationIntent::Resume)
                && self
                    .with_session_manager(|manager| manager.leaf_id().map(str::to_owned))
                    .as_deref()
                    != Some(target_id)
            {
                self.with_session_manager_mut(|manager| manager.set_leaf(Some(target_id)))?;
                self.rebuild_session_context()?;
            }
            Ok(AssistantEditResult {
                editor_text,
                summary_entry,
                entry_id: replacement_entry
                    .as_ref()
                    .and_then(|entry| entry.get("id"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                ..Default::default()
            })
        }
        .await;
        // The navigation's two busy facts end together, then raise the one lifecycle edge (senpi#4154): the
        // controller clears silently while the barrier still holds, and the barrier's release is the edge.
        self.state().branch_summary_abort_controller = None;
        session_work.finish();
        result
    }

    pub fn abort_branch_summary(&self) {
        if let Some(controller) = &self.state().branch_summary_abort_controller {
            controller.abort(None);
        }
    }

    async fn generate_branch_summary_with_signal(
        &self,
        entries: &[Value],
        custom_instructions: Option<&str>,
        replace_instructions: bool,
        signal: maho_ai::utils::abort::AbortSignal,
    ) -> Result<Value, String> {
        use crate::compaction::{branch_summarization, utils};
        let extension_signal = maho_ext_api::AbortSignal::default();
        let hook_signal = extension_signal.clone();
        let provider_signal = signal.clone();
        let _bridge = AbortSignalBridge(tokio::spawn(async move {
            provider_signal.cancelled().await;
            hook_signal.abort();
        }));
        let model = self.model();
        let reserve = self
            .with_settings_manager(|manager| {
                manager
                    .get_value("branchSummary")
                    .and_then(|value| value.get("reserveTokens"))
                    .and_then(Value::as_i64)
            })
            .unwrap_or(16_384);
        let preparation = branch_summarization::prepare_branch_entries(
            entries,
            if model.context_window == 0 {
                128_000
            } else {
                model.context_window as i64
            } - reserve,
        );
        if preparation.messages.is_empty() {
            return Ok(serde_json::json!({"summary":"No content to summarize"}));
        }
        if self
            .has_extension_handlers(maho_ext_api::EventKind::SessionBeforeCompact)
            .await
        {
            let compaction = branch_summarization::create_branch_compaction_preparation(
                entries,
                &preparation,
                reserve,
            );
            let before = self
                .session_before(maho_ext_api::ExtensionEvent::SessionBeforeCompact(
                    maho_ext_api::SessionBeforeCompactEvent {
                        reason: maho_ext_api::CompactionReason::Branch,
                        will_retry: false,
                        request_id: uuid::Uuid::new_v4().to_string(),
                        preparation: maho_ext_api::CompactionPreparation {
                            settings: maho_ext_api::CompactionSettings {
                                enabled: compaction.settings.enabled,
                                reserve_tokens: compaction.settings.reserve_tokens as u64,
                                keep_recent_tokens: compaction.settings.keep_recent_tokens as u64,
                            },
                            messages_to_summarize: compaction
                                .messages_to_summarize
                                .iter()
                                .cloned()
                                .map(serde_json::from_value)
                                .collect::<Result<_, _>>()
                                .map_err(|error| error.to_string())?,
                            turn_prefix_messages: compaction
                                .turn_prefix_messages
                                .iter()
                                .cloned()
                                .map(serde_json::from_value)
                                .collect::<Result<_, _>>()
                                .map_err(|error| error.to_string())?,
                            tokens_before: compaction.tokens_before as u64,
                            first_kept_entry_id: compaction.first_kept_entry_id,
                            previous_summary: compaction.previous_summary,
                        },
                        branch_entries: entries
                            .iter()
                            .cloned()
                            .map(session_entry_from_value)
                            .collect(),
                        custom_instructions: custom_instructions.map(str::to_owned),
                        signal: extension_signal,
                    },
                ))
                .await?;
            if before.cancel == Some(true) {
                return Ok(serde_json::json!({"aborted":true}));
            }
            if let Some(result) = before.compaction {
                return Ok(serde_json::json!({"summary":result.summary}));
            }
        }
        if signal.aborted() {
            return Ok(serde_json::json!({"aborted":true}));
        }
        let instructions = match custom_instructions {
            Some(instructions) if replace_instructions => instructions.to_owned(),
            Some(instructions) => format!(
                "{}\n\nAdditional focus: {instructions}",
                branch_summarization::BRANCH_SUMMARY_PROMPT
            ),
            None => branch_summarization::BRANCH_SUMMARY_PROMPT.to_owned(),
        };
        let transcript =
            utils::serialize_conversation(&crate::messages::convert_to_llm(&preparation.messages));
        let prompt = format!("<conversation>\n{transcript}\n</conversation>\n\n{instructions}");
        let auth = self.get_summarization_request_auth(&model).await?;
        let AgentMessage::Llm(message) = make_user_message(&prompt, None) else {
            return Err("Invalid branch summary prompt".to_owned());
        };
        let callbacks = self.summarization_retry_callbacks("branchSummary", None);
        let response = self
            .complete_summary_stream(
                &auth.model,
                &maho_ai::types::Context {
                    active_tool_names: None,
                    system_prompt: Some(utils::SUMMARIZATION_SYSTEM_PROMPT.to_owned()),
                    messages: vec![message],
                    tools: None,
                },
                Some(maho_ai::types::StreamOptions {
                    request: maho_ai::types::ProviderRequestOptions {
                        api_key: auth.api_key,
                        env: auth.env,
                        signal: Some(signal),
                        headers: auth.headers.map(|headers| {
                            headers
                                .into_iter()
                                .map(|(key, value)| (key, Some(value)))
                                .collect()
                        }),
                        ..Default::default()
                    },
                    extra_body: self
                        .model_runtime()
                        .get_compatibility_request_config(&model)
                        .extra_body,
                    max_tokens: Some(if model.max_tokens == 0 {
                        4096
                    } else {
                        model.max_tokens.min(4096)
                    }),
                    ..Default::default()
                }),
                Some(&callbacks),
                None,
            )
            .await
            .map_err(|error| error.to_string())?;
        if response.stop_reason == maho_ai::types::StopReason::Aborted {
            return Ok(serde_json::json!({"aborted":true}));
        }
        if let Some(error) = crate::compaction::compaction::get_summarization_failure(
            &response,
            "Branch summarization",
        ) {
            return Err(error);
        }
        if response
            .content
            .iter()
            .any(|content| matches!(content, maho_ai::types::ContentBlock::ToolCall(_)))
        {
            return Err("Branch summarization attempted to call a tool".to_owned());
        }
        let (read_files, modified_files) = utils::compute_file_lists(&preparation.file_ops);
        let summary = format!(
            "{}{}{}",
            branch_summarization::BRANCH_SUMMARY_PREAMBLE,
            utils::content_text_for_summary(
                &serde_json::to_value(&response.content).map_err(|error| error.to_string())?,
                "\n"
            ),
            utils::format_file_operations(&read_files, &modified_files)
        );
        Ok(
            serde_json::json!({"summary":summary,"usage":response.usage,"details":{"readFiles":read_files,"modifiedFiles":modified_files}}),
        )
    }

    pub fn set_prompt_resources(
        &self,
        templates: Vec<crate::prompt_templates::PromptTemplate>,
        skills: Vec<crate::skills::Skill>,
    ) {
        self.set_prompt_resources_with_diagnostics(templates, skills, Vec::new());
    }

    /// Replace the session's prompt templates and loaded skills together with the load diagnostics
    /// (the pinned `resourceLoader.getSkills()` result), so [`Self::loaded_skills`] can return them.
    pub fn set_prompt_resources_with_diagnostics(
        &self,
        templates: Vec<crate::prompt_templates::PromptTemplate>,
        skills: Vec<crate::skills::Skill>,
        diagnostics: Vec<crate::diagnostics::ResourceDiagnostic>,
    ) {
        let security_diagnostics = diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.category.as_deref() == Some("skill-link-blocked"))
            .map(|diagnostic| {
                serde_json::json!({
                    "category": "skill-link-blocked",
                    "message": diagnostic.message,
                    "path": diagnostic.path,
                })
            })
            .collect::<Vec<_>>();
        self.set_resource_security_notices(&security_diagnostics);
        let mut state = self.state();
        state.prompt_templates = templates;
        state.skills = skills;
        state.skill_diagnostics = diagnostics;
        state.skills_loaded = true;
    }

    /// The CLI/SDK resource inputs every [`Self::load_resources`] pass reuses.
    pub fn set_resource_loader_inputs(&self, inputs: crate::resource_loader::ResourceLoaderInputs) {
        self.state().resource_loader_inputs = inputs;
    }

    pub fn resource_loader_inputs(&self) -> crate::resource_loader::ResourceLoaderInputs {
        self.state().resource_loader_inputs.clone()
    }

    /// Theme files contributed by extension `resources_discover` handlers (senpi `extendResources`
    /// theme paths), resolved against the session cwd.
    pub fn discovered_theme_paths(&self) -> Vec<String> {
        self.state()
            .discovered_resources
            .theme_paths
            .iter()
            .map(|entry| entry.path.clone())
            .collect()
    }

    /// Prompt-template load diagnostics, including name collisions (pinned `getPrompts().diagnostics`).
    pub fn prompt_diagnostics(&self) -> Vec<crate::diagnostics::ResourceDiagnostic> {
        self.state().prompt_diagnostics.clone()
    }

    /// Theme load diagnostics, including name collisions (pinned `getThemes().diagnostics`). The
    /// theme half of the resource load runs at the CLI startup seam, which records them here.
    pub fn theme_diagnostics(&self) -> Vec<crate::diagnostics::ResourceDiagnostic> {
        self.state().theme_diagnostics.clone()
    }

    pub fn set_theme_diagnostics(&self, diagnostics: Vec<crate::diagnostics::ResourceDiagnostic>) {
        self.state().theme_diagnostics = diagnostics;
    }

    /// senpi `DefaultResourceLoader.reload()` for skills, prompt templates and hook sources: package
    /// and auto-discovered resources plus the stored CLI inputs, loaded into this session.
    pub async fn load_resources(&self) -> Result<(), String> {
        self.load_resources_with_refresh(false).await
    }

    async fn load_resources_with_refresh(&self, refresh: bool) -> Result<(), String> {
        let inputs = self.resource_loader_inputs();
        let settings = self.with_settings_manager(crate::resource_loader::snapshot_settings);
        let loaded = crate::resource_loader::load_resources_with_refresh(
            &self.cwd(),
            &self.agent_dir(),
            settings,
            &inputs,
            refresh,
        )
        .await?;
        {
            let mut state = self.state();
            state.loaded_context_files = None;
            state.resolved_system_prompt = None;
        }
        self.set_prompt_resources_with_diagnostics(
            loaded.prompts,
            loaded.skills.skills,
            loaded.skills.diagnostics,
        );
        self.state().prompt_diagnostics = loaded.prompt_diagnostics;
        self.set_hook_source_paths(loaded.hook_resources, loaded.pre_session_hook_paths);
        Ok(())
    }

    /// The skills this session loaded, with their diagnostics (pinned `resourceLoader.getSkills()`).
    ///
    /// `None` when the session has no loaded skill cache (e.g. `minimalResources`, or before the
    /// first load), so a consumer falls back to loading; otherwise the session's own live skills are
    /// returned instead of a fresh process-wide `load_skills`.
    pub fn loaded_skills(&self) -> Option<crate::skills::LoadSkillsResult> {
        let state = self.state();
        state
            .skills_loaded
            .then(|| crate::skills::LoadSkillsResult {
                skills: state.skills.clone(),
                diagnostics: state.skill_diagnostics.clone(),
            })
    }

    pub fn prompt_templates(&self) -> Vec<crate::prompt_templates::PromptTemplate> {
        self.state().prompt_templates.clone()
    }

    pub async fn generate_session_title_if_needed(&self, prompt: &str) {
        if self.session_name().is_some()
            || crate::session_title_generator::should_skip_session_title(prompt)
        {
            return;
        }
        let controller = maho_ai::utils::abort::AbortController::new();
        {
            let mut state = self.state();
            if state.disposed || state.session_title_abort_controller.is_some() {
                return;
            }
            state.session_title_abort_controller = Some(controller.clone());
        }
        let signal = controller.signal();
        let session_id = self.session_id();
        let generation = async {
            let model = self.model();
            let auth = self.get_summarization_request_auth(&model).await?;
            let context = crate::session_title_generator::build_title_context(prompt);
            let options = maho_ai::types::StreamOptions {
                request: maho_ai::types::ProviderRequestOptions {
                    api_key: auth.api_key,
                    signal: Some(signal.clone()),
                    headers: auth.headers.map(|headers| {
                        headers
                            .into_iter()
                            .map(|(key, value)| (key, Some(value)))
                            .collect()
                    }),
                    stream_kind: Some(maho_ai::types::StreamKind::Auxiliary),
                    env: auth.env,
                    timeout_ms: self.agent.timeout_ms(),
                    max_retry_delay_ms: self.agent.max_retry_delay_ms(),
                    ..Default::default()
                },
                transport: self.agent.transport(),
                max_tokens: Some(64),
                session_id: Some(session_id.clone()),
                cache_retention: Some(
                    if auth.model.cache_retention == Some(maho_ai::types::CacheRetention::None) {
                        maho_ai::types::CacheRetention::None
                    } else {
                        maho_ai::types::CacheRetention::Short
                    },
                ),
                ..Default::default()
            };
            // `sessionTitleRetryPolicy(settingsManager.getRetrySettings())`.
            let retry = self.with_settings_manager(|manager| manager.get_retry_settings());
            let policy = maho_ai::utils::retry::RetryPolicy {
                enabled: retry.enabled,
                max_retries: retry.max_retries.min(1),
                base_delay_ms: retry.base_delay_ms.min(2_000),
                max_agent_delay_ms: None,
                random: Some(self.retry_random.clone()),
            };
            let response = maho_ai::utils::retry::retry_transient_call(
                || async {
                    let response = self
                        .model_runtime()
                        .complete(&auth.model, &context, Some(options.clone()))
                        .await
                        .map_err(|error| error.to_string())?;
                    if let Some(error) =
                        crate::session_title_generator::title_error_message(&response)
                    {
                        return Err(error);
                    }
                    Ok(response)
                },
                |error: &String| maho_ai::utils::retry::is_retryable_error_message(error),
                Some(&policy),
                Some(&signal),
                None,
            )
            .await?;
            Ok::<_, String>(crate::session_title_generator::parse_session_title(
                &response,
            ))
        }
        .await;
        if signal.aborted() {
            return;
        }
        self.state().session_title_abort_controller = None;
        match generation {
            Ok(Some(title)) if self.session_id() == session_id && self.session_name().is_none() => {
                self.set_session_name(&title)
            }
            Ok(_) => {}
            Err(error) => {
                let error = ExtensionError {
                    extension_path: "<runtime>".to_owned(),
                    event: "session_title_generation".to_owned(),
                    error,
                    stack: None,
                };
                // A bound runner forwards to the session error listener through its `on_error` hook.
                let emitted = match self.extension_runner.lock().await.as_mut() {
                    Some(runner) => {
                        runner.emit_error(error.clone());
                        true
                    }
                    None => false,
                };
                let listener = if emitted {
                    None
                } else {
                    self.state().extension_error_listener.clone()
                };
                if let Some(listener) = listener {
                    listener(&error);
                }
            }
        }
    }

    pub async fn native_tool_renderers_snapshot<TState: 'static, TArgs: 'static>(
        &self,
    ) -> BTreeMap<String, Arc<maho_ext_api::ToolRenderers<TState, TArgs>>> {
        self.extension_runner
            .lock()
            .await
            .as_ref()
            .map(|runner| runner.native_tool_renderers_snapshot())
            .unwrap_or_default()
    }

    /// Test seam: hold the real extension-runner lock so a test can prove a path does not await it.
    #[doc(hidden)]
    pub async fn hold_extension_runner_lock_for_test(
        &self,
    ) -> tokio::sync::MutexGuard<'_, Option<ExtensionRunner>> {
        self.extension_runner.lock().await
    }

    /// A cheap key that changes whenever [`Self::native_tool_renderers_snapshot`] could change: a
    /// new runner was installed or its runtime's live tool registrations moved. `None` while no
    /// runner is bound. Callers poll this every frame and rebuild the snapshot only on a change.
    pub fn tool_renderers_revision(&self) -> Option<(u64, u64)> {
        let epoch = self.extension_runner_epoch.load(Ordering::SeqCst);
        let runner = self.extension_runner.try_lock().ok()?;
        runner
            .as_ref()
            .map(|runner| (epoch, runner.runtime.tool_generation()))
    }

    /// The object-safe erased renderer set for every registered tool, keyed by tool name, over the
    /// runner's canonical per-name lookup (`get_erased_tool_renderers`). Used by the interactive card
    /// path to render a registered tool by name without type parameters.
    pub async fn tool_renderers_snapshot(
        &self,
    ) -> BTreeMap<String, Arc<dyn maho_ext_api::ErasedToolRenderers>> {
        let guard = self.extension_runner.lock().await;
        let Some(runner) = guard.as_ref() else {
            return BTreeMap::new();
        };
        let mut snapshot = BTreeMap::new();
        for registered in runner.get_all_registered_tools() {
            let name = registered.definition.name.clone();
            if let Some(renderers) = runner.get_erased_tool_renderers(&name) {
                snapshot.insert(name, renderers);
            }
        }
        snapshot
    }

    pub async fn user_bash_hook(
        &self,
        command: &str,
        exclude_from_context: bool,
    ) -> Result<maho_ext_api::EventResult, String> {
        let Some(mut runner) = self.extension_runner.lock().await.clone() else {
            return Ok(maho_ext_api::EventResult::None);
        };
        runner
            .emit_user_bash(command.to_owned(), exclude_from_context, self.cwd().into())
            .await
            .map_err(|error| error.message)
    }

    pub(crate) async fn replacement_extension_runner(
        &self,
        target: &AgentSession,
    ) -> Result<Option<maho_ext_host::runner::ExtensionRunner>, String> {
        let runner = self.extension_runner.lock().await.clone();
        match runner {
            Some(runner) => runner
                .recreate_with_context(crate::sdk::extension_context::create(target))
                .await
                .map(Some)
                .map_err(|error| error.message),
            None => Ok(None),
        }
    }

    /// senpi `AgentSessionRuntime.reportRemovedExtensions`: the outgoing runner reports, as
    /// `session_extensions_removed`, every extension the replacement runner no longer loads.
    pub(crate) async fn report_removed_extensions(
        &self,
        replacement: &maho_ext_host::runner::ExtensionRunner,
        reason: maho_ext_api::SessionReason,
    ) {
        let Some(mut old) = self.extension_runner.lock().await.clone() else {
            return;
        };
        let paths: BTreeSet<_> = replacement
            .extensions
            .iter()
            .map(|extension| extension.identity.resolved_path.clone())
            .collect();
        let removed: Vec<_> = old
            .extensions
            .iter()
            .filter(|extension| !paths.contains(&extension.identity.resolved_path))
            .map(|extension| extension.identity.clone())
            .collect();
        if !removed.is_empty() {
            old.emit_removed_extensions(reason, removed).await;
        }
    }

    pub async fn execute_user_bash(
        &self,
        command: &str,
        on_chunk: Option<maho_tools::bash_executor::BashChunkCallback>,
        exclude_from_context: bool,
        id: Option<String>,
    ) -> Result<maho_tools::bash_executor::BashResult, String> {
        // A session the host marked restrictive (a role policy that withholds bash) runs no terminal
        // command, client-submitted user bash included. Plain tool exclusions restrict only the model.
        if self.state().user_bash_denied {
            return Err("bash_excluded: this session's role policy withholds bash, so user bash commands are refused".to_owned());
        }
        match self.user_bash_hook(command, exclude_from_context).await? {
            maho_ext_api::EventResult::UserBash {
                result: Some(result),
                ..
            } => {
                self.record_bash_result(command, &result, exclude_from_context);
                Ok(result)
            }
            maho_ext_api::EventResult::UserBash {
                operations,
                result: None,
            } => {
                self.execute_bash(command, on_chunk, exclude_from_context, id, operations)
                    .await
            }
            _ => {
                self.execute_bash(command, on_chunk, exclude_from_context, id, None)
                    .await
            }
        }
    }

    pub async fn execute_bash(
        &self,
        command: &str,
        on_chunk: Option<maho_tools::bash_executor::BashChunkCallback>,
        exclude_from_context: bool,
        id: Option<String>,
        operations: Option<Arc<dyn maho_tools::bash::BashOperations>>,
    ) -> Result<maho_tools::bash_executor::BashResult, String> {
        let signal = maho_ext_api::AbortSignal::default();
        let key = id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        self.state()
            .bash_abort_signals
            .insert(key.clone(), signal.clone());
        // A running user bash is a lifecycle fact: its start and end are edges (senpi#3980).
        self.refresh_lifecycle();
        let prefix = self.with_settings_manager(|manager| manager.get_string("shellCommandPrefix"));
        let shell = self.with_settings_manager(|manager| manager.get_shell_path());
        let local = self.tool_dispatcher.bash_operations(shell.as_deref());
        let resolved = prefix.map_or_else(
            || command.to_owned(),
            |prefix| format!("{prefix}\n{command}"),
        );
        let session = self.clone();
        let cwd = self.cwd();
        let result = maho_tools::bash_executor::execute_bash_with_operations(
            &resolved,
            std::path::Path::new(&cwd),
            operations.as_deref().unwrap_or(local.as_ref()),
            maho_tools::bash_executor::BashExecutorOptions {
                signal,
                on_chunk: Some(Arc::new(move |chunk| {
                    if let Some(callback) = &on_chunk {
                        callback(chunk)?;
                    }
                    session.emit(AgentSessionEvent::BashExecutionUpdate {
                        id: id.clone(),
                        delta: chunk.to_owned(),
                    });
                    Ok(())
                })),
                on_chunk_async: None,
                env: Some(crate::utils::shell::get_shell_env()),
                redact_secrets_in_output: self
                    .with_settings_manager(|manager| manager.get_bash_redact_secrets_in_output()),
            },
        )
        .await
        .map_err(|error| error.to_string());
        self.state().bash_abort_signals.remove(&key);
        self.refresh_lifecycle();
        if let Ok(result) = &result {
            self.record_bash_result(command, result, exclude_from_context);
        }
        result
    }

    pub fn record_bash_result(
        &self,
        command: &str,
        result: &maho_tools::bash_executor::BashResult,
        exclude_from_context: bool,
    ) {
        let message = maho_agent::harness::messages::BashExecutionMessage {
            role: "bashExecution".to_owned(),
            command: command.to_owned(),
            output: result.output.clone(),
            exit_code: result.exit_code.map(i64::from),
            cancelled: result.cancelled,
            truncated: result.truncated,
            full_output_path: result
                .full_output_path
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            timestamp: maho_ai::utils::diagnostics::now_ms(),
            exclude_from_context: Some(exclude_from_context),
        };
        self.state().pending_bash_messages.push(message);
        if !self.is_streaming() {
            self.flush_pending_bash_messages();
        }
    }

    pub fn flush_pending_bash_messages(&self) {
        let pending = std::mem::take(&mut self.state().pending_bash_messages);
        if pending.is_empty() {
            return;
        }
        let mut messages = self.messages();
        for message in pending {
            match serde_json::to_value(&message) {
                Ok(value) => {
                    self.with_session_manager_mut(|manager| manager.append_message(value));
                    self.state().message_revision += 1;
                }
                Err(error) => self.emit(AgentSessionEvent::ContinuationError {
                    error_message: error.to_string(),
                }),
            }
            messages.push(AgentMessage::Custom(
                maho_agent::types::CustomAgentMessage::BashExecution(message),
            ));
        }
        self.agent.set_messages(messages);
    }

    pub fn is_bash_running(&self) -> bool {
        !self.state().bash_abort_signals.is_empty()
    }
    pub fn has_pending_bash_messages(&self) -> bool {
        !self.state().pending_bash_messages.is_empty()
    }
    pub fn abort_bash(&self) {
        for signal in self.state().bash_abort_signals.values() {
            signal.abort();
        }
    }
    pub async fn cleanup_bash_output(&self, path: &std::path::Path) -> Result<(), String> {
        match tokio::fs::remove_file(path).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }

    /// senpi `this._promptSurface ?? resolvePromptSurface(process.env)`.
    fn effective_prompt_surface(&self) -> maho_ext_api::PromptSurface {
        if let Some(surface) = self.state().prompt_surface {
            return surface;
        }
        match crate::dynamic_prompt::resolve_prompt_surface(|name| std::env::var(name).ok()) {
            crate::dynamic_prompt::PromptSurface::Terminal => maho_ext_api::PromptSurface::Terminal,
            crate::dynamic_prompt::PromptSurface::App => maho_ext_api::PromptSurface::App,
            crate::dynamic_prompt::PromptSurface::Chat => maho_ext_api::PromptSurface::Chat,
        }
    }

    /// senpi `setPromptSurface`: rebuilds the prompt for another surface; the system prompt override is dropped
    /// and the tool declarations re-applied only when the effective surface changes.
    pub fn set_prompt_surface(&self, surface: maho_ext_api::PromptSurface) {
        let current = self.effective_prompt_surface();
        self.state().prompt_surface = Some(surface);
        if current == surface {
            return;
        }
        self.state().system_prompt_override = None;
        self.set_active_tools_by_name(self.get_active_tool_names());
    }

    /// senpi `setBrowserEngine`: the next tool call sees it.
    pub fn set_browser_engine(&self, engine: String) {
        self.state().browser_engine = Some(engine);
    }

    /// senpi `browserEngine` getter.
    pub fn browser_engine(&self) -> Option<String> {
        self.state().browser_engine.clone()
    }

    pub fn rebuild_system_prompt(&self) {
        let skills = self.state().skills.clone();
        let active = self.get_active_tool_names();
        let (snippets, guidelines) = self.tool_prompt_contributions(&active);
        let snippets = snippets.into_iter().collect();
        let (custom_prompt, append) = self.resolved_system_prompt();
        let base_prompt = custom_prompt.unwrap_or_else(|| {
            crate::dynamic_prompt::build::build_dynamic_system_prompt(
                &crate::dynamic_prompt::build::BuildDynamicSystemPromptOptions {
                    custom_prompt: None,
                    append_system_prompt: None,
                    on_parts: None,
                    cwd: self.cwd(),
                    selected_tools: active,
                    tool_snippets: snippets,
                    prompt_guidelines: guidelines,
                    context_files: self.project_context_files(),
                    skills,
                    tuning_section: None,
                    core_prompt: None,
                    workstation_dialect: None,
                    surface: match self.effective_prompt_surface() {
                        maho_ext_api::PromptSurface::Terminal => {
                            crate::dynamic_prompt::PromptSurface::Terminal
                        }
                        maho_ext_api::PromptSurface::App => {
                            crate::dynamic_prompt::PromptSurface::App
                        }
                        maho_ext_api::PromptSurface::Chat => {
                            crate::dynamic_prompt::PromptSurface::Chat
                        }
                    },
                },
            )
        });
        let mut base = if append.is_empty() {
            base_prompt
        } else {
            format!("{base_prompt}\n\n{}", append.join("\n\n"))
        };
        if self.is_eval_only_policy_armed() {
            let registered: BTreeSet<String> = {
                let state = self.state();
                state
                    .eval_only_tool_names
                    .clone()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|name| state.tool_registry.contains_key(name))
                    .collect()
            };
            let mut sentences = Vec::new();
            let shell_helpers: Vec<String> = ["bash", "powershell"]
                .into_iter()
                .filter(|name| registered.contains(*name))
                .map(eval_helper_call)
                .collect();
            if !shell_helpers.is_empty() {
                sentences.push(format!("Shell commands run ONLY inside eval cells via {}; hooks and permissions still apply.", shell_helpers.join(" or ")));
            }
            if registered.contains("workflow") {
                sentences.push(format!("The workflow tool runs ONLY inside eval cells via {}; hooks and permissions still apply.", eval_helper_call("workflow")));
            }
            let other_helpers: Vec<String> = registered
                .iter()
                .filter(|name| !["bash", "powershell", "workflow", "grep"].contains(&name.as_str()))
                .map(|name| eval_helper_call(name))
                .collect();
            if !other_helpers.is_empty() {
                sentences.push(format!("These tools run ONLY inside eval cells via {}; hooks and permissions still apply.", other_helpers.join(" or ")));
            }
            if registered.contains("grep") {
                sentences.push(format!("Text search runs ONLY inside eval cells via {}; it returns structured matches, respects .gitignore, and applies hooks and permissions - prefer it over rg/grep in tool.bash.", eval_helper_call("grep")));
            }
            if !sentences.is_empty() {
                base = format!("{base}\n\n{}", sentences.join("\n\n"));
            }
        }
        self.state().base_system_prompt = base.clone();
        let prompt = self.state().system_prompt_override.clone().unwrap_or(base);
        self.agent.set_system_prompt(prompt);
    }

    /// Snippets and guidelines from the active tools plus withheld eval-only tools, which stay
    /// callable as `tool.<name>(...)` so their contributions survive the withholding.
    fn tool_prompt_contributions(
        &self,
        active: &[String],
    ) -> (BTreeMap<String, String>, Vec<String>) {
        let state = self.state();
        let mut contributors = active.to_vec();
        for name in &state.withheld_eval_only_tool_names {
            if state.tool_registry.contains_key(name) && !contributors.contains(name) {
                contributors.push(name.clone());
            }
        }
        let snippets = contributors
            .iter()
            .filter_map(|name| {
                state
                    .tool_prompt_snippets
                    .get(name)
                    .map(|snippet| (name.clone(), snippet.clone()))
            })
            .collect();
        let guidelines = contributors
            .iter()
            .filter_map(|name| state.tool_prompt_guidelines.get(name))
            .flatten()
            .cloned()
            .collect();
        (snippets, guidelines)
    }

    fn extension_system_prompt_options(&self) -> maho_ext_api::BuildSystemPromptOptions {
        let tools = self.get_active_tool_names();
        let (tool_snippets, prompt_guidelines) = self.tool_prompt_contributions(&tools);
        let (custom_prompt, append) = self.resolved_system_prompt();
        maho_ext_api::BuildSystemPromptOptions {
            surface: self.effective_prompt_surface(),
            custom_prompt,
            append_system_prompt: (!append.is_empty()).then(|| append.join("\n\n")),
            context_files: self
                .project_context_files()
                .into_iter()
                .map(|file| maho_ext_api::ContextFile {
                    path: file.path,
                    content: file.content,
                    scope: file.scope,
                })
                .collect(),
            cwd: self.cwd().into(),
            tools,
            tool_snippets,
            prompt_guidelines,
            skills: self
                .state()
                .skills
                .iter()
                .map(|skill| maho_ext_api::Skill {
                    provenance: skill.provenance.as_ref().map(|provenance| {
                        maho_ext_api::SkillProvenance {
                            kind: match provenance.kind {
                                crate::skill_access::SkillProvenanceKind::User => "user",
                                crate::skill_access::SkillProvenanceKind::ProjectAuto => {
                                    "project-auto"
                                }
                                crate::skill_access::SkillProvenanceKind::ProjectSelected => {
                                    "project-selected"
                                }
                            }
                            .into(),
                            selected_root: provenance.selected_root.clone(),
                            project_root: provenance.project_root.clone(),
                        }
                    }),
                    allowed_set: skill.allowed_set.as_ref().map(|allowed| {
                        maho_ext_api::SkillAllowedSet {
                            roots: allowed.roots.clone(),
                            files: allowed.files.clone(),
                            digest: allowed.digest.clone(),
                        }
                    }),
                    name: skill.name.clone(),
                    description: skill.description.clone(),
                    file_path: skill.file_path.clone(),
                    base_dir: skill.base_dir.clone(),
                    disable_model_invocation: skill.disable_model_invocation,
                    source_info: maho_ext_api::SourceInfo {
                        path: skill.source_info.path.clone(),
                        source: skill.source_info.source.clone(),
                        base_dir: skill.source_info.base_dir.clone(),
                        scope: match skill.source_info.scope {
                            crate::source_info::SourceScope::User => {
                                maho_ext_api::SourceScope::User
                            }
                            crate::source_info::SourceScope::Project => {
                                maho_ext_api::SourceScope::Project
                            }
                            crate::source_info::SourceScope::Temporary => {
                                maho_ext_api::SourceScope::Temporary
                            }
                            crate::source_info::SourceScope::System => {
                                maho_ext_api::SourceScope::System
                            }
                        },
                        origin: match skill.source_info.origin {
                            crate::source_info::SourceOrigin::Package => {
                                maho_ext_api::SourceOrigin::Package
                            }
                            crate::source_info::SourceOrigin::TopLevel => {
                                maho_ext_api::SourceOrigin::TopLevel
                            }
                        },
                    },
                })
                .collect(),
        }
    }

    pub async fn reload(&self) -> Result<maho_ext_api::ReloadVetoDecision, String> {
        let _reload = self.reload_admission.lock().await;
        let veto = self.check_reload_veto().await?;
        if veto.cancelled {
            return Ok(veto);
        }
        self.rebuild_runtime_for_reload().await
    }

    /// senpi `applyProjectTrustChange` (senpi#3420): loads or unloads the project's code after the
    /// session's project trust changed (Full access left or re-entered). It rebuilds the runtime exactly
    /// like [`Self::reload`], but nothing can cancel it: no `session_before_reload` handler runs, so a
    /// project extension cannot keep itself loaded. A reload already in flight holds the reload
    /// admission; this one waits for it and then rebuilds under the new trust.
    pub async fn apply_project_trust_change(&self) -> Result<(), String> {
        let _reload = self.reload_admission.lock().await;
        self.rebuild_runtime_for_reload().await.map(|_| ())
    }

    async fn rebuild_runtime_for_reload(&self) -> Result<maho_ext_api::ReloadVetoDecision, String> {
        self.ensure_extension_recreation().await?;
        self.abort().await;
        let previous_flag_values = self
            .extension_runner
            .lock()
            .await
            .as_ref()
            .map(|runner| runner.runtime.flag_values())
            .unwrap_or_default();
        let session_flag_values = self.state().flag_values.clone();
        self.emit_session_shutdown(maho_ext_api::SessionReason::Reload)
            .await;
        self.invalidate_extension_runtime().await;
        let mut trust_epoch = self.with_settings_manager_mut(|manager| {
            manager.reload();
            manager.trust_epoch()
        });
        self.sync_queue_modes_from_settings();
        // `modelRuntime.reloadConfig()`: the registry's runtime shares its config/provider state, so
        // reloading a clone re-reads models.json for the session's live catalog.
        self.model_runtime().clone().reload_config().await;
        self.state().discovered_resources = maho_ext_api::DiscoveredResources::default();
        self.load_resources_with_refresh(true).await?;
        // senpi#3420 trust epoch: a trust change that landed while the resources loaded must not leave
        // the session with what the previous trust allowed; load again until the trust stops moving.
        while self.with_settings_manager(|manager| manager.trust_epoch()) != trust_epoch {
            trust_epoch = self.with_settings_manager_mut(|manager| {
                manager.reload();
                manager.trust_epoch()
            });
            self.load_resources_with_refresh(true).await?;
        }
        self.refresh_eval_only_tool_names();
        self.rebuild_system_prompt();
        self.publish_eval_only_tool_hints();
        let retiring = self.install_session_control();
        let renewed = self
            .renew_extension_runtime(maho_ext_api::SessionReason::Reload)
            .await;
        self.retire_session_control(retiring).await;
        renewed?;
        if let Some(runner) = self.extension_runner.lock().await.as_ref() {
            for (name, value) in previous_flag_values {
                runner.runtime.set_flag(&name, value);
            }
            // A flag set while this reload ran (a permission preset change) reached only the session
            // and the retired runner; it carries over to the fresh one (senpi#2842, #3420).
            let current = self.state().flag_values.clone();
            for (name, value) in current {
                if session_flag_values.get(&name) != Some(&value) {
                    runner.runtime.set_flag(&name, value);
                }
            }
        }
        // Scopes resolve after the runner renewal re-registered extension-native providers.
        let available = self
            .model_runtime()
            .get_available(None)
            .await
            .map_err(|error| error.to_string())?;
        let resolve_scope = |patterns: Vec<String>| {
            crate::model_resolver::resolve_model_scope_from_models(&patterns, &available)
                .scoped_models
                .into_iter()
                .map(|entry| SessionModelEntry {
                    model: entry.model,
                    thinking_level: entry
                        .thinking_level
                        .filter(|level| *level != maho_ai::types::ModelThinkingLevel::Off)
                        .and_then(|level| ThinkingLevel::parse(level.as_str())),
                    thinking_selection: entry.thinking_selection,
                    service_tier: match entry.service_tier.as_deref() {
                        Some("auto") => Some(ServiceTier::Auto),
                        Some("flex") => Some(ServiceTier::Flex),
                        Some("priority") => Some(ServiceTier::Priority),
                        _ => None,
                    },
                })
                .collect::<Vec<_>>()
        };
        let (enabled_patterns, favorite_patterns) = self.with_settings_manager(|manager| {
            let patterns = |key| {
                manager
                    .get_value(key)
                    .and_then(Value::as_array)
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect::<Vec<_>>()
                    })
            };
            (
                patterns("enabledModels"),
                patterns("favoriteModels").unwrap_or_default(),
            )
        });
        self.set_scoped_models(resolve_scope(
            crate::model_resolver::get_model_narrowing_patterns(None, enabled_patterns.as_deref())
                .to_vec(),
        ));
        self.set_favorite_models(resolve_scope(favorite_patterns));
        // senpi emits the reload `session_start` only once a host bound the extensions (UI,
        // command actions, shutdown or error handlers); an unbound session stays silent.
        let has_bindings = {
            let state = self.state();
            state.extension_lifecycle_started
                || state.extension_ui_context.is_some()
                || state.extension_abort_handler.is_some()
                || state.extension_error_listener.is_some()
        };
        if has_bindings {
            let _settlement = self.begin_session_start_settlement();
            self.dispatch_extension_event(maho_ext_api::ExtensionEvent::SessionStart(
                maho_ext_api::SessionStartEvent {
                    sizes_only: false,
                    reason: maho_ext_api::SessionReason::Reload,
                    initial_model_provenance: None,
                    previous_session_file: self.session_file(),
                    attended: None,
                },
            ))
            .await;
            self.extend_resources_from_extensions(maho_ext_api::SessionReason::Reload)
                .await;
        }
        // senpi refreshes the live model when the rebuilt runner replays provider registration.
        // A models.json edit has no provider payload, so reload itself also copies the catalog
        // model back onto the session. Otherwise the footer and the next turn keep the old window.
        self.refresh_current_model_from_registry();
        self.event_bus.emit(
            SESSION_REBIND_CHANNEL,
            &serde_json::json!({"reason": "reload"}),
        );
        Ok(maho_ext_api::ReloadVetoDecision::default())
    }

    /// senpi `checkReloadVeto`: `{ cancelled, reason? }`, the reason only when cancelled.
    pub async fn check_reload_veto(&self) -> Result<maho_ext_api::ReloadVetoDecision, String> {
        let result = self
            .session_before(maho_ext_api::ExtensionEvent::SessionBeforeReload)
            .await?;
        let cancelled = result.cancel == Some(true);
        Ok(maho_ext_api::ReloadVetoDecision {
            cancelled,
            reason: if cancelled { result.reason } else { None },
        })
    }

    fn expand_input(&self, text: &str, templates: bool) -> Result<String, String> {
        let skills = self.state().skills.clone();
        let tokens = crate::skill_invocation::parse_skill_invocation_tokens(
            text,
            &crate::skill_invocation::ParseSkillInvocationOptions {
                known_skill_names: Some(skills.iter().map(|skill| skill.name.clone()).collect()),
            },
        );
        let mut blocks = Vec::new();
        let mut removed = Vec::new();
        let mut metadata = Vec::new();
        let mut seen = BTreeSet::new();
        let emit_error = |extension_path: &str, error: String| {
            let error = ExtensionError {
                extension_path: extension_path.to_owned(),
                event: "skill_expansion".to_owned(),
                error,
                stack: None,
            };
            if let Ok(mut runner) = self.extension_runner.try_lock()
                && let Some(runner) = runner.as_mut()
            {
                runner.emit_error(error.clone());
            }
            let listener = self.state().extension_error_listener.clone();
            if let Some(listener) = listener {
                listener(&error);
            }
        };
        for token in tokens {
            if let Some(skill) = skills.iter().find(|skill| skill.name == token.name) {
                if blocks.len() >= crate::skill_invocation::MAX_SKILL_EXPANSIONS_PER_PROMPT {
                    emit_error(
                        "skill:expansion",
                        format!(
                            "Expanded at most {} skills; remaining skill commands were left as literal text.",
                            crate::skill_invocation::MAX_SKILL_EXPANSIONS_PER_PROMPT
                        ),
                    );
                    break;
                }
                removed.push(token.clone());
                if seen.contains(&skill.name) {
                    emit_error(
                        &skill.file_path,
                        format!("Skipped duplicate skill invocation: {}", skill.name),
                    );
                    continue;
                }
                let source = if skill.provenance.as_ref().is_some_and(|provenance| {
                    provenance.kind != crate::skill_access::SkillProvenanceKind::User
                }) {
                    crate::skill_access::read_skill_file_checked(
                        std::path::Path::new(&skill.file_path),
                        skill.provenance.as_ref(),
                        &skill.allowed_set.clone().unwrap_or_default(),
                    )
                    .map_err(|error| {
                        self.accept_security_notice(&serde_json::json!({
                            "category": "skill-link-blocked",
                            "message": error.message,
                            "path": skill.file_path,
                        }));
                        error.to_string()
                    })
                } else {
                    std::fs::read_to_string(&skill.file_path).map_err(|error| error.to_string())
                };
                let body = match source.and_then(|source| {
                    crate::frontmatter::strip_frontmatter(&source)
                        .map_err(|error| error.to_string())
                }) {
                    Ok(body) => body,
                    Err(error) => {
                        emit_error(&skill.file_path, error);
                        return Ok(text.to_owned());
                    }
                };
                seen.insert(skill.name.clone());
                blocks.push(crate::skill_invocation::SkillInvocationPromptSkill {
                    name: skill.name.clone(),
                    file_path: skill.file_path.clone(),
                    base_dir: skill.base_dir.clone(),
                    body: body.trim().to_owned(),
                });
                metadata.push(maho_ext_api::SkillInvocation {
                    name: skill.name.clone(),
                    path: skill.file_path.clone(),
                    syntax: match token.syntax {
                        crate::skill_invocation::SkillInvocationSyntax::Slash => "slash",
                        crate::skill_invocation::SkillInvocationSyntax::Dollar => "dollar",
                    }
                    .to_owned(),
                });
            } else if token.position == crate::skill_invocation::SkillInvocationPosition::Leading {
                break;
            }
        }
        let expanded = if blocks.is_empty() {
            text.to_owned()
        } else {
            self.emit(AgentSessionEvent::SkillInvocation { skills: metadata });
            crate::skill_invocation::format_skill_invocation_prompt(
                &blocks,
                Some(&crate::skill_invocation::remove_skill_invocation_tokens(
                    text, &removed,
                )),
            )
        };
        if !templates {
            return Ok(expanded);
        }
        let expansion = crate::prompt_templates::expand_prompt_template_with_metadata(
            &expanded,
            &self.prompt_templates(),
        );
        if let Some(template) = expansion.template {
            self.emit(AgentSessionEvent::CommandInvocation { command: serde_json::json!({
            "name":template.name,"source":"prompt","syntax":"slash","path":template.file_path,
        }) });
        }
        Ok(expansion.text)
    }

    async fn try_execute_extension_command(&self, text: &str) -> Result<bool, String> {
        let command_text = text.strip_prefix('/').unwrap_or(text);
        let (name, args) = command_text.split_once(' ').unwrap_or((command_text, ""));
        let mut runner = self.extension_runner.lock().await.clone();
        let Some(runner) = runner.as_mut() else {
            return Ok(false);
        };
        let Some(command) = runner.get_command(name) else {
            return Ok(false);
        };
        // A gated session refuses the command before it runs; the prompt fails with the typed reason.
        let gate = self.state().extension_command_gate.clone();
        if let Some(refusal) = gate.and_then(|gate| gate(name, args, &command.command.source_info))
        {
            return Err(refusal);
        }
        let info = &command.command.source_info;
        self.emit(AgentSessionEvent::CommandInvocation { command: serde_json::json!({"name":name,"source":"extension","syntax":"slash",
            "sourceInfo":{"path":info.path,"source":info.source,"scope":match info.scope {
                maho_ext_api::SourceScope::User => "user", maho_ext_api::SourceScope::Project => "project",
                maho_ext_api::SourceScope::Temporary => "temporary", maho_ext_api::SourceScope::System => "system",
            },"origin":match info.origin { maho_ext_api::SourceOrigin::Package => "package", maho_ext_api::SourceOrigin::TopLevel => "top-level" },
                "baseDir":info.base_dir}
        }) });
        let context = runner
            .create_command_context(Arc::new(SessionExtensionActions(Arc::downgrade(
                &self.inner,
            ))))
            .map_err(|error| error.to_string())?;
        if let Err(error) = runner.invoke_command(name, args, &context).await {
            let error = ExtensionError {
                extension_path: format!("command:{name}"),
                event: "command".to_owned(),
                error: error.message,
                stack: error.stack,
            };
            runner.emit_error(error);
        }
        Ok(true)
    }

    pub(crate) async fn run_replacement_callbacks(
        &self,
        setup: Option<maho_ext_api::SessionSetup>,
        callback: Option<maho_ext_api::WithSession>,
    ) -> Result<(), maho_ext_api::ExtensionFailure> {
        if setup.is_none() && callback.is_none() {
            return Ok(());
        }
        let mut runner =
            self.extension_runner.lock().await.clone().ok_or_else(|| {
                maho_ext_api::ExtensionFailure::new("Extension runner is not bound")
            })?;
        let actions = Arc::new(SessionExtensionActions(Arc::downgrade(&self.inner)));
        let mut base = runner.create_context()?;
        base.session_manager = Arc::new(ExtensionSessionManagerView {
            session: Arc::downgrade(&self.inner),
            id: self.session_id(),
            file: self.session_file().map(Into::into),
        });
        runner.bind_core(actions.clone(), base);
        runner.bind_context_actions(actions.clone())?;
        let context = runner.create_command_context(actions.clone())?;
        if let Some(setup) = setup {
            setup(context.session_manager.as_ref()).await?;
            self.rebuild_session_context()
                .map_err(maho_ext_api::ExtensionFailure::new)?;
        }
        if let Some(callback) = callback {
            callback(&maho_ext_api::ReplacedSessionContext {
                context,
                message_actions: actions,
            })
            .await?;
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
                if error
                    .message
                    .contains("authHeader requires a resolved API key")
                {
                    return Err(crate::auth_guidance::format_no_api_key_found_message(
                        &model.provider,
                    ));
                }
                return Err(error.message);
            }
        };
        if let Some(result) = result
            && (result.ambient || result.auth.api_key.is_some() || result.auth.headers.is_some())
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
                extra_body: self
                    .model_runtime()
                    .get_compatibility_request_config(model)
                    .extra_body,
                env: result.env,
            });
        }
        if self.model_runtime().is_using_oauth(&model.provider) {
            return Err(format!(
                "Authentication failed for \"{}\". Credentials may have expired or network is unavailable. Run '/login {}' to re-authenticate.",
                model.provider, model.provider
            ));
        }
        Err(crate::auth_guidance::format_no_api_key_found_message(
            &model.provider,
        ))
    }

    /// senpi `completeSummarization` over the session's `agent.streamFunction`;
    /// `summarization_max_duration_ms_override` is senpi's `summarizationMaxDurationMsOverride`.
    async fn complete_summary_stream(
        &self,
        model: &Model,
        context: &maho_ai::types::Context,
        options: Option<maho_ai::types::StreamOptions>,
        callbacks: Option<&maho_ai::utils::retry::RetryCallbacks<'_>>,
        summarization_max_duration_ms_override: Option<f64>,
    ) -> Result<maho_ai::types::AssistantMessage, maho_ai::utils::event_stream::StreamError> {
        use crate::compaction::stream_watchdog;
        let mut options = options.unwrap_or_default();
        let override_ms = summarization_max_duration_ms_override;
        let tokens = maho_ai::utils::estimate::estimate_context_tokens(context).tokens;
        let duration = std::time::Duration::from_secs_f64(
            stream_watchdog::summarization_max_duration_ms(tokens as f64, override_ms) / 1000.0,
        );
        let duration_ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
        let caller = options.request.signal.clone();
        options.request.affinity_session_id = options
            .request
            .affinity_session_id
            .clone()
            .or_else(|| options.session_id.clone());
        options.session_id = Some(uuid::Uuid::new_v4().to_string());
        options
            .cache_retention
            .get_or_insert(maho_ai::types::CacheRetention::None);
        let policy = self.summarization_retry_policy();
        let stream_fn = self.agent.stream_function();
        // senpi `completeSummarization`: error-terminated responses retry through `retryAssistantCall`
        // and the final error response is returned, so the caller labels the failure.
        let failure: Mutex<Option<String>> = Mutex::new(None);
        let failure_slot = &failure;
        let response = maho_ai::utils::retry::retry_assistant_call(
            || {
                let mut options = options.clone();
                let caller = caller.clone();
                let stream_fn = stream_fn.clone();
                async move {
                    let controller = maho_ai::utils::abort::AbortController::new();
                    let listener = caller.as_ref().map(|signal| {
                        let request = controller.clone();
                        let listener = signal
                            .add_abort_listener(move |reason| request.abort(Some(reason.clone())));
                        if signal.aborted() {
                            controller.abort(signal.reason());
                        }
                        listener
                    });
                    options.request.signal = Some(controller.signal());
                    let stream = stream_fn(
                        model,
                        context,
                        Some(maho_agent::types::AgentStreamOptions {
                            simple: maho_ai::types::SimpleStreamOptions {
                                stream: options,
                                ..Default::default()
                            },
                            ..Default::default()
                        }),
                    );
                    let result = stream_watchdog::consume_stream_with_idle_timeout(
                        &stream,
                        u64::try_from(stream_watchdog::DEFAULT_SUMMARIZATION_IDLE_TIMEOUT_MS)
                            .unwrap_or_default(),
                        Some(duration_ms),
                        caller.as_ref(),
                        || controller.abort(None),
                    )
                    .await;
                    if let (Some(signal), Some(listener)) = (caller, listener) {
                        signal.remove_abort_listener(listener);
                    }
                    match result {
                        Ok(response) => response,
                        Err(error) => {
                            let error = error.to_string();
                            let mut message =
                                maho_ai::utils::lazy::setup_error_message(model, &error);
                            // Aborted stops the retry loop; the recorded failure is returned below.
                            message.stop_reason = maho_ai::types::StopReason::Aborted;
                            *lock(failure_slot) = Some(error);
                            message
                        }
                    }
                }
            },
            Some(&policy),
            caller.as_ref(),
            callbacks,
        )
        .await;
        if let Some(error) = lock(&failure).take() {
            return Err(maho_ai::utils::event_stream::StreamError::new(error));
        }
        Ok(response)
    }

    /// `settingsManager.getRetrySettings()` as the summarization retry policy.
    fn summarization_retry_policy(&self) -> maho_ai::utils::retry::RetryPolicy {
        let retry = self.with_settings_manager(|manager| manager.get_retry_settings());
        maho_ai::utils::retry::RetryPolicy {
            random: Some(self.retry_random.clone()),
            ..retry
        }
    }

    /// senpi `_summarizationRetryCallbacks`: surface summarization retries as session events.
    fn summarization_retry_callbacks(
        &self,
        source: &'static str,
        reason: Option<maho_ext_api::CompactionReason>,
    ) -> maho_ai::utils::retry::RetryCallbacks<'static> {
        let scheduled = self.clone();
        let attempt_start = self.clone();
        let finished = self.clone();
        maho_ai::utils::retry::RetryCallbacks {
            on_retry_scheduled: Some(Box::new(
                move |attempt, max_attempts, delay_ms, error_message| {
                    scheduled.emit(AgentSessionEvent::SummarizationRetryScheduled {
                        attempt,
                        max_attempts,
                        delay_ms,
                        error_message: error_message.to_owned(),
                    });
                },
            )),
            on_retry_attempt_start: Some(Box::new(move || {
                attempt_start.emit(AgentSessionEvent::SummarizationRetryAttemptStart {
                    source: source.to_owned(),
                    reason,
                });
            })),
            on_retry_finished: Some(Box::new(move |_, _, _| {
                finished.emit(AgentSessionEvent::SummarizationRetryFinished)
            })),
        }
    }

    /// senpi `_runDefaultCompaction`: generate the built-in summary, reusing the exact provider
    /// context prefix (experimental) so the summary request can hit the prompt cache.
    async fn run_default_compaction(
        &self,
        preparation: &crate::compaction::compaction::CompactionPreparation,
        auth: RequestAuth,
        instructions: Option<&str>,
        signal: maho_ai::utils::abort::AbortSignal,
        reason: maho_ext_api::CompactionReason,
    ) -> Result<crate::compaction::compaction::CompactionResult, String> {
        use crate::compaction::compaction as library;
        let to_agent_messages = |messages: &[Value]| {
            messages
                .iter()
                .cloned()
                .map(session_message_from_value)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())
        };
        let mut cache_friendly = None;
        if crate::experimental::are_experimental_features_enabled() {
            let state = self.agent.state();
            let system_prompt = self.agent.system_prompt();
            let tools = state.tools().to_vec();
            let build_source_context = |messages: Vec<AgentMessage>| {
                let context = maho_agent::types::AgentContext {
                    declared_tools: None,
                    system_prompt: system_prompt.clone(),
                    messages,
                    tools: Some(tools.clone()),
                };
                let signal = signal.clone();
                async move {
                    self.agent
                        .build_provider_context(&context, Some(signal))
                        .await
                }
            };
            let full_context = build_source_context(state.messages().to_vec()).await;
            let is_exact_prefix = |candidate: &maho_ai::types::Context| {
                !candidate.messages.is_empty()
                    && candidate.messages.len() <= full_context.messages.len()
                    && candidate.messages[..] == full_context.messages[..candidate.messages.len()]
            };
            let mut source_context = None;
            if !preparation.messages_to_summarize.is_empty()
                && !preparation.source_messages.is_empty()
            {
                let candidate =
                    build_source_context(to_agent_messages(&preparation.source_messages)?).await;
                if is_exact_prefix(&candidate) {
                    source_context = Some(candidate);
                }
            }
            let mut turn_prefix_source_context = None;
            if preparation.is_split_turn
                && !preparation.turn_prefix_messages.is_empty()
                && !preparation.turn_prefix_source_messages.is_empty()
            {
                let candidate = build_source_context(to_agent_messages(
                    &preparation.turn_prefix_source_messages,
                )?)
                .await;
                let provider_turn_prefix = (self.agent.convert_to_llm())(to_agent_messages(
                    &preparation.turn_prefix_messages,
                )?)
                .await;
                if is_exact_prefix(&candidate)
                    && !provider_turn_prefix.is_empty()
                    && provider_turn_prefix.len() <= candidate.messages.len()
                    && candidate.messages[candidate.messages.len() - provider_turn_prefix.len()..]
                        == provider_turn_prefix[..]
                {
                    turn_prefix_source_context = Some(candidate);
                }
            }
            if source_context.is_some() || turn_prefix_source_context.is_some() {
                cache_friendly = Some(library::CacheFriendlySummaryOptions {
                    source_context,
                    turn_prefix_source_context,
                    request_options: Some(library::SummaryRequestOptions {
                        session_id: self.agent.session_id(),
                        on_payload: self.agent.on_payload(),
                        on_response: self.agent.on_response(),
                        transport: self.agent.transport(),
                        thinking_budgets: self.agent.thinking_budgets(),
                        max_retry_delay_ms: self.agent.max_retry_delay_ms(),
                    }),
                });
            }
        }
        let transform_context = self.agent.transform_context().map(|transform| -> library::SummaryTransformContext {
            Arc::new(move |messages: Vec<Value>, signal: Option<maho_ai::utils::abort::AbortSignal>|
                -> maho_ai::types::BoxFuture<'static, Result<Vec<Value>, String>>
            {
                let transform = transform.clone();
                Box::pin(async move {
                    let messages = messages.into_iter().map(session_message_from_value).collect::<Result<Vec<_>, _>>()
                        .map_err(|error| error.to_string())?;
                    let transformed = transform(messages, signal).await;
                    transformed.iter().map(session_message_to_value).collect::<Result<Vec<_>, _>>()
                        .map_err(|error| error.to_string())
                })
            })
        });
        // senpi passes `this.agent.streamFunction` (the SDK's settings-aware stream function).
        let agent_stream_fn = self.agent.stream_function();
        let stream_fn =
            move |model: &Model,
                  context: &maho_ai::types::Context,
                  options: maho_ai::types::SimpleStreamOptions| {
                agent_stream_fn(
                    model,
                    context,
                    Some(maho_agent::types::AgentStreamOptions {
                        simple: options,
                        ..Default::default()
                    }),
                )
            };
        let retry = self.summarization_retry_policy();
        let callbacks = self.summarization_retry_callbacks("compaction", Some(reason));
        let request = library::SummarizationRequest {
            model: &auth.model,
            api_key: auth.api_key.clone(),
            headers: auth.headers.clone(),
            env: auth.env.clone(),
            signal: Some(signal.clone()),
            extra_body: auth.extra_body.clone(),
            thinking_level: Some(self.thinking_level()),
            stream_fn: Some(&stream_fn as &library::SummarizationStreamFn<'_>),
            transform_context,
            retry: Some(&retry),
            callbacks: Some(&callbacks),
            session_id: Some(self.session_id()),
            summarization_max_duration_ms: None,
        };
        library::compact(preparation, instructions, cache_friendly.as_ref(), &request).await
    }

    /// Resolve optional auth for a summarization stream.
    ///
    /// Adaptations: senpi's ambient-credential refinement reads `agent.getApiKey` and
    /// `AuthResolution.source`; the Rust agent exposes no `getApiKey` getter and `AuthResolution`
    /// carries no `source`, so the stored resolution is used as-is.
    pub async fn get_summarization_request_auth(
        &self,
        model: &Model,
    ) -> Result<SummarizationRequestAuth, String> {
        if self.state().uses_default_stream_function {
            let auth = self.get_required_request_auth(model).await?;
            return Ok(SummarizationRequestAuth {
                model: auth.model,
                api_key: auth.api_key,
                headers: auth.headers,
                env: auth.env,
            });
        }
        let auth = self.model_runtime().get_auth(&model.provider).await;
        if let Err(error) = &auth
            && error.code == maho_ai::models::ModelsErrorCode::ConsentRequired
        {
            return Err(error.message.clone());
        }
        let Ok(Some(result)) = auth else {
            return Ok(SummarizationRequestAuth {
                model: model.clone(),
                api_key: None,
                headers: None,
                env: None,
            });
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
            extra_body: self
                .model_runtime()
                .get_compatibility_request_config(model)
                .extra_body,
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
        let result = f(&lock(&self.session_manager));
        self.emit_pending_session_recoveries();
        result
    }

    /// senpi `getSessionContextEntryId` for each live message: a message carries the id of the
    /// session entry it was projected from while it still sits at its projected position in the
    /// rebuilt session context. Messages not projected from session history stay unstamped.
    fn session_context_entry_ids(&self, messages: &[AgentMessage]) -> Vec<Option<String>> {
        let context = self.with_session_manager(|manager| manager.current_context());
        let projected = context
            .messages
            .into_iter()
            .map(|message| session_message_from_value(message).ok());
        let mut entry_ids: Vec<Option<String>> = messages
            .iter()
            .zip(projected.zip(context.entry_ids))
            .map(|(message, (projected, entry_id))| {
                (projected.as_ref() == Some(message)).then_some(entry_id)
            })
            .collect();
        entry_ids.resize(messages.len(), None);
        entry_ids
    }

    pub fn with_session_manager_mut<T>(&self, f: impl FnOnce(&mut SessionManager) -> T) -> T {
        let result = f(&mut lock(&self.session_manager));
        self.emit_pending_session_recoveries();
        result
    }

    pub fn with_settings_manager<T>(&self, f: impl FnOnce(&SettingsManager) -> T) -> T {
        f(&lock(&self.settings_manager))
    }

    pub(crate) fn shared_settings_manager(&self) -> Arc<Mutex<SettingsManager>> {
        self.settings_manager.clone()
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

    pub fn export_to_html(
        &self,
        output_path: Option<&str>,
        theme_name: Option<&str>,
    ) -> Result<String, String> {
        let configured = theme_name
            .map(str::to_owned)
            .or_else(|| self.with_settings_manager(|manager| manager.get_string("theme")));
        let resolved = lock(&self.inner.html_theme_resolver)
            .clone()
            .map(|resolver| resolver(configured.as_deref()))
            .transpose()?;
        match resolved {
            Some((render_theme, colors)) => {
                self.export_to_html_with_theme(output_path, theme_name, render_theme, colors)
            }
            None => self.export_to_html_with_theme(
                output_path,
                theme_name,
                maho_ext_api::Theme::default(),
                None,
            ),
        }
    }

    pub fn set_html_theme_resolver(&self, resolver: HtmlThemeResolver) {
        *lock(&self.inner.html_theme_resolver) = Some(resolver);
    }

    /// Installs the renderers of the built-in tool definitions for HTML export.
    pub fn set_html_built_in_tool_renderers(
        &self,
        lookup: crate::export_html::tool_renderer::ToolRendererLookup,
    ) {
        *lock(&self.inner.html_built_in_tool_renderers) = Some(lookup);
    }

    pub fn export_to_html_with_theme(
        &self,
        output_path: Option<&str>,
        theme_name: Option<&str>,
        render_theme: maho_ext_api::Theme,
        colors: Option<(
            std::collections::BTreeMap<String, String>,
            std::collections::BTreeMap<String, String>,
        )>,
    ) -> Result<String, String> {
        // Pinned: the explicit theme wins, else the configured theme.
        let theme = theme_name
            .map(str::to_owned)
            .or_else(|| self.with_settings_manager(|manager| manager.get_string("theme")));
        let tools = serde_json::Value::Array(self.agent.state().tools().iter().map(|tool| serde_json::json!({
            "name": tool.tool.name, "description": tool.tool.description, "parameters": tool.tool.parameters,
        })).collect());
        let runner = self
            .extension_runner
            .try_lock()
            .map_err(|error| error.to_string())?
            .clone();
        let renderers = runner
            .as_ref()
            .map(|runner| {
                runner
                    .get_all_registered_tools()
                    .into_iter()
                    .filter_map(|tool| {
                        runner
                            .get_erased_tool_renderers(&tool.definition.name)
                            .map(|renderers| (tool.definition.name, renderers))
                    })
                    .collect::<std::collections::HashMap<_, _>>()
            })
            .unwrap_or_default();
        let built_in = lock(&self.inner.html_built_in_tool_renderers).clone();
        let mut renderer = crate::export_html::tool_renderer::ToolHtmlRenderer::new(
            crate::export_html::tool_renderer::ToolHtmlRendererDeps {
                get_tool_renderer: Arc::new(move |name| {
                    renderers
                        .get(name)
                        .cloned()
                        .or_else(|| built_in.as_ref().and_then(|built_in| built_in(name)))
                }),
                theme: render_theme,
                cwd: PathBuf::from(self.cwd()),
                width: None,
            },
        );
        self.with_session_manager(|manager| {
            let file = manager
                .session_file()
                .ok_or_else(|| "Cannot export in-memory session to HTML".to_owned())?;
            if !std::path::Path::new(file).exists() {
                return Err("Nothing to export yet - start a conversation first".to_owned());
            }
            let default_output = format!(
                "{}-session-{}.html",
                crate::config::app_name(),
                std::path::Path::new(file).file_stem().map_or_else(
                    || "session".into(),
                    |stem| stem.to_string_lossy().into_owned()
                )
            );
            let entries = manager.entries();
            let rendered =
                crate::export_html::index::pre_render_custom_tools(&entries, &mut renderer);
            if let Some((colors, export_colors)) = colors {
                return crate::export_html::index::export_session_to_html_with_theme(
                    manager,
                    Some(output_path.unwrap_or(&default_output)),
                    theme.as_deref(),
                    (Some(self.system_prompt()), Some(tools), Some(rendered)),
                    (colors, export_colors),
                );
            }
            crate::export_html::index::export_session_data_to_html(
                manager.header().unwrap_or(serde_json::Value::Null),
                entries,
                manager.leaf_id().map(str::to_owned),
                Some(output_path.unwrap_or(&default_output)),
                theme.as_deref(),
                (Some(self.system_prompt()), Some(tools), Some(rendered)),
            )
        })
    }

    pub fn session_name(&self) -> Option<String> {
        self.with_session_manager(|manager| manager.session_name().map(str::to_owned))
    }

    /// A copy of the agent transcript; the TS getter hands out the live array.
    pub fn messages(&self) -> Vec<AgentMessage> {
        self.runtime_messages()
    }

    /// Transcript length; materialization never changes the count, so no copy is needed.
    pub fn message_count(&self) -> usize {
        self.agent.message_count()
    }

    pub fn state_snapshot(&self) -> AgentState {
        // PERF-DIVERGENCE(senpi agent-session.ts): upstream reads the live array; the port used to deep-clone the whole transcript via runtime_messages() and discard it, so we only re-hydrate resident tokens when pending. The returned state is identical.
        if self.agent_run_active.load(Ordering::SeqCst) {
            self.hydrate_runtime_messages_for_run();
            return self.agent.state();
        }
        let mut snapshot = self.agent.state();
        if self.runtime_messages_tokenized.load(Ordering::SeqCst) {
            snapshot.set_messages(self.materialize_resident_messages(snapshot.messages().to_vec()));
        }
        snapshot
    }

    fn hydrate_runtime_messages_for_run(&self) {
        if self
            .runtime_messages_tokenized
            .swap(false, Ordering::SeqCst)
        {
            let messages =
                self.materialize_resident_messages(self.agent.state().messages().to_vec());
            self.agent.set_messages(messages);
        }
    }

    /// senpi `_runtimeMessages()`: the agent transcript with resident tokens hydrated. The idle
    /// release tokenizes the runtime messages in place; every reader that can run between two
    /// turns goes through here so a sentinel never reaches a consumer.
    fn runtime_messages(&self) -> Vec<AgentMessage> {
        if !self.agent_run_active.load(Ordering::SeqCst) {
            if self.runtime_messages_tokenized.load(Ordering::SeqCst) {
                return self.materialize_resident_messages(self.agent.state().messages().to_vec());
            }
            return self.agent.state().messages().to_vec();
        }
        if self
            .runtime_messages_tokenized
            .swap(false, Ordering::SeqCst)
        {
            let messages =
                self.materialize_resident_messages(self.agent.state().messages().to_vec());
            self.agent.set_messages(messages.clone());
            return messages;
        }
        self.agent.state().messages().to_vec()
    }

    fn materialize_resident_messages(&self, messages: Vec<AgentMessage>) -> Vec<AgentMessage> {
        if messages.is_empty() {
            let values = self.with_session_manager(|manager| {
                let context = manager.current_context();
                manager.drop_materialized_cache();
                context.messages
            });
            if !values.is_empty() {
                return values
                    .into_iter()
                    .filter_map(|value| session_message_from_value(value).ok())
                    .collect();
            }
        }
        let store = self.with_session_manager(|manager| manager.resident_store().clone());
        messages
            .into_iter()
            .map(|message| {
                let Ok(mut value) = session_message_to_value(&message) else {
                    return message;
                };
                store.materialize_in_place(&mut value);
                session_message_from_value(value).unwrap_or(message)
            })
            .collect()
    }

    /// The idle half of senpi `_emitAgentIdleAfterDeferredTurns`. Releasing frees memory only for
    /// strings the store already spilled to its blob backing: a resident string is shared with the
    /// store, so tokenizing it hands back nothing while costing every settled-time reader a
    /// re-materialization. senpi also drops the session manager's memoized materialized views here;
    /// the native `SessionManager` keeps none (every view re-materializes from the store).
    fn release_idle_runtime_messages(&self) {
        self.state().abort_provenance = Default::default();
        let park_generation = *lock(&self.idle_park_generation);
        let _ = self.with_session_manager_mut(|manager| manager.park_idle_memory());
        let store = self.with_session_manager(|manager| manager.resident_store().clone());
        store.spill_resident();
        if store.stats().evicted_count == 0 {
            maho_tool_child::allocator::reclaim_unused_heap();
            return;
        }
        let messages = self
            .agent
            .state()
            .messages()
            .iter()
            .map(|message| {
                let Ok(mut value) = session_message_to_value(message) else {
                    return message.clone();
                };
                store.externalize_in_place_with_min(&mut value, 1024);
                session_message_from_value(value).unwrap_or_else(|_| message.clone())
            })
            .collect();
        store.spill_resident();
        #[cfg(test)]
        if let Some(hook) = IDLE_RELEASE_BEFORE_COMMIT.with(|slot| slot.borrow_mut().take()) {
            hook();
        }
        {
            let generation = lock(&self.idle_park_generation);
            if *generation == park_generation {
                self.agent.set_messages(messages);
                self.runtime_messages_tokenized
                    .store(true, Ordering::SeqCst);
            }
        }
        maho_tool_child::allocator::reclaim_unused_heap();
    }

    pub fn park_idle_child_memory(&self) {
        let parked = {
            let mut generation = lock(&self.idle_park_generation);
            let parked = self.with_session_manager_mut(|manager| manager.park_idle_memory());
            if parked {
                *generation += 1;
                self.agent.set_messages(Vec::new());
                self.runtime_messages_tokenized
                    .store(true, Ordering::SeqCst);
            }
            parked
        };
        if !parked {
            self.release_idle_runtime_messages();
        }
        maho_tool_child::allocator::reclaim_unused_heap();
    }

    pub fn model(&self) -> Model {
        self.agent.model()
    }

    /// senpi `_refreshCurrentModelFromRegistry`. Assigns the catalog object directly: `set_model`
    /// would re-check auth and emit a model switch, which a reload must not do.
    fn refresh_current_model_from_registry(&self) {
        self.state().delegated_compaction_key = None;
        let current = self.model();
        let Some(refreshed) = self
            .model_runtime()
            .get_model(&current.provider, &current.id)
        else {
            return;
        };
        if refreshed == current {
            return;
        }
        self.agent.set_model(refreshed);
    }

    /// The agent carries `"off" | ThinkingLevel`; senpi's `ThinkingLevel` includes `"off"`.
    pub fn thinking_level(&self) -> ModelThinkingLevel {
        self.agent.thinking_level()
    }

    pub fn thinking_selection(&self) -> Option<ThinkingSelection> {
        self.agent.thinking_selection()
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
        self.agent.is_streaming()
    }

    pub fn is_idle(&self) -> bool {
        !self.agent.is_streaming_or_has_pending_tool_calls()
            && !self.work_barrier.has_active_work()
            && !self.is_compacting()
            && !self.is_retrying()
    }

    pub fn system_prompt(&self) -> String {
        self.agent.system_prompt()
    }

    pub fn set_system_prompt_sources(&self, custom: Option<String>, append: Vec<String>) {
        let mut state = self.state();
        state.custom_system_prompt_source = custom;
        state.append_system_prompt_sources = append;
        state.resolved_system_prompt = None;
        drop(state);
        self.rebuild_system_prompt();
    }

    pub fn set_context_files_enabled(&self, enabled: bool) {
        // senpi's `noContextFiles` is a loader option: changing it re-reads the context files.
        {
            let mut state = self.state();
            state.context_files_enabled = enabled;
            state.loaded_context_files = None;
        }
        self.rebuild_system_prompt();
    }

    /// senpi `resourceLoader.getAgentsFiles()`: the context files loaded by the last resource load.
    pub fn project_context_files(&self) -> Vec<crate::system_prompt::ContextFile> {
        if !self.state().context_files_enabled {
            return Vec::new();
        }
        if let Some(files) = self.state().loaded_context_files.clone() {
            return files;
        }
        let agent_dir = self.agent_dir();
        let limits = self.with_settings_manager(|manager| manager.get_context_files_limits());
        let loaded =
            crate::resource_loader::load_project_context_files(&self.cwd(), &agent_dir, limits);
        let mut files = loaded.files;
        // senpi `loadProjectContextFiles`: the agent-dir file and a file in the home directory are the user's
        // own standing instructions (`scope: "user"`). The loader lives in resource_loader.rs, which another
        // lane owns, so the scope is applied here until it produces it itself.
        let directory_key = |path: &std::path::Path| {
            std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
        };
        let user_dirs = [
            directory_key(std::path::Path::new(&agent_dir)),
            directory_key(&crate::config::home_dir_path()),
        ];
        for file in &mut files {
            if file.scope.is_none()
                && std::path::Path::new(&file.path)
                    .parent()
                    .is_some_and(|dir| user_dirs.contains(&directory_key(dir)))
            {
                file.scope = Some(crate::system_prompt::ContextFileScope::User);
            }
        }
        self.state().loaded_context_files = Some(files.clone());
        files
    }

    /// senpi `resourceLoader.getSystemPrompt()` / `getAppendSystemPrompt()`: the `--system-prompt`
    /// and `--append-system-prompt` inputs resolved once per resource load.
    pub fn resolved_system_prompt(&self) -> (Option<String>, Vec<String>) {
        if let Some(resolved) = self.state().resolved_system_prompt.clone() {
            return resolved;
        }
        let (custom, append) = self.system_prompt_sources();
        let resolved = (
            crate::resource_loader::resolve_prompt_input(custom.as_deref(), "system prompt"),
            append
                .iter()
                .filter_map(|source| {
                    crate::resource_loader::resolve_prompt_input(
                        Some(source),
                        "append system prompt",
                    )
                })
                .collect(),
        );
        self.state().resolved_system_prompt = Some(resolved.clone());
        resolved
    }

    pub fn system_prompt_sources(&self) -> (Option<String>, Vec<String>) {
        let state = self.state();
        (
            state.custom_system_prompt_source.clone(),
            state.append_system_prompt_sources.clone(),
        )
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
            has_session_work: self.work_barrier.has_active_work()
                || self.is_retrying()
                || lock(&self.work_obligations)
                    .as_ref()
                    .is_some_and(|registry| !registry.active().is_empty()),
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
        let mut tools: BTreeMap<String, ToolInfo> = state
            .tool_definitions
            .iter()
            .map(|(name, entry)| {
                (
                    name.clone(),
                    normalize_tool_exposure(&entry.definition, entry.source_info.clone()),
                )
            })
            .collect();
        for tool in self.agent.state().tools().iter() {
            tools
                .entry(tool.name().to_owned())
                .or_insert_with(|| ToolInfo {
                    name: tool.name().to_owned(),
                    label: tool.label.clone(),
                    description: tool.tool.description.clone(),
                    parameters: tool.tool.parameters.clone(),
                    prompt_guidelines: None,
                    kernel_prelude: None,
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
        self.agent.tool_names()
    }

    /// A runtime extension flag value (constructor input in TS).
    pub fn flag_value(&self, name: &str) -> Option<FlagValue> {
        self.state().flag_values.get(name).cloned()
    }

    /// senpi `extensionRunner.setFlagValue(name, value)`: the bound runner's extensions read it from
    /// their next call, and the session keeps it for a runner that binds later.
    pub async fn set_extension_flag_value(&self, name: &str, value: FlagValue) {
        // A runner a reload or dispose invalidated panics on writes; the session keeps the value, and a
        // rebuild carries the session's flags to its fresh runner (senpi#3420: a preset change can land
        // while a reload or replacement is in flight).
        if let Some(runner) = self.extension_runner.lock().await.as_ref()
            && runner.runtime.assert_active().is_ok()
        {
            runner.runtime.set_flag(name, value.clone());
        }
        self.state().flag_values.insert(name.to_owned(), value);
    }

    pub(crate) fn replacement_custom_tools(&self) -> Vec<ToolDefinition> {
        self.state().custom_tools.clone()
    }

    pub(crate) fn replacement_auto_title(&self) -> bool {
        self.state().auto_title_sessions
    }

    pub fn get_tool_definition(&self, name: &str) -> Option<ToolDefinition> {
        self.state()
            .tool_definitions
            .get(name)
            .map(|entry| entry.definition.clone())
    }

    /// The tool a call named `requested` runs, by the same rule the agent loop applies (exact name,
    /// else the unique alias among callable tools), without activating anything.
    pub fn resolve_tool_call_name(&self, requested: &str) -> String {
        resolve_tool_name_alias(requested, self.callable_tool_names())
            .unwrap_or_else(|| requested.to_owned())
    }

    /// senpi `_emit(withResolvedToolName(event, name => this.resolveToolCallName(name)))` for a
    /// published `message_update`: the `resolvedToolName` a `toolcall_start`/`toolcall_end` carries.
    pub fn message_update_resolved_tool_name(
        &self,
        event: &maho_agent::types::AgentEvent,
    ) -> Option<String> {
        crate::tool_call_display_name::streamed_tool_call_name(event)
            .map(|name| self.resolve_tool_call_name(&name))
    }

    /// senpi `_buildRuntime`'s tool-runtime half of the constructor: the session host-call handler shared by both
    /// runtimes, the tool child (`child` runtime only) and the dispatcher over them. Every callback reaches the
    /// session through `session_cell`, so a session that is already dropped serves nothing.
    #[allow(clippy::type_complexity)]
    fn build_tool_runtime(
        runtime: crate::tool_dispatch::tool_dispatcher::ToolRuntimeKind,
        session_cell: &SessionCell,
        session_logger: &SessionLogger,
        agent_dir: &str,
        cwd: &str,
        session_id: &str,
    ) -> (
        Arc<dyn crate::tool_dispatch::tool_dispatcher::ToolDispatcher>,
        Option<Arc<maho_tool_child::session_child::SessionToolChild>>,
        ToolChildHostTargets,
    ) {
        use crate::tool_child_host_calls::{SessionHostCallDeps, create_session_host_call_handler};
        use crate::tool_dispatch::offload::BoxError;
        use crate::tool_dispatch::tool_dispatcher::{
            ToolDispatcherOptions, ToolRuntimeKind, create_tool_dispatcher,
        };
        use maho_tool_child::session_child::{
            SessionToolChild, SessionToolChildOptions, SessionToolChildRecordState,
        };

        let targets: ToolChildHostTargets = Arc::default();
        // One host-call handler for both runtimes: the child's reverse bridge and an in-process ctx.offload module.
        let nested_cell = Arc::clone(session_cell);
        let names_cell = Arc::clone(session_cell);
        let policy_cell = Arc::clone(session_cell);
        let target_list = Arc::clone(&targets);
        let host_calls = create_session_host_call_handler(SessionHostCallDeps {
            cwd: PathBuf::from(cwd),
            execute_nested: Arc::new(move |parent_tool_call_id, name, args, signal| {
                let cell = Arc::clone(&nested_cell);
                Box::pin(async move {
                    let session = upgrade_session_cell(&cell)
                        .ok_or_else(|| BoxError::from("session disposed"))?;
                    session
                        .execute_nested_tool_call(&parent_tool_call_id, &name, args, signal)
                        .await
                })
            }),
            callable_tool_names: Arc::new(move || {
                upgrade_session_cell(&names_cell)
                    .map(|session| session.callable_tool_names())
                    .unwrap_or_default()
            }),
            target: Arc::new(move |kind| {
                lock(&target_list)
                    .iter()
                    .find(|(registered, _)| *registered == kind)
                    .map(|(_, target)| Arc::clone(target))
            }),
            filesystem_policy: Arc::new(move || {
                upgrade_session_cell(&policy_cell)
                    .and_then(|session| session.filesystem_policy_checker())
            }),
        });
        let tool_child = (runtime == ToolRuntimeKind::Child).then(|| {
            let roots_cell = Arc::clone(session_cell);
            let record_cell = Arc::clone(session_cell);
            let wake_cell = Arc::clone(session_cell);
            Arc::new(SessionToolChild::new(SessionToolChildOptions {
                dir: Path::new(agent_dir).join("tool-children").join(session_id),
                cwd: PathBuf::from(cwd),
                module_roots: Arc::new(move || {
                    upgrade_session_cell(&roots_cell)
                        .map(|session| session.tool_child_module_roots())
                        .unwrap_or_default()
                }),
                on_record: Arc::new(move |record| {
                    let Some(session) = upgrade_session_cell(&record_cell) else {
                        return;
                    };
                    let state = match record.state {
                        SessionToolChildRecordState::Spawned => "spawned",
                        SessionToolChildRecordState::Exited => "exited",
                        SessionToolChildRecordState::Respawned => "respawned",
                    }
                    .to_owned();
                    session.emit(AgentSessionEvent::SessionToolChild {
                        state: state.clone(),
                        generation: record.generation,
                        pid: record.pid,
                    });
                    if state == "exited" {
                        for job in session.child_jobs() {
                            if let Some(child_exited) = &job.child_exited {
                                child_exited(record.generation);
                            }
                        }
                    }
                }),
                on_host_call: Some(host_calls.clone()),
                on_wake: Some(Arc::new(move |wake| {
                    let Some(session) = upgrade_session_cell(&wake_cell) else {
                        return;
                    };
                    for job in session.child_jobs() {
                        if job.kind == wake.source
                            && let Some(on_wake) = &job.wake
                        {
                            on_wake(wake.value.as_ref().unwrap_or(&Value::Null), wake.generation);
                        }
                    }
                })),
                env: None,
                command: None,
                now: None,
            }))
        });
        let mut options = ToolDispatcherOptions::new(runtime);
        let notify_logger = session_logger.clone();
        options.notify = Some(Arc::new(move |event, data| {
            notify_logger.info(event, data.as_ref());
        }));
        options.host_calls = Some(host_calls);
        let stuck_cell = Arc::clone(session_cell);
        let stuck_logger = session_logger.clone();
        options.on_module_stuck = Some(Arc::new(move |notice| {
            let mut data = Map::new();
            data.insert(
                "modulePath".to_owned(),
                Value::String(notice.module_path.clone()),
            );
            data.insert("export".to_owned(), Value::String(notice.export.clone()));
            data.insert("deadlineMs".to_owned(), Value::from(notice.deadline_ms));
            data.insert(
                "files".to_owned(),
                Value::Array(notice.files.iter().cloned().map(Value::String).collect()),
            );
            stuck_logger.warn("offload_module_stuck", Some(&data));
            if let Some(session) = upgrade_session_cell(&stuck_cell) {
                session.emit(AgentSessionEvent::OffloadModuleStuck {
                    module_path: notice.module_path,
                    export: notice.export,
                    deadline_ms: notice.deadline_ms,
                    files: notice.files,
                    notice: notice.notice,
                });
            }
        }));
        if let Some(child) = &tool_child {
            options.child_runner = Some(child.runner());
            options.child_kind_runner = Some(child.kind_runner());
        }
        (create_tool_dispatcher(options), tool_child, targets)
    }

    /// senpi `whenToolChildClosed`: settles once `dispose` has stopped the tool child, swept what it launched, and
    /// closed its in-process offload jobs. Ready at once while the session has not been disposed.
    pub async fn when_tool_child_closed(&self) {
        let closed = lock(&self.tool_child_closed).clone();
        if let Some(closed) = closed {
            closed.await;
        }
    }

    /// senpi `toolChildRow`: the `list_sessions` row for this session's tool child.
    pub fn tool_child_row(&self) -> ToolChildRow {
        use maho_tool_child::session_child::SessionToolChildRowState;
        let Some(child) = &self.tool_child else {
            return ToolChildRow {
                state: "none",
                generation: None,
                pid: None,
            };
        };
        let row = child.row();
        ToolChildRow {
            state: match row.state {
                SessionToolChildRowState::None => "none",
                SessionToolChildRowState::Live => "live",
                SessionToolChildRowState::Exited => "exited",
            },
            generation: row.generation,
            pid: row.pid,
        }
    }

    /// senpi `toolDispatcher`.
    pub fn tool_dispatcher(
        &self,
    ) -> Arc<dyn crate::tool_dispatch::tool_dispatcher::ToolDispatcher> {
        Arc::clone(&self.tool_dispatcher)
    }

    /// senpi `registerToolChildHostTarget`: serves the tool child's `completion` or `reserved` calls on the server
    /// (codemode registers its completion handler and `runReservedTool`). Returns the unregister function; a later
    /// registration replaces it.
    pub fn register_tool_child_host_target(
        &self,
        kind: crate::tool_child_host_calls::RegisteredHostCallKind,
        target: crate::tool_child_host_calls::HostCallTarget,
    ) -> Box<dyn FnOnce() + Send + Sync> {
        let targets = Arc::clone(&self.tool_child_host_targets);
        {
            let mut list = lock(&targets);
            list.retain(|(registered, _)| *registered != kind);
            list.push((kind, Arc::clone(&target)));
        }
        Box::new(move || {
            lock(&targets).retain(|(registered, current)| {
                !(*registered == kind && Arc::ptr_eq(current, &target))
            });
        })
    }

    /// Records the placement fields (`placement`, `serverPinned`, `childModule`, `childJob`) senpi reads off a
    /// registered `ToolDefinition`, which the dispatcher and the child's wake / exit routing need by tool name.
    pub fn register_tool_placement(
        &self,
        info: crate::tool_dispatch::tool_dispatcher::ToolPlacementInfo,
    ) {
        lock(&self.tool_placements).insert(info.name.clone(), info);
    }

    /// senpi `wrapRegisteredTools(..., this._toolDispatcher)`: records the tool's placement fields, then wraps it so its
    /// body runs through the session's dispatcher.
    fn wrap_extension_tool(
        &self,
        registered: maho_ext_api::RegisteredTool,
        runtime: maho_ext_api::ExtensionRuntime,
        context_factory: maho_ext_host::wrapper::ToolContextFactory,
    ) -> AgentTool {
        let placement = maho_ext_api::ToolPlacementInfo::from_definition(&registered.definition);
        self.register_tool_placement(placement.clone());
        let dispatcher: Arc<dyn maho_ext_api::ToolRunDispatch> = Arc::new(
            crate::tool_dispatch::tool_dispatcher::DispatcherRun(Arc::clone(&self.tool_dispatcher)),
        );
        maho_ext_host::wrapper::wrap_registered_tool(
            registered,
            runtime,
            context_factory,
            dispatcher,
            placement,
        )
    }

    /// senpi `_childJobs`: the child jobs the registered tools declare (`ToolDefinition.childJob`): who a child's
    /// wake or exit concerns.
    fn child_jobs(&self) -> Vec<crate::tool_dispatch::tool_dispatcher::ToolChildJob> {
        lock(&self.tool_placements)
            .values()
            .filter_map(|info| info.child_job.clone())
            .collect()
    }

    /// The module files the session's child may load from: the files tools declared (`toolChildModuleRoots`) and the
    /// files registered child modules name (`childModuleRoots`).
    fn tool_child_module_roots(&self) -> Vec<String> {
        let Some(runner) = self.extension_runner_snapshot() else {
            return Vec::new();
        };
        let declared: Vec<maho_tool_child::session_child::RegisteredToolChildModule> = {
            let placements = lock(&self.tool_placements);
            runner
                .get_all_registered_tools()
                .into_iter()
                .map(
                    |tool| maho_tool_child::session_child::RegisteredToolChildModule {
                        child_module: placements
                            .get(&tool.definition.name)
                            .and_then(|info| info.child_module.clone()),
                        source_path: tool.source_info.path,
                    },
                )
                .collect()
        };
        let mut roots = maho_tool_child::session_child::tool_child_module_roots(&declared);
        roots.extend(crate::tool_dispatch::offload::child_module_roots(
            &runner.get_all_child_modules(),
        ));
        roots
    }

    /// The session's composed filesystem policy (senpi `_filesystemPolicy`); `None` when no extension registered one.
    fn filesystem_policy_checker(&self) -> Option<maho_tools::FilesystemPolicyChecker> {
        let runner = self.extension_runner_snapshot()?;
        let policies: Vec<maho_tools::FilesystemPolicy> = runner
            .extensions
            .iter()
            .flat_map(|extension| {
                runner
                    .runtime
                    .live_filesystem_policies(&extension.identity.path)
                    .unwrap_or_else(|| extension.filesystem_policies.clone())
            })
            .collect();
        maho_tools::filesystem_policy::compose_filesystem_policies(policies)
    }

    /// senpi `_executeNestedToolCall`: runs `name` as a nested call of `parent_tool_call_id` through the session's
    /// tool pipeline (the same path as in-process `ctx.executeTool`, hooks once). A failed call is an outcome with
    /// `is_error`, not an error.
    async fn execute_nested_tool_call(
        &self,
        parent_tool_call_id: &str,
        name: &str,
        args: Value,
        signal: maho_ext_api::AbortSignal,
    ) -> Result<maho_agent::types::AgentToolCallOutcome, crate::tool_dispatch::offload::BoxError>
    {
        let (recorder, sequence) = {
            let mut calls = lock(&self.nested_host_calls);
            let (recorder, counter) = calls
                .entry(parent_tool_call_id.to_owned())
                .or_insert_with(|| (Arc::default(), Arc::new(AtomicU64::new(1))));
            (Arc::clone(recorder), counter.fetch_add(1, Ordering::SeqCst))
        };
        let id = format!("{parent_tool_call_id}/{sequence}");
        let controller = maho_ai::utils::abort::AbortController::new();
        let call_signal = controller.signal();
        let watcher = tokio::spawn(async move {
            signal.cancelled().await;
            controller.abort(None);
        });
        let result = NESTED_TOOL_SCOPE
            .scope(
                (
                    parent_tool_call_id.to_owned(),
                    recorder,
                    Arc::new(AtomicU64::new(sequence)),
                ),
                self.execute_tool(
                    name,
                    args.clone(),
                    ExecuteToolOptions {
                        signal: Some(call_signal),
                        activate_inactive_tool: None,
                    },
                ),
            )
            .await;
        watcher.abort();
        let (result, is_error) = match result {
            Ok(result) => {
                let is_error = result.is_error.unwrap_or(false);
                (result, is_error)
            }
            Err(error) => {
                let mut failed = AgentToolResult::text(error.message);
                failed.is_error = Some(true);
                (failed, true)
            }
        };
        Ok(maho_agent::types::AgentToolCallOutcome {
            tool_call: maho_ai::types::ToolCall {
                id,
                name: name.to_owned(),
                arguments: args.as_object().cloned().unwrap_or_default(),
                incomplete: None,
                error_message: None,
                thought_signature: None,
                namespace: None,
            },
            result,
            is_error,
        })
    }

    /// Active tool names plus search-exposed lazy tools; senpi also appends the tool-search catalog,
    /// which has no Rust counterpart (the service is a TS extension), so it contributes nothing.
    fn callable_tool_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.agent.tool_names();
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
        let mut names: BTreeSet<String> = EVAL_ONLY_TOOL_NAMES
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        for entry in state.tool_definitions.values() {
            if normalize_tool_exposure(&entry.definition, entry.source_info.clone()).exposure
                == ToolExposure::Eval
            {
                names.insert(entry.definition.name.clone());
            }
        }
        names
    }

    /// Re-resolve the eval-only policy against the current registry (senpi re-resolves on every
    /// registry rebuild and reload, so tools declaring `exposure: Eval` arm it by default).
    fn refresh_eval_only_tool_names(&self) {
        let names = self.resolve_eval_only_tool_names();
        self.state().eval_only_tool_names = Some(names);
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
            if registered
                .as_ref()
                .is_some_and(|registered| registered.contains(&name))
            {
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
                format!(
                    "Run {name} inside an eval cell via {}; hooks and permissions still apply.",
                    eval_helper_call(&name)
                ),
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
            self.state()
                .tool_definitions
                .get(tool_name)
                .map(|entry| entry.source_info.clone())
                .unwrap_or_else(empty_source_info),
        );
        if !exposure.allow_lazy_activation {
            return false;
        }
        let activators = self.state().lazy_tool_activators.clone();
        if activators.iter().any(|activate| activate(tool_name)) {
            return true;
        }
        if exposure.exposure == ToolExposure::Search
            && !self
                .get_active_tool_names()
                .iter()
                .any(|name| name == tool_name)
        {
            let mut names = self.get_active_tool_names();
            names.push(tool_name.to_owned());
            self.set_active_tools_by_name(names);
        }
        self.get_active_tool_names()
            .iter()
            .any(|name| name == tool_name)
    }

    /// Set active tools by name; only tools in the registry can be enabled.
    pub fn set_active_tools_by_name(&self, tool_names: Vec<String>) {
        self.refresh_eval_only_tool_names();
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
                tool_names
                    .iter()
                    .filter(|name| !policy_names.contains(*name))
                    .cloned()
                    .collect()
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
        let changed = self.get_active_tool_names()
            != tools
                .iter()
                .map(|tool| tool.tool.name.clone())
                .collect::<Vec<_>>();
        self.agent.set_tools(tools);
        let mut state = self.state();
        state.withheld_eval_only_tool_names = withheld;
        state.requested_active_tool_names = Some(requested);
        drop(state);
        self.rebuild_system_prompt();
        if changed && lock(&self.binding_readiness).is_none() {
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
    pub fn register_tool_definition(
        &self,
        definition: ToolDefinition,
        source_info: SourceInfo,
        mut tool: AgentTool,
    ) {
        let execute = tool.execute.clone();
        tool.execute = Arc::new(move |id, args, signal, update| {
            let execute = execute.clone();
            let parent = NESTED_TOOL_SCOPE.try_with(|scope| scope.1.clone()).ok();
            let queue_held = NESTED_TOOL_QUEUE_HELD
                .try_with(|held| *held)
                .unwrap_or(false);
            Box::pin(async move {
                let root = parent.is_none();
                let recorder = parent.unwrap_or_else(|| Arc::new(Mutex::new(Default::default())));
                let mut result = NESTED_TOOL_QUEUE_HELD
                    .scope(
                        queue_held,
                        NESTED_TOOL_SCOPE.scope(
                            (id.clone(), recorder.clone(), Arc::new(AtomicU64::new(1))),
                            async move { execute(id, args, signal, update).await },
                        ),
                    )
                    .await;
                if root {
                    let recorder = lock(&recorder);
                    result.nested_calls = recorder.snapshot();
                    if let Some(usage) = &recorder.usage {
                        result.usage = Some(match result.usage {
                            Some(own) => crate::compaction::combine_usage(&own, usage),
                            None => *usage,
                        });
                    }
                }
                result
            })
        });
        let mut state = self.state();
        // An exclusion entry ending in `*` withholds every tool name with that prefix.
        if state.excluded_tool_names.as_ref().is_some_and(|names| {
            names.contains(&definition.name)
                || names.iter().any(|name| {
                    name.strip_suffix('*')
                        .is_some_and(|prefix| definition.name.starts_with(prefix))
                })
        }) {
            state.withheld_excluded_tool_names.insert(definition.name);
            return;
        }
        if state
            .allowed_tool_names
            .as_ref()
            .is_some_and(|names| !names.contains(&definition.name))
        {
            return;
        }
        if let Some(snippet) = definition
            .prompt_snippet
            .as_deref()
            .map(|snippet| snippet.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|snippet| !snippet.is_empty())
        {
            state
                .tool_prompt_snippets
                .insert(definition.name.clone(), snippet);
        }
        if let Some(guidelines) = &definition.prompt_guidelines {
            let mut normalized = Vec::new();
            for guideline in guidelines {
                let guideline = guideline.trim().to_owned();
                if !guideline.is_empty() && !normalized.contains(&guideline) {
                    normalized.push(guideline);
                }
            }
            state
                .tool_prompt_guidelines
                .insert(definition.name.clone(), normalized);
        }
        state.tool_registry.insert(definition.name.clone(), tool);
        state
            .base_tool_definitions
            .insert(definition.name.clone(), definition.clone());
        state.tool_definitions.insert(
            definition.name.clone(),
            ToolDefinitionEntry {
                definition,
                source_info,
            },
        );
    }

    /// Every tool name a registration offered but the session's exclusion list withheld.
    pub fn withheld_excluded_tool_names(&self) -> Vec<String> {
        self.state()
            .withheld_excluded_tool_names
            .iter()
            .cloned()
            .collect()
    }

    /// The session's tool exclusion entries (names and `prefix*` patterns), carried into a
    /// replacement session.
    pub fn excluded_tool_names(&self) -> Option<Vec<String>> {
        self.state()
            .excluded_tool_names
            .as_ref()
            .map(|names| names.iter().cloned().collect())
    }

    /// Installs the host's refusal of prompt-dispatched extension commands: `Some(reason)` from the
    /// gate fails the prompt before the command runs. Replacement sessions inherit it.
    pub fn set_extension_command_gate(&self, gate: Option<ExtensionCommandGate>) {
        self.state().extension_command_gate = gate;
    }

    pub fn extension_command_gate(&self) -> Option<ExtensionCommandGate> {
        self.state().extension_command_gate.clone()
    }

    /// Marks the session's role restrictive, refusing client-submitted user bash. Replacement
    /// sessions inherit it.
    pub fn set_user_bash_denied(&self, denied: bool) {
        self.state().user_bash_denied = denied;
    }

    pub fn user_bash_denied(&self) -> bool {
        self.state().user_bash_denied
    }

    pub fn replace_custom_tool(&self, definition: ToolDefinition) -> Result<(), String> {
        if self.is_streaming() {
            return Err("Cannot replace a tool during an active turn".into());
        }
        let weak = self.weak_accessor();
        let context: maho_tools::tool_definition_wrapper::ContextFactory = Arc::new(move |_, _| {
            let session = weak().expect("Custom tool session retained during tool execution");
            Arc::new(crate::sdk::extension_context::create(&session))
        });
        let tool = maho_tools::tool_definition_wrapper::wrap_tool_definition(
            definition.clone(),
            Some(context),
            Some(crate::tool_dispatch::tool_dispatcher::tool_run_dispatcher(
                self.tool_dispatcher(),
            )),
        );
        let name = definition.name.clone();
        {
            let mut state = self.state();
            if let Some(existing) = state
                .custom_tools
                .iter_mut()
                .find(|existing| existing.name == name)
            {
                *existing = definition.clone();
            } else {
                state.custom_tools.push(definition.clone());
            }
        }
        self.register_tool_definition(
            definition,
            SourceInfo {
                source: "sdk".into(),
                ..Default::default()
            },
            tool,
        );
        let active = self.get_active_tool_names();
        self.set_active_tools_by_name(active);
        Ok(())
    }

    fn register_extension_tool(
        &self,
        definition: ToolDefinition,
        source_info: SourceInfo,
        tool: AgentTool,
    ) {
        {
            let mut state = self.state();
            if (source_info.source == "builtin" || source_info.path.starts_with("<builtin:"))
                && state
                    .default_tool_names
                    .as_ref()
                    .is_some_and(|names| !names.contains(&definition.name))
            {
                return;
            }
            if !state.extension_tool_backups.contains_key(&definition.name) {
                let previous = state
                    .tool_definitions
                    .get(&definition.name)
                    .cloned()
                    .zip(state.tool_registry.get(&definition.name).cloned());
                state
                    .extension_tool_backups
                    .insert(definition.name.clone(), previous);
            }
        }
        self.register_tool_definition(definition, source_info, tool);
    }

    /// Add a lazy-tool activator (extension or SDK supplied).
    pub fn add_lazy_tool_activator(&self, activator: LazyToolActivator) {
        self.state().lazy_tool_activators.push(activator);
    }

    /// Register a tool context disposer callback, run on session dispose.
    pub fn add_tool_context_disposer(&self, disposer: impl FnOnce() + Send + 'static) {
        lock(&self.inner.tool_context_disposers).push(Box::new(disposer));
    }

    /// Register a tool context disposer callback, run on session dispose.
    pub fn register_tool_context_disposer(&self, disposer: impl FnOnce() + Send + 'static) {
        self.add_tool_context_disposer(disposer);
    }

    /// Execute a tool by name through the same preparation and hook path the agent loop uses.
    pub async fn execute_tool(
        &self,
        tool_name: &str,
        params: Value,
        options: ExecuteToolOptions,
    ) -> Result<AgentToolResult, ExecuteToolError> {
        self.execute_tool_with_updates(tool_name, params, options, None, None, false)
            .await
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
        self.execute_tool_with_updates(tool_name, params, options, None, Some(tool_call_id), false)
            .await
    }

    pub async fn execute_tool_with_call_id_and_updates(
        &self,
        tool_call_id: &str,
        tool_name: &str,
        params: Value,
        options: ExecuteToolOptions,
        on_update: Option<maho_agent::types::AgentToolUpdateCallback>,
    ) -> Result<AgentToolResult, ExecuteToolError> {
        self.execute_tool_with_updates(
            tool_name,
            params,
            options,
            on_update,
            Some(tool_call_id),
            false,
        )
        .await
    }

    /// Shared child arguments have already passed the child's normalizer.
    /// Parent validation and permission admission still run against those arguments.
    pub async fn execute_prepared_shared_tool(
        &self,
        tool_call_id: &str,
        tool_name: &str,
        params: Value,
        options: ExecuteToolOptions,
        on_update: Option<maho_agent::types::AgentToolUpdateCallback>,
    ) -> Result<AgentToolResult, ExecuteToolError> {
        let disposed = self.state().disposed;
        if disposed {
            return Err(ExecuteToolError {
                code: "blocked".into(),
                tool_name: tool_name.into(),
                message: "Shared tool parent session retired".into(),
                active_tools: self.get_active_tool_names(),
            });
        }
        self.execute_tool_with_updates(
            tool_name,
            params,
            options,
            on_update,
            Some(tool_call_id),
            true,
        )
        .await
    }

    async fn execute_tool_with_updates(
        &self,
        tool_name: &str,
        params: Value,
        options: ExecuteToolOptions,
        on_update: Option<maho_agent::types::AgentToolUpdateCallback>,
        tool_call_id: Option<&str>,
        arguments_prepared: bool,
    ) -> Result<AgentToolResult, ExecuteToolError> {
        self.wait_for_tool_barrier(tool_name, options.signal.as_ref())
            .await;
        if options
            .signal
            .as_ref()
            .is_some_and(|signal| signal.aborted())
        {
            return Err(ExecuteToolError {
                code: "aborted".into(),
                tool_name: tool_name.into(),
                message: "Tool call aborted before execution".into(),
                active_tools: self.get_active_tool_names(),
            });
        }
        let nested = NESTED_TOOL_SCOPE
            .try_with(|(parent, recorder, sequence)| {
                (
                    format!("{parent}/{}", sequence.fetch_add(1, Ordering::SeqCst)),
                    recorder.clone(),
                )
            })
            .ok();
        let record = nested
            .as_ref()
            .and_then(|(id, recorder)| lock(recorder).start(id, tool_name, &params));
        let started = std::time::Instant::now();
        let caller = NESTED_TOOL_SCOPE.try_with(|scope| scope.0.clone()).ok();
        let (updates, mut update_events) = tokio::sync::mpsc::unbounded_channel();
        let on_update = if let Some((id, _)) = &nested {
            let event = maho_agent::types::AgentEvent::ToolExecutionStart {
                tool_call_id: id.clone(),
                tool_name: tool_name.into(),
                args: params.clone(),
                parent_tool_call_id: caller.clone(),
            };
            self.dispatch_agent_extension_event(&event, None, false)
                .await;
            self.emit(AgentSessionEvent::Agent(event));
            let session = self.clone();
            let id = id.clone();
            let name = tool_name.to_owned();
            let args = params.clone();
            let caller = caller.clone();
            Some(Arc::new(move |partial: AgentToolResult| {
                let event = maho_agent::types::AgentEvent::ToolExecutionUpdate {
                    tool_call_id: id.clone(),
                    tool_name: name.clone(),
                    args: args.clone(),
                    partial_result: serde_json::to_value(&partial)
                        .expect("tool result is serializable"),
                    parent_tool_call_id: caller.clone(),
                };
                let _ = updates.send(event.clone());
                session.emit(AgentSessionEvent::Agent(event));
                if let Some(update) = &on_update {
                    update(partial);
                }
            }) as maho_agent::types::AgentToolUpdateCallback)
        } else {
            on_update
        };
        let execution = async {
            let held = NESTED_TOOL_QUEUE_HELD
                .try_with(|held| *held)
                .unwrap_or(false);
            let exclusive = nested.is_some()
                && !held
                && (self.agent.tool_execution()
                    == maho_agent::types::ToolExecutionMode::Sequential
                    || self
                        .state()
                        .tool_registry
                        .get(tool_name)
                        .is_some_and(|tool| {
                            tool.execution_mode
                                == Some(maho_agent::types::ToolExecutionMode::Sequential)
                        }));
            let _queue = if exclusive {
                Some(if let Some(signal) = &options.signal {
                    tokio::select! {
                        biased;
                        () = signal.cancelled() => return Err(ExecuteToolError { code: "blocked".into(), tool_name: tool_name.into(),
                            message: "Tool execution cancelled".into(), active_tools: self.get_active_tool_names() }),
                        queue = self.nested_tool_queue.lock() => queue,
                    }
                } else {
                    self.nested_tool_queue.lock().await
                })
            } else {
                None
            };
            NESTED_TOOL_QUEUE_HELD
                .scope(
                    held || exclusive,
                    self.execute_tool_with_updates_inner(
                        tool_name,
                        params,
                        options,
                        on_update,
                        nested.as_ref().map(|(id, _)| id.as_str()).or(tool_call_id),
                        arguments_prepared,
                    ),
                )
                .await
        };
        tokio::pin!(execution);
        let result = loop {
            tokio::select! {
                biased;
                Some(event) = update_events.recv() => self.dispatch_agent_extension_event(&event, None, false).await,
                result = &mut execution => break result,
            }
        };
        while let Ok(event) = update_events.try_recv() {
            self.dispatch_agent_extension_event(&event, None, false)
                .await;
        }
        if let Some((id, recorder)) = &nested {
            let event_result = match &result {
                Ok(result) => result.clone(),
                Err(error) => {
                    let mut failed = AgentToolResult::text(error.message.clone());
                    failed.is_error = Some(true);
                    failed
                }
            };
            let event = maho_agent::types::AgentEvent::ToolExecutionEnd {
                tool_call_id: id.clone(),
                tool_name: tool_name.into(),
                result: serde_json::to_value(&event_result).expect("tool result is serializable"),
                is_error: event_result.is_error.unwrap_or(false),
                parent_tool_call_id: caller,
            };
            self.dispatch_agent_extension_event(&event, None, false)
                .await;
            self.emit(AgentSessionEvent::Agent(event));
            match &result {
                Ok(result) => lock(recorder).finish(record, started.elapsed(), result),
                Err(error) => {
                    let mut failed = AgentToolResult::text(error.message.clone());
                    failed.is_error = Some(true);
                    lock(recorder).finish(record, started.elapsed(), &failed);
                }
            }
        }
        result
    }

    async fn execute_tool_with_updates_inner(
        &self,
        tool_name: &str,
        params: Value,
        options: ExecuteToolOptions,
        on_update: Option<maho_agent::types::AgentToolUpdateCallback>,
        tool_call_id: Option<&str>,
        arguments_prepared: bool,
    ) -> Result<AgentToolResult, ExecuteToolError> {
        let (mut active_tools, mut tool) = self.agent.tool_names_and_find(tool_name);
        if tool.is_none() && self.is_eval_only_policy_armed() {
            let armed = self
                .state()
                .eval_only_tool_names
                .clone()
                .unwrap_or_default();
            if armed.contains(tool_name) {
                tool = self.state().tool_registry.get(tool_name).cloned();
            }
        }
        let registered = self.state().tool_definitions.contains_key(tool_name);
        if tool.is_none()
            && options.activate_inactive_tool == Some(true)
            && registered
            && self.activate_lazy_tool(tool_name)
        {
            (active_tools, tool) = self.agent.tool_names_and_find(tool_name);
        }
        let Some(tool) = tool else {
            let known = self.state().tool_definitions.contains_key(tool_name);
            let code = if known {
                "inactive_tool"
            } else {
                "unknown_tool"
            };
            let active_list = if active_tools.is_empty() {
                "(none)".to_owned()
            } else {
                active_tools.join(", ")
            };
            let message = if known {
                format!("Tool {tool_name} is registered but inactive. Active tools: {active_list}")
            } else {
                format!("Unknown tool {tool_name}. Active tools: {active_list}")
            };
            return Err(ExecuteToolError {
                code: code.to_owned(),
                tool_name: tool_name.to_owned(),
                message,
                active_tools,
            });
        };
        let tool_call = maho_agent::types::AgentToolCall {
            id: tool_call_id
                .map(str::to_owned)
                .unwrap_or_else(|| format!("codemode-{}", uuid::Uuid::new_v4())),
            name: tool_name.to_owned(),
            arguments: params.as_object().cloned().unwrap_or_default(),
            ..Default::default()
        };
        let prepared = if arguments_prepared {
            tool_call
        } else {
            maho_agent::tool_arguments::prepare_agent_tool_call_arguments(&tool, &tool_call)
                .map_err(|message| ExecuteToolError {
                    code: "invalid_params".to_owned(),
                    tool_name: tool_name.to_owned(),
                    message,
                    active_tools: active_tools.clone(),
                })?
        };
        let mut input = maho_ai::utils::validation::validate_tool_arguments(&tool.tool, &prepared)
            .map_err(|error| ExecuteToolError {
                code: "invalid_params".to_owned(),
                tool_name: tool_name.to_owned(),
                message: error.to_string(),
                active_tools: active_tools.clone(),
            })?;
        let invocation = monitor_invocation::Invocation::new(
            Arc::as_ptr(&self.inner) as usize,
            self.monitor_generation.load(Ordering::SeqCst),
            prepared.id.clone(),
            prepared.name.clone(),
            input.clone(),
            self.cwd().into(),
        );
        let retirement = monitor_invocation::Retirement(invocation.clone());
        // One input identity per executed call, shared by its tool_call hook, body and tool_result hook.
        let input_identity = maho_agent::invocation_input::InvocationInput::new();
        maho_agent::invocation_input::scope_invocation_input(input_identity, monitor_invocation::CURRENT.scope(invocation.clone(), async move {
        let _retirement = retirement;
        let hook_result = {
            let mut guard = self.extension_runner.lock().await.clone();
            if let Some(runner) = guard.as_mut() {
                // The tool that issued this call through a codemode script, when there is one: a
                // permission dialog names it so a client can bind the prompt to a nested call.
                let parent_tool_call_id = NESTED_TOOL_SCOPE
                    .try_with(|scope| scope.0.clone())
                    .ok();
                let mut event = ToolCallEvent { tool_call_id: prepared.id.clone(), tool_name: prepared.name.clone(), input, parent_tool_call_id };
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
        result.content = normalize_tool_result_images(
            &result.content,
            Some(self.with_settings_manager(SettingsManager::get_image_auto_resize)),
        ).await.into_owned();
        Ok(result)
        })).await
    }

    /// `preflightToolCall`: run the `tool_call` extension hook. The Rust agent exposes no
    /// `_agentEventQueue`, so the `waitForEventQueue` wait is not applied.
    pub async fn preflight_tool_call(
        &self,
        tool_call: &maho_agent::types::AgentToolCall,
        input: Value,
    ) -> Option<(maho_ext_api::ToolCallEventResult, Value)> {
        let mut guard = self.extension_runner.lock().await.clone();
        let runner = guard.as_mut()?;
        if !runner.has_handlers(maho_ext_api::EventKind::ToolCall) {
            return None;
        }
        let mut event = ToolCallEvent {
            tool_call_id: tool_call.id.clone(),
            tool_name: tool_call.name.clone(),
            input,
            parent_tool_call_id: NESTED_TOOL_SCOPE.try_with(|scope| scope.0.clone()).ok(),
        };
        match runner.emit_tool_call(&mut event).await {
            Ok(result) => Some((result.unwrap_or_default(), event.input)),
            Err(error) => Some((
                maho_ext_api::ToolCallEventResult {
                    block: Some(true),
                    reason: Some(error.message),
                    terminate: None,
                },
                event.input,
            )),
        }
    }

    /// senpi `session.extensionRunner.getRegisteredCommands()`, read synchronously for the UI.
    pub fn extension_registered_commands(&self) -> Vec<maho_ext_api::ResolvedCommand> {
        self.extension_runner_snapshot()
            .map(|runner| runner.get_registered_commands())
            .unwrap_or_default()
    }

    /// senpi `command.getArgumentCompletions(prefix)` for an extension command, by invocation name.
    /// The autocomplete provider is synchronous here, so the completion future is polled once:
    /// registered completions resolve immediately; one still pending yields no suggestions.
    pub fn extension_command_argument_completions(
        &self,
        name: &str,
        prefix: &str,
    ) -> Option<Vec<maho_ext_api::AutocompleteItem>> {
        let runner = self.extension_runner_snapshot()?;
        let future = runner.get_command_argument_completions(name, prefix);
        let mut future = std::pin::pin!(future);
        match std::future::Future::poll(
            future.as_mut(),
            &mut std::task::Context::from_waker(std::task::Waker::noop()),
        ) {
            std::task::Poll::Ready(Ok(items)) => items,
            std::task::Poll::Ready(Err(_)) | std::task::Poll::Pending => None,
        }
    }

    /// senpi `session.extensionRunner.getMessageRenderer(customType)`.
    pub fn extension_message_renderer(
        &self,
        custom_type: &str,
    ) -> Option<maho_ext_api::MessageRenderer> {
        self.extension_runner_snapshot()?
            .get_message_renderer(custom_type)
    }

    /// Live extension figures, collected lazily by the on-demand memory report.
    pub fn extension_memory_reporters(&self) -> Vec<maho_ext_api::NamedMemoryReporter> {
        self.extension_runner_snapshot()
            .map(|runner| runner.get_memory_reporters())
            .unwrap_or_default()
    }

    /// senpi `getEntryRenderer(customType)` with its `getEntryRendererOptions(customType)`.
    pub fn extension_entry_renderer(
        &self,
        custom_type: &str,
    ) -> Option<(
        maho_ext_api::EntryRenderer,
        Option<maho_ext_api::EntryRendererOptions>,
    )> {
        self.extension_runner_snapshot()?
            .get_entry_renderer_owned(custom_type)
    }

    /// senpi's synchronous `session.extensionRunner` getter: the bound runner (its registrations are
    /// read live through the shared runtime), for UI paths that cannot await the runner lock.
    fn extension_runner_snapshot(&self) -> Option<ExtensionRunner> {
        self.state().extension_runner_snapshot.clone()
    }

    /// The extension runner currently bound to the session, if any.
    pub async fn extension_runner_bound(&self) -> bool {
        self.extension_runner.lock().await.is_some()
    }

    /// The filesystem policies the bound extensions registered, in extension order (live
    /// bind-time registrations win over load-time ones, as in `get_filesystem_policy_denied_roots`).
    pub async fn filesystem_policies(&self) -> Vec<maho_tools::FilesystemPolicy> {
        let guard = self.extension_runner.lock().await;
        let Some(runner) = guard.as_ref() else {
            return Vec::new();
        };
        runner
            .extensions
            .iter()
            .flat_map(|extension| {
                runner
                    .runtime
                    .live_filesystem_policies(&extension.identity.path)
                    .unwrap_or_else(|| extension.filesystem_policies.clone())
            })
            .collect()
    }

    pub fn extension_context_actions(&self) -> Arc<dyn maho_ext_api::ExtensionContextActions> {
        Arc::new(SessionExtensionActions(Arc::downgrade(&self.inner)))
    }

    pub fn weak_accessor(&self) -> Arc<dyn Fn() -> Option<Self> + Send + Sync> {
        let inner = Arc::downgrade(&self.inner);
        Arc::new(move || inner.upgrade().map(|inner| Self { inner }))
    }

    /// senpi `sdk.ts` `streamFn`: per request, read the live provider retry/timeout settings,
    /// resolve the request service tier from the session, merge provider attribution headers and run
    /// the extension `before_provider_headers` hook as the `transformHeaders` stage, then forward
    /// the payload/response hooks to the extension runner.
    pub(crate) fn install_default_stream_function(&self) {
        let weak = Arc::downgrade(&self.inner);
        self.agent
            .set_stream_function(Arc::new(move |model, context, options| {
                let stream = maho_ai::utils::event_stream::create_assistant_message_event_stream();
                let output = stream.clone();
                let weak = weak.clone();
                let model = model.clone();
                let context = context.clone();
                let cancellation = maho_ai::utils::abort::AbortController::new();
                let cancelled = cancellation.signal();
                stream.set_cancellation_handler(Box::new(move || {
                    Box::pin(async move {
                        cancellation.abort(None);
                        Ok(())
                    })
                }));
                let relay = tokio::spawn(async move {
                    if cancelled.aborted() {
                        output.end(None);
                        return;
                    }
                    let Some(inner) = weak.upgrade() else {
                        output.fail(maho_ai::utils::event_stream::StreamError::new(
                            "Session disposed",
                        ));
                        return;
                    };
                    let session = AgentSession { inner };
                    let mut options = options.unwrap_or_default();
                    if let Err(error) =
                        session.apply_stream_request_settings(&model, &mut options.simple)
                    {
                        output.fail(maho_ai::utils::event_stream::StreamError::new(error));
                        return;
                    }
                    let header_session = Arc::downgrade(&session.inner);
                    let header_model = model.clone();
                    let header_session_id = options.simple.stream.session_id.clone();
                    let transforms = maho_ai::models::ModelsRequestTransforms {
                        transform_headers: Some(Arc::new(
                            move |request_headers: ProviderHeaders| {
                                let weak = header_session.clone();
                                let model = header_model.clone();
                                let session_id = header_session_id.clone();
                                Box::pin(async move {
                                    let Some(inner) = weak.upgrade() else {
                                        return request_headers;
                                    };
                                    let session = AgentSession { inner };
                                    let headers = session.merge_request_attribution_headers(
                                        &model,
                                        session_id.as_deref(),
                                        request_headers,
                                    );
                                    let mut runner = session.extension_runner.lock().await.clone();
                                    match runner.as_mut() {
                                        Some(runner)
                                            if runner.has_handlers(
                                                maho_ext_api::EventKind::BeforeProviderHeaders,
                                            ) =>
                                        {
                                            match runner
                                                .emit_before_provider_headers(headers.clone())
                                                .await
                                            {
                                                Ok(headers) => headers,
                                                // senpi gates the hook on `headerRunner.isActive`; a stale runner
                                                // (the only error source) leaves the attributed headers as-is.
                                                Err(_) => headers,
                                            }
                                        }
                                        _ => headers,
                                    }
                                })
                                    as maho_ai::types::BoxFuture<'static, ProviderHeaders>
                            },
                        )),
                    };
                    let previous_payload = options.simple.stream.request.async_on_payload.take();
                    let payload_session = Arc::downgrade(&session.inner);
                    options.simple.stream.request.async_on_payload =
                        Some(Arc::new(move |payload, model, metadata| {
                            let previous = previous_payload.clone();
                            let weak = payload_session.clone();
                            Box::pin(async move {
                                // The event carries the resolved request model. The direct API lanes pass it
                                // as `model` and leave `metadata` unset, so fall back to it rather than
                                // dropping the model when only the metadata is absent.
                                let event_model = metadata
                                    .as_ref()
                                    .map(|request| request.model.clone())
                                    .unwrap_or_else(|| model.clone());
                                let payload = match previous {
                                    Some(previous) => {
                                        previous(payload.clone(), model, metadata.clone())
                                            .await?
                                            .unwrap_or(payload)
                                    }
                                    None => payload,
                                };
                                let inner = weak.upgrade().ok_or("Session disposed")?;
                                let runner =
                                    AgentSession { inner }.extension_runner.lock().await.clone();
                                match runner {
                                    Some(mut runner) => runner
                                        .emit_before_provider_request_with_metadata(
                                            payload,
                                            Some(event_model),
                                            metadata.map(|request| request.headers),
                                            None,
                                        )
                                        .await
                                        .map(Some)
                                        .map_err(|error| error.message),
                                    None => Ok(Some(payload)),
                                }
                            })
                        }));
                    let previous_response = options.simple.stream.request.async_on_response.take();
                    let response_session = Arc::downgrade(&session.inner);
                    options.simple.stream.request.async_on_response =
                        Some(Arc::new(move |response, model| {
                            let previous = previous_response.clone();
                            let weak = response_session.clone();
                            Box::pin(async move {
                                if let Some(previous) = previous {
                                    previous(response.clone(), model).await?;
                                }
                                let inner = weak.upgrade().ok_or("Session disposed")?;
                                let runner =
                                    AgentSession { inner }.extension_runner.lock().await.clone();
                                if let Some(mut runner) = runner {
                                    runner
                                        .emit(maho_ext_api::ExtensionEvent::AfterProviderResponse {
                                            status: response.status,
                                            headers: response.headers,
                                        })
                                        .await
                                        .map_err(|error| error.message)?;
                                }
                                Ok(())
                            })
                        }));
                    let upstream = session.model_runtime().stream_simple_with_transforms(
                        &model,
                        &context,
                        Some(options.simple),
                        transforms,
                    );
                    loop {
                        let next = tokio::select! {
                            biased;
                            _ = cancelled.cancelled() => break,
                            next = upstream.next() => next,
                        };
                        match next {
                            Ok(Some(event)) => output.push(event),
                            Ok(None) => break,
                            Err(error) => {
                                output.fail(error);
                                break;
                            }
                        }
                    }
                    if let Err(error) = upstream.cancel().await {
                        output.fail(error);
                        return;
                    }
                    match upstream.result_if_ready() {
                        Some(Ok(message)) => output.end(Some(message)),
                        Some(Err(error)) => output.fail(error),
                        None => output.end(None),
                    }
                });
                stream.set_cancellation_task(relay);
                stream
            }));
    }

    /// The request-option half of senpi `sdk.ts` `streamFn`. Caller-pinned values win; otherwise
    /// the live settings fill timeout, websocket connect timeout, the provider retry budget (a
    /// provider-declared profile owns it), the retry delay cap and the request service tier.
    /// senpi throws `Invalid <name> setting` from the timeout getters, failing the request.
    fn apply_stream_request_settings(
        &self,
        model: &Model,
        options: &mut maho_ai::types::SimpleStreamOptions,
    ) -> Result<(), String> {
        let (provider_retry, http_idle_timeout_ms, websocket_connect_timeout_ms) = self
            .with_settings_manager(|manager| {
                (
                    manager.get_provider_retry_settings(),
                    manager.get_http_idle_timeout_ms(),
                    manager.get_websocket_connect_timeout_ms(),
                )
            });
        let http_idle_timeout_ms = http_idle_timeout_ms?;
        // SDKs treat timeout=0 as 0ms (immediate timeout), not "no timeout"; max int32 disables it.
        let effective_timeout_ms = if http_idle_timeout_ms == 0 {
            i32::MAX as u64
        } else {
            http_idle_timeout_ms
        };
        let declared_policy =
            self.model_runtime()
                .get_provider(&model.provider)
                .and_then(|provider| {
                    maho_ai::models::Provider::retry_policy(provider.as_ref()).map(|policy| {
                        (
                            policy.provider_request.enabled,
                            policy.provider_request.max_retries,
                        )
                    })
                });
        let profile_max_retries = match declared_policy {
            None => provider_retry.max_retries,
            Some((true, max_retries)) => Some(max_retries),
            Some((false, _)) => Some(0),
        };
        let request = &mut options.stream.request;
        request.timeout_ms = request
            .timeout_ms
            .or(provider_retry.timeout_ms)
            .or(Some(effective_timeout_ms));
        request.max_retries = request.max_retries.or(profile_max_retries);
        request.max_retry_delay_ms = request
            .max_retry_delay_ms
            .or(Some(provider_retry.max_retry_delay_ms));
        options.stream.websocket_connect_timeout_ms =
            match options.stream.websocket_connect_timeout_ms {
                Some(pinned) => Some(pinned),
                None => websocket_connect_timeout_ms?,
            };
        if options.stream.session_id.is_none() {
            options.stream.session_id = Some(self.session_id());
        }
        // senpi sdk.ts streamFn: every model request runs in the session cwd, overriding the
        // caller's, so subprocess-backed lanes start there.
        options.stream.request.cwd = Some(self.cwd());
        options.service_tier = options
            .service_tier
            .or_else(|| self.resolve_request_service_tier(model));
        Ok(())
    }

    /// senpi `sdk.ts` `resolveRequestServiceTier`: the session's effective tier for the active
    /// model, else the request model's own catalog tier; only the OpenAI Responses family accepts it.
    fn resolve_request_service_tier(&self, request_model: &Model) -> Option<ServiceTierPreference> {
        if !matches!(
            request_model.api.as_str(),
            "openai-responses" | "openai-codex-responses"
        ) {
            return None;
        }
        if models_are_equal(Some(&self.model()), Some(request_model)) {
            return self.effective_service_tier().map(|tier| match tier {
                ServiceTier::Auto => ServiceTierPreference::Auto,
                ServiceTier::Flex => ServiceTierPreference::Flex,
                ServiceTier::Priority => ServiceTierPreference::Priority,
                ServiceTier::Ultrafast => ServiceTierPreference::Ultrafast,
            });
        }
        self.model_runtime()
            .get_compatibility_request_config(request_model)
            .service_tier
    }

    /// senpi `mergeProviderAttributionHeaders(model, settingsManager, sessionId, requestHeaders)`:
    /// session and attribution defaults first, the assembled request headers (including `None`
    /// deletions) last.
    fn merge_request_attribution_headers(
        &self,
        model: &Model,
        session_id: Option<&str>,
        request_headers: ProviderHeaders,
    ) -> ProviderHeaders {
        let mut merged: ProviderHeaders = self
            .with_settings_manager(|manager| {
                crate::provider_attribution::merge_provider_attribution_headers(
                    model,
                    manager,
                    session_id,
                    &[],
                )
            })
            .unwrap_or_default()
            .into_iter()
            .map(|(name, value)| (name, Some(value)))
            .collect();
        merged.extend(request_headers);
        merged
    }

    async fn renew_extension_runtime(
        &self,
        reason: maho_ext_api::SessionReason,
    ) -> Result<(), String> {
        let old = self.extension_runner.lock().await.clone();
        let Some(mut old) = old else {
            return Ok(());
        };
        let next = old
            .recreate_with_context(crate::sdk::extension_context::create(self))
            .await
            .map_err(|error| error.message)?;
        let paths: BTreeSet<_> = next
            .extensions
            .iter()
            .map(|extension| extension.identity.resolved_path.clone())
            .collect();
        let removed: Vec<_> = old
            .extensions
            .iter()
            .filter(|extension| !paths.contains(&extension.identity.resolved_path))
            .map(|extension| extension.identity.clone())
            .collect();
        old.invalidate("This extension ctx is stale after session replacement or reload.");
        old.runtime
            .dispose_providers()
            .map_err(|error| error.message)?;
        let mut active = self.get_active_tool_names();
        let mut hints = self.agent.removed_tool_hints();
        {
            let mut state = self.state();
            let activators = std::mem::take(&mut state.extension_lazy_activators);
            state.native_tool_search_injection_failure_source = None;
            state
                .lazy_tool_activators
                .retain(|activator| !activators.iter().any(|owned| Arc::ptr_eq(owned, activator)));
            for (name, previous) in std::mem::take(&mut state.extension_hint_backups) {
                if let Some(previous) = previous {
                    hints.insert(name, previous);
                } else {
                    hints.remove(&name);
                }
            }
            for (name, previous) in std::mem::take(&mut state.extension_tool_backups) {
                if let Some((entry, tool)) = previous {
                    state
                        .base_tool_definitions
                        .insert(name.clone(), entry.definition.clone());
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
        if !removed.is_empty() {
            old.emit_removed_extensions(reason, removed).await;
        }
        Ok(())
    }

    /// Rebinds how `reload`/session replacement recreate the bound extension runner (the CLI
    /// installs senpi's resource-loader re-resolution here).
    pub async fn set_extension_runtime_factory(
        &self,
        factory: maho_ext_host::runner::RuntimeFactory,
    ) {
        if let Some(runner) = self.extension_runner.lock().await.as_mut() {
            runner.set_runtime_factory(factory);
        }
    }

    async fn ensure_extension_recreation(&self) -> Result<(), String> {
        if let Some(runner) = self.extension_runner.lock().await.as_ref() {
            runner
                .ensure_recreation_available()
                .map_err(|error| error.message)?;
        }
        Ok(())
    }

    /// senpi `onBeforeSessionReplace` for this session's in-place replacements (new, switch, import, fork; never
    /// reload or dispose). Returns the id [`Self::remove_before_session_replace`] takes.
    pub fn on_before_session_replace(&self, listener: Arc<dyn Fn() + Send + Sync>) -> u64 {
        let id = self
            .next_before_session_replace
            .fetch_add(1, Ordering::Relaxed);
        lock(&self.before_session_replace).push((id, listener));
        id
    }

    /// Removes a listener [`Self::on_before_session_replace`] added.
    pub fn remove_before_session_replace(&self, id: u64) {
        lock(&self.before_session_replace).retain(|(listener, _)| *listener != id);
    }

    fn run_before_session_replace(&self) {
        let listeners: Vec<_> = lock(&self.before_session_replace)
            .iter()
            .map(|(_, listener)| listener.clone())
            .collect();
        for listener in listeners {
            listener();
        }
    }

    /// senpi `setBeforeSessionInvalidate`.
    pub fn set_before_session_invalidate(&self, callback: Option<Arc<dyn Fn() + Send + Sync>>) {
        *lock(&self.before_session_invalidate) = callback;
    }

    async fn invalidate_extension_runtime(&self) {
        let before = lock(&self.before_session_invalidate).clone();
        if let Some(before) = before {
            before();
        }
        self.monitor_generation.fetch_add(1, Ordering::SeqCst);
        lock(&self.settled_delivery).cancel();
        self.audit_queued_custom_receipts("session invalidated before custom message delivery");
        if let Some(runner) = self.extension_runner.lock().await.as_ref() {
            runner.invalidate("This extension ctx is stale after session replacement or reload.");
        }
    }

    /// Cursor states a model's real context ceiling on every conversation checkpoint. Once observed,
    /// it replaces the catalog guess on the live model so context usage, compaction thresholds and
    /// admission all size against the window the server will actually enforce.
    fn apply_observed_cursor_context_window(&self, model: &mut Model) {
        let Some(observed) =
            maho_ai::utils::cursor_context_limit::get_cursor_context_limit(&model.id)
        else {
            return;
        };
        if !observed.is_finite() || observed <= 0.0 || observed == model.context_window as f64 {
            return;
        }
        let previous = model.context_window;
        model.context_window = observed as u64;
        self.agent.set_model(model.clone());
        let mut data = Map::new();
        data.insert("modelId".to_owned(), Value::String(model.id.clone()));
        data.insert("previous".to_owned(), serde_json::json!(previous));
        data.insert("observed".to_owned(), serde_json::json!(observed));
        self.session_logger
            .info("cursor_context_window_observed", Some(&data));
    }

    /// Cursor request admission (upstream `_installAgentNextTurnRefresh` transformContext): per-result
    /// caps, then blanking the oldest tool result bodies while the history exceeds the window.
    fn admit_cursor_context(&self, transformed: Vec<AgentMessage>) -> Vec<AgentMessage> {
        let mut model = self.model();
        if model.provider != "cursor" && model.provider != "cursor-cli-oauth" {
            return transformed;
        }
        self.apply_observed_cursor_context_window(&mut model);
        let budget_bytes = crate::cursor_history_admission::cursor_admission_budget_bytes(
            model.context_window as f64,
        );
        let Ok(values) = transformed
            .iter()
            .map(serde_json::to_value)
            .collect::<Result<Vec<Value>, _>>()
        else {
            return transformed;
        };
        let admission = crate::cursor_history_admission::admit_cursor_history(
            Some(&values),
            budget_bytes,
            None,
        );
        if admission.blanked_tool_results > 0 {
            let mut data = Map::new();
            data.insert(
                "blankedToolResults".to_owned(),
                serde_json::json!(admission.blanked_tool_results),
            );
            data.insert(
                "bytesBefore".to_owned(),
                serde_json::json!(admission.bytes_before),
            );
            data.insert(
                "bytesAfter".to_owned(),
                serde_json::json!(admission.bytes_after),
            );
            data.insert("budgetBytes".to_owned(), serde_json::json!(budget_bytes));
            self.session_logger
                .info("cursor_admission_truncated", Some(&data));
        }
        if admission.over_budget {
            // The request is still admitted: Cursor answers an oversized history with a 0-token
            // resource_exhausted, which the session layer compacts.
            let mut data = Map::new();
            data.insert("bytes".to_owned(), serde_json::json!(admission.bytes_after));
            data.insert("budgetBytes".to_owned(), serde_json::json!(budget_bytes));
            self.session_logger
                .warn("cursor_admission_over_budget", Some(&data));
        }
        if !admission.changed {
            return transformed;
        }
        match admission.messages.map(|messages| {
            messages
                .into_iter()
                .map(serde_json::from_value)
                .collect::<Result<Vec<AgentMessage>, _>>()
        }) {
            Some(Ok(messages)) => messages,
            _ => transformed,
        }
    }

    /// Bind the extension runner the tool hooks read at execution time.
    /// senpi `setControlEndpointHost`: installed by the interactive mode before it binds extensions.
    pub fn set_control_endpoint_host(
        &self,
        host: Option<Arc<dyn crate::session_control_actions::ControlEndpointHost>>,
    ) {
        *lock(&self.control_endpoint_host) = host;
    }

    /// The current extension generation's `pi.session`, created with that generation's runner.
    pub fn session_control(
        &self,
    ) -> Option<Arc<crate::session_control_actions::GenerationSessionControlActions>> {
        lock(&self.session_control).clone()
    }

    /// senpi `_buildRuntime` -> `createSessionControlActions`: a new extension generation gets its own `pi.session`,
    /// returning the generation it replaces.
    fn install_session_control(
        &self,
    ) -> Option<Arc<crate::session_control_actions::GenerationSessionControlActions>> {
        let host = Arc::downgrade(&self.inner);
        let generation = crate::session_control_actions::create_session_control_actions(
            crate::session_control_actions::SessionControlActionDeps {
                base: Arc::new(SessionControlBase(Arc::downgrade(&self.inner))),
                host: Arc::new(move || {
                    host.upgrade()
                        .and_then(|inner| lock(&inner.control_endpoint_host).clone())
                }),
            },
        );
        lock(&self.session_control).replace(Arc::new(generation))
    }

    /// senpi `_rebuildRuntimeForReload` finally: the control endpoints the retiring generation registered drain
    /// through its closures, so they go with it; a failed retirement is a `<session-control>` reload error and does
    /// not block the new generation (senpi#3246).
    async fn retire_session_control(
        &self,
        retired: Option<Arc<crate::session_control_actions::GenerationSessionControlActions>>,
    ) {
        let Some(retired) = retired else {
            return;
        };
        if let Err(error) = retired.retire().await
            && let Some(runner) = self.extension_runner.lock().await.as_mut()
        {
            runner.emit_error(ExtensionError {
                extension_path: "<session-control>".into(),
                event: "reload".into(),
                error,
                stack: None,
            });
        }
    }

    pub async fn set_extension_runner(&self, mut runner: ExtensionRunner) {
        // The first generation's `pi.session`; a reload installs the next one before it renews the runner.
        if lock(&self.session_control).is_none() {
            self.install_session_control();
        }
        // senpi `_applyExtensionBindings`: the bound `onError` listener observes every error the
        // runner emits (handler failures included), read at emit time so a later rebind wins. A
        // listener copied onto a replacement runner stays inert once its session is disposed.
        let weak = Arc::downgrade(&self.inner);
        runner.set_session_error_listener(Some(Arc::new(move |error: &ExtensionError| {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            let listener = {
                let state = lock(&inner.state);
                if state.disposed {
                    None
                } else {
                    state.extension_error_listener.clone()
                }
            };
            if let Some(listener) = listener {
                listener(error);
            }
        })));
        let weak = Arc::downgrade(&self.inner);
        runner.set_shutdown_budget_resolver(Arc::new(move || {
            let Some(inner) = weak.upgrade() else { return (2_000, 10_000); };
            AgentSession { inner }.with_settings_manager(|manager| {
                let parse = |key: &str, default| match manager.get_value(key) {
                    None => Some(default), Some(value) => crate::http_dispatcher::parse_http_idle_timeout_ms(value),
                };
                match (parse("sessionShutdownHandlerWarnMs", 2_000), parse("sessionShutdownHandlerTimeoutMs", 10_000)) {
                    (Some(warn), Some(timeout)) => (warn, timeout),
                    (warn, _) => {
                        // senpi runner.ts `resolveSessionShutdownBudget`: an invalid setting is reported.
                        let (key, value) = if warn.is_none() { ("sessionShutdownHandlerWarnMs", manager.get_value("sessionShutdownHandlerWarnMs")) }
                            else { ("sessionShutdownHandlerTimeoutMs", manager.get_value("sessionShutdownHandlerTimeoutMs")) };
                        let value = value.map(|value| value.as_str().map_or_else(|| value.to_string(), str::to_owned)).unwrap_or_default();
                        crate::output_guard::maho_write_stderr(&format!(
                            "Using the default session_shutdown handler budget (warn 2000ms, hard cap 10000ms, quit 2000ms): Invalid {key} setting: {value}\n"));
                        (2_000, 10_000)
                    }
                }
            })
        }));
        // senpi runner.ts `resolveSessionShutdownBudget`: the total quit budget comes from settings; an
        // invalid value is reported once per read and the shipped default stands.
        let weak = Arc::downgrade(&self.inner);
        runner.set_shutdown_quit_budget_resolver(Arc::new(move || {
            let default_ms = crate::settings_manager::DEFAULT_SESSION_SHUTDOWN_QUIT_BUDGET_MS;
            let Some(inner) = weak.upgrade() else { return default_ms; };
            AgentSession { inner }.with_settings_manager(|manager| match manager.get_session_shutdown_quit_budget_ms() {
                Ok(budget) => budget,
                Err(message) => {
                    crate::output_guard::maho_write_stderr(&format!(
                        "Using the default session_shutdown handler budget (warn 2000ms, hard cap 10000ms, quit {default_ms}ms): {message}\n"));
                    default_ms
                }
            })
        }));
        // senpi `_applyExtensionBindings` `setEventHandlerBudget` (senpi#3650): the lifecycle handler
        // budget comes from settings, every stall stage is logged and a warn-stage stall is shown, and
        // a wait belongs to the run that started it.
        let weak = Arc::downgrade(&self.inner);
        let budget: maho_ext_host::runner::handler_budget::HandlerBudgetResolver =
            Arc::new(move || {
                let Some(inner) = weak.upgrade() else {
                    return Ok(
                        maho_ext_host::runner::handler_budget::ExtensionHandlerBudget::DEFAULT,
                    );
                };
                AgentSession { inner }.with_settings_manager(|manager| {
                    Ok(
                        maho_ext_host::runner::handler_budget::ExtensionHandlerBudget {
                            warn_ms: manager.get_extension_event_handler_warn_ms()?,
                            timeout_ms: manager.get_extension_event_handler_timeout_ms()?,
                        },
                    )
                })
            });
        let weak = Arc::downgrade(&self.inner);
        let on_stall: maho_ext_host::runner::handler_budget::HandlerStallListener =
            Arc::new(move |stall| {
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                let session = AgentSession { inner };
                crate::extension_handler_stall_report::report_extension_handler_stall(
                    &crate::extension_handler_stall_report::ExtensionHandlerStall::from(stall),
                    &session.session_logger,
                    |message, _| {
                        session.notify_ui(message, maho_ext_api::NotificationType::Warning);
                    },
                );
            });
        let scope = Arc::clone(&self.handler_turn_scope);
        runner.set_event_handler_budget(
            budget,
            on_stall,
            Some(Arc::new(move || scope.load(Ordering::SeqCst))),
        );
        // senpi runner.ts: a slow `session_shutdown` handler warns through `console.warn`.
        runner.set_warning_listener(Some(Arc::new(|message: &str| {
            crate::output_guard::maho_write_stderr(&format!("{message}\n"));
        })));
        let weak = Arc::downgrade(&self.inner);
        *lock(&self.extension_rpc_event_forwarding) = Some(runner.events.on(
            crate::event_bus::EXTENSION_RPC_EVENT_CHANNEL,
            Arc::new(move |event: &Value| {
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                let Some(name) = event.get("name").and_then(Value::as_str) else {
                    return;
                };
                let data = event.get("data").cloned().unwrap_or(Value::Null);
                let listeners = lock(&inner.extension_rpc_event_listeners).clone();
                for listener in listeners {
                    listener(name, &data);
                }
            }),
        ));
        lock(&self.lifecycle_fact_subscriptions).clear();
        for channel in [
            "wake_source_state",
            "goal_store_changed",
            "goal_continuation_timer_state",
            "turn_end_liveness",
            "goal_lifecycle_facts",
            "security_notice",
        ] {
            let weak = Arc::downgrade(&self.inner);
            let subscription = runner.events.on(
                channel,
                Arc::new(move |data| {
                    if let Some(inner) = weak.upgrade() {
                        let session = AgentSession { inner };
                        if channel == "security_notice" {
                            session.accept_security_notice(data);
                        }
                        if channel == "goal_lifecycle_facts"
                            && data["sessionId"] == session.session_id()
                        {
                            let goal = data.get("goal").filter(|g| !g.is_null());
                            session.update_lifecycle_facts(Vec::new(), Vec::new(), goal, None);
                        }
                        if channel == "turn_end_liveness"
                            && data["sessionId"] == session.session_id()
                        {
                            *lock(&session.lifecycle_turn_facts) = data.get("input").cloned();
                            *lock(&session.lifecycle_goal) =
                                data.get("goal").filter(|v| !v.is_null()).cloned();
                            session.refresh_lifecycle();
                        }
                        session.event_bus.emit(channel, data);
                    }
                }),
            );
            lock(&self.lifecycle_fact_subscriptions).push(subscription);
        }
        let weak = Arc::downgrade(&self.inner);
        *lock(&self.wake_source_subscription) = Some(runner.events.on(
            "wake_source_state",
            Arc::new(move |data| {
                if let Some(inner) = weak.upgrade() {
                    lock(&inner.state).wake_sources.observe(data);
                    if let (Some(source), Some(count)) =
                        (data["source"].as_str(), data["activeCount"].as_f64())
                        && let Some(registry) = lock(&inner.work_obligations).as_mut()
                    {
                        let kind = if data["kind"] == "children" {
                            crate::work_custody_record::ObligationKind::Children
                        } else {
                            crate::work_custody_record::ObligationKind::Background
                        };
                        registry.sync_source(
                            source,
                            kind,
                            count,
                            crate::lifecycle_wait_items::wake_source_items(data),
                        );
                    }
                    AgentSession { inner }.refresh_lifecycle();
                }
            }),
        ));
        let weak = Arc::downgrade(&self.inner);
        *lock(&self.extension_rpc_event_forward) = Some(runner.events.on(
            crate::event_bus::EXTENSION_RPC_EVENT_CHANNEL,
            Arc::new(move |data: &Value| {
                if let Some(inner) = weak.upgrade() {
                    inner
                        .event_bus
                        .emit(crate::event_bus::EXTENSION_RPC_EVENT_CHANNEL, data);
                }
            }),
        ));
        if let Ok(mut context) = runner.create_context() {
            context.cwd = self.cwd().into();
            context.agent_dir = self.agent_dir().into();
            context.model = Some(self.model());
            context.model_registry =
                Arc::new(ExtensionModelRegistryView::new(self, runner.events.clone()));
            let weak = Arc::downgrade(&self.inner);
            context.wait_for_idle_fn = Arc::new(move || {
                let weak = weak.clone();
                Box::pin(async move {
                    let inner = weak.upgrade().unwrap_or_else(|| {
                        std::panic::panic_any(maho_ext_api::ExtensionFailure::new(
                            "Agent session has been disposed",
                        ))
                    });
                    AgentSession { inner }.wait_for_idle().await;
                })
            });
            context.session_manager = Arc::new(ExtensionSessionManagerView {
                session: Arc::downgrade(&self.inner),
                id: self.session_id(),
                file: self.session_file().map(Into::into),
            });
            runner.bind_core(
                Arc::new(SessionExtensionActions(Arc::downgrade(&self.inner))),
                context,
            );
        }
        if let Err(error) = runner.bind_session_actions(Arc::new(SessionExtensionActions(
            Arc::downgrade(&self.inner),
        ))) {
            self.emit(AgentSessionEvent::ContinuationError {
                error_message: error.message,
            });
        }
        if let Err(error) = runner.bind_context_actions(Arc::new(SessionExtensionActions(
            Arc::downgrade(&self.inner),
        ))) {
            self.emit(AgentSessionEvent::ContinuationError {
                error_message: error.message,
            });
        }
        if let Err(error) = runner.bind_providers(Arc::new(SessionProviderActions {
            runtime: crate::agent_session_runtime::ExtensionModelRuntimeActions(Mutex::new(
                self.model_runtime().clone(),
            )),
            session: Arc::downgrade(&self.inner),
        })) {
            self.emit(AgentSessionEvent::ContinuationError {
                error_message: error.message,
            });
        }
        if let Ok(context) = runner.create_context() {
            let (events, mut receiver) = tokio::sync::mpsc::unbounded_channel();
            self.state().extension_event_sender = Some(events.clone());
            self.state().extension_events = Some(runner.events.clone());
            // Retain MCP wire-status snapshots the moment they are published on the runner bus
            // (the channel carries an erased `Arc<dyn Any>`), so an extension that only holds the
            // bus reaches the session's retained snapshot. Installed here, before attach settles.
            let retention = Arc::downgrade(&self.inner);
            *lock(&self.mcp_wire_status_retention) = Some(runner.events.on_native::<Arc<
                dyn std::any::Any + Send + Sync,
            >>(
                MCP_WIRE_STATUS_CHANGED_EVENT,
                Arc::new(move |value| {
                    if let Some(inner) = retention.upgrade() {
                        lock(&inner.state).mcp_wire_status = Some(value.clone());
                    }
                }),
            ));
            let weak = Arc::downgrade(&self.inner);
            let ui = maho_ext_host::ui::LifecycleUi::new(
                context.ui,
                runner.runtime.clone(),
                Arc::new(move |event| {
                    let _ = events.send(event);
                }),
            );
            if let Err(error) = runner.bind_ui(Arc::new(ui)) {
                self.emit(AgentSessionEvent::ContinuationError {
                    error_message: error.message,
                });
            }
            tokio::spawn(async move {
                while let Some(event) = receiver.recv().await {
                    let Some(inner) = weak.upgrade() else {
                        break;
                    };
                    AgentSession { inner }.dispatch_extension_event(event).await;
                }
            });
        }
        let tool_runner = runner.clone();
        let context_factory: maho_ext_host::wrapper::ToolContextFactory =
            Arc::new(move || tool_runner.create_context());
        self.state().extension_tool_context =
            Some((runner.runtime.clone(), context_factory.clone()));
        let mut active = self.get_active_tool_names();
        for registered in runner.get_all_registered_tools() {
            let definition = registered.definition.clone();
            let source = registered.source_info.clone();
            let tool = self.wrap_extension_tool(
                registered,
                runner.runtime.clone(),
                context_factory.clone(),
            );
            if normalize_tool_exposure(&definition, source.clone()).exposure != ToolExposure::Search
                && !active.contains(&definition.name)
            {
                active.push(definition.name.clone());
            }
            self.register_extension_tool(definition, source, tool);
        }
        self.set_active_tools_by_name(active);
        self.state().extension_commands = runner
            .get_registered_commands()
            .into_iter()
            .map(|command| maho_ext_api::SlashCommandInfo {
                name: command.invocation_name,
                description: command.command.description,
                argument_hint: command.command.argument_hint,
                source_info: Some(command.command.source_info),
            })
            .collect();
        let command_runner = runner.clone();
        self.state().extension_command_catalog = Some(Arc::new(move || {
            command_runner
                .get_registered_commands()
                .into_iter()
                .map(|command| maho_ext_api::SlashCommandInfo {
                    name: command.invocation_name,
                    description: command.command.description,
                    argument_hint: command.command.argument_hint,
                    source_info: Some(command.command.source_info),
                })
                .collect()
        }));
        self.state().extension_runner_snapshot = Some(runner.clone());
        self.state().extension_bound_ui = None;
        *self.extension_runner.lock().await = Some(runner);
        self.extension_runner_epoch.fetch_add(1, Ordering::SeqCst);
        self.apply_extension_bindings().await;
        if self.state().uses_default_stream_function {
            self.install_default_stream_function();
        }
        let weak = Arc::downgrade(&self.inner);
        self.agent
            .set_before_tool_call(Some(Arc::new(move |context, signal| {
                let weak = weak.clone();
                Box::pin(async move {
                    let inner = weak.upgrade()?;
                    let session = AgentSession { inner };
                    if let Some(id) = session.tool_barrier.id()
                        && session.get_tool_workspace_effect(&context.tool_call.name) != crate::session_work_barrier::ToolWorkspaceEffect::Read {
                            session.emit_extension_event("tool_barrier_waiting", &json!({"type":"tool_barrier_waiting","id":id,"toolCallId":context.tool_call.id,"toolName":context.tool_call.name}));
                        }
                    session
                        .wait_for_tool_barrier(&context.tool_call.name, signal.as_ref())
                        .await;
                    if signal.as_ref().is_some_and(|signal| signal.aborted()) {
                        return Some(maho_agent::types::BeforeToolCallResult {
                            block: Some(true),
                            reason: Some("Tool call aborted before execution".into()),
                            ..Default::default()
                        });
                    }
                    let original = context.args.clone();
                    session
                        .preflight_tool_call(&context.tool_call, context.args)
                        .await
                        .map(|(result, input)| maho_agent::types::BeforeToolCallResult {
                            block: result.block,
                            reason: result.reason,
                            terminate: result.terminate,
                            args: (input != original).then_some(input),
                        })
                })
            })));
        let weak = Arc::downgrade(&self.inner);
        self.agent
            .set_transform_context(Some(Arc::new(move |messages, signal| {
                let weak = weak.clone();
                Box::pin(async move {
                    let Some(inner) = weak.upgrade() else {
                        return messages;
                    };
                    let session = AgentSession { inner };
                    let extension_signal = maho_ext_api::AbortSignal::default();
                    if signal
                        .as_ref()
                        .is_some_and(maho_ai::utils::abort::AbortSignal::aborted)
                    {
                        extension_signal.abort();
                    }
                    session.state().extension_event_signal = Some(extension_signal.clone());
                    let _bridge = signal.map(|signal| {
                        AbortSignalBridge(tokio::spawn(async move {
                            signal.cancelled().await;
                            extension_signal.abort();
                        }))
                    });
                    // The idle path tokenizes the runtime messages in place; a provider request that
                    // starts before any reader re-hydrates them materializes here so tokens never reach a model.
                    let messages = if session
                        .runtime_messages_tokenized
                        .swap(false, Ordering::SeqCst)
                    {
                        let state_messages = session.agent.state().messages().to_vec();
                        session
                            .agent
                            .set_messages(session.materialize_resident_messages(state_messages));
                        session.materialize_resident_messages(messages)
                    } else {
                        messages
                    };
                    let mut runner = session.extension_runner.lock().await.clone();
                    let transformed = match runner.as_mut() {
                        Some(runner) => {
                            // PERF-DIVERGENCE(senpi agent-session.ts): upstream looks up entry ids per message in O(1) via a WeakMap; we skip the O(history) context rebuild when no Context handler consumes the ids (emit_context_owned returns early without handlers).
                            let entry_ids = if runner.has_handlers(maho_ext_api::EventKind::Context)
                            {
                                session.session_context_entry_ids(&messages)
                            } else {
                                Vec::new()
                            };
                            match runner.emit_context_owned(messages, &entry_ids, None).await {
                                Ok(messages) => messages,
                                Err(failure) => {
                                    session.emit(AgentSessionEvent::ContinuationError {
                                        error_message: failure.error.message,
                                    });
                                    failure.messages
                                }
                            }
                        }
                        None => messages,
                    };
                    let transformed = lock(&session.request_shape_recovery).apply(transformed);
                    session.admit_cursor_context(transformed)
                })
            })));
    }

    /// The three command groups `get_commands` merges, each still classified by its real source.
    /// Upstream `buildRpcCommandsForSession` reads the same three: the extension runner's registered
    /// commands, the prompt templates, and raw skills. Each consumer applies its own skill prefix.
    pub fn command_groups(
        &self,
    ) -> (
        Vec<maho_ext_api::SlashCommandInfo>,
        Vec<maho_ext_api::SlashCommandInfo>,
        Vec<maho_ext_api::SlashCommandInfo>,
    ) {
        let extensions = {
            let state = self.state();
            state
                .extension_command_catalog
                .as_ref()
                .map_or_else(|| state.extension_commands.clone(), |catalog| catalog())
        };
        let source_info = |source: crate::source_info::SourceInfo| maho_ext_api::SourceInfo {
            path: source.path,
            source: source.source,
            base_dir: source.base_dir,
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
        let templates = self
            .prompt_templates()
            .into_iter()
            .map(|template| maho_ext_api::SlashCommandInfo {
                name: template.name,
                description: Some(template.description),
                argument_hint: template.argument_hint,
                source_info: Some(source_info(template.source_info)),
            })
            .collect();
        let skills = self
            .state()
            .skills
            .clone()
            .into_iter()
            .map(|skill| maho_ext_api::SlashCommandInfo {
                name: skill.name,
                description: Some(skill.description),
                argument_hint: None,
                source_info: Some(source_info(skill.source_info)),
            })
            .collect();
        (extensions, templates, skills)
    }

    pub fn get_commands(&self) -> Vec<maho_ext_api::SlashCommandInfo> {
        let (mut commands, templates, skills) = self.command_groups();
        commands.extend(templates);
        commands.extend(skills.into_iter().map(|mut skill| {
            skill.name = format!("skill:{}", skill.name);
            skill
        }));
        commands
    }

    pub async fn continue_session(&self) -> Result<(), String> {
        self.continue_session_internal(None).await
    }

    async fn continue_session_internal(
        &self,
        expected_abort_generation: Option<u64>,
    ) -> Result<(), String> {
        let _admission = self.prompt_admission.lock().await;
        let _work = self.work_barrier.begin();
        if expected_abort_generation.is_some_and(|generation| {
            generation != self.user_abort_generation.load(Ordering::SeqCst)
        }) {
            return Ok(());
        }
        self.state().user_aborted = false;
        self.restore_fallback_primary().await;
        let prior_compaction_generation = self.compaction_state().generation();
        self.revalidate_scheduled_continuation_admission().await?;
        if expected_abort_generation.is_some_and(|generation| {
            generation != self.user_abort_generation.load(Ordering::SeqCst)
        }) {
            return Ok(());
        }
        let lifecycle = self.compaction_state();
        if lifecycle.generation() > prior_compaction_generation && lifecycle.status() == "completed"
        {
            let mut messages = self.messages();
            if messages
                .last()
                .and_then(AgentMessage::as_assistant)
                .is_some_and(|message| {
                    matches!(message.stop_reason, StopReason::Error | StopReason::Aborted)
                })
            {
                messages.pop();
                self.agent.set_messages(messages);
                self.state().message_revision += 1;
            }
        }
        self.agent_run_active.store(true, Ordering::SeqCst);
        self.hydrate_runtime_messages_for_run();
        self.agent
            .set_abort_server_side_fallback(Some(self.abort_server_side_fallback_enabled()));
        if let Err(error) = self
            .agent
            .continue_with_queued_messages(Default::default())
            .await
        {
            self.settle_refused_continuation(error);
            self.agent_run_active.store(false, Ordering::SeqCst);
            return Err(error.to_string());
        }
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

    /// Id of the entry nearest the leaf on the current branch with `type == kind` (and the given
    /// `message.role`): the same result as `current_branch().iter().rev().find(..)`, but resident
    /// entries are walked by reference without materializing any of them.
    fn latest_current_branch_entry_id(&self, kind: &str, role: Option<&str>) -> Option<String> {
        self.with_session_manager(|manager| manager.latest_current_branch_entry_id(kind, role))
    }

    async fn revalidate_continuation_admission(&self, proactive: bool) -> Result<(), String> {
        let model = self.model();
        let latest_assistant_entry =
            self.latest_current_branch_entry_id("message", Some("assistant"));
        if latest_assistant_entry.is_some_and(|id| {
            self.state()
                .post_compaction_usage_exempt_entries
                .contains(&id)
        }) {
            return Ok(());
        }
        let raw = self.with_settings_manager(|manager| manager.get_value("compaction").cloned());
        let settings = raw
            .map(serde_json::from_value::<crate::compaction_settings_access::CompactionSettings>)
            .transpose()
            .map_err(|error| error.to_string())?;
        let resolved = crate::compaction_settings_resolver::resolve_compaction_settings(
            settings.as_ref(),
            Some(crate::compaction_settings_access::CompactionModelSelector {
                provider: &model.provider,
                id: &model.id,
            }),
        )?;
        if !resolved.enabled || self.is_compaction_delegated() {
            return Ok(());
        }
        let messages = self.with_session_manager(|manager| manager.current_context().messages);
        let messages = crate::messages::filter_context_excluded_messages(messages);
        let estimate = crate::compaction::estimate_context_tokens(&messages);
        let stale_usage = proactive
            && self.with_session_manager(|manager| {
                let branch = manager.current_branch();
                let Some(boundary) = branch
                    .iter()
                    .rposition(|entry| entry["type"] == "compaction")
                else {
                    return false;
                };
                crate::compaction::get_last_assistant_usage(&branch[boundary + 1..]).is_none()
            });
        let tokens = if stale_usage {
            messages
                .iter()
                .map(crate::compaction::estimate_tokens)
                .sum()
        } else {
            estimate.tokens
        };
        let reserve = if resolved.reserve_scaling_enabled {
            crate::compaction::compaction::resolve_reserve_tokens(
                model.context_window as f64,
                resolved.reserve_tokens as f64,
            ) as u64
        } else {
            resolved.reserve_tokens as u64
        };
        let at_hard_limit = tokens > model.context_window.saturating_sub(reserve);
        let ratio = match model.context_window {
            0 => 0.5,
            1..=16_000 => 0.45,
            16_001..=32_000 => 0.5,
            32_001..=64_000 => 0.55,
            64_001..=128_000 => 0.6,
            128_001..=512_000 => 0.7,
            _ => 0.8,
        };
        let over_proactive_threshold =
            model.context_window > 0 && tokens as f64 >= model.context_window as f64 * ratio;
        if !(at_hard_limit || proactive && over_proactive_threshold) {
            return Ok(());
        }
        let result = self
            .compact_for_model_splitting_turn(
                &model,
                if proactive { "pre-prompt" } else { "threshold" },
            )
            .await;
        let on_cooldown = matches!(self.compaction_state(), crate::compaction::lifecycle::CompactionLifecycleState::Failed(_, _, Some(cause), _)
            if cause == "circuit-breaker");
        if result.is_err() && at_hard_limit && !self.is_compaction_delegated() && !on_cooldown {
            return Err("Compaction required before provider request".to_owned());
        }
        Ok(())
    }

    async fn emit_agent_settled(&self) {
        if self.is_streaming() {
            self.agent.wait_for_idle().await;
        }
        if !self.agent_run_active.load(Ordering::SeqCst) {
            self.state().abort_provenance.close_agent_end_boundary();
            return;
        }
        self.agent_run_active.store(false, Ordering::SeqCst);
        let generation = self.user_abort_generation.load(Ordering::SeqCst);
        let runtime_generation = self.monitor_generation.load(Ordering::SeqCst);
        let epoch = self.settlement_epoch.fetch_add(1, Ordering::SeqCst) + 1;
        lock(&self.settled_delivery).begin(generation);
        // Match senpi's _settlingWithBackgroundWork (agent-session.ts:1896/1905): the ctx idle gate
        // reports busy for the agent_settled window only while background wake sources are active,
        // and the flag is restored on every exit path via the guard, as senpi does in its `finally`.
        self.settling_with_background_work
            .store(self.state().wake_sources.has_active(), Ordering::SeqCst);
        let settling = SettlingBackgroundWorkGuard(&self.settling_with_background_work);
        self.dispatch_extension_event(maho_ext_api::ExtensionEvent::AgentSettled)
            .await;
        self.emit(AgentSessionEvent::AgentSettled);
        // senpi emits agent_settled first, then delivers a late User join as session_abort. The take
        // and the boundary close happen atomically so a join arriving after this point is not
        // swallowed (source cleared -> normal abort path) and the captured join is delivered once.
        let late_user_abort = self.state().abort_provenance.take_late_join_and_close();
        if late_user_abort {
            self.dispatch_extension_event(maho_ext_api::ExtensionEvent::SessionAbort)
                .await;
            self.emit(AgentSessionEvent::SessionAbort);
        }
        let batch =
            lock(&self.settled_delivery).finish(self.user_abort_generation.load(Ordering::SeqCst));
        drop(settling);
        for action in batch.actions {
            action();
        }
        let session = self.clone();
        tokio::spawn(async move {
            for claim in batch.turn_claims {
                if claim.disposition().await
                    == Some(crate::agent_settled_delivery::DeferredTurnDisposition::Started)
                {
                    return;
                }
            }
            session.wait_for_idle().await;
            if !session.state().disposed
                && session.monitor_generation.load(Ordering::SeqCst) == runtime_generation
                && session.settlement_epoch.load(Ordering::SeqCst) == epoch
                && !session.is_streaming()
                && !session.agent_run_active.load(Ordering::SeqCst)
                && !session.work_barrier.has_active_work()
            {
                session.audit_queued_custom_receipts(
                    "custom message left pending queue without being appended",
                );
                session.release_idle_runtime_messages();
                // senpi `_emitAgentIdleAfterDeferredTurns`: the run settled idle; its deadline is over.
                if let Some(tracker) = session.deadline_tracker.get() {
                    tracker.disarm();
                }
                session.emit(AgentSessionEvent::AgentIdle);
            }
            // Run settlement is a settle point even when a deferred turn takes over above.
            session.settle_input_release().await;
        });
    }

    async fn emit_late_user_abort(&self) {
        let joined = self.state().abort_provenance.take_late_user_join();
        if joined {
            self.dispatch_extension_event(maho_ext_api::ExtensionEvent::SessionAbort)
                .await;
            self.emit(AgentSessionEvent::SessionAbort);
        }
    }

    async fn apply_extension_bindings(&self) {
        let (ui, mode, has_ui) = {
            let state = self.state();
            (
                state.extension_ui_context.clone(),
                state.extension_mode,
                state.extension_ui_context.is_some(),
            )
        };
        if let Some(ui) = ui
            && let Err(error) = self.rebind_extension_ui(ui).await
        {
            self.emit(AgentSessionEvent::ContinuationError {
                error_message: error,
            });
        }
        let error = {
            let mut runner = self.extension_runner.lock().await;
            runner
                .as_mut()
                .and_then(|runner| runner.set_extension_mode(mode, has_ui).err())
        };
        if let Some(error) = error {
            self.emit(AgentSessionEvent::ContinuationError {
                error_message: error.message,
            });
        }
    }

    /// `_installAgentNextTurnRefresh`: before every follow-on provider request of a run, enforce
    /// threshold compaction when a continuation is about to be admitted, then refresh the system
    /// prompt, tools, model and thinking level/selection the next turn is sent with.
    ///
    /// Adaptations: the resident-store rehydration and Cursor admission belong to other parity rows;
    /// the refreshed prompt is the agent's live prompt, which `rebuild_system_prompt` keeps equal to
    /// `override ?? base`; a required compaction that fails ends the run through the loop's
    /// preparation failure, the native form of the upstream callback throwing `RequiredCompactionError`.
    fn install_agent_next_turn_refresh(&self) {
        let previous: Option<maho_agent::PrepareNextTurnWithContext> =
            self.agent.prepare_next_turn_with_context().or_else(|| {
                self.agent.prepare_next_turn().map(
                    |without| -> maho_agent::PrepareNextTurnWithContext {
                        Arc::new(move |_turn, signal| without(signal))
                    },
                )
            });
        let weak = Arc::downgrade(&self.inner);
        self.agent
            .set_prepare_next_turn_with_context(Some(Arc::new(move |turn, signal| {
                let weak = weak.clone();
                let previous = previous.clone();
                Box::pin(async move {
                    let inner = weak.upgrade()?;
                    let session = AgentSession { inner };
                    let compacted_before_callback =
                        match session.compact_before_next_admission(&turn).await {
                            Ok(compacted) => compacted,
                            Err(failure) => {
                                return Some(maho_agent::types::AgentLoopTurnUpdate {
                                    failure: Some(failure),
                                    ..Default::default()
                                });
                            }
                        };
                    let boundary_changed = session.boundary_context_changed.swap(false, Ordering::SeqCst);
                    if previous.is_none() {
                        let messages = if compacted_before_callback || boundary_changed {
                            session.messages()
                        } else {
                            turn.context.messages
                        };
                        let state = session.agent.state();
                        return Some(maho_agent::types::AgentLoopTurnUpdate {
                            context: Some(maho_agent::types::AgentContext {
                                declared_tools: None,
                                system_prompt: state.system_prompt.clone(),
                                messages,
                                tools: Some(state.tools().to_vec()),
                            }),
                            messages: None,
                            model: Some(state.model.clone()),
                            thinking_level: Some(state.thinking_level),
                            thinking_selection: Some(state.thinking_selection.clone()),
                            abort_server_side_fallback: None,
                            failure: None,
                        });
                    }
                    let messages = if compacted_before_callback || boundary_changed {
                        session.messages()
                    } else {
                        turn.context.messages.clone()
                    };
                    let post_compaction_turn = maho_agent::types::PrepareNextTurnContext {
                        context: maho_agent::types::AgentContext {
                            messages,
                            ..turn.context.clone()
                        },
                        ..turn.clone()
                    };
                    let mut previous_snapshot = match &previous {
                        Some(previous) => {
                            previous(post_compaction_turn.clone(), signal.clone()).await
                        }
                        None => None,
                    };
                    if previous_snapshot
                        .as_ref()
                        .is_some_and(|snapshot| snapshot.failure.is_some())
                    {
                        return previous_snapshot;
                    }
                    let mut previous_context = previous_snapshot
                        .as_ref()
                        .and_then(|snapshot| snapshot.context.clone())
                        .unwrap_or_else(|| post_compaction_turn.context.clone());
                    // The previous callback may await while continuation work is enqueued; re-sample once.
                    let compacted_after_callback = !compacted_before_callback
                        && match session.compact_before_next_admission(&turn).await {
                            Ok(compacted) => compacted,
                            Err(failure) => {
                                return Some(maho_agent::types::AgentLoopTurnUpdate {
                                    failure: Some(failure),
                                    ..Default::default()
                                });
                            }
                        };
                    if compacted_after_callback {
                        let post_late_compaction_turn = maho_agent::types::PrepareNextTurnContext {
                            context: maho_agent::types::AgentContext {
                                messages: session.messages(),
                                ..turn.context.clone()
                            },
                            ..turn.clone()
                        };
                        let late_snapshot = match &previous {
                            Some(previous) => {
                                previous(post_late_compaction_turn.clone(), signal.clone()).await
                            }
                            None => None,
                        };
                        let base = previous_snapshot.unwrap_or_default();
                        let context = late_snapshot
                            .as_ref()
                            .and_then(|snapshot| snapshot.context.clone())
                            .unwrap_or_else(|| post_late_compaction_turn.context.clone());
                        previous_snapshot = Some(match late_snapshot {
                            Some(late) => maho_agent::types::AgentLoopTurnUpdate {
                                context: Some(context.clone()),
                                messages: late.messages.or(base.messages),
                                model: late.model.or(base.model),
                                thinking_level: late.thinking_level.or(base.thinking_level),
                                thinking_selection: late
                                    .thinking_selection
                                    .or(base.thinking_selection),
                                abort_server_side_fallback: late
                                    .abort_server_side_fallback
                                    .or(base.abort_server_side_fallback),
                                failure: late.failure.or(base.failure),
                            },
                            None => maho_agent::types::AgentLoopTurnUpdate {
                                context: Some(context.clone()),
                                ..base
                            },
                        });
                        previous_context = context;
                    }
                    let state = session.agent.state();
                    let (previous_messages, previous_abort_fallback) = match previous_snapshot {
                        Some(snapshot) => (snapshot.messages, snapshot.abort_server_side_fallback),
                        None => (None, None),
                    };
                    Some(maho_agent::types::AgentLoopTurnUpdate {
                        context: Some(maho_agent::types::AgentContext {
                            declared_tools: None,
                            system_prompt: state.system_prompt.clone(),
                            messages: previous_context.messages,
                            tools: Some(state.tools().to_vec()),
                        }),
                        messages: previous_messages,
                        model: Some(state.model.clone()),
                        thinking_level: Some(state.thinking_level),
                        thinking_selection: Some(state.thinking_selection.clone()),
                        abort_server_side_fallback: previous_abort_fallback,
                        failure: None,
                    })
                })
            })));
    }

    /// `compactBeforeNextAdmission` inside `_installAgentNextTurnRefresh`: threshold compaction runs
    /// only when this prepare precedes an actual provider admission (a tool continuation or queued
    /// messages). Returns whether a compaction landed.
    async fn compact_before_next_admission(
        &self,
        turn: &maho_agent::types::PrepareNextTurnContext,
    ) -> Result<bool, String> {
        let model = self.model();
        // Cursor rebuilds the full conversation each hop; compacting mid-run poisons its conversation id.
        if model.provider == "cursor"
            || model.provider == "cursor-cli-oauth"
            || model.context_window == 0
        {
            return Ok(false);
        }
        if turn.tool_results.is_empty() && !self.agent.has_queued_messages() {
            return Ok(false);
        }
        let latest_compaction = || self.latest_current_branch_entry_id("compaction", None);
        let before = latest_compaction();
        if self.revalidate_continuation_admission(false).await.is_err() {
            // senpi: the callback throws `RequiredCompactionError`; the run ends with an error
            // assistant message that the agent_end handler recognises for recovery.
            let error = RequiredCompactionError.to_string();
            let mut state = self.state();
            state.required_compaction_turn_error = Some(error.clone());
            if self.agent.has_queued_messages() {
                state.required_compaction_admission_error = Some(error.clone());
            }
            return Err(error);
        }
        Ok(latest_compaction() != before)
    }

    /// senpi `_isRequiredCompactionError`.
    fn is_required_compaction_error(&self, message: &maho_ai::types::AssistantMessage) -> bool {
        message.stop_reason == StopReason::Error
            && self
                .state()
                .required_compaction_turn_error
                .as_deref()
                .is_some_and(|error| message.error_message.as_deref() == Some(error))
    }

    /// Whether a required-compaction error turn still needs threshold compaction (senpi
    /// `_getRequiredAutoCompactionReason` for that turn's error message).
    fn required_compaction_still_needed(
        &self,
        message: &maho_ai::types::AssistantMessage,
    ) -> Result<bool, String> {
        if !self.is_required_compaction_error(message)
            || !self.auto_compaction_enabled()
            || self.is_compaction_delegated()
        {
            return Ok(false);
        }
        let model = self.model();
        let resolved = self.get_compaction_settings(Some(&model))?;
        if !resolved.enabled {
            return Ok(false);
        }
        let reserve = if resolved.reserve_scaling_enabled {
            crate::compaction::compaction::resolve_reserve_tokens(
                model.context_window as f64,
                resolved.reserve_tokens as f64,
            ) as u64
        } else {
            resolved.reserve_tokens as u64
        };
        Ok(self.threshold_context_tokens(message)? > model.context_window.saturating_sub(reserve))
    }

    fn install_after_tool_call_hook(&self) {
        let weak = Arc::downgrade(&self.inner);
        self.agent
            .set_after_tool_call(Some(Arc::new(move |context, _| {
                let weak = weak.clone();
                Box::pin(async move {
                    let inner = weak.upgrade()?;
                    let session = AgentSession { inner };
                    session
                        .emit_after_tool_call_hooks(
                            context.tool_call,
                            context.args,
                            context.result,
                            context.is_error,
                        )
                        .await
                })
            })));
    }

    /// Pinned `_emitAfterToolCallHooks`: run the extension `tool_result` hook, then normalize images.
    async fn emit_after_tool_call_hooks(
        &self,
        tool_call: maho_agent::types::AgentToolCall,
        args: Value,
        result: AgentToolResult,
        is_error: bool,
    ) -> Option<maho_agent::types::AfterToolCallResult> {
        let original_details = result.details.clone();
        let mut guard = self.extension_runner.lock().await.clone();
        let original_content = result.content.clone();
        let structured_content = result.structured_content.clone();
        let rewrite = if let Some(runner) = guard.as_mut() {
            let content =
                serde_json::from_value(serde_json::to_value(original_content.clone()).ok()?)
                    .ok()?;
            runner
                .emit_tool_result(maho_ext_api::ToolResultEvent {
                    tool_call_id: tool_call.id,
                    tool_name: tool_call.name,
                    input: args,
                    content,
                    details: Some(result.details),
                    is_error,
                    usage: result.usage,
                })
                .await
                .ok()
                .flatten()
        } else {
            None
        };
        let content = normalize_tool_result_images(
            &rewrite
                .as_ref()
                .and_then(|rewrite| rewrite.content.as_ref())
                .and_then(|content| serde_json::to_value(content).ok())
                .and_then(|content| serde_json::from_value(content).ok())
                .unwrap_or(original_content.clone()),
            Some(self.with_settings_manager(SettingsManager::get_image_auto_resize)),
        )
        .await
        .into_owned();
        let stop = lock(&self.stop_interrupt).clone();
        if rewrite.is_none() && content == original_content {
            return crate::interrupted_tool_result::mark_interrupted(
                stop.as_ref(),
                is_error,
                &original_details,
                None,
            );
        }
        // A hook that replaced the content drops structured content that may no longer match it.
        let structured_content = if rewrite
            .as_ref()
            .is_some_and(|rewrite| rewrite.content.is_some())
        {
            None
        } else {
            structured_content
        };
        let (details, is_error, usage) = rewrite.map_or((None, Some(is_error), None), |rewrite| {
            (
                rewrite.details,
                Some(rewrite.is_error.unwrap_or(is_error)),
                rewrite.usage,
            )
        });
        let rewritten = Some(maho_agent::types::AfterToolCallResult {
            content: Some(serde_json::from_value(serde_json::to_value(content).ok()?).ok()?),
            details,
            structured_content,
            is_error,
            usage,
            terminate: None,
        });
        crate::interrupted_tool_result::mark_interrupted(
            stop.as_ref(),
            is_error.unwrap_or(false),
            &original_details,
            rewritten,
        )
    }

    /// Pinned `emitExecBridgeToolResult`: Cursor exec already ran the tool; still emit
    /// `tool_result` so plan-touch trackers see `.omo/plans` writes.
    pub async fn emit_exec_bridge_tool_result(
        &self,
        tool_name: String,
        tool_call_id: String,
        args: Value,
        result: AgentToolResult,
        is_error: bool,
    ) {
        let arguments = args.as_object().cloned().unwrap_or_default();
        let tool_call = maho_agent::types::AgentToolCall {
            id: tool_call_id,
            name: tool_name,
            arguments,
            ..Default::default()
        };
        self.emit_after_tool_call_hooks(tool_call, args, result, is_error)
            .await;
    }

    /// Pinned sdk `cursorExecHandlers`: the Agent asks this factory for one bridge per Cursor run.
    pub fn set_cursor_exec_bridge_factory(
        &self,
        factory: Arc<
            dyn Fn(
                    Option<maho_ai::utils::abort::AbortSignal>,
                ) -> Arc<maho_ai::api::cursor_agent::types::CursorExecHandlers>
                + Send
                + Sync,
        >,
    ) {
        self.agent.set_cursor_exec_handlers(Some(
            maho_agent::types::CursorExecHandlersConfig::Factory(factory),
        ));
    }

    pub async fn bind_extensions(&self, bindings: ExtensionBindings) {
        let _settlement = self.begin_session_start_settlement();
        let binding_work = self.work_barrier.begin();
        let pending = Arc::new(Mutex::new(Vec::new()));
        *lock(&self.binding_readiness) = Some(pending.clone());
        let readiness_scope = ExtensionBindingReadiness {
            session: self,
            pending: pending.clone(),
        };
        {
            let mut state = self.state();
            state.sizes_only_startup = bindings.sizes_only;
            state.prompt_size_inventory = if bindings.sizes_only {
                bindings.prompt_size_inventory
            } else {
                None
            };
            if let Some(ui) = bindings.ui_context {
                state.extension_ui_context = Some(ui);
            }
            if let Some(mode) = bindings.mode {
                state.extension_mode = mode;
            }
            if let Some(handler) = bindings.abort_handler {
                state.extension_abort_handler = Some(handler);
            }
            if let Some(listener) = bindings.on_error {
                state.extension_error_listener = Some(listener);
            }
            if let Some(handler) = bindings.shutdown_handler {
                state.extension_shutdown_handler = Some(handler);
            }
        }
        self.apply_extension_bindings().await;
        // Opening a session is not explicit Resume. Install the same hold used by Stop before session_start producers
        // run, so automatic work waits quietly in notice custody (senpi#4204).
        if let Some(key) = self.opening_hold_key() {
            lock(&self.engine_turn_hold).add_hold(
                crate::engine_turn_hold::HoldKind::SessionHold,
                Some(key),
                None,
            );
        }
        lock(&self.engine_turn_hold).start();
        let event = {
            let state = self.state();
            maho_ext_api::SessionStartEvent {
                // agent-session.ts: `...(this._sizesOnlyStartup ? { sizesOnly: true } : {})`.
                sizes_only: state.session_start_event.sizes_only || state.sizes_only_startup,
                ..state.session_start_event.clone()
            }
        };
        let reason = if event.reason == maho_ext_api::SessionReason::Reload {
            maho_ext_api::SessionReason::Reload
        } else {
            maho_ext_api::SessionReason::Startup
        };
        self.dispatch_extension_event(maho_ext_api::ExtensionEvent::SessionStart(event))
            .await;
        if let Some(file) = self
            .with_session_manager(|manager| manager.session_file().map(std::path::PathBuf::from))
        {
            let session = self.clone();
            if let Err(error_message) =
                crate::session_move::extension_state::restore_moved_state_with_mark(
                    &file,
                    move |delivery| async move {
                        session
                            .emit_session_move(
                                delivery.move_id,
                                delivery.dir,
                                Some(delivery.redelivered),
                            )
                            .await
                            .map(|()| true)
                    },
                    |data| {
                        self.with_session_manager_mut(|manager| {
                            manager.append_custom(
                                crate::session_move::extension_state::MOVED_STATE_RESTORED,
                                Some(data),
                            );
                            manager.take_write_error().map_or(Ok(()), Err)
                        })
                    },
                )
                .await
            {
                self.emit(AgentSessionEvent::ContinuationError { error_message });
            }
        }
        self.with_work_obligations(|registry| registry.retire_restored_sources());
        if let Err(error_message) = self.with_work_obligations(|registry| registry.flush()) {
            self.emit(AgentSessionEvent::ContinuationError { error_message });
        }
        self.state().extension_lifecycle_started = true;
        let defaults = self.state().default_tool_names.clone();
        if let Some(defaults) = defaults {
            let active = self
                .get_active_tool_names()
                .into_iter()
                .filter(|name| {
                    if defaults.contains(name) {
                        return true;
                    }
                    self.state()
                        .tool_definitions
                        .get(name)
                        .is_some_and(|entry| {
                            entry.source_info.source != "builtin"
                                && !entry.source_info.path.starts_with("<builtin:")
                        })
                })
                .collect();
            self.set_active_tools_by_name(active);
        }
        self.extend_resources_from_extensions(reason).await;
        drop(readiness_scope);
        drop(binding_work);
        let receivers = std::mem::take(&mut *lock(&pending));
        for receiver in receivers {
            // Sender drop signals admission or an early exit, including cancellation.
            match receiver.await {
                Ok(()) | Err(_) => {}
            }
        }
    }

    /// Whether the first extension lifecycle dispatch has completed.
    pub fn has_started_extension_lifecycle(&self) -> bool {
        self.state().extension_lifecycle_started
    }

    /// Update an already-started session's connection context without repeating SessionStart.
    /// First binds and explicit Reload dispatches continue to use `bind_extensions`.
    pub async fn rebind_extension_context(&self, bindings: ExtensionBindings) {
        {
            let mut state = self.state();
            if let Some(ui) = bindings.ui_context {
                state.extension_ui_context = Some(ui);
            }
            if let Some(mode) = bindings.mode {
                state.extension_mode = mode;
            }
            if let Some(handler) = bindings.abort_handler {
                state.extension_abort_handler = Some(handler);
            }
            if let Some(listener) = bindings.on_error {
                state.extension_error_listener = Some(listener);
            }
            if let Some(handler) = bindings.shutdown_handler {
                state.extension_shutdown_handler = Some(handler);
            }
        }
        self.apply_extension_bindings().await;
    }

    async fn extend_resources_from_extensions(&self, reason: maho_ext_api::SessionReason) {
        let discovered = {
            let mut guard = self.extension_runner.lock().await.clone();
            let Some(runner) = guard.as_mut() else {
                return;
            };
            if !runner.has_handlers(maho_ext_api::EventKind::ResourcesDiscover) {
                return;
            }
            let resources = runner
                .emit_resources_discover(self.cwd().into(), reason)
                .await;
            resources.map(|mut resources| {
                for entry in resources
                    .skill_paths
                    .iter_mut()
                    .chain(&mut resources.prompt_paths)
                    .chain(&mut resources.theme_paths)
                    .chain(&mut resources.hook_paths)
                {
                    if entry.scope.is_some() {
                        continue;
                    }
                    let contributor = runner
                        .extensions
                        .iter()
                        .find(|extension| extension.identity.path == entry.extension_path);
                    if contributor.is_some_and(|extension| {
                        extension.source_info.scope == maho_ext_api::SourceScope::System
                            && (extension.identity.path.starts_with("<builtin:")
                                || extension.source_info.base_dir.as_ref().is_some_and(|root| {
                                    let path = crate::paths::lexical_resolve(&entry.path);
                                    let root = crate::paths::lexical_resolve(root);
                                    std::path::Path::new(&path).starts_with(&root)
                                }))
                    }) {
                        entry.scope = Some(maho_ext_api::SourceScope::System);
                    }
                }
                resources
            })
        };
        match discovered {
            Ok(resources) => self.extend_discovered_resources(resources),
            Err(error) => self.emit(AgentSessionEvent::ContinuationError {
                error_message: error.message,
            }),
        }
    }

    pub fn set_hook_source_paths(
        &self,
        resources: Vec<crate::package_manager::ResolvedResource>,
        additional: Vec<String>,
    ) {
        let cwd = self.cwd();
        let mut state = self.state();
        state.global_hook_source_paths.clear();
        state.project_hook_source_paths.clear();
        for resource in resources.into_iter().filter(|resource| resource.enabled) {
            let path = std::path::PathBuf::from(crate::paths::resolve_path(
                &resource.path,
                resource.metadata.base_dir.as_deref().unwrap_or(&cwd),
                &Default::default(),
            ));
            let paths = if resource.metadata.scope == crate::source_info::SourceScope::Project {
                &mut state.project_hook_source_paths
            } else {
                &mut state.global_hook_source_paths
            };
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
        state.pre_session_hook_source_paths.clear();
        for path in additional {
            let path = std::path::PathBuf::from(crate::paths::resolve_path(
                &path,
                &cwd,
                &Default::default(),
            ));
            if !state.pre_session_hook_source_paths.contains(&path) {
                state.pre_session_hook_source_paths.push(path);
            }
        }
    }

    pub fn set_hook_sources(&self, sources: Option<maho_ext_api::LoadedHookSources>) {
        self.state().loaded_hook_sources = sources;
    }

    pub fn extension_actions(&self) -> Arc<dyn maho_ext_api::ExtensionActions> {
        Arc::new(SessionExtensionActions(Arc::downgrade(&self.inner)))
    }

    pub fn extension_context(
        &self,
        ui: Arc<dyn maho_ext_api::ExtensionUi>,
    ) -> maho_ext_api::ExtensionContext {
        let mut context = crate::sdk::extension_context::create(self);
        context.ui = ui;
        context
    }

    pub async fn bind_loaded_extensions(
        &self,
        loaded: &maho_ext_host::loader::LoadExtensionsResult,
        ui: Arc<dyn maho_ext_api::ExtensionUi>,
        loader: Arc<dyn Fn() -> Vec<maho_ext_host::loader::NativeExtensionFactory> + Send + Sync>,
    ) {
        let mut runner = maho_ext_host::runner::ExtensionRunner::new(
            loaded.extensions.clone(),
            loaded.runtime.clone(),
            loaded.events.clone(),
            self.extension_context(ui),
        );
        runner.bind_native_factory_loader(loader, Default::default());
        self.set_extension_runner(runner).await;
    }

    fn extend_discovered_resources(&self, mut resources: maho_ext_api::DiscoveredResources) {
        let refresh_prompts = !resources.prompt_paths.is_empty();
        let refresh_skills = !resources.skill_paths.is_empty();
        let rebuild_prompt = refresh_prompts || refresh_skills || !resources.theme_paths.is_empty();
        if !rebuild_prompt && resources.hook_paths.is_empty() {
            return;
        }
        let cwd = self.cwd();
        for entry in resources
            .skill_paths
            .iter_mut()
            .chain(&mut resources.prompt_paths)
            .chain(&mut resources.theme_paths)
            .chain(&mut resources.hook_paths)
        {
            entry.path = crate::paths::resolve_path(
                &entry.path,
                &cwd,
                &crate::paths::PathInputOptions::default(),
            );
        }
        let mut state = self.state();
        let stored = &mut state.discovered_resources;
        for (existing, additions) in [
            (&mut stored.skill_paths, resources.skill_paths),
            (&mut stored.prompt_paths, resources.prompt_paths),
            (&mut stored.theme_paths, resources.theme_paths),
            (&mut stored.hook_paths, resources.hook_paths),
        ] {
            for entry in additions {
                if let Some(known) = existing.iter_mut().find(|known| known.path == entry.path) {
                    *known = entry;
                } else {
                    existing.push(entry);
                }
            }
        }
        let resources = state.discovered_resources.clone();
        drop(state);
        let mut templates = self.prompt_templates();
        let mut skills = self.state().skills.clone();
        // senpi `updateSkillsFromPaths`/`updatePromptsFromPaths` recompute the diagnostics over the
        // merged set: load diagnostics and name collisions of the discovered resources join them.
        let mut skill_diagnostics = self.state().skill_diagnostics.clone();
        let mut prompt_diagnostics = self.prompt_diagnostics();
        let push_unique =
            |diagnostics: &mut Vec<crate::diagnostics::ResourceDiagnostic>,
             diagnostic: crate::diagnostics::ResourceDiagnostic| {
                if !diagnostics.contains(&diagnostic) {
                    diagnostics.push(diagnostic);
                }
            };
        let collision = |resource_type, message: String, winner: &str, loser: &str, name: &str| {
            crate::diagnostics::ResourceDiagnostic {
                category: None,
                diagnostic_type: crate::diagnostics::ResourceDiagnosticType::Collision,
                message,
                path: Some(loser.to_owned()),
                collision: Some(crate::diagnostics::ResourceCollision {
                    resource_type,
                    name: name.to_owned(),
                    winner_path: winner.to_owned(),
                    loser_path: loser.to_owned(),
                    winner_source: None,
                    loser_source: None,
                }),
            }
        };
        let source_info = |path: &str, entry: &maho_ext_api::DiscoveredResourceEntry| {
            crate::source_info::create_synthetic_source_info(
                path,
                crate::source_info::SyntheticSourceInfoOptions {
                    source: crate::discovered_resource_scope::get_extension_source_label(
                        &entry.extension_path,
                    ),
                    scope: Some(
                        match entry.scope.unwrap_or(maho_ext_api::SourceScope::Temporary) {
                            maho_ext_api::SourceScope::User => {
                                crate::source_info::SourceScope::User
                            }
                            maho_ext_api::SourceScope::Project => {
                                crate::source_info::SourceScope::Project
                            }
                            maho_ext_api::SourceScope::System => {
                                crate::source_info::SourceScope::System
                            }
                            maho_ext_api::SourceScope::Temporary => {
                                crate::source_info::SourceScope::Temporary
                            }
                        },
                    ),
                    base_dir: if entry.extension_path.starts_with('<') {
                        None
                    } else {
                        std::path::Path::new(&entry.extension_path)
                            .parent()
                            .map(|path| path.to_string_lossy().into_owned())
                    },
                    ..Default::default()
                },
            )
        };
        for entry in resources
            .prompt_paths
            .into_iter()
            .filter(|_| refresh_prompts)
        {
            let (loaded, mut load_diagnostics) = crate::prompt_templates::load_prompt_templates(
                &crate::prompt_templates::LoadPromptTemplatesOptions {
                    cwd: cwd.clone(),
                    agent_dir: self.agent_dir(),
                    prompt_paths: vec![entry.path.clone()],
                    include_defaults: false,
                },
            );
            for diagnostic in load_diagnostics.drain(..) {
                push_unique(&mut prompt_diagnostics, diagnostic);
            }
            for mut template in loaded {
                template.source_info = source_info(&template.file_path, &entry);
                if let Some(known) = templates
                    .iter_mut()
                    .find(|known| known.name == template.name)
                {
                    if known.file_path == template.file_path {
                        *known = template;
                    } else {
                        push_unique(
                            &mut prompt_diagnostics,
                            collision(
                                crate::diagnostics::ResourceCollisionType::Prompt,
                                format!("name \"/{}\" collision", template.name),
                                &known.file_path,
                                &template.file_path,
                                &template.name,
                            ),
                        );
                    }
                } else {
                    templates.push(template);
                }
            }
        }
        for entry in resources.skill_paths.into_iter().filter(|_| refresh_skills) {
            let loaded = crate::skills::load_skills(&crate::skills::LoadSkillsOptions {
                cwd: cwd.clone(),
                agent_dir: self.agent_dir(),
                skill_paths: vec![entry.path.clone()],
                include_defaults: false,
            });
            for diagnostic in loaded.diagnostics {
                push_unique(&mut skill_diagnostics, diagnostic);
            }
            for mut skill in loaded.skills {
                skill.source_info = source_info(&skill.file_path, &entry);
                if let Some(known) = skills.iter_mut().find(|known| known.name == skill.name) {
                    if known.file_path == skill.file_path {
                        *known = skill;
                    } else {
                        push_unique(
                            &mut skill_diagnostics,
                            collision(
                                crate::diagnostics::ResourceCollisionType::Skill,
                                format!("name \"{}\" collision", skill.name),
                                &known.file_path,
                                &skill.file_path,
                                &skill.name,
                            ),
                        );
                    }
                } else {
                    skills.push(skill);
                }
            }
        }
        if rebuild_prompt {
            self.set_prompt_resources_with_diagnostics(templates, skills, skill_diagnostics);
            self.state().prompt_diagnostics = prompt_diagnostics;
            self.rebuild_system_prompt();
        }
    }

    /// Subscribe to the internal event bus shared by this session's extensions.
    pub fn on_extension_event(&self, channel: &str, handler: EventHandler) -> EventSubscription {
        self.event_bus.on(channel, handler)
    }

    /// Publish on the internal event bus shared by this session's extensions.
    ///
    /// Routed to the bound runner's bus (the pinned `resourceLoader` bus extension handlers
    /// subscribe to) as well as the session-private bus, falling back to the private bus alone
    /// when no runner is bound.
    pub fn security_notice_state(&self) -> crate::security_notices::SecurityNoticeState {
        self.state().security_notices.snapshot()
    }
    pub fn accept_security_notice(&self, notice: &Value) {
        self.state().security_notices.accept(notice);
    }

    pub fn set_resource_security_notices(&self, diagnostics: &[Value]) {
        self.state().security_notices.set_resources(diagnostics);
    }
    pub fn subscribe_security_notices(
        &self,
        listener: Arc<dyn Fn(crate::security_notices::SecurityNoticeState) + Send + Sync>,
    ) -> crate::security_notices::SecurityNoticeSubscription {
        self.state().security_notices.subscribe(listener)
    }

    pub fn emit_extension_event(&self, channel: &str, data: &Value) {
        if channel == "security_notice" {
            self.accept_security_notice(data);
        }
        self.event_bus.emit(channel, data);
        if let Some(events) = self.state().extension_events.clone() {
            events.emit(channel, data);
        }
    }

    /// senpi `extensionRunner.onRpcEvent`: receive every `senpi:extension-rpc-event` the bound
    /// runner publishes, across runner reloads, for the rest of this session's lifetime.
    pub fn add_extension_rpc_event_listener(&self, listener: ExtensionRpcEventListener) {
        lock(&self.extension_rpc_event_listeners).push(listener);
    }

    /// Publish a typed payload natively on the bound runner's bus (no serialization), so a consumer
    /// can hand a non-JSON value such as a command context to a native subscriber.
    pub fn emit_extension_event_typed<T: Send + Sync + 'static>(&self, channel: &str, event: &T) {
        if let Some(events) = self.state().extension_events.clone() {
            events.emit_native(channel, event);
        }
    }

    /// The session's command context (pinned `extensionRunner.createCommandContext()`), for a
    /// consumer that needs the typed context the runner would pass to a command handler.
    pub async fn extension_command_context(&self) -> Option<maho_ext_api::ExtensionCommandContext> {
        let runner = self.extension_runner.lock().await.clone()?;
        runner
            .create_command_context(Arc::new(SessionExtensionActions(Arc::downgrade(
                &self.inner,
            ))))
            .ok()
    }

    /// senpi `extensionRunner.getShortcuts(resolvedKeybindings)`: the extension shortcuts resolved
    /// against the built-in keybindings (reserved editor-global actions block an override), plus the
    /// conflict diagnostics. Empty when no runner is bound.
    pub async fn extension_shortcuts(
        &self,
        resolved: &maho_tui::keybindings::KeybindingsConfig,
    ) -> (
        BTreeMap<String, maho_ext_api::ExtensionShortcut>,
        Vec<String>,
    ) {
        const RESERVED: [&str; 18] = [
            "app.interrupt",
            "app.clear",
            "app.exit",
            "app.suspend",
            "app.thinking.cycle",
            "app.model.cycleForward",
            "app.model.cycleBackward",
            "app.model.select",
            "app.tools.expand",
            "app.thinking.toggle",
            "app.editor.external",
            "app.message.copy",
            "app.message.followUp",
            "tui.input.submit",
            "tui.select.confirm",
            "tui.select.cancel",
            "tui.input.copy",
            "tui.editor.deleteToLineEnd",
        ];
        let Some(runner) = self.extension_runner.lock().await.clone() else {
            return (BTreeMap::new(), Vec::new());
        };
        let mut builtins: BTreeMap<String, maho_ext_host::runner::BuiltinShortcut> =
            BTreeMap::new();
        for (keybinding, keys) in resolved {
            let keys = match keys {
                Some(maho_tui::keybindings::Keys::One(key)) => vec![key.clone()],
                Some(maho_tui::keybindings::Keys::Many(keys)) => keys.clone(),
                None => continue,
            };
            let restrict_override = RESERVED.contains(&keybinding.as_str());
            for key in keys {
                let key = key.to_lowercase();
                // The reserved action wins when several actions bind the same key.
                if builtins
                    .get(&key)
                    .is_some_and(|existing| existing.restrict_override && !restrict_override)
                {
                    continue;
                }
                builtins.insert(
                    key,
                    maho_ext_host::runner::BuiltinShortcut {
                        keybinding: keybinding.clone(),
                        restrict_override,
                    },
                );
            }
        }
        let (shortcuts, diagnostics) = runner.resolve_shortcuts(&builtins);
        (
            shortcuts,
            diagnostics
                .into_iter()
                .map(|diagnostic| diagnostic.message)
                .collect(),
        )
    }

    /// A fresh runner context for an extension shortcut handler (senpi `setupExtensionShortcuts`'
    /// `createContext`); `None` when no runner is bound or it is stale.
    pub async fn extension_shortcut_context(&self) -> Option<maho_ext_api::ExtensionContext> {
        let runner = self.extension_runner.lock().await.clone()?;
        runner.create_context().ok()
    }

    pub async fn apply_fast_mode(
        &self,
        enabled: bool,
    ) -> Result<maho_ext_api::FastModeResult, maho_ext_api::ExtensionFailure> {
        let runner =
            self.extension_runner.lock().await.clone().ok_or_else(|| {
                maho_ext_api::ExtensionFailure::new("Extension runner is not bound")
            })?;
        let context = runner.create_context()?;
        runner.apply_fast_mode(enabled, &context).await
    }

    pub async fn extension_runner_request(&self, name: &str, data: Value) -> Result<Value, String> {
        self.extension_runner_request_typed(name, data)
            .await
            .map_err(|error| error.message)
    }

    /// [`Self::extension_runner_request`] keeping the handler's failure `code` (the RPC `errorCode`).
    pub async fn extension_runner_request_typed(
        &self,
        name: &str,
        data: Value,
    ) -> Result<Value, maho_ext_api::ExtensionFailure> {
        self.extension_runner
            .lock()
            .await
            .clone()
            .ok_or_else(|| maho_ext_api::ExtensionFailure::new("Extension runner is not bound"))?
            .handle_rpc_request(name, data)
            .await
    }

    /// Whether the bound extension runner serves the RPC request `name`; `false` when unbound.
    pub async fn extension_runner_has_rpc_handler(&self, name: &str) -> bool {
        self.extension_runner
            .lock()
            .await
            .as_ref()
            .is_some_and(|runner| runner.has_rpc_handler(name))
    }

    pub async fn loaded_extension_surfaces(&self) -> Vec<Value> {
        let runner = self.extension_runner.lock().await.clone();
        runner.map(|runner| runner.extensions.iter().map(|extension| {
            let path = &extension.identity.path;
            let name = if (path.starts_with("<builtin:") || path.starts_with("<inline:")) && path.ends_with('>') {
                path.trim_start_matches('<').trim_end_matches('>').split_once(':').map_or(path.as_str(), |(_, id)| id).to_owned()
            } else {
                let file = Path::new(path);
                let stem = file.file_stem().map_or_else(|| path.clone(), |name| name.to_string_lossy().into_owned());
                if stem == "index" {
                    let parent = file.parent().unwrap_or(file);
                    let parent_name = parent.file_name().map_or_else(String::new, |name| name.to_string_lossy().into_owned());
                    if parent_name == "src" || parent_name == "dist" {
                        parent.parent().and_then(Path::file_name).map_or(parent_name, |name| name.to_string_lossy().into_owned())
                    } else { parent_name }
                } else { stem }
            };
            serde_json::json!({"name":name,"path":path,"sourceInfo":extension.source_info,"enabled":true})
        }).collect()).unwrap_or_default()
    }

    /// Subscribe to live per-session MCP wire-status snapshots. `T` is the MCP extension's
    /// `McpWireStatusSnapshot`; the channel carries an erased `Arc<dyn Any>` so maho-core needs no
    /// dependency on the MCP crate. `None` when no extension runner is bound. Use
    /// [`Self::bind_mcp_wire_status`] when the initial snapshot must not be missed.
    pub async fn subscribe_mcp_wire_status<T: Clone + Send + Sync + 'static>(
        &self,
        handler: Arc<dyn Fn(&T) + Send + Sync>,
    ) -> Option<maho_ext_api::BusSubscription> {
        let runner = self.extension_runner.lock().await.clone()?;
        Some(
            runner
                .events
                .on_native::<Arc<dyn std::any::Any + Send + Sync>>(
                    MCP_WIRE_STATUS_CHANGED_EVENT,
                    downcast_handler(handler),
                ),
        )
    }

    /// The latest MCP wire-status snapshot this session retained (pinned
    /// `mcpService.getWireStatusSnapshot(threadId)`); `None` before the first publish or on a type
    /// mismatch. `T` is the MCP extension's `McpWireStatusSnapshot`.
    pub fn mcp_wire_status_snapshot<T: Clone + Send + Sync + 'static>(&self) -> Option<T> {
        self.state()
            .mcp_wire_status
            .as_ref()
            .and_then(|value| value.downcast_ref::<T>())
            .cloned()
    }

    pub async fn emit_native_extension_event<T: Send + Sync + 'static>(
        &self,
        channel: &str,
        payload: &T,
    ) {
        if let Some(runner) = self.extension_runner.lock().await.clone() {
            runner.events.emit_native(channel, payload);
        }
    }

    /// Bind to this session's MCP wire status, delivering the retained initial snapshot first and
    /// then every later one (pinned `createMcpWireStatusAdapter(getWireStatusSnapshot(threadId))`
    /// followed by `bindLiveUpdates(onWireStatusChanged)`). The live subscription is installed
    /// before the retained read, both under the same order lock as [`Self::publish_mcp_wire_status`],
    /// so a snapshot published around the bind is delivered exactly once (never missed, never
    /// duplicated). The order lock is held across the handler call to keep the retained snapshot
    /// ahead of live updates, so the handler must not re-enter `bind_mcp_wire_status` /
    /// `publish_mcp_wire_status` (the consumer callback only updates its holder). `None` when no
    /// extension runner is bound.
    pub async fn bind_mcp_wire_status<T: Clone + Send + Sync + 'static>(
        &self,
        handler: Arc<dyn Fn(&T) + Send + Sync>,
    ) -> Option<maho_ext_api::BusSubscription> {
        let _order = self.mcp_wire_status_order.lock().await;
        if self.state().disposed {
            return None;
        }
        let runner = self.extension_runner.lock().await.clone()?;
        let subscription = runner
            .events
            .on_native::<Arc<dyn std::any::Any + Send + Sync>>(
                MCP_WIRE_STATUS_CHANGED_EVENT,
                downcast_handler(handler.clone()),
            );
        let retained = self
            .state()
            .mcp_wire_status
            .as_ref()
            .and_then(|value| value.downcast_ref::<T>())
            .cloned();
        if let Some(snapshot) = retained {
            handler(&snapshot);
        }
        Some(subscription)
    }

    /// Publish a live MCP wire-status snapshot for this session; `false` when no runner is bound.
    /// The snapshot is emitted on the runner bus as an erased `Arc<dyn Any>` (the channel contract),
    /// which the session's retention subscription catches; retaining it lets a late subscriber read
    /// the current inventory instead of a process fallback (pinned session-owned adapter).
    pub async fn publish_mcp_wire_status<T: Clone + Send + Sync + 'static>(
        &self,
        snapshot: &T,
    ) -> bool {
        let _order = self.mcp_wire_status_order.lock().await;
        let runner = self.extension_runner.lock().await.clone();
        let Some(runner) = runner else {
            return false;
        };
        let erased: Arc<dyn std::any::Any + Send + Sync> = Arc::new(snapshot.clone());
        self.state().mcp_wire_status = Some(erased.clone());
        runner
            .events
            .emit_native::<Arc<dyn std::any::Any + Send + Sync>>(
                MCP_WIRE_STATUS_CHANGED_EVENT,
                &erased,
            );
        true
    }

    /// Rebind the live connection's UI into the bound extension runner, preserving the session's
    /// extension-event forwarding (pinned `runner.setUIContext` at connection attach).
    pub async fn rebind_extension_ui(&self, ui: Arc<dyn ExtensionUi>) -> Result<(), String> {
        self.state().extension_ui_context = Some(ui.clone());
        let sender = self.state().extension_event_sender.clone();
        let mut guard = self.extension_runner.lock().await;
        let Some(runner) = guard.as_mut() else {
            return Ok(());
        };
        if self
            .state()
            .extension_bound_ui
            .as_ref()
            .is_some_and(|(source, _)| Arc::ptr_eq(source, &ui))
        {
            return Ok(());
        }
        let source = ui.clone();
        let bound: Arc<dyn ExtensionUi> = match sender {
            // The wrapper keeps the host's MCP presentation (and private forms) (senpi#d61b732).
            Some(events) => crate::mcp_mrtr_presentations::inherit_mcp_mrtr_presentation(
                &ui,
                Arc::new(maho_ext_host::ui::LifecycleUi::new(
                    ui.clone(),
                    runner.runtime.clone(),
                    Arc::new(move |event| {
                        let _ = events.send(event);
                    }),
                )),
            ),
            None => ui,
        };
        runner
            .bind_ui(bound.clone())
            .map_err(|error| error.message)?;
        self.state().extension_bound_ui = Some((source, bound));
        Ok(())
    }

    /// Append a transport-provided entry and publish it on the RPC event stream.
    pub fn append_session_entry(&self, entry: Value) {
        let entry_id = entry.get("id").and_then(Value::as_str).map(str::to_owned);
        self.with_session_manager_mut(|manager| manager.append_entry_raw(entry));
        let messages = self.with_session_manager(|manager| manager.current_context().messages);
        let agent_messages: Vec<AgentMessage> = messages
            .iter()
            .filter_map(|message| serde_json::from_value(message.clone()).ok())
            .collect();
        self.agent.set_messages(agent_messages);
        self.state().context_usage_cache = None;
        if let Some(entry_id) = entry_id {
            self.emit_entry_appended(&entry_id);
        }
    }

    pub async fn send_itemized_message(
        &self,
        message: maho_ext_api::CustomMessage,
        options: maho_ext_api::SendMessageOptions,
        items: Vec<crate::extensions::types::SendMessageItem>,
    ) -> Result<Vec<crate::extensions::types::SendMessageReceipt>, String> {
        let value = json!({"customType":message.custom_type,"content":message.content,"display":message.display,"details":message.details});
        let held = self.holds();
        let batch_held = held
            .iter()
            .any(|h| h.kind != crate::engine_turn_hold::HoldKind::RunFence)
            || held
                .iter()
                .filter(|h| h.kind == crate::engine_turn_hold::HoldKind::RunFence)
                .any(|h| {
                    items.iter().any(|item| {
                        item.run_ref
                            .as_ref()
                            .is_some_and(|r| h.data.as_ref() == Some(r))
                    })
                });
        if options.trigger_turn && batch_held {
            let held_items = items
                .iter()
                .map(|item| crate::engine_turn_hold_notices::HeldItem {
                    source: item.producer.clone(),
                    message: crate::itemized_send::item_notice_message(&value, item),
                    item_id: item.item_id.clone(),
                    run_ref: item.run_ref.clone(),
                })
                .collect::<Vec<_>>();
            let receipts = lock(&self.engine_turn_hold).note_items(&held_items, |data| {
                self.with_session_manager_mut(|manager| {
                    let entry = manager.append_custom("session-notice", Some(data));
                    if let Some(error) = manager.last_persistence_error() {
                        return Err(error.to_owned());
                    }
                    entry["id"]
                        .as_str()
                        .map(str::to_owned)
                        .ok_or("notice entry has no id".into())
                })
            })?;
            return Ok(receipts
                .into_iter()
                .map(|r| crate::extensions::types::SendMessageReceipt {
                    item_id: r.item_id,
                    entry_id: r.entry_id,
                    notice_id: Some(r.notice_id),
                    disposition: "notice_persisted".into(),
                })
                .collect());
        }
        let receipt = maho_ext_api::ExtensionActions::send_message_with_receipt(
            &SessionExtensionActions(Arc::downgrade(&self.inner)),
            message.clone(),
            options,
        )
        .map_err(|e| e.message)?;
        let (tx, rx) = tokio::sync::oneshot::channel();
        receipt.on_settled(move |result| {
            let _ = tx.send(result);
        });
        rx.await.map_err(|e| e.to_string())??;
        let entry_id = self
            .with_session_manager(|manager| {
                manager
                    .entries()
                    .iter()
                    .rev()
                    .find(|e| {
                        e["type"] == "custom_message"
                            && e["customType"] == message.custom_type
                            && e["content"] == value["content"]
                    })
                    .and_then(|e| e["id"].as_str())
                    .map(str::to_owned)
            })
            .ok_or("delivered batch has no transcript entry")?;
        Ok(crate::itemized_send::delivered_receipts(&items, &entry_id))
    }

    pub async fn send_custom_message(
        &self,
        message: maho_ext_api::CustomMessage,
        options: maho_ext_api::SendMessageOptions,
    ) -> Result<(), String> {
        maho_ext_api::ExtensionActions::send_message(
            &SessionExtensionActions(Arc::downgrade(&self.inner)),
            message,
            options,
        )
        .map_err(|error| error.message)
    }

    fn send_custom_message_content_inner(
        &self,
        message: maho_agent::harness::messages::CustomMessage,
        options: maho_ext_api::SendMessageOptions,
    ) -> Result<(), maho_ext_api::ExtensionFailure> {
        self.send_custom_message_content_with_receipt(message, options, None)
    }

    fn send_custom_message_content_with_receipt(
        &self,
        message: maho_agent::harness::messages::CustomMessage,
        options: maho_ext_api::SendMessageOptions,
        receipt: Option<maho_ext_api::MessageDeliveryReceipt>,
    ) -> Result<(), maho_ext_api::ExtensionFailure> {
        // senpi `sendCustomMessage`: an engine wake that would start a turn while its source is held is
        // kept in notice custody, not delivered (`triggerTurn` is a bool here, so "undefined" reads as false).
        if options.trigger_turn
            && options.deliver_as != Some(maho_ext_api::DeliverAs::NextTurn)
            && crate::input_origin::custom_turn_origin(&message.custom_type) == InputOrigin::Engine
        {
            let source = crate::engine_turn_hold::engine_wake_source(&message.custom_type);
            let mut hold = lock(&self.engine_turn_hold);
            if hold.holds_source(&source)
                || crate::run_holds::RunHolds::new(&mut hold)
                    .holds_wake(message.details.as_ref().unwrap_or(&Value::Null))
            {
                let noted = serde_json::to_value(&message)
                    .map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?;
                hold.note(&source, Some(noted));
                if let Some(receipt) = receipt {
                    receipt.resolve_delivered();
                }
                return Ok(());
            }
        }
        {
            let mut settled_delivery = lock(&self.settled_delivery);
            if settled_delivery.active() {
                let deferred_session = self.clone();
                let deferred_message = message.clone();
                let deferred_options = options.clone();
                let deferred_generation = self.monitor_generation.load(Ordering::SeqCst);
                let deferred_abort_generation = self.user_abort_generation.load(Ordering::SeqCst);
                let deferred_guard = receipt.clone().map(|receipt| {
                    maho_ext_api::MessageDeliveryGuard::new(
                        receipt,
                        "deferred custom message dropped before settlement",
                    )
                });
                let deferred_receipt = receipt.clone();
                if options.trigger_turn {
                    let spawner = self.clone();
                    settled_delivery.defer_trigger_turn(move |claim| {
                        let guard = deferred_session.work_barrier.begin();
                        let mut delivery_guard = deferred_guard;
                        spawner.spawn_session_task(async move {
                            if deferred_session.state().disposed
                                || deferred_session.monitor_generation.load(Ordering::SeqCst)
                                    != deferred_generation
                                || deferred_session
                                    .user_abort_generation
                                    .load(Ordering::SeqCst)
                                    != deferred_abort_generation
                            {
                                claim.resolve(
                                    crate::agent_settled_delivery::DeferredTurnDisposition::FinishedWithoutStart,
                                );
                                if let Some(receipt) = &deferred_receipt {
                                    receipt.resolve_failed(
                                        "deferred custom message cancelled before settlement",
                                    );
                                }
                                if let Some(guard) = &mut delivery_guard {
                                    guard.disarm();
                                }
                                return;
                            }
                            claim.resolve(
                                crate::agent_settled_delivery::DeferredTurnDisposition::Delegated,
                            );
                            if let Some(guard) = &mut delivery_guard {
                                guard.disarm();
                            }
                            if let Err(error) = deferred_session
                                .send_custom_message_content_with_receipt(
                                    deferred_message,
                                    deferred_options,
                                    deferred_receipt.clone(),
                                )
                            {
                                if let Some(receipt) = &deferred_receipt {
                                    receipt.resolve_failed(error.message.clone());
                                }
                                deferred_session.emit(AgentSessionEvent::ContinuationError {
                                    error_message: error.message,
                                });
                            }
                            drop(guard);
                        });
                    });
                } else {
                    let mut delivery_guard = deferred_guard;
                    settled_delivery.defer(Box::new(move || {
                        if deferred_session.state().disposed
                            || deferred_session.monitor_generation.load(Ordering::SeqCst)
                                != deferred_generation
                            || deferred_session
                                .user_abort_generation
                                .load(Ordering::SeqCst)
                                != deferred_abort_generation
                        {
                            if let Some(receipt) = &deferred_receipt {
                                receipt.resolve_failed(
                                    "deferred custom message cancelled before settlement",
                                );
                            }
                            if let Some(guard) = &mut delivery_guard {
                                guard.disarm();
                            }
                            return;
                        }
                        if let Some(guard) = &mut delivery_guard {
                            guard.disarm();
                        }
                        if let Err(error) = deferred_session
                            .send_custom_message_content_with_receipt(
                                deferred_message,
                                deferred_options,
                                deferred_receipt.clone(),
                            )
                        {
                            if let Some(receipt) = &deferred_receipt {
                                receipt.resolve_failed(error.message.clone());
                            }
                            deferred_session.emit(AgentSessionEvent::ContinuationError {
                                error_message: error.message,
                            });
                        }
                    }));
                }
                return Ok(());
            }
        }
        let agent_message = AgentMessage::Custom(maho_agent::types::CustomAgentMessage::Custom(
            message.clone(),
        ));
        if self.state().sizes_only_startup && options.trigger_turn {
            if let Some(receipt) = receipt {
                self.track_queued_custom_receipt(message, receipt);
            }
            self.state().pending_next_turn_messages.push(agent_message);
            return Ok(());
        }
        if options.deliver_as == Some(maho_ext_api::DeliverAs::NextTurn) {
            if let Some(receipt) = receipt {
                self.track_queued_custom_receipt(message, receipt);
            }
            self.state().pending_next_turn_messages.push(agent_message);
            return Ok(());
        }
        if (self.is_streaming() || self.is_compacting()) && options.trigger_turn {
            if let Some(receipt) = receipt {
                self.track_queued_custom_receipt(message, receipt);
            }
            match options.deliver_as {
                Some(maho_ext_api::DeliverAs::FollowUp) => self.agent.follow_up(agent_message),
                _ => self.agent.steer(agent_message),
            }
        } else if self.is_streaming() {
            if let Some(receipt) = receipt {
                self.track_queued_custom_receipt(message.clone(), receipt);
            }
            self.state().pending_custom_messages.push(message);
        } else if options.trigger_turn {
            if let Some(receipt) = receipt.clone() {
                self.track_queued_custom_receipt(message.clone(), receipt);
            }
            let mut delivery_guard = receipt.clone().map(|receipt| {
                maho_ext_api::MessageDeliveryGuard::new(
                    receipt,
                    "custom message turn dropped before settlement",
                )
            });
            let guard = self.work_barrier.begin();
            let generation = self.user_abort_generation.load(Ordering::SeqCst);
            let runtime_generation = self.monitor_generation.load(Ordering::SeqCst);
            let session = self.clone();
            let deliver_as = options.deliver_as;
            self.spawn_session_task(async move {
                if session.state().disposed
                    || session.monitor_generation.load(Ordering::SeqCst) != runtime_generation
                {
                    session.settle_queued_custom_receipt(
                        &message,
                        Err("session disposed before custom message turn started".to_owned()),
                    );
                    if let Some(guard) = &mut delivery_guard {
                        guard.disarm();
                    }
                    drop(guard);
                    return;
                }
                let result = session
                    .trigger_custom_message_turn(message.clone(), deliver_as, generation)
                    .await;
                if let Some(guard) = &mut delivery_guard {
                    guard.disarm();
                }
                match result {
                    Ok(()) => {
                        if let Some(receipt) = &receipt
                            && receipt.outcome().is_none()
                            && !session.agent.contains_queued_message(&agent_message)
                        {
                            session.settle_queued_custom_receipt(
                                &message,
                                Err("custom message turn ended without appending message"
                                    .to_owned()),
                            );
                        }
                    }
                    Err(error) => {
                        if !session.agent.contains_queued_message(&agent_message) {
                            session.settle_queued_custom_receipt(&message, Err(error.clone()));
                        }
                        session.emit(AgentSessionEvent::ContinuationError {
                            error_message: error,
                        });
                    }
                }
                drop(guard);
            });
        } else {
            if let Err(error) = self.append_extension_custom_message(message) {
                if let Some(receipt) = receipt {
                    receipt.resolve_failed(error.clone());
                }
                return Err(maho_ext_api::ExtensionFailure::new(error));
            }
            if let Some(receipt) = receipt {
                receipt.resolve_delivered();
            }
        }
        Ok(())
    }

    fn track_queued_custom_receipt(
        &self,
        message: maho_agent::harness::messages::CustomMessage,
        receipt: maho_ext_api::MessageDeliveryReceipt,
    ) {
        lock(&self.queued_custom_receipts).push((message, receipt));
    }

    fn settle_queued_custom_receipt(
        &self,
        message: &maho_agent::harness::messages::CustomMessage,
        outcome: Result<(), String>,
    ) {
        let receipt = {
            let mut receipts = lock(&self.queued_custom_receipts);
            receipts
                .iter()
                .position(|(queued, _)| queued == message)
                .map(|index| receipts.remove(index).1)
        };
        if let Some(receipt) = receipt {
            match outcome {
                Ok(()) => receipt.resolve_delivered(),
                Err(reason) => receipt.resolve_failed(reason),
            }
        }
    }

    fn fail_all_queued_custom_receipts(&self, reason: &str) {
        let receipts: Vec<_> = std::mem::take(&mut *lock(&self.queued_custom_receipts));
        for (_, receipt) in receipts {
            receipt.resolve_failed(reason.to_owned());
        }
    }

    fn audit_queued_custom_receipts(&self, reason: &str) {
        let mut failed = Vec::new();
        {
            let state = self.state();
            let mut receipts = lock(&self.queued_custom_receipts);
            let mut index = 0;
            while index < receipts.len() {
                if receipts[index].1.outcome().is_some() {
                    receipts.remove(index);
                    continue;
                }
                let custom = &receipts[index].0;
                let agent_message = AgentMessage::Custom(
                    maho_agent::types::CustomAgentMessage::Custom(custom.clone()),
                );
                let still_queued = self.agent.contains_queued_message(&agent_message)
                    || state.pending_custom_messages.contains(custom)
                    || state.pending_next_turn_messages.contains(&agent_message);
                if still_queued {
                    index += 1;
                } else {
                    failed.push(receipts.remove(index).1);
                }
            }
        }
        for receipt in failed {
            receipt.resolve_failed(reason.to_owned());
        }
    }

    /// Spawns on the session's own runtime (see `runtime_handle`), falling back to the ambient one.
    fn spawn_session_task(&self, future: impl std::future::Future<Output = ()> + Send + 'static) {
        match &self.runtime_handle {
            Some(runtime) => {
                runtime.spawn(future);
            }
            None => {
                tokio::spawn(future);
            }
        }
    }

    /// senpi `sendCustomMessage` with `triggerTurn` while idle: enforce compaction before the
    /// provider, emit `before_agent_start` for the message text, apply its result, run the final
    /// provider admission, then prompt the agent with the batch. A user abort or an admission
    /// rejection inside that window queues the message for later delivery instead of dropping it.
    async fn trigger_custom_message_turn(
        &self,
        message: maho_agent::harness::messages::CustomMessage,
        deliver_as: Option<maho_ext_api::DeliverAs>,
        user_abort_generation: u64,
    ) -> Result<(), String> {
        self.trigger_custom_message_turn_with(message, deliver_as, user_abort_generation, None)
            .await
            .map(|_| ())
    }

    /// `released`: set when the turn delivers a held wake; it claims the wake once the turn is admitted and
    /// the wake never rides a native queue. `Ok(true)` means the run started and settled.
    async fn trigger_custom_message_turn_with(
        &self,
        message: maho_agent::harness::messages::CustomMessage,
        deliver_as: Option<maho_ext_api::DeliverAs>,
        user_abort_generation: u64,
        released: Option<ReleasedWakeAdmit>,
    ) -> Result<bool, String> {
        let (held_messages, held_ids) = Box::pin(self.take_held_input_messages()).await?;
        let _held_reservation = HeldInputReservation {
            session: self.clone(),
            ids: held_ids.clone(),
        };
        let _admission = self.prompt_admission.lock().await;
        let _work = self.work_barrier.begin();
        let app_message = AgentMessage::Custom(maho_agent::types::CustomAgentMessage::Custom(
            message.clone(),
        ));
        let queue_trigger_for_later = || {
            // A released wake stays in notice custody until final admission claims it.
            if released.is_some() {
                return;
            }
            match deliver_as {
                Some(maho_ext_api::DeliverAs::FollowUp) => {
                    self.agent.follow_up(app_message.clone())
                }
                _ => self.agent.steer(app_message.clone()),
            }
        };
        // senpi returns without delivering when a user abort landed while waiting for session work.
        if user_abort_generation != self.user_abort_generation.load(Ordering::SeqCst) {
            return Ok(false);
        }
        if self.stop_engine_turn(&message.custom_type) {
            if released.is_none() {
                self.record_refused_engine_turn_message(message);
            }
            return Ok(false);
        }
        self.state().user_aborted = false;
        let mut messages = held_messages;
        let prompt_index = messages.len();
        messages.push(app_message.clone());
        self.state().trigger_turn_admission_abort_generation = Some(user_abort_generation);
        let admitted: Result<bool, String> = async {
            self.revalidate_continuation_admission(false).await?;
            let prompt = match &message.content {
                maho_agent::harness::messages::CustomMessageContent::Text(text) => text.clone(),
                maho_agent::harness::messages::CustomMessageContent::Blocks(blocks) => blocks
                    .iter()
                    .filter_map(|block| match block {
                        maho_ai::types::ContentBlock::Text(text) => Some(text.text.as_str()),
                        _ => None,
                    })
                    .collect::<String>(),
            };
            let base_system_prompt = self.state().base_system_prompt.clone();
            self.prompt_cache_prefix_builds.cancel_all();
            let before = {
                let mut runner = self.extension_runner.lock().await.clone();
                match runner.as_mut() {
                    Some(runner) => runner
                        .emit_before_agent_start(maho_ext_api::BeforeAgentStartEvent {
                            prompt,
                            images: None,
                            system_prompt: base_system_prompt,
                            // senpi deliveryIdOf: only validated session-control metadata proves delivery.
                            trigger: if message.role == "custom"
                                && message.custom_type == "session_control_delivery"
                                && message.details.as_ref().is_some_and(|details| {
                                    details.get("delivery_id").is_some_and(Value::is_string)
                                        && details.get("source").and_then(Value::as_str)
                                            == Some("session_control")
                                        && matches!(
                                            details.get("deliverAs").and_then(Value::as_str),
                                            Some("steer" | "followUp")
                                        )
                                }) {
                                "delivery"
                            } else {
                                "extension"
                            }
                            .into(),
                            preview: false,
                            system_prompt_options: self.extension_system_prompt_options(),
                        })
                        .await
                        .map_err(|error| error.to_string())?,
                    None => None,
                }
            };
            if user_abort_generation != self.user_abort_generation.load(Ordering::SeqCst) {
                return Ok(false);
            }
            self.apply_before_agent_start_result(&mut messages, before, prompt_index)?;
            self.enforce_final_provider_admission(&messages).await?;
            Ok::<bool, String>(
                user_abort_generation == self.user_abort_generation.load(Ordering::SeqCst),
            )
        }
        .await;
        self.state().trigger_turn_admission_abort_generation = None;
        match admitted {
            Ok(true) => {}
            Ok(false) => {
                queue_trigger_for_later();
                return Ok(false);
            }
            Err(error) => {
                queue_trigger_for_later();
                return Err(error);
            }
        }
        // senpi `_promptAgent` refuses before `onTurnAdmitted`: a refused turn never claims a released wake.
        if self.requires_human_continuation() {
            lock(&self.delivered_held_inputs)
                .extend(held_ids.into_iter().map(|id| (id, user_abort_generation)));
            return Err(INTERRUPTED_RUN_REFUSAL.to_owned());
        }
        if let Some(claim) = &released
            && !claim()
        {
            return Ok(false);
        }
        lock(&self.delivered_held_inputs)
            .extend(held_ids.into_iter().map(|id| (id, user_abort_generation)));
        if crate::input_origin::custom_turn_origin(&message.custom_type) == InputOrigin::Engine {
            self.record_engine_turn_start(&message.custom_type);
        }
        // senpi `_promptAgent`: a new turn admitted arms the inactivity lease and the hard max-runtime cap,
        // and owns the held handler waits it starts.
        if let Some(tracker) = self.deadline_tracker.get() {
            tracker.arm();
        }
        self.handler_turn_scope.fetch_add(1, Ordering::SeqCst);
        self.agent_run_active.store(true, Ordering::SeqCst);
        self.hydrate_runtime_messages_for_run();
        self.agent
            .set_abort_server_side_fallback(Some(self.abort_server_side_fallback_enabled()));
        // Boxed: the agent run and its retry/compaction recovery futures are large enough that
        // holding them inline in this frame overflows a 2 MiB runtime thread stack.
        Box::pin(
            self.agent
                .prompt(maho_agent::agent::AgentPromptInput::Messages(messages)),
        )
        .await;
        Box::pin(self.finish_provider_turn()).await?;
        self.flush_pending_bash_messages();
        drop(_work);
        drop(_admission);
        self.emit_agent_settled().await;
        Ok(true)
    }

    pub async fn send_custom_message_content(
        &self,
        custom_type: String,
        content: maho_agent::harness::messages::CustomMessageContent,
        display: bool,
        details: Option<Value>,
        options: maho_ext_api::SendMessageOptions,
    ) -> Result<(), String> {
        let message = maho_agent::harness::messages::CustomMessage {
            role: "custom".to_owned(),
            custom_type,
            content,
            display,
            details,
            timestamp: maho_ai::utils::diagnostics::now_ms(),
        };
        self.send_custom_message_content_inner(message, options)
            .map_err(|error| error.message)
    }

    fn emit_entry_appended(&self, entry_id: &str) {
        if self.state().extension_mode != ExtensionMode::Rpc {
            return;
        }
        if let Some(entry) = self.with_session_manager(|manager| manager.entry(entry_id)) {
            self.emit(AgentSessionEvent::EntryAppended {
                entry: session_entry_from_value(entry),
            });
        }
    }

    /// Emit an event to all listeners.
    pub fn emit(&self, event: AgentSessionEvent) {
        let carrier = match &event {
            AgentSessionEvent::Agent(event) => self.observe_recovery(event),
            _ => None,
        };
        self.emit_carried(event, carrier);
    }

    /// senpi `setSessionRecoveryEpoch`: the host lane's nonce; a same-epoch rebind keeps open messages, a new epoch
    /// clears them. A session without compact updates mints nothing.
    pub fn set_session_recovery_epoch(&self, epoch: &str) {
        if let Some(identity) = lock(&self.recovery_identity).as_mut() {
            identity.reset(epoch);
        }
    }

    /// senpi `recoveryCarrier(record)`: the carrier of the record this session is emitting, read by a listener
    /// while `emit` delivers it. Rust records have no object identity, so the carrier travels beside the record.
    pub fn recovery_carrier(
        &self,
        _record: &Value,
    ) -> Option<crate::session_recovery_identity::RecoveryCarrier> {
        let owner = Arc::as_ptr(&self.inner) as usize;
        EMITTING_RECOVERY.with(|stack| {
            stack
                .borrow()
                .iter()
                .rev()
                .find(|(session, _)| *session == owner)
                .and_then(|(_, carrier)| carrier.clone())
        })
    }

    /// senpi `_recoveryIdentity.observe(event)`: mints or finds the carrier of one message event.
    fn observe_recovery(
        &self,
        event: &maho_agent::types::AgentEvent,
    ) -> Option<crate::session_recovery_identity::RecoveryCarrier> {
        use maho_agent::types::AgentEvent;
        let (kind, message) = match event {
            AgentEvent::MessageStart { message, .. } => ("message_start", message),
            AgentEvent::MessageUpdate { message, .. } => ("message_update", message),
            AgentEvent::MessageEnd { message } => ("message_end", message),
            _ => return None,
        };
        let mut identity = lock(&self.recovery_identity);
        let identity = identity.as_mut()?;
        let value = session_message_to_value(message).ok()?;
        // senpi keys non-assistant messages by object identity; a message's role, timestamp and tool call or custom
        // type are its stable handle across the start/end copies here.
        let key = format!(
            "{}:{}:{}:{}",
            message.role(),
            value["timestamp"],
            value["toolCallId"].as_str().unwrap_or_default(),
            value["customType"].as_str().unwrap_or_default()
        );
        identity.observe(kind, &value, &key)
    }

    /// `_emit` with the record's recovery carrier (`carryRecovery`): listeners read it through `recovery_carrier`.
    fn emit_carried(
        &self,
        event: AgentSessionEvent,
        carrier: Option<crate::session_recovery_identity::RecoveryCarrier>,
    ) {
        struct Pop;
        impl Drop for Pop {
            fn drop(&mut self) {
                EMITTING_RECOVERY.with(|stack| {
                    stack.borrow_mut().pop();
                });
            }
        }
        EMITTING_RECOVERY.with(|stack| {
            stack
                .borrow_mut()
                .push((Arc::as_ptr(&self.inner) as usize, carrier))
        });
        let _pop = Pop;
        self.emit_inner(event);
    }

    fn emit_inner(&self, event: AgentSessionEvent) {
        let mut event = event;
        if let AgentSessionEvent::Agent(maho_agent::types::AgentEvent::MessageEnd {
            message: AgentMessage::Llm(maho_ai::types::Message::Assistant(message)),
        }) = &mut event
        {
            *message = std::sync::Arc::new(
                crate::retry_fallback::usage_limit::describe_usage_limit_message(message),
            );
        }
        // Compaction start and end are lifecycle edges: the lifecycle getter no longer recomputes (senpi#3980).
        if matches!(
            event,
            AgentSessionEvent::CompactionStart { .. } | AgentSessionEvent::CompactionEnd { .. }
        ) {
            self.refresh_lifecycle();
        }
        self.log_session_event(&event);
        // PERF-DIVERGENCE(senpi agent-session.ts): upstream iterates its listener array; the port cloned the whole listener Vec per event, so we snapshot one copy-on-write Arc instead. Snapshot-at-emit semantics are unchanged.
        let listeners = Arc::clone(&lock(&self.event_listeners));
        for (_, listener) in listeners.iter() {
            listener(&event);
        }
        if self.structured_lane.active() {
            self.structured_lane.emit_event(&event);
        }
    }

    /// The internal structured lane (senpi#4272): every session event in `emit` order with its provider capture, plus
    /// block controls at their position. Attach before a turn's first provider push; a stream already pushing keeps
    /// its decision and yields no capture.
    pub fn subscribe_structured(
        &self,
        listener: crate::structured_session_lane::StructuredListener,
    ) -> Result<
        crate::structured_session_lane::StructuredSubscription,
        crate::structured_session_lane::StructuredLaneUnavailableError,
    > {
        self.structured_lane.subscribe(listener)
    }

    fn estimate_compaction_log_tokens(&self, source: &str) -> Option<u64> {
        let values: Vec<Value> = if source == "active" {
            self.messages()
                .iter()
                .filter_map(|m| session_message_to_value(m).ok())
                .collect()
        } else {
            self.with_session_manager(|manager| manager.current_context().messages)
        };
        let filtered = crate::messages::filter_context_excluded_messages(values);
        let estimate = crate::compaction::estimate_context_tokens(&filtered);
        Some(estimate.tokens)
    }

    /// Mirror stuck-prone lifecycle transitions into logs/session.log (content-free).
    fn log_session_event(&self, event: &AgentSessionEvent) {
        if let AgentSessionEvent::CompactionStart { reason, request_id } = event {
            let previous_attempt = self.state().active_compaction_log_attempt.clone();
            if let Some(previous_attempt) = previous_attempt {
                let tokens_after = self.estimate_compaction_log_tokens("persisted");
                let mut data = Map::new();
                data.insert(
                    "attemptId".to_owned(),
                    Value::String(previous_attempt.id.clone()),
                );
                data.insert(
                    "reason".to_owned(),
                    Value::String(
                        match previous_attempt.reason {
                            maho_ext_api::CompactionReason::Manual => "manual",
                            maho_ext_api::CompactionReason::Threshold => "threshold",
                            maho_ext_api::CompactionReason::Overflow => "overflow",
                            maho_ext_api::CompactionReason::PrePrompt => "pre-prompt",
                            maho_ext_api::CompactionReason::Branch => "branch",
                            maho_ext_api::CompactionReason::Extension => "extension",
                        }
                        .to_owned(),
                    ),
                );
                data.insert(
                    "mode".to_owned(),
                    Value::String(
                        if previous_attempt.reason == maho_ext_api::CompactionReason::Manual {
                            "manual"
                        } else {
                            "auto"
                        }
                        .to_owned(),
                    ),
                );
                data.insert("action".to_owned(), Value::String("compact".to_owned()));
                data.insert(
                    "disposition".to_owned(),
                    Value::String("superseded".to_owned()),
                );
                data.insert("accepted".to_owned(), Value::Bool(false));
                data.insert("skipped".to_owned(), Value::Bool(true));
                data.insert("aborted".to_owned(), Value::Bool(true));
                data.insert("willRetry".to_owned(), Value::Bool(false));
                if let Some(tokens_before) = previous_attempt.tokens_before {
                    data.insert("tokensBefore".to_owned(), serde_json::json!(tokens_before));
                }
                if let Some(tokens_after) = tokens_after {
                    data.insert("tokensAfter".to_owned(), serde_json::json!(tokens_after));
                }
                self.session_logger.info("compaction_decision", Some(&data));
                self.state()
                    .superseded_compaction_log_attempt_ids
                    .insert(previous_attempt.id);
            }
            let attempt_id = request_id
                .clone()
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let tokens_before = self.estimate_compaction_log_tokens("persisted");
            let attempt = CompactionLogAttempt {
                id: attempt_id.clone(),
                reason: *reason,
                tokens_before,
            };
            self.state().active_compaction_log_attempt = Some(attempt.clone());
            let mut data = Map::new();
            data.insert("attemptId".to_owned(), Value::String(attempt.id));
            data.insert(
                "reason".to_owned(),
                Value::String(
                    match reason {
                        maho_ext_api::CompactionReason::Manual => "manual",
                        maho_ext_api::CompactionReason::Threshold => "threshold",
                        maho_ext_api::CompactionReason::Overflow => "overflow",
                        maho_ext_api::CompactionReason::PrePrompt => "pre-prompt",
                        maho_ext_api::CompactionReason::Branch => "branch",
                        maho_ext_api::CompactionReason::Extension => "extension",
                    }
                    .to_owned(),
                ),
            );
            data.insert(
                "mode".to_owned(),
                Value::String(
                    if *reason == maho_ext_api::CompactionReason::Manual {
                        "manual"
                    } else {
                        "auto"
                    }
                    .to_owned(),
                ),
            );
            data.insert("action".to_owned(), Value::String("compact".to_owned()));
            if let Some(tokens_before) = attempt.tokens_before {
                data.insert("tokensBefore".to_owned(), serde_json::json!(tokens_before));
            }
            self.session_logger.info("compaction_start", Some(&data));
            return;
        }

        if let AgentSessionEvent::CompactionEnd {
            reason,
            result,
            aborted,
            will_retry,
            request_id,
            accepted,
            rejection_cause,
            error_message,
        } = event
        {
            if let Some(req_id) = request_id
                && self
                    .state()
                    .superseded_compaction_log_attempt_ids
                    .remove(req_id)
            {
                return;
            }
            let active_attempt = self.state().active_compaction_log_attempt.clone();
            let attempt = if request_id.is_some()
                && active_attempt.as_ref().map(|a| &a.id) == request_id.as_ref()
            {
                active_attempt
            } else {
                None
            };
            let is_accepted = accepted.unwrap_or(result.is_some());
            let is_rejected = rejection_cause.is_some();
            let is_skipped = !is_accepted
                && (attempt.is_none() || (!is_rejected && !*aborted && error_message.is_none()));
            let disposition = if is_accepted {
                "committed"
            } else if attempt.is_none() {
                "skipped"
            } else if is_rejected {
                "rejected"
            } else if *aborted {
                "aborted"
            } else if error_message.is_some() {
                "failed"
            } else {
                "skipped"
            };
            let tokens_after = self.estimate_compaction_log_tokens(if is_accepted {
                "active"
            } else {
                "persisted"
            });
            let mut data = Map::new();
            if let Some(attempt_id) = attempt
                .as_ref()
                .map(|a| a.id.as_str())
                .or(request_id.as_deref())
            {
                data.insert("attemptId".to_owned(), Value::String(attempt_id.to_owned()));
            }
            data.insert(
                "reason".to_owned(),
                Value::String(
                    match reason {
                        maho_ext_api::CompactionReason::Manual => "manual",
                        maho_ext_api::CompactionReason::Threshold => "threshold",
                        maho_ext_api::CompactionReason::Overflow => "overflow",
                        maho_ext_api::CompactionReason::PrePrompt => "pre-prompt",
                        maho_ext_api::CompactionReason::Branch => "branch",
                        maho_ext_api::CompactionReason::Extension => "extension",
                    }
                    .to_owned(),
                ),
            );
            data.insert(
                "mode".to_owned(),
                Value::String(
                    if *reason == maho_ext_api::CompactionReason::Manual {
                        "manual"
                    } else {
                        "auto"
                    }
                    .to_owned(),
                ),
            );
            data.insert(
                "action".to_owned(),
                Value::String(
                    if is_accepted || attempt.is_some() {
                        "compact"
                    } else {
                        "none"
                    }
                    .to_owned(),
                ),
            );
            data.insert(
                "disposition".to_owned(),
                Value::String(disposition.to_owned()),
            );
            data.insert("accepted".to_owned(), Value::Bool(is_accepted));
            data.insert("skipped".to_owned(), Value::Bool(is_skipped));
            data.insert("aborted".to_owned(), Value::Bool(*aborted));
            data.insert("willRetry".to_owned(), Value::Bool(*will_retry));
            if let Some(cause) = rejection_cause {
                data.insert(
                    "rejectionCause".to_owned(),
                    Value::String(
                        match cause {
                            maho_ext_api::CompactionRejectionCause::CancelledByExtension => {
                                "cancelled-by-extension"
                            }
                            maho_ext_api::CompactionRejectionCause::ExternalOwner => {
                                "external-owner"
                            }
                            maho_ext_api::CompactionRejectionCause::WouldOverflow => {
                                "would-overflow"
                            }
                            maho_ext_api::CompactionRejectionCause::CircuitBreaker => {
                                "circuit-breaker"
                            }
                            maho_ext_api::CompactionRejectionCause::PerTurnCap => "per-turn-cap",
                            maho_ext_api::CompactionRejectionCause::StaleRevision => {
                                "stale-revision"
                            }
                        }
                        .to_owned(),
                    ),
                );
            }
            if let Some(err) = error_message {
                data.insert("error".to_owned(), Value::String(err.clone()));
            }
            let tokens_before = attempt
                .as_ref()
                .and_then(|a| a.tokens_before)
                .or(tokens_after);
            if let Some(tokens_before) = tokens_before {
                data.insert("tokensBefore".to_owned(), serde_json::json!(tokens_before));
            }
            if let Some(tokens_after) = tokens_after {
                data.insert("tokensAfter".to_owned(), serde_json::json!(tokens_after));
            }
            self.session_logger.info("compaction_decision", Some(&data));
            if attempt.is_some() {
                self.state().active_compaction_log_attempt = None;
            }
            return;
        }

        if let AgentSessionEvent::Agent(maho_agent::types::AgentEvent::MessageEnd { message }) =
            event
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

    pub fn admit_resume_compaction_required(&self, projection: serde_json::Value, notice: String) {
        self.state().resume_compaction_requirement = Some((projection, notice));
    }

    pub fn apply_resume_slice(&self, slice: ResumeSliceState) {
        self.state().resume_slice = Some(slice.clone());
        self.emit(AgentSessionEvent::ResumeContextReduced {
            tokens_before: slice.tokens_before,
            tokens_after: slice.tokens_after,
            dropped_entries: slice.dropped_entries,
            notice: slice.notice,
        });
    }

    /// Subscribe to session events. The returned handle unsubscribes when dropped.
    pub fn subscribe(&self, listener: AgentSessionEventListener) -> AgentSessionSubscription {
        let id = self.next_listener_id.fetch_add(1, Ordering::SeqCst);
        // PERF-DIVERGENCE(senpi agent-session.ts): copy-on-write listener list so emit can snapshot with one Arc clone; subscription order is unchanged.
        let mut guard = lock(&self.event_listeners);
        Arc::make_mut(&mut guard).push((id, listener.clone()));
        drop(guard);
        let (resume_req, resume_slice, standing_recovery) = {
            let state = self.state();
            (
                state.resume_compaction_requirement.clone(),
                state.resume_slice.clone(),
                state.standing_recovery.clone(),
            )
        };
        if let Some((projection, notice)) = resume_req {
            listener(&AgentSessionEvent::ResumeCompactionRequired { projection, notice });
        }
        if let Some(slice) = resume_slice {
            listener(&AgentSessionEvent::ResumeContextReduced {
                tokens_before: slice.tokens_before,
                tokens_after: slice.tokens_after,
                dropped_entries: slice.dropped_entries,
                notice: slice.notice,
            });
        }
        if let Some(recovery) = standing_recovery {
            listener(&recovery);
        }
        let sources = self.with_settings_manager(|manager| manager.get_selected_settings_sources());
        for source in sources {
            listener(&AgentSessionEvent::SettingsSourceSelected {
                selection: serde_json::json!({
                    "path": source.path,
                    "scope": source.scope.as_str(),
                    "format": match source.format { crate::settings_manager::SettingsFormat::Jsonc => "jsonc", crate::settings_manager::SettingsFormat::Json => "json" },
                    "reason": match source.reason { crate::settings_manager::SettingsSourceReason::ExplicitJsonc => "explicit-jsonc", crate::settings_manager::SettingsSourceReason::JsonOnly => "json-only" },
                }),
            });
        }
        AgentSessionSubscription {
            listeners: Arc::clone(&self.event_listeners),
            id,
        }
    }

    /// Remove all listeners and release session-scoped resources.
    pub async fn dispose(&self) {
        if self.state().disposed {
            return;
        }
        self.structured_lane.dispose();
        if let Some(unregister) = lock(&self.inner.memory_report_unregister).take() {
            unregister();
        }
        lock(&self.inner.nested_host_calls).clear();
        let disposers = std::mem::take(&mut *lock(&self.inner.tool_context_disposers));
        for dispose in disposers {
            dispose();
        }
        self.monitor_generation.fetch_add(1, Ordering::SeqCst);
        self.state().disposed = true;
        self.lifecycle_disposed.store(true, Ordering::SeqCst);
        lock(&self.engine_turn_hold).invalidate();
        if let Some(tracker) = self.deadline_tracker.get() {
            tracker.disarm();
        }
        lock(&self.settled_delivery).cancel();
        self.fail_all_queued_custom_receipts("session disposed before custom message delivery");
        if let Some(controller) = self.state().session_title_abort_controller.take() {
            controller.abort(None);
        }
        self.cancel_probe_back();
        self.abort_retry();
        self.abort_compaction();
        self.abort_branch_summary();
        self.abort_bash();
        self.agent.abort(None);
        lock(&self.wake_source_subscription).take();
        lock(&self.lifecycle_fact_subscriptions).clear();
        lock(&self.session_manager).clear_recovery_listener();
        self.state().security_notices.dispose();
        crate::mcp_grant_presentations::clear_mcp_grant_presentations(&self.session_id());
        lock(&self.extension_rpc_event_forward).take();
        lock(&self.settings_source_subscription).take();
        lock(&self.agent_subscription).take();
        self.state().extension_event_sender.take();
        self.state().extension_events = None;
        self.state().mcp_wire_status = None;
        lock(&self.mcp_wire_status_retention).take();
        lock(&self.extension_rpc_event_forwarding).take();
        lock(&self.extension_rpc_event_listeners).clear();
        {
            let mut guard = self.extension_runner.lock().await;
            if let Some(runner) = guard.as_mut() {
                runner.invalidate(
                    "This extension ctx is stale after session replacement or reload. Do not use a captured pi or command ctx after ctx.newSession(), ctx.fork(), ctx.switchSession(), or ctx.reload(). For newSession, fork, and switchSession, move post-replacement work into withSession and use the ctx passed to withSession. For reload, do not use the old ctx after await ctx.reload().",
                );
            }
        }
        // In-process offload jobs are closed with the child: replacement awaits both before the next session runs.
        {
            let dispatcher = Arc::clone(&self.tool_dispatcher);
            let child = self.tool_child.clone();
            let closed: futures::future::BoxFuture<'static, ()> = Box::pin(async move {
                let in_process_closed = dispatcher.dispose();
                let child_closed = async {
                    if let Some(child) = child {
                        child.close().await;
                    }
                };
                futures::future::join(in_process_closed, child_closed).await;
            });
            let closed = futures::future::FutureExt::shared(closed);
            tokio::spawn(closed.clone());
            *lock(&self.tool_child_closed) = Some(closed);
        }
        // PERF-DIVERGENCE(senpi agent-session.ts): listener list is a copy-on-write Arc (see emit); clearing swaps in an empty one.
        *lock(&self.event_listeners) = Arc::new(Vec::new());
        let _ = maho_ai::session_resources::cleanup_session_resources(Some(&self.session_id()));
        self.agent.set_messages(Vec::new());
        self.agent.set_tools(Vec::new());
        self.agent.set_declared_tools(None);
        self.agent.set_system_prompt(String::new());
        {
            let mut state = self.state();
            state.tool_registry.clear();
            state.base_tool_definitions.clear();
            state.tool_definitions.clear();
            state.base_tools_override = None;
            state.custom_tools.clear();
            state.tool_prompt_snippets.clear();
            state.tool_prompt_guidelines.clear();
            state.base_system_prompt = String::new();
            state.resolved_system_prompt = None;
            state.abort_provenance = Default::default();
        }
        self.with_session_manager_mut(|manager| manager.dispose());
        maho_tool_child::allocator::reclaim_unused_heap();
    }

    /// Reserve a global order for input temporarily owned outside the native queues.
    pub fn reserve_queued_input_order(&self) -> u64 {
        let mut state = self.state();
        state.next_queued_input_order += 1;
        state.next_queued_input_order
    }

    fn queued_delivery_tickets(&self) -> Vec<String> {
        self.agent
            .peek_queued_messages(true)
            .iter()
            .filter_map(message_ticket)
            .collect()
    }

    fn release_queued_deliveries(&self, _tickets: &[String], _cause: &str) {}

    /// senpi `_recordQueuedInput`: the queued record at revision 1, under the id the store admitted it with or a
    /// newly minted one.
    fn record_queued_input(
        &self,
        text: &str,
        mode: StreamingBehavior,
        enqueue_order: Option<u64>,
        client_message_id: Option<String>,
        admitted_queued_id: Option<String>,
    ) -> String {
        let mut state = self.state();
        let order = enqueue_order.unwrap_or_else(|| {
            state.next_queued_input_order += 1;
            state.next_queued_input_order
        });
        state.next_queued_input_order = state.next_queued_input_order.max(order);
        let queued_id = admitted_queued_id.unwrap_or_else(|| mint_queued_id(&mut state));
        state
            .queued_input_ages
            .note_queued(&queued_id, chrono::Utc::now().timestamp_millis());
        state.queued_input_order.push(QueuedInput {
            text: text.to_owned(),
            mode,
            enqueue_order: order,
            queued_id: queued_id.clone(),
            revision: 1,
            client_message_id,
        });
        queued_id
    }

    /// senpi `_removeQueuedInput`: a queued input reached a turn; forget its record and remember its id as
    /// delivered.
    fn remove_queued_input(&self, text: &str, mode: StreamingBehavior) {
        let mut state = self.state();
        let Some(index) = state
            .queued_input_order
            .iter()
            .position(|message| message.mode == mode && message.text == text)
        else {
            return;
        };
        let delivered = state.queued_input_order.remove(index);
        state
            .queued_messages
            .retain(|(queued_id, _)| *queued_id != delivered.queued_id);
        state.delivered_queued_ids.push_back(delivered.queued_id);
        if state.delivered_queued_ids.len() > 256 {
            state.delivered_queued_ids.pop_front();
        }
    }

    /// senpi `dequeueQueuedInput`: removes one queued input before it reaches a turn, from the visible lists and
    /// the native queue alike. An input already drained into a turn answers `already_delivered`.
    pub fn dequeue_queued_input(
        &self,
        queued_id: &str,
        expected_revision: Option<u64>,
    ) -> QueuedInputChange {
        self.change_queued_input_now(queued_id, None, expected_revision)
    }

    /// senpi `editQueuedInput`: replaces the text of one queued input in place, keeping its order and identity,
    /// and bumps its revision.
    pub fn edit_queued_input(
        &self,
        queued_id: &str,
        text: &str,
        expected_revision: Option<u64>,
    ) -> QueuedInputChange {
        self.change_queued_input_now(queued_id, Some(text), expected_revision)
    }

    /// senpi `changeQueuedInput`: removes (`text: None`) or edits one queued input. An input in durable custody
    /// (admitted with a client id) changes in its record first and in the native queue in the same gated step;
    /// any other input changes as `dequeue_queued_input` / `edit_queued_input` do.
    pub async fn change_queued_input(
        &self,
        queued_id: &str,
        text: Option<String>,
        expected_revision: Option<u64>,
        effect_id: Option<String>,
    ) -> Result<QueuedInputChange, String> {
        let client_message_id = self
            .state()
            .queued_input_order
            .iter()
            .find(|input| input.queued_id == queued_id)
            .and_then(|input| input.client_message_id.clone());
        if let (Some(client_message_id), Some(pending)) =
            (client_message_id, self.pending_inputs.get())
        {
            let native_text = text.clone();
            let durable = pending
                .change(
                    crate::pending_input_session::QueuedInputChangeRequest {
                        client_message_id,
                        text,
                        expected_revision,
                        effect_id,
                    },
                    || self.change_queued_input_now(queued_id, native_text.as_deref(), None),
                )
                .await
                .map_err(|error| error.to_string())?;
            if let Some(outcome) = durable {
                return Ok(outcome);
            }
            return Ok(self.change_queued_input_now(
                queued_id,
                native_text.as_deref(),
                expected_revision,
            ));
        }
        Ok(self.change_queued_input_now(queued_id, text.as_deref(), expected_revision))
    }

    /// senpi `_changeQueuedInput`: synchronous from the revision check to the change, so nothing else on the
    /// session can land in between.
    fn change_queued_input_now(
        &self,
        queued_id: &str,
        next_text: Option<&str>,
        expected_revision: Option<u64>,
    ) -> QueuedInputChange {
        let changed = {
            let mut guard = self.state();
            // A plain `&mut` so the fields below borrow apart.
            let state = &mut *guard;
            let Some(index) = state
                .queued_input_order
                .iter()
                .position(|input| input.queued_id == queued_id)
            else {
                return if state.delivered_queued_ids.iter().any(|id| id == queued_id) {
                    QueuedInputChange::AlreadyDelivered
                } else {
                    QueuedInputChange::NotFound
                };
            };
            let record = state.queued_input_order[index].clone();
            let message = state
                .queued_messages
                .iter()
                .find(|(id, _)| id == queued_id)
                .map(|(_, message)| message.clone());
            // Only user input is queued by id; anything else cannot be edited, and must not be removed by an edit.
            let is_user = message
                .as_ref()
                .is_some_and(|message| message.role() == "user");
            if next_text.is_some() && !is_user {
                return QueuedInputChange::NotFound;
            }
            let next_message = match (next_text, &message) {
                (Some(text), Some(message)) if is_user => Some(with_user_text(message, text)),
                _ => None,
            };
            // Equal queued messages are told apart by their place among the inputs queued in the same mode.
            let occurrence = message.as_ref().map_or(0, |message| {
                state.queued_input_order[..index]
                    .iter()
                    .filter(|input| input.mode == record.mode)
                    .filter(|input| {
                        state
                            .queued_messages
                            .iter()
                            .any(|(id, queued)| *id == input.queued_id && queued == message)
                    })
                    .count()
            });
            let deferred = match record.mode {
                StreamingBehavior::Steer => &state.post_compaction_deferred_steering_messages,
                StreamingBehavior::FollowUp => &state.post_compaction_deferred_follow_up_messages,
            };
            let deferred_index = message.as_ref().and_then(|message| {
                deferred
                    .iter()
                    .enumerate()
                    .filter(|(_, queued)| *queued == message)
                    .nth(occurrence)
                    .map(|(position, _)| position)
            });
            let queue = match record.mode {
                StreamingBehavior::Steer => maho_agent::types::PendingQueue::Steering,
                StreamingBehavior::FollowUp => maho_agent::types::PendingQueue::FollowUp,
            };
            if expected_revision.is_some_and(|expected| expected != record.revision) {
                // An input that already left the native queue answers already_delivered, as without a revision;
                // replacing the message with itself is a read-only probe of that queue.
                let queued = message.as_ref().is_some_and(|message| {
                    deferred_index.is_some()
                        || self.agent.change_queued_message(
                            queue,
                            message,
                            occurrence,
                            Some(message.clone()),
                        )
                });
                return if queued {
                    QueuedInputChange::Conflict
                } else {
                    QueuedInputChange::AlreadyDelivered
                };
            }
            let still_queued = message.as_ref().is_some_and(|message| {
                deferred_index.is_some()
                    || self.agent.change_queued_message(
                        queue,
                        message,
                        occurrence,
                        next_message.clone(),
                    )
            });
            if !still_queued {
                return QueuedInputChange::AlreadyDelivered;
            }
            if let Some(position) = deferred_index {
                let deferred = match record.mode {
                    StreamingBehavior::Steer => {
                        &mut state.post_compaction_deferred_steering_messages
                    }
                    StreamingBehavior::FollowUp => {
                        &mut state.post_compaction_deferred_follow_up_messages
                    }
                };
                match &next_message {
                    None => {
                        deferred.remove(position);
                    }
                    Some(next) => deferred[position] = next.clone(),
                }
            }
            let text_occurrence = state.queued_input_order[..index]
                .iter()
                .filter(|input| input.mode == record.mode && input.text == record.text)
                .count();
            let texts = match record.mode {
                StreamingBehavior::Steer => &mut state.steering_messages,
                StreamingBehavior::FollowUp => &mut state.follow_up_messages,
            };
            let text_index = texts
                .iter()
                .enumerate()
                .filter(|(_, text)| **text == record.text)
                .nth(text_occurrence)
                .map(|(position, _)| position);
            match (next_text, next_message) {
                (Some(text), Some(next_message)) => {
                    if let Some(position) = text_index {
                        texts[position] = text.to_owned();
                    }
                    state.queued_input_order[index] = QueuedInput {
                        text: text.to_owned(),
                        revision: record.revision + 1,
                        ..record
                    };
                    if let Some(slot) = state
                        .queued_messages
                        .iter_mut()
                        .find(|(id, _)| id == queued_id)
                    {
                        slot.1 = next_message;
                    }
                    QueuedInputChange::Edited
                }
                _ => {
                    if let Some(position) = text_index {
                        texts.remove(position);
                    }
                    state.queued_input_order.remove(index);
                    state.queued_messages.retain(|(id, _)| id != queued_id);
                    QueuedInputChange::Removed
                }
            }
        };
        self.emit_queue_update();
        changed
    }

    /// senpi `getQueuedInputs`: the queued inputs sorted by enqueue order, each with its `queuedId` and revision.
    pub fn get_queued_inputs(&self) -> Vec<QueuedInput> {
        self.get_queued_input_order()
    }

    /// senpi `getPendingInputs`: the pending inputs in durable custody, and an explicit outcome for each id the
    /// caller already knows.
    pub async fn get_pending_inputs(
        &self,
        known_client_message_ids: &[String],
    ) -> Result<crate::pending_input_session::PendingInputSnapshot, String> {
        let Some(pending) = self.pending_inputs.get() else {
            return Ok(crate::pending_input_session::PendingInputSnapshot {
                lifetime_id: String::new(),
                through_cursor: 0,
                inputs: Vec::new(),
                outcomes: known_client_message_ids
                    .iter()
                    .map(|client_message_id| {
                        crate::pending_input_session::PendingInputOutcome::NotAdmitted {
                            client_message_id: client_message_id.clone(),
                        }
                    })
                    .collect(),
            });
        };
        pending
            .snapshot(known_client_message_ids)
            .await
            .map_err(|error| error.to_string())
    }

    /// senpi constructor: `this._pendingInputs = new SessionPendingInputs({ sessionManager, nextQueuedId })`,
    /// sharing the session's queue-id counter. A session file that already journals pending inputs opens the
    /// store now.
    ///
    /// GAP: upstream also installs the store's queue port on the agent, publishes `pending_input_update`, and
    /// admits identified steer/follow-up input into the store (`_enqueueQueuedInput`); core's queue entry points
    /// carry no client id yet, so nothing here admits into the store and the port stays uninstalled.
    fn install_pending_inputs(&self) {
        let weak = Arc::downgrade(&self.inner);
        let next_weak = weak.clone();
        let pending = crate::pending_input_session::SessionPendingInputs::new(
            crate::pending_input_session::SessionPendingInputsOptions {
                session_manager: Arc::new(SessionJournal(weak)),
                next_queued_id: Arc::new(move || {
                    next_weak
                        .upgrade()
                        .map(|inner| mint_queued_id(&mut AgentSession { inner }.state()))
                        .unwrap_or_default()
                }),
                on_change: None,
                journal: None,
                client_message_id_of: Arc::new(|_| None),
            },
        );
        let _ = self.pending_inputs.set(pending);
    }

    /// Clear all queued messages and return them. `ordered` preserves the recovery order across
    /// both native queue modes.
    pub fn clear_queue(&self, abort_will_follow: bool) -> ClearedQueue {
        let cleared_custom_messages: Vec<maho_agent::harness::messages::CustomMessage> = {
            let state = self.state();
            let receipts = lock(&self.queued_custom_receipts);
            receipts
                .iter()
                .filter(|(_, receipt)| receipt.outcome().is_none())
                .filter_map(|(custom, _)| {
                    let agent_message = AgentMessage::Custom(
                        maho_agent::types::CustomAgentMessage::Custom(custom.clone()),
                    );
                    let in_cleared_queue = self.agent.contains_queued_message(&agent_message)
                        || state
                            .post_compaction_deferred_steering_messages
                            .contains(&agent_message)
                        || state
                            .post_compaction_deferred_follow_up_messages
                            .contains(&agent_message);
                    in_cleared_queue.then(|| custom.clone())
                })
                .collect()
        };
        let steering;
        let follow_up;
        let ordered;
        {
            let mut state = self.state();
            steering = std::mem::take(&mut state.steering_messages);
            follow_up = std::mem::take(&mut state.follow_up_messages);
            ordered = std::mem::take(&mut state.queued_input_order);
            state.queued_messages.clear();
            state.post_compaction_deferred_steering_messages.clear();
            state.post_compaction_deferred_follow_up_messages.clear();
            if abort_will_follow && (!steering.is_empty() || !follow_up.is_empty()) {
                state.had_cleared_queued_messages = true;
            }
        }
        let mut ordered = ordered;
        ordered.sort_by_key(|input| input.enqueue_order);
        let cleared_deliveries = self.queued_delivery_tickets();
        self.agent.clear_all_queues();
        self.release_queued_deliveries(&cleared_deliveries, "cleared");
        // A removed queued message is never written: every send waiting on it settles now as cleared (senpi#4106).
        for custom in cleared_custom_messages {
            let is_appended =
                serde_json::to_value(&custom.content)
                    .ok()
                    .is_some_and(|content_val| {
                        self.with_session_manager(|manager| {
                            manager.entries().iter().rev().any(|e| {
                                let details_match = match (&custom.details, e.get("details")) {
                                    (Some(expected), Some(actual)) => expected == actual,
                                    (None, None) | (None, Some(Value::Null)) => true,
                                    _ => false,
                                };
                                e.get("type").and_then(Value::as_str) == Some("custom_message")
                                    && e.get("customType").and_then(Value::as_str)
                                        == Some(custom.custom_type.as_str())
                                    && e.get("content") == Some(&content_val)
                                    && e.get("display").and_then(Value::as_bool)
                                        == Some(custom.display)
                                    && details_match
                            })
                        })
                    });
            if !is_appended {
                self.settle_queued_custom_receipt(
                    &custom,
                    Err(crate::itemized_send::QueuedDeliveryCleared {
                        item_ids: Vec::new(),
                    }
                    .to_string()),
                );
            }
        }
        self.emit_queue_update();
        ClearedQueue {
            steering,
            follow_up,
            ordered,
        }
    }

    pub fn pending_message_count(&self) -> usize {
        let state = self.state();
        state.steering_messages.len() + state.follow_up_messages.len()
    }

    #[cfg(test)]
    fn flush_pending_next_turn_messages(&self) -> Result<(), String> {
        let pending = std::mem::take(&mut self.state().pending_next_turn_messages);
        for message in pending {
            if let AgentMessage::Custom(maho_agent::types::CustomAgentMessage::Custom(custom)) =
                message
            {
                self.append_extension_custom_message(custom)?;
            }
        }
        Ok(())
    }

    fn append_extension_custom_message(
        &self,
        message: maho_agent::harness::messages::CustomMessage,
    ) -> Result<(), String> {
        let content = serde_json::to_value(&message.content).map_err(|error| {
            let error_message = error.to_string();
            self.settle_queued_custom_receipt(&message, Err(error_message.clone()));
            error_message
        })?;
        let start = maho_agent::types::AgentEvent::MessageStart {
            message: AgentMessage::Custom(maho_agent::types::CustomAgentMessage::Custom(
                message.clone(),
            )),
            streaming: None,
        };
        let recovery = self.observe_recovery(&start);
        let entry = self.with_session_manager_mut(|manager| {
            manager.append_custom_message_with_recovery(
                &message.custom_type,
                content,
                message.display,
                message.details.clone(),
                recovery.as_ref(),
            )
        });
        if let Some(id) = entry.get("id").and_then(Value::as_str) {
            self.emit_entry_appended(id);
        }
        self.settle_queued_custom_receipt(&message, Ok(()));
        let message = AgentMessage::Custom(maho_agent::types::CustomAgentMessage::Custom(message));
        let mut messages = self.messages();
        messages.push(message.clone());
        self.agent.set_messages(messages);
        self.state().message_revision += 1;
        self.emit_carried(AgentSessionEvent::Agent(start), recovery);
        self.emit(AgentSessionEvent::Agent(
            maho_agent::types::AgentEvent::MessageEnd { message },
        ));
        Ok(())
    }

    pub fn get_steering_messages(&self) -> Vec<String> {
        self.state().steering_messages.clone()
    }

    pub fn get_follow_up_messages(&self) -> Vec<String> {
        self.state().follow_up_messages.clone()
    }

    /// senpi's session-state `ordered` projection: the queued inputs sorted by enqueue order.
    pub fn get_queued_input_order(&self) -> Vec<QueuedInput> {
        let mut ordered = self.state().queued_input_order.clone();
        ordered.sort_by_key(|input| input.enqueue_order);
        ordered
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
            Some(
                state
                    .scoped_models
                    .iter()
                    .map(|scoped| format!("{}/{}", scoped.model.provider, scoped.model.id))
                    .collect(),
            )
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
            if narrowed
                .as_ref()
                .is_some_and(|narrowed| !narrowed.contains(&id))
            {
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
    /// explain the switch. `chainConfigured` is the retry-fallback controller's
    /// `hasConfiguredChain()`: the current selector against the canonical chains, so a chain keyed
    /// by a bare model family counts.
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

    /// senpi `this._retryFallback.hasConfiguredChain()`. The event is emitted synchronously, so a
    /// controller that is busy at this instant (or not yet installed) is answered from the settings'
    /// chains as written.
    fn has_configured_fallback_chain(&self) -> bool {
        if let Ok(mut controller) = self.retry_fallback.try_lock()
            && let Some(controller) = controller.as_mut()
        {
            return controller.has_configured_chain();
        }
        let settings = self.with_settings_manager(|manager| manager.get_value("retry").cloned());
        let chains =
            crate::retry_fallback::settings::resolve_retry_fallback_settings(settings.as_ref())
                .chains;
        crate::retry_fallback::chains::resolve_chain_key(
            &self.model(),
            Some(self.agent.thinking_level()),
            &chains,
        )
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
        self.emit(AgentSessionEvent::SessionInfoChanged {
            name: self.session_name(),
        });
        let sender = self.state().extension_event_sender.clone();
        if let Some(sender) = sender {
            let _ = sender.send(maho_ext_api::ExtensionEvent::SessionInfoChanged {
                name: self.session_name(),
            });
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
            let text = match message.get("content") {
                Some(Value::String(text)) => text.clone(),
                Some(content) => {
                    serde_json::from_value::<Vec<maho_ai::types::ContentBlock>>(content.clone())
                        .map_or_else(
                            |_| String::new(),
                            |content| maho_ai::utils::text::content_text(&content, ""),
                        )
                }
                None => String::new(),
            };
            if !text.is_empty() {
                let entry_id = entry
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                result.push((entry_id, text));
            }
        }
        result
    }

    /// Session statistics, aggregated over every session entry (including compacted-away history).
    pub fn get_session_stats(&self) -> SessionStats {
        enum FoldInput {
            Cached(SessionStats),
            Appended(Vec<Value>, SessionStats, crate::usage_totals::UsageTotals),
            All(SessionStatsCounts, crate::usage_totals::UsageTotals),
        }
        let previous = self.state().session_stats_cache.clone();
        // Entries are append-only, so (session id, entry count, leaf id) pins the aggregate below.
        // The key and the entries to fold are read under one lock so an append cannot land between
        // them and be folded twice. Within a session only the entries appended since the cached
        // aggregate are folded; everything is rescanned when the session changed or the tail is gone.
        let (key, input): (SessionStatsCacheKey, FoldInput) =
            self.with_session_manager(|manager| {
                let key = (
                    manager.session_id().to_owned(),
                    manager.get_entry_count(),
                    manager.leaf_id().map(str::to_owned),
                );
                // A full rescan folds the history in place: the counters read only types, roles,
                // usage and block kinds, so the spilled tool-result strings are never rehydrated.
                let fold_all = || {
                    let mut counts = SessionStatsCounts::default();
                    let mut totals = crate::usage_totals::create_usage_totals();
                    manager.visit_history_unmaterialized(&mut |entry| {
                        counts.fold_entry(entry, &mut totals);
                    });
                    FoldInput::All(counts, totals)
                };
                let input = match previous {
                    Some((cached_key, stats, _)) if cached_key == key => FoldInput::Cached(stats),
                    Some((cached_key, stats, totals))
                        if cached_key.0 == key.0 && cached_key.1 <= key.1 =>
                    {
                        match manager.entries_after(cached_key.1) {
                            Some(entries) => FoldInput::Appended(entries, stats, totals),
                            None => fold_all(),
                        }
                    }
                    _ => fold_all(),
                };
                (key, input)
            });
        let (counts, totals) = match input {
            FoldInput::Cached(stats) => {
                return SessionStats {
                    session_file: self.session_file(),
                    context_usage: self.get_context_usage(),
                    ..stats
                };
            }
            FoldInput::Appended(entries, stats, mut totals) => {
                let mut counts = SessionStatsCounts::from_stats(&stats);
                counts.fold(&entries, &mut totals);
                (counts, totals)
            }
            FoldInput::All(counts, totals) => (counts, totals),
        };
        let SessionStatsCounts {
            user_messages,
            assistant_messages,
            tool_results,
            total_messages,
            tool_calls,
        } = counts;
        let stats = SessionStats {
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
                total: (totals.input + totals.output + totals.cache_read + totals.cache_write)
                    as u64,
            },
            cost: totals.cost,
            cost_unknown: totals.cost_unknown,
            context_usage: self.get_context_usage(),
        };
        self.state().session_stats_cache = Some((key, stats.clone(), totals));
        stats
    }

    /// Context usage for the live model, or `None` when the model has no context window.
    pub fn get_context_usage(&self) -> Option<ContextUsage> {
        let model = self.model();
        let context_window = model.context_window;
        if context_window == 0 {
            return None;
        }
        let (session_id, entry_count, leaf_id) = self.with_session_manager(|manager| {
            (
                manager.session_id().to_owned(),
                manager.get_entry_count(),
                manager.leaf_id().map(str::to_owned),
            )
        });
        let last_fingerprint = message_fingerprint(self.messages().last());
        let key = {
            let state = self.state();
            ContextUsageCacheKey {
                session_id,
                message_revision: state.message_revision,
                entry_count,
                leaf_id,
                awaiting_persistence: state.messages_awaiting_persistence.len(),
                provider: model.provider,
                model_id: model.id,
                context_window,
                last_fingerprint,
            }
        };
        let cached = self
            .state()
            .context_usage_cache
            .as_ref()
            .and_then(|(cached_key, usage)| (*cached_key == key).then_some(*usage));
        if let Some(usage) = cached {
            return usage;
        }
        let usage = self.compute_context_usage(context_window);
        self.state().context_usage_cache = Some((key, usage));
        usage
    }

    fn compute_context_usage(&self, context_window: u64) -> Option<ContextUsage> {
        let messages: Vec<Value> = self
            .messages()
            .iter()
            .filter_map(|message| serde_json::to_value(message).ok())
            .collect();
        let messages = crate::messages::filter_context_excluded_messages(messages);
        // Id-only walk first: only a compacted branch needs entries, and only from its latest
        // compaction to the leaf; materializing the whole branch rehydrated every spilled tool result.
        let compacted = self
            .latest_current_branch_entry_id("compaction", None)
            .is_some();
        let branch = if compacted {
            self.with_session_manager(|manager| {
                manager
                    .current_branch_tail(0, &mut |entry: &Value| {
                        entry.get("type").and_then(Value::as_str) == Some("compaction")
                    })
                    .1
            })
        } else {
            Vec::new()
        };
        if let Some(latest) = crate::session_manager::get_latest_compaction_entry(&branch) {
            let compaction_index = branch
                .iter()
                .rposition(|entry| entry == &latest)
                .unwrap_or(0);
            let has_post_compaction_usage =
                branch[compaction_index + 1..].iter().rev().any(|entry| {
                    entry.get("type").and_then(Value::as_str) == Some("message")
                        && entry
                            .get("message")
                            .and_then(|message| message.get("role"))
                            .and_then(Value::as_str)
                            == Some("assistant")
                        && entry
                            .get("message")
                            .and_then(|message| {
                                serde_json::from_value::<maho_ai::types::AssistantMessage>(
                                    message.clone(),
                                )
                                .ok()
                            })
                            .is_some_and(|assistant| {
                                !matches!(
                                    assistant.stop_reason,
                                    maho_ai::types::StopReason::Aborted
                                        | maho_ai::types::StopReason::Error
                                ) && crate::compaction::calculate_context_tokens(&assistant.usage)
                                    > 0
                            })
                });
            if !has_post_compaction_usage {
                let tokens: u64 = messages
                    .iter()
                    .map(crate::compaction::estimate_tokens)
                    .sum();
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
            || {
                format!(
                    "session-{}.jsonl",
                    chrono::Utc::now().to_rfc3339().replace([':', '.'], "-")
                )
            },
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
        let branch = self.with_session_manager(|manager| manager.current_branch());
        let mut lines = vec![header.to_string()];
        let mut previous: Option<String> = None;
        for entry in branch {
            let mut linear = entry.clone();
            if let Some(object) = linear.as_object_mut() {
                object.insert(
                    "parentId".to_owned(),
                    previous.clone().map_or(Value::Null, Value::String),
                );
            }
            lines.push(linear.to_string());
            previous = entry.get("id").and_then(Value::as_str).map(str::to_owned);
        }
        std::fs::write(&file_path, format!("{}\n", lines.join("\n")))?;
        Ok(file_path)
    }

    /// Text content of the last assistant message, for `/copy`.
    pub fn get_last_assistant_text(&self) -> Option<String> {
        let state = self.agent.state();
        if state.messages().is_empty() && self.runtime_messages_tokenized.load(Ordering::SeqCst) {
            drop(state);
            let messages = self.messages();
            let last = messages.iter().rev().find_map(|message| {
                let assistant = message.as_assistant()?;
                if assistant.stop_reason == maho_ai::types::StopReason::Aborted
                    && assistant.content.is_empty()
                {
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
            return (!trimmed.is_empty()).then(|| trimmed.to_owned());
        }
        let last = state.messages().iter().rev().find_map(|message| {
            let assistant = message.as_assistant()?;
            if assistant.stop_reason == maho_ai::types::StopReason::Aborted
                && assistant.content.is_empty()
            {
                return None;
            }
            Some(assistant)
        })?;
        let mut store = None;
        let mut text = String::new();
        for content in &last.content {
            if let maho_ai::types::ContentBlock::Text(block) = content {
                if block
                    .text
                    .starts_with(crate::session_resident_store::RESIDENT_STRING_PREFIX)
                {
                    let store = store.get_or_insert_with(|| {
                        self.with_session_manager(|manager| manager.resident_store().clone())
                    });
                    text.push_str(&store.materialize_string(&block.text, None));
                } else {
                    text.push_str(&block.text);
                }
            }
        }
        let trimmed = text.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_owned())
        }
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
        self.work_barrier
            .wait_for_settled(|| self.agent.wait_for_idle())
            .await;
    }

    /// Emit a `session_shutdown` event through the bound extension runner.
    pub async fn emit_session_shutdown(&self, reason: maho_ext_api::SessionReason) {
        let mut guard = self.extension_runner.lock().await.clone();
        if let Some(runner) = guard.as_mut() {
            let event = maho_ext_api::SessionShutdownEvent {
                reason,
                target_session_file: None,
                signal: Some(maho_ext_api::AbortSignal::default()),
            };
            let _ = runner.emit_session_shutdown_event(event, false).await;
        }
    }

    /// senpi `session.extensionRunner.emit(event)` for a `session_control_wake` event (the control
    /// endpoints' wake pass). A handler's failure is reported through the runner's error listeners, as
    /// senpi's `emit` does; only an inactive runner fails the pass. No bound runner is a no-op.
    pub async fn emit_session_control_wake(&self, event: &Value) -> Result<(), String> {
        let strings = |value: &Value| -> Vec<String> {
            value
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default()
        };
        let reason = event["reason"].as_str().unwrap_or("inbox").to_owned();
        let reasons = strings(&event["reasons"]);
        let delivery_ids = event
            .get("delivery_ids")
            .filter(|ids| ids.is_array())
            .map(strings);
        let mut guard = self.extension_runner.lock().await.clone();
        let Some(runner) = guard.as_mut() else {
            return Ok(());
        };
        runner
            .emit_session_control_wake(reason, reasons, delivery_ids)
            .await
            .map_err(|failure| failure.message)
    }

    /// senpi `emitSessionShutdownEvent(runner, {reason: "quit"}, {processExit})`: with `process_exit` the
    /// handlers share the total quit budget counted from this request (senpi#3711).
    pub async fn emit_session_shutdown_for_quit(&self, process_exit: bool) {
        let mut guard = self.extension_runner.lock().await.clone();
        if let Some(runner) = guard.as_mut() {
            let event = maho_ext_api::SessionShutdownEvent {
                reason: maho_ext_api::SessionReason::Quit,
                target_session_file: None,
                signal: Some(maho_ext_api::AbortSignal::default()),
            };
            if process_exit {
                runner.arm_process_exit_budget();
            }
            let _ = runner
                .emit(maho_ext_api::ExtensionEvent::SessionShutdown(event))
                .await;
        }
    }

    fn emit_queue_update(&self) {
        let (steering, follow_up, mut ordered) = {
            let state = self.state();
            (
                state.steering_messages.clone(),
                state.follow_up_messages.clone(),
                state.queued_input_order.clone(),
            )
        };
        ordered.sort_by_key(|input| input.enqueue_order);
        let ordered = ordered
            .into_iter()
            .map(|input| maho_ext_api::QueuedInput {
                text: input.text,
                mode: input.mode,
                enqueue_order: input.enqueue_order,
                queued_id: input.queued_id,
                revision: input.revision,
                client_message_id: input.client_message_id,
            })
            .collect();
        self.emit(AgentSessionEvent::QueueUpdate {
            steering,
            follow_up,
            ordered,
        });
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
        settings.insert(
            key.to_owned(),
            Value::String(queue_mode_str(mode).to_owned()),
        );
        self.with_settings_manager_mut(|manager| {
            let _ = manager.set(crate::settings_manager::SettingsScope::Global, &settings);
        });
    }

    /// Apply the persisted steering/follow-up modes to the agent.
    pub fn sync_queue_modes_from_settings(&self) {
        let (steering, follow_up) = self.with_settings_manager(|manager| {
            (manager.get_steering_mode(), manager.get_follow_up_mode())
        });
        self.agent.set_steering_mode(steering);
        self.agent.set_follow_up_mode(follow_up);
    }

    fn get_compaction_settings(
        &self,
        model: Option<&Model>,
    ) -> Result<crate::compaction_settings_resolver::ResolvedCompactionSettings, String> {
        let session_model = self.model();
        let effective_model = model.unwrap_or(&session_model);
        let raw = self.with_settings_manager(|manager| manager.get_value("compaction").cloned());
        let configured = raw
            .map(serde_json::from_value::<crate::compaction_settings_access::CompactionSettings>)
            .transpose()
            .map_err(|error| error.to_string())?;
        let mut resolved = crate::compaction_settings_resolver::resolve_compaction_settings(
            configured.as_ref(),
            Some(crate::compaction_settings_access::CompactionModelSelector {
                provider: &effective_model.provider,
                id: &effective_model.id,
            }),
        )?;
        if let Some(enabled) = self.state().auto_compaction_session_override {
            resolved.enabled = enabled;
        }
        Ok(resolved)
    }

    /// senpi `_resolveCompactionModel`: the model used for compaction summarization. A `compaction.model`
    /// override ("provider/model") serves healthy sessions; unset, malformed, unknown, spent, or
    /// credential-less overrides keep a usable session model, and a set override that cannot serve logs why.
    /// A spent account instead uses the held target, the override, or another authenticated model whose
    /// window fits the live context, and otherwise fails with the account usage-limit advice.
    fn resolve_compaction_model(&self, session_model: &Model) -> Result<Model, String> {
        // senpi `!override`: an empty `compaction.model` is unset, not a malformed override.
        let override_selector = self.with_settings_manager(|manager| {
            manager
                .get_value("compaction")
                .and_then(|settings| settings.get("model"))
                .and_then(Value::as_str)
                .filter(|selector| !selector.is_empty())
                .map(str::to_owned)
        });
        let runtime = self.model_runtime();
        let split = override_selector
            .as_deref()
            .and_then(|selector| selector.split_once('/'))
            .filter(|(provider, _)| !provider.is_empty());
        let configured = split.and_then(|(provider, id)| runtime.get_model(provider, id));
        let has_provider = split.is_some();
        // The snapshot the controller's writers publish: unlike a `try_lock` on the controller, it cannot
        // read a spent provider as usable while another task holds the lock.
        let spent = self.state().spent_accounts.clone();
        let now = self.fallback_now();
        let usable = |model: &Model| !spent.is_spent(&model.provider, now);
        // Preserve the explicit override and original-model geometry on healthy sessions.
        // A held downswitch uses its target only when the original account cannot serve.
        if usable(session_model) {
            let Some(selector) = override_selector else {
                return Ok(session_model.clone());
            };
            let fallback_reason = match &configured {
                None if has_provider => Some("unknown_model"),
                None => Some("malformed"),
                Some(configured) if !usable(configured) => Some("provider_spent"),
                // Only a published availability pass can say "no credentials": before it (or after a busy
                // store read) the answer is unknown, the override is kept, and request-time auth decides.
                Some(configured)
                    if runtime.has_fresh_availability_snapshot()
                        && !runtime.has_configured_auth(&configured.provider) =>
                {
                    Some("no_credentials")
                }
                Some(_) => None,
            };
            let Some(reason) = fallback_reason else {
                self.state().compaction_model_fallback_notice = None;
                return Ok(configured.expect("usable override"));
            };
            let notice = format!("{selector}\u{0}{reason}");
            if self.state().compaction_model_fallback_notice.as_deref() != Some(notice.as_str()) {
                self.state().compaction_model_fallback_notice = Some(notice);
                self.session_logger.warn(
                    "compaction_model_fallback",
                    Some(&Map::from_iter([
                        ("model".to_owned(), json!(selector)),
                        ("reason".to_owned(), json!(reason)),
                        (
                            "to".to_owned(),
                            json!(format!("{}/{}", session_model.provider, session_model.id)),
                        ),
                    ])),
                );
            }
            return Ok(session_model.clone());
        }
        if let Some(pending) = self.pending_model_switch().map(|pending| pending.model)
            && usable(&pending)
        {
            return Ok(pending);
        }
        if let Some(configured) = configured.filter(|configured| usable(configured)) {
            return Ok(configured);
        }
        let reserve_tokens = self
            .get_compaction_settings(Some(session_model))?
            .reserve_tokens
            .max(0) as u64;
        let live = self
            .runtime_messages()
            .iter()
            .map(session_message_to_value)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        let required_context_window =
            estimate_messages_tokens(&crate::messages::filter_context_excluded_messages(live))
                + reserve_tokens;
        if let Some(available) = runtime.get_models(None).into_iter().find(|model| {
            model.context_window >= required_context_window
                && runtime.has_configured_auth(&model.provider)
                && usable(model)
        }) {
            return Ok(available);
        }
        let raw = spent.account_error(&session_model.provider, now);
        Err(
            crate::retry_fallback::usage_limit::account_usage_limit_error(
                &session_model.provider,
                raw.as_deref(),
            ),
        )
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
    /// Persistent calls refresh per-model memory; session entries and events are emitted only on change.
    pub fn set_thinking_level(&self, level: ModelThinkingLevel) {
        self.apply_thinking_level(level, true, Some(explicit_thinking_selection(level)));
    }

    /// Set the thinking level for this session without changing the global default.
    pub fn set_session_thinking_level(&self, level: ModelThinkingLevel) {
        self.apply_thinking_level(level, false, Some(explicit_thinking_selection(level)));
    }

    fn apply_thinking_level(
        &self,
        level: ModelThinkingLevel,
        update_global_default: bool,
        selection: Option<ThinkingSelection>,
    ) {
        let available = self.get_available_thinking_levels();
        let effective = if available.contains(&level) {
            level
        } else {
            clamp_thinking_level(level, &available)
        };
        let previous = self.agent.thinking_level();
        let previous_selection = self.agent.thinking_selection();
        let effective_selection = selection.map(|selection| ThinkingSelection {
            level: effective,
            ..selection
        });
        let selection_changed = previous_selection != effective_selection;
        let changing = effective != previous;
        if (changing || selection_changed)
            && let Ok(mut controller) = self.retry_fallback.try_lock()
            && let Some(controller) = controller.as_mut()
        {
            controller.note_manual_thinking_level();
        }

        self.agent.set_thinking_level(effective);
        self.agent
            .set_thinking_selection(effective_selection.clone());

        let model = self.model();
        if update_global_default {
            // `setModelThinkingLevel`: `off` must not erase the level `/reasoning on` restores.
            let key = format!("{}/{}", model.provider, model.id);
            let global = self.with_settings_manager(|manager| manager.get_global().clone());
            let mut levels = global
                .get("modelThinkingLevels")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            levels.insert(key.clone(), Value::String(effective.as_str().to_owned()));
            let mut values =
                Map::from_iter([("modelThinkingLevels".to_owned(), Value::Object(levels))]);
            if effective != ModelThinkingLevel::Off {
                let mut last_on = global
                    .get("modelLastOnThinkingLevels")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                last_on.insert(key, Value::String(effective.as_str().to_owned()));
                values.insert(
                    "modelLastOnThinkingLevels".to_owned(),
                    Value::Object(last_on),
                );
            }
            if (changing || selection_changed)
                && (self.supports_thinking() || effective != ModelThinkingLevel::Off)
            {
                values.insert(
                    "defaultThinkingLevel".to_owned(),
                    Value::String(effective.as_str().to_owned()),
                );
            }
            if let Err(error) = self.with_settings_manager_mut(|manager| {
                manager.set(crate::settings_manager::SettingsScope::Global, &values)
            }) {
                self.emit(AgentSessionEvent::ContinuationError {
                    error_message: error,
                });
            }
        }

        if changing || selection_changed {
            let selection_value = effective_selection
                .as_ref()
                .and_then(|selection| serde_json::to_value(selection).ok());
            self.with_session_manager_mut(|manager| {
                manager.append_thinking_level_change_with_context(
                    effective.as_str(),
                    selection_value,
                    Some("explicit"),
                    Some(&model),
                );
            });
            if changing
                && model.id == "gpt-6-astra"
                && matches!(model.provider.as_str(), "openai" | "chatgpt-subscription")
            {
                if self.agent.reasoning_baseline().is_none() {
                    self.agent
                        .set_reasoning_baseline(Some(previous.as_str().to_owned()));
                }
                self.with_session_manager_mut(|manager| {
                    manager.append_configuration_update(effective.as_str());
                });
                let context = self.with_session_manager(|manager| manager.current_context());
                match context
                    .messages
                    .into_iter()
                    .map(session_message_from_value)
                    .collect::<Result<Vec<_>, _>>()
                {
                    Ok(messages) => {
                        self.agent.set_messages(messages);
                        self.state().message_revision += 1;
                    }
                    Err(error) => self.emit(AgentSessionEvent::ContinuationError {
                        error_message: error.to_string(),
                    }),
                }
            }
            if let Some(level) = thinking_level_from_model_level(effective) {
                self.emit(AgentSessionEvent::ThinkingLevelChanged {
                    level,
                    thinking_selection: effective_selection,
                });
            }
            // `void this._extensionRunner.emit(...)`: handlers run without blocking the setter.
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                let session = self.clone();
                runtime.spawn(async move {
                    session
                        .dispatch_extension_event(
                            maho_ext_api::ExtensionEvent::ThinkingLevelSelect {
                                level: effective,
                                previous_level: previous,
                            },
                        )
                        .await;
                });
            }
            self.emit_high_reasoning_warning_if_needed();
        }
    }

    fn emit_high_reasoning_warning_if_needed(&self) {
        let model = self.model();
        let level = self.agent.thinking_level();
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
            thinking_level: thinking_level_from_model_level(level)
                .unwrap_or(ThinkingLevel::Minimal),
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
        let current = self.agent.thinking_level();
        let index = levels
            .iter()
            .position(|level| *level == current)
            .unwrap_or(levels.len() - 1);
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

    /// The thinking level and selection a model switch would land on, from an explicit level or
    /// selection, the model's remembered level, the configured default, or `DEFAULT_THINKING_LEVEL`.
    pub fn get_thinking_for_model_switch(
        &self,
        model: &Model,
        explicit_level: Option<ModelThinkingLevel>,
        explicit_selection: Option<ThinkingSelection>,
    ) -> (ModelThinkingLevel, Option<ThinkingSelection>) {
        let mut requested = explicit_level;
        let mut selection = explicit_selection;
        if let Some(level) = requested
            && selection.is_none()
        {
            selection = Some(explicit_thinking_selection(level));
        }
        if requested.is_none()
            && let Some(level_str) =
                self.get_session_model_thinking_level(&model.provider, &model.id)
            && let Some(level) = ModelThinkingLevel::ALL
                .into_iter()
                .find(|candidate| candidate.as_str() == level_str)
        {
            requested = Some(level);
            selection = Some(explicit_thinking_selection(level));
        }
        if requested.is_none() {
            let remembered = self.with_settings_manager(|manager| {
                manager
                    .get_value("modelThinkingLevels")
                    .and_then(|levels| levels.get(format!("{}/{}", model.provider, model.id)))
                    .and_then(Value::as_str)
                    .and_then(|level| {
                        ModelThinkingLevel::ALL
                            .into_iter()
                            .find(|candidate| candidate.as_str() == level)
                    })
            });
            if let Some(level) = remembered {
                requested = Some(level);
                selection = Some(explicit_thinking_selection(level));
            }
        }
        if requested.is_none() {
            let default = self
                .with_settings_manager(|manager| manager.get_string("defaultThinkingLevel"))
                .and_then(|level| {
                    ModelThinkingLevel::ALL
                        .into_iter()
                        .find(|candidate| candidate.as_str() == level)
                });
            if let Some(level) = default {
                requested = Some(level);
                selection = Some(explicit_thinking_selection(level));
            }
        }
        let requested = requested.unwrap_or(ModelThinkingLevel::Medium);
        let available = crate::thinking_levels::get_supported_thinking_levels(model);
        let level = if available.contains(&requested) {
            requested
        } else {
            clamp_thinking_level(requested, &available)
        };
        (
            level,
            selection.map(|selection| ThinkingSelection { level, ..selection }),
        )
    }

    /// senpi `getThinkingLevelForModel(model)`: the level a switch to `model` would run at, where it comes from, and
    /// the levels the model supports. The candidates are senpi `_getThinkingForModelSwitch`'s: this session's memory,
    /// the global per-model level, the model's own default, then the configured default.
    ///
    /// ADAPTATION: senpi answers the current model with its tracked `_thinkingLevelSource`; this session does not
    /// track the source of the level in force, so the current model reports its live level with the source the same
    /// candidate chain names.
    pub fn thinking_level_for_model(
        &self,
        model: &Model,
    ) -> (
        ModelThinkingLevel,
        crate::session_thinking_memory::ThinkingLevelSource,
        Vec<ModelThinkingLevel>,
    ) {
        use crate::session_thinking_memory::ThinkingLevelSource as Source;
        let available = crate::thinking_levels::get_supported_thinking_levels(model);
        let parse = |level: &str| {
            ModelThinkingLevel::ALL
                .into_iter()
                .find(|candidate| candidate.as_str() == level)
        };
        let candidates = [
            (
                self.get_session_model_thinking_level(&model.provider, &model.id)
                    .and_then(|level| parse(&level)),
                Source::Session,
            ),
            (
                self.with_settings_manager(|manager| {
                    manager
                        .get_value("modelThinkingLevels")
                        .and_then(|levels| levels.get(format!("{}/{}", model.provider, model.id)))
                        .and_then(Value::as_str)
                        .and_then(parse)
                }),
                Source::Global,
            ),
            (model.default_thinking_level, Source::ModelDefault),
            (
                self.with_settings_manager(|manager| manager.get_string("defaultThinkingLevel"))
                    .and_then(|level| parse(&level)),
                Source::Default,
            ),
        ];
        let (requested, source) = candidates
            .into_iter()
            .find_map(|(level, source)| level.map(|level| (level, source)))
            .unwrap_or((ModelThinkingLevel::Medium, Source::Default));
        let current = self.model();
        if models_are_equal(Some(&current), Some(model)) {
            return (self.thinking_level(), source, available);
        }
        let level = clamp_thinking_level(requested, &available);
        let source = if level != requested && source != Source::Default {
            Source::Clamped
        } else {
            source
        };
        (level, source, available)
    }

    /// Why `continue_from_leaf` would refuse now (senpi's `ContinueFromLeafError` checks), so a caller can answer
    /// with the error's code.
    pub fn continue_from_leaf_refusal(
        &self,
    ) -> Option<crate::continue_from_leaf::ContinueFromLeafError> {
        use crate::continue_from_leaf::{ContinueFromLeafCode, ContinueFromLeafError};
        if self.is_streaming() {
            return Some(ContinueFromLeafError::new(ContinueFromLeafCode::Streaming));
        }
        let messages = self.messages();
        let Some(leaf) = messages.last() else {
            return Some(ContinueFromLeafError::new(
                ContinueFromLeafCode::NothingToContinue,
            ));
        };
        // Only an answer can be continued; a prompt left as the leaf (an edited one) is retried, not continued.
        (leaf.role() != "assistant")
            .then(|| ContinueFromLeafError::new(ContinueFromLeafCode::LeafNotAssistant))
    }

    /// senpi `continueFromLeaf()`: continues the conversation from its last answer with no new prompt, as a hidden
    /// `continue-from-leaf` custom message that triggers a turn.
    ///
    /// ADAPTATION: senpi answers once the turn was admitted (its `agent_start`, or a delegation). The native custom
    /// message starts its turn on a spawned task and reports a start-time failure from the send itself, so this
    /// answers once the send accepted the turn; a later failure is a normal turn error event, as upstream.
    pub async fn continue_from_leaf(&self) -> Result<(), String> {
        if let Some(refusal) = self.continue_from_leaf_refusal() {
            return Err(refusal.to_string());
        }
        self.send_custom_message_content(
            crate::continue_from_leaf::CONTINUE_FROM_LEAF_CUSTOM_TYPE.to_owned(),
            maho_agent::harness::messages::CustomMessageContent::Text(
                crate::continue_from_leaf::CONTINUE_FROM_LEAF_DIRECTIVE.to_owned(),
            ),
            false,
            None,
            maho_ext_api::SendMessageOptions {
                trigger_turn: true,
                deliver_as: None,
            },
        )
        .await
    }

    // Relocation API (senpi session-relocation.ts, session-control-actions.ts)
    pub fn set_relocation_host(
        &self,
        host: Option<Arc<dyn crate::session_relocation::SessionRelocationHost>>,
    ) {
        *lock(&self.relocation_host) = host;
    }

    pub fn relocation_offered(&self) -> bool {
        lock(&self.relocation_host)
            .as_ref()
            .map(|h| h.offered())
            .unwrap_or(false)
    }

    pub async fn relocation_targets(&self) -> crate::session_relocation::RelocationTargetsResult {
        let host = lock(&self.relocation_host).clone();
        match host {
            Some(host) => host.targets().await,
            None => crate::session_relocation::relocation_not_offered_targets(),
        }
    }

    pub async fn request_relocation(
        &self,
        request: crate::session_relocation::RelocationRequest,
    ) -> crate::session_relocation::RelocationRequestResult {
        let host = lock(&self.relocation_host).clone();
        match host {
            Some(host) => host.request(request).await,
            None => crate::session_relocation::relocation_not_offered_request(),
        }
    }

    // Session tool barrier (senpi tool-barrier.ts)
    pub fn arm_tool_barrier(
        &self,
        spec: crate::session_work_barrier::ToolBarrierSpec,
    ) -> Result<(), String> {
        let value = serde_json::to_value(&spec).map_err(|e| e.to_string())?;
        if let Some(error) = crate::session_work_barrier::tool_barrier_spec_error(&value) {
            return Err(error);
        }
        self.tool_barrier.arm(spec);
        Ok(())
    }

    pub fn release_tool_barrier(&self, id: &str) -> bool {
        self.tool_barrier.release(id)
    }

    pub fn clear_tool_barrier(&self) {
        self.tool_barrier.clear();
    }

    pub fn get_tool_workspace_effect(
        &self,
        tool_name: &str,
    ) -> crate::session_work_barrier::ToolWorkspaceEffect {
        self.state()
            .tool_definitions
            .get(tool_name)
            .and_then(|entry| entry.definition.workspace_effect)
            .unwrap_or(crate::session_work_barrier::ToolWorkspaceEffect::Write)
    }

    pub async fn wait_for_tool_barrier(
        &self,
        tool_name: &str,
        signal: Option<&maho_ai::utils::abort::AbortSignal>,
    ) -> Option<String> {
        if self.get_tool_workspace_effect(tool_name)
            == crate::session_work_barrier::ToolWorkspaceEffect::Read
        {
            return None;
        }
        self.tool_barrier.wait(signal).await
    }

    // Per-session model thinking memory (senpi session-thinking-memory.ts)
    pub fn get_session_model_thinking_levels(&self) -> HashMap<String, String> {
        // Thinking levels derive from thinking_level_change entries alone, so only those are materialized.
        let (branch, leaf_id) = self.with_session_manager(|manager| {
            (
                manager.current_branch_of_types(&["thinking_level_change"]),
                manager.leaf_id().map(str::to_owned),
            )
        });
        let mut memory = lock(&self.session_thinking_memory);
        memory.levels(&branch, leaf_id.as_deref())
    }

    pub fn get_session_model_thinking_level(
        &self,
        provider: &str,
        model_id: &str,
    ) -> Option<String> {
        // Thinking levels derive from thinking_level_change entries alone, so only those are materialized.
        let (branch, leaf_id) = self.with_session_manager(|manager| {
            (
                manager.current_branch_of_types(&["thinking_level_change"]),
                manager.leaf_id().map(str::to_owned),
            )
        });
        let mut memory = lock(&self.session_thinking_memory);
        memory.level_for(&branch, leaf_id.as_deref(), provider, model_id)
    }

    // Stop resume directive (senpi stop-resume-directive.ts)
    pub fn stop_resume_directive(&self) -> crate::stop_resume_directive::StopResumeDirective {
        crate::stop_resume_directive::stop_resume_directive(self.resume_cause())
    }
}

/// `{ level, source: "explicit" }`.
/// The `assistantMessageEvent` JSON handed to extension `message_update` handlers. Stream deltas
/// omit the accumulated `partial` (re-serializing it per delta is quadratic over a message);
/// `toolcall_delta` keeps only the target block's `type`/`name` at `partial.content[contentIndex]`
/// (nulls before it), which is all ttsr reads to resolve the tool name. Other events serialize in full.
fn extension_assistant_message_event(event: &maho_ai::types::AssistantMessageEvent) -> Value {
    use maho_ai::types::{AssistantMessageEvent as Event, ContentBlock};
    match event {
        Event::TextDelta {
            content_index,
            delta,
            ..
        } => {
            serde_json::json!({ "type": "text_delta", "contentIndex": content_index, "delta": delta })
        }
        Event::ThinkingDelta {
            content_index,
            delta,
            ..
        } => {
            serde_json::json!({ "type": "thinking_delta", "contentIndex": content_index, "delta": delta })
        }
        Event::ToolcallDelta {
            content_index,
            delta,
            partial,
        } => {
            let content = match partial.content.get(*content_index) {
                Some(ContentBlock::ToolCall(call)) => {
                    let mut content = vec![Value::Null; *content_index];
                    content.push(serde_json::json!({ "type": "toolCall", "name": call.name }));
                    content
                }
                _ => Vec::new(),
            };
            serde_json::json!({ "type": "toolcall_delta", "contentIndex": content_index, "delta": delta, "partial": { "content": content } })
        }
        _ => serde_json::to_value(event).unwrap_or(Value::Null),
    }
}

fn explicit_thinking_selection(level: ModelThinkingLevel) -> ThinkingSelection {
    ThinkingSelection {
        level,
        source: maho_ai::types::ThinkingSelectionSource::Explicit,
        legacy_variant_id: None,
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

fn clamp_thinking_level(
    level: ModelThinkingLevel,
    available: &[ModelThinkingLevel],
) -> ModelThinkingLevel {
    let Some(requested) = THINKING_LEVELS_WITH_MAX
        .iter()
        .position(|candidate| *candidate == level)
    else {
        return available
            .first()
            .copied()
            .unwrap_or(ModelThinkingLevel::Off);
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
    available
        .first()
        .copied()
        .unwrap_or(ModelThinkingLevel::Off)
}

fn make_user_message(text: &str, images: Option<Vec<ImageContent>>) -> AgentMessage {
    let mut content = vec![maho_ai::types::ContentBlock::Text(
        maho_ai::types::TextContent {
            text: text.to_owned(),
            ..Default::default()
        },
    )];
    content.extend(
        images
            .unwrap_or_default()
            .into_iter()
            .map(maho_ai::types::ContentBlock::Image),
    );
    maho_ai::types::Message::User(maho_ai::types::UserMessage {
        content: maho_ai::types::UserContent::Blocks(content),
        timestamp: maho_ai::utils::diagnostics::now_ms(),
    })
    .into()
}

fn user_message_text(message: &AgentMessage) -> String {
    match message.try_as_llm() {
        Some(maho_ai::types::Message::User(user)) => match &user.content {
            maho_ai::types::UserContent::Text(text) => text.clone(),
            maho_ai::types::UserContent::Blocks(content) => {
                maho_ai::utils::text::content_text(content, "")
            }
        },
        _ => String::new(),
    }
}

/// senpi `estimateMessagesTokens` (agent-session.ts module helper).
fn estimate_messages_tokens(messages: &[Value]) -> u64 {
    messages
        .iter()
        .map(crate::compaction::estimate_tokens)
        .sum()
}

pub fn session_message_to_value(message: &AgentMessage) -> Result<Value, serde_json::Error> {
    use maho_agent::types::CustomAgentMessage;
    match message {
        AgentMessage::Llm(message) => serde_json::to_value(message),
        AgentMessage::Custom(CustomAgentMessage::Custom(message)) => serde_json::to_value(message),
        AgentMessage::Custom(CustomAgentMessage::BashExecution(message)) => {
            serde_json::to_value(message)
        }
        AgentMessage::Custom(CustomAgentMessage::BranchSummary(message)) => {
            serde_json::to_value(message)
        }
        AgentMessage::Custom(CustomAgentMessage::CompactionSummary(message)) => {
            serde_json::to_value(message)
        }
    }
}

/// senpi gates `configuration_update` entries on gpt-6-astra over the OpenAI Responses providers.
fn is_reasoning_configuration_model(model: &Model) -> bool {
    model.id == "gpt-6-astra"
        && matches!(model.provider.as_str(), "openai" | "chatgpt-subscription")
}

pub(crate) fn session_message_from_value(
    mut message: Value,
) -> Result<AgentMessage, serde_json::Error> {
    if let Some(timestamp) = message.get("timestamp").and_then(Value::as_str) {
        let millis = chrono::DateTime::parse_from_rfc3339(timestamp)
            .map(|time| time.timestamp_millis())
            .unwrap_or(0);
        message["timestamp"] = Value::from(millis);
    }
    use maho_agent::types::CustomAgentMessage;
    let custom = match message.get("role").and_then(Value::as_str) {
        Some("compactionSummary") => Some(CustomAgentMessage::CompactionSummary(
            serde_json::from_value(message.clone())?,
        )),
        Some("branchSummary") => Some(CustomAgentMessage::BranchSummary(serde_json::from_value(
            message.clone(),
        )?)),
        Some("bashExecution") => Some(CustomAgentMessage::BashExecution(serde_json::from_value(
            message.clone(),
        )?)),
        Some("custom") => Some(CustomAgentMessage::Custom(serde_json::from_value(
            message.clone(),
        )?)),
        _ => None,
    };
    match custom {
        Some(custom) => Ok(AgentMessage::Custom(custom)),
        None => serde_json::from_value(message),
    }
}

fn queue_mode_str(mode: maho_agent::types::QueueMode) -> &'static str {
    match mode {
        maho_agent::types::QueueMode::All => "all",
        maho_agent::types::QueueMode::OneAtATime => "one-at-a-time",
    }
}

fn session_entry_from_value(entry: Value) -> maho_ext_api::SessionEntry {
    let entry = crate::retry_fallback::usage_limit::describe_usage_limit_entry(entry);
    maho_ext_api::SessionEntry {
        id: entry
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        parent_id: entry
            .get("parentId")
            .and_then(Value::as_str)
            .map(str::to_owned),
        timestamp: entry
            .get("timestamp")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        kind: entry
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
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
        ServiceTierPreference::Ultrafast => ServiceTier::Ultrafast,
    }
}

fn resolve_effective_service_tier(
    tier: Option<ServiceTier>,
    fast_mode: bool,
) -> Option<ServiceTier> {
    if fast_mode {
        Some(ServiceTier::Priority)
    } else {
        tier
    }
}

fn rand_unit() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.subsec_nanos());
    f64::from(nanos % 1_000_000) / 1_000_000.0
}

fn extension_thinking_level(level: ThinkingLevel) -> ModelThinkingLevel {
    match level {
        ThinkingLevel::Minimal => ModelThinkingLevel::Minimal,
        ThinkingLevel::Low => ModelThinkingLevel::Low,
        ThinkingLevel::Medium => ModelThinkingLevel::Medium,
        ThinkingLevel::High => ModelThinkingLevel::High,
        ThinkingLevel::Xhigh => ModelThinkingLevel::Xhigh,
        ThinkingLevel::Max => ModelThinkingLevel::Max,
    }
}

struct SessionCustodyStore(std::sync::Weak<AgentSessionInner>);
impl crate::work_obligations::CustodyStore for SessionCustodyStore {
    fn entries(&self) -> Vec<Value> {
        self.0.upgrade().map_or_else(Vec::new, |inner| {
            lock(&inner.session_manager).entries_of_types(&["custom"])
        })
    }
    fn append(
        &mut self,
        record: &crate::work_custody_record::WorkCustodyRecord,
    ) -> Result<(), String> {
        let inner = self.0.upgrade().ok_or("Session disposed")?;
        let mut manager = lock(&inner.session_manager);
        manager.append_custom(
            "work-custody",
            Some(serde_json::to_value(record).map_err(|e| e.to_string())?),
        );
        manager.take_write_error().map_or(Ok(()), Err)
    }
    fn changed(&mut self) {
        if let Some(inner) = self.0.upgrade() {
            inner
                .event_bus
                .emit("work-custody-changed", &serde_json::json!({}));
        }
    }
}
impl AgentSession {
    pub fn with_work_obligations<R>(
        &self,
        f: impl FnOnce(&mut crate::work_obligations::SessionWorkObligations) -> R,
    ) -> R {
        let result = {
            f(lock(&self.work_obligations)
                .as_mut()
                .expect("session custody initialized"))
        };
        // Custody appends go through the manager directly (SessionCustodyStore).
        self.emit_pending_session_recoveries();
        self.refresh_lifecycle();
        result
    }
}

/// The session half of `pi.session` (senpi `createSessionControlActions` deps): admission, ledger, header write and
/// relocation come from the bound session.
struct SessionControlBase(std::sync::Weak<AgentSessionInner>);

impl SessionControlBase {
    fn session(&self) -> Result<AgentSession, String> {
        self.0
            .upgrade()
            .map(|inner| AgentSession { inner })
            .ok_or_else(|| "session disposed".to_owned())
    }
}

impl crate::extensions::session_control_types::SessionControlActions for SessionControlBase {
    fn admission_gate(&self) -> Value {
        self.session()
            .map(|session| session.external_admission.gate(&json!({})))
            .unwrap_or_else(|error| json!({"can_admit":false,"hold_reason":error}))
    }
    fn admit_external_message(&self, input: Value) -> Result<Value, String> {
        self.session()?.admit_external_message(&input, &json!({}))
    }
    fn list_admitted_deliveries(&self) -> Value {
        self.session()
            .map(|session| session.external_admission.list())
            .unwrap_or_else(|_| json!({"pending":[],"emitted":[]}))
    }
    fn persist_header_now(&self) -> futures::future::BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            let session = self.session()?;
            session.with_session_manager_mut(|manager| manager.persist_header_now())
        })
    }
    fn relocation_offered(&self) -> bool {
        self.session()
            .is_ok_and(|session| session.relocation_offered())
    }
    fn relocation_targets(
        &self,
        _signal: Option<maho_ext_api::AbortSignal>,
    ) -> futures::future::BoxFuture<'_, Result<Value, String>> {
        Box::pin(async move {
            let targets = self.session()?.relocation_targets().await;
            serde_json::to_value(targets).map_err(|error| error.to_string())
        })
    }
    fn request_relocation(
        &self,
        request: Value,
        _signal: Option<maho_ext_api::AbortSignal>,
    ) -> futures::future::BoxFuture<'_, Result<Value, String>> {
        Box::pin(async move {
            let request = serde_json::from_value(request)
                .map_err(|error| format!("invalid relocation request: {error}"))?;
            let result = self.session()?.request_relocation(request).await;
            serde_json::to_value(result).map_err(|error| error.to_string())
        })
    }
}

/// Forwards the binding's lifecycle edges to the session that owns the ledger.
struct SessionLifecycleHost(std::sync::Weak<AgentSessionInner>);

impl crate::lifecycle_session_binding::LifecycleBindingHost for SessionLifecycleHost {
    fn event(&self, _: &str, _: Value) {
        if let Some(inner) = self.0.upgrade() {
            AgentSession { inner }.refresh_lifecycle();
        }
    }
    fn facts_failed(&self, error: String) {
        if let Some(inner) = self.0.upgrade() {
            AgentSession { inner }.emit(AgentSessionEvent::ContinuationError {
                error_message: error,
            });
        }
    }
    fn mark_interrupted_from_custody(&self, delivered: &dyn Fn(&str) -> bool) {
        if let Some(inner) = self.0.upgrade() {
            AgentSession { inner }.mark_interrupted_from_custody(delivered);
        }
    }
}

impl AgentSession {
    /// senpi `LifecycleSessionBinding` constructor (senpi#11209): a crashed receiver may hold durable custody but no
    /// transcript entry yet. Recovery owns that delivery already, so it is reserved before session_start can let the
    /// gateway enqueue a second carrier; consumed work marks the ledger interrupted first.
    fn restore_queued_external_deliveries(&self) {
        let deliveries = self.with_session_manager(|manager| {
            crate::lifecycle_delivery::lifecycle_delivery_entries(&manager.entries())
        });
        let active: Vec<crate::work_custody_record::WorkCustodyRecord> =
            lock(&self.work_obligations)
                .as_ref()
                .map(|registry| registry.active().into_iter().cloned().collect())
                .unwrap_or_default();
        self.mark_interrupted_from_custody(&|ticket| deliveries.contains_key(ticket));
        for record in active {
            let Some(ticket) = record.ticket_id.clone() else {
                continue;
            };
            if record.state != crate::work_custody_record::CustodyState::Queued {
                continue;
            }
            let Some(payload) = &record.payload else {
                continue;
            };
            let mut message = payload.message.clone();
            message["role"] = json!("custom");
            message["timestamp"] = json!(0);
            let Some(delivery_id) =
                crate::external_admission::delivery_id_of(&message).map(str::to_owned)
            else {
                continue;
            };
            if deliveries.contains_key(&ticket) {
                self.external_admission.observe_persisted(&message);
                continue;
            }
            let weak = Arc::downgrade(&self.inner);
            self.external_admission.restore_queued(
                &delivery_id,
                Arc::new(move || {
                    let Some(inner) = weak.upgrade() else {
                        return false;
                    };
                    let session = AgentSession { inner };
                    // Consumption writes the entry before message_end observes it in external admission.
                    session.with_session_manager(|manager| {
                        crate::lifecycle_delivery::lifecycle_delivery_entries(&manager.entries())
                            .contains_key(&ticket)
                    }) || lock(&session.work_obligations)
                        .as_ref()
                        .is_some_and(|registry| {
                            registry.active().iter().any(|current| {
                                current.ticket_id.as_deref() == Some(ticket.as_str())
                                    && current.state
                                        == crate::work_custody_record::CustodyState::Queued
                            })
                        })
                }),
            );
        }
    }

    /// Ticket admission precedes every write. Delivery is acknowledged only by the transcript receipt.
    pub fn send_work_ticket(
        &self,
        ticket: &str,
        payload: crate::work_custody_record::CustodyPayload,
        trigger_turn: bool,
    ) -> Result<(), String> {
        let admission = self.with_work_obligations(|registry| registry.admission(ticket));
        if admission != crate::work_obligations::TicketAdmission::Admitted {
            return Err(crate::work_obligations::DeliveryTicketRefused {
                ticket_id: ticket.into(),
                reason: crate::work_obligations::DeliveryTicketRefusal::Admission(admission),
            }
            .to_string());
        }
        // A re-send of a queued ticket must not replace the batch that queued message will write (senpi#4106).
        if let Some(items) = &payload.item_ids
            && let Some(queued) =
                self.with_work_obligations(|registry| registry.queued_item_ids(ticket))
            && !crate::itemized_send::same_items(&queued, items)
        {
            return Err(crate::work_obligations::DeliveryTicketRefused {
                ticket_id: ticket.into(),
                reason: crate::work_obligations::DeliveryTicketRefusal::DifferentItems,
            }
            .to_string());
        }
        let mut wire = payload.message.clone();
        wire["timestamp"] = serde_json::json!(maho_ai::utils::diagnostics::now_ms());
        let message: maho_agent::harness::messages::CustomMessage =
            serde_json::from_value(wire).map_err(|e| e.to_string())?;
        let options = maho_ext_api::SendMessageOptions {
            trigger_turn,
            deliver_as: payload.deliver_as.as_deref().map(|s| match s {
                "followUp" => maho_ext_api::DeliverAs::FollowUp,
                "nextTurn" => maho_ext_api::DeliverAs::NextTurn,
                _ => maho_ext_api::DeliverAs::Steer,
            }),
        };
        if !self
            .with_work_obligations(|registry| registry.queue(ticket, payload))
            .map_err(|e| e.to_string())?
        {
            return Err("Delivery ticket no longer admitted".into());
        }
        let receipt = maho_ext_api::MessageDeliveryReceipt::pending();
        let weak = Arc::downgrade(&self.inner);
        let ticket_id = ticket.to_owned();
        receipt.on_settled(move |outcome| {
            if let Some(inner) = weak.upgrade() {
                let session = AgentSession { inner };
                let run_id = lock(&session.custody_run_id).clone();
                let entry_id = session.with_session_manager(|manager| {
                    manager
                        .entries()
                        .last()
                        .and_then(|e| e["id"].as_str())
                        .unwrap_or_default()
                        .to_owned()
                });
                session.with_work_obligations(|registry| {
                    let result = if outcome.is_ok() {
                        registry.consumed(&ticket_id, &run_id, &entry_id)
                    } else {
                        registry.failed(&ticket_id)
                    };
                    registry.end_delivery(&ticket_id);
                    if let Err(error) = result {
                        session.emit(AgentSessionEvent::ContinuationError {
                            error_message: error.to_string(),
                        });
                    }
                });
            }
        });
        let result = self
            .send_custom_message_content_with_receipt(message, options, Some(receipt.clone()))
            .map_err(|e| e.message);
        if let Err(error) = &result {
            receipt.resolve_failed(error.clone());
        }
        result
    }
}

#[cfg(test)]
#[path = "agent_session_tests.rs"]
mod tests;
