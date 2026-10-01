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
use std::sync::{Arc, Mutex, MutexGuard};

use maho_agent::types::{AgentMessage, AgentTool, AgentState};
use maho_agent::Agent;
use maho_ai::model::Model;
use maho_ai::models::models_are_equal;
use maho_ai::types::{
    ImageContent, ModelThinkingLevel, ServiceTierPreference, ThinkingLevel, ThinkingSelection,
};
use maho_ext_api::{
    CompactionRejectionCause, ExtensionError, ExtensionMode, ExtensionUi, FlagValue, InputSource,
    ServiceTier, SessionReason, SessionStartEvent, SourceInfo, SourceOrigin, SourceScope,
    StreamingBehavior, ToolDefinition, ToolExposure, ToolInfo,
};
use serde_json::Value;

use crate::model_registry::ModelRegistry;
use crate::model_runtime::ModelRuntime;
use crate::session_activity::{SessionActivitySnapshot, WakeSourceTracker, is_session_busy_snapshot};
use crate::session_manager::SessionManager;
use crate::settings_manager::SettingsManager;

/// Sample eval-cell call for an eval-only tool, using the argument name that tool actually takes.
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
    session_start_event: SessionStartEvent,
    auto_title_sessions: bool,
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
}

fn name_set(names: Option<Vec<String>>) -> Option<BTreeSet<String>> {
    names.map(|names| names.into_iter().collect())
}

pub struct AgentSession {
    agent: Agent,
    session_manager: Mutex<SessionManager>,
    settings_manager: Mutex<SettingsManager>,
    model_runtime: ModelRuntime,
    state: Mutex<AgentSessionState>,
    fallback_now: Arc<dyn Fn() -> f64 + Send + Sync>,
    retry_random: Arc<dyn Fn() -> f64 + Send + Sync>,
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
        let model_runtime = match (config.model_runtime, config.model_registry) {
            (Some(runtime), _) => runtime,
            (None, Some(registry)) => registry.model_runtime,
            (None, None) => return Err(MissingModelAccessError),
        };
        let agent_dir = config.agent_dir.clone().unwrap_or_else(crate::config::get_agent_dir);
        let agent = config.agent;
        let state = AgentSessionState {
            scoped_models: config.scoped_models,
            favorite_models: config.favorite_models,
            cwd: config.cwd,
            agent_dir,
            custom_tools: config.custom_tools,
            initial_active_tool_names: config.initial_active_tool_names,
            default_tool_names: name_set(config.default_tool_names),
            eval_only_tool_names: name_set(config.eval_only_tool_names),
            allowed_tool_names: name_set(config.allowed_tool_names),
            excluded_tool_names: name_set(config.excluded_tool_names),
            base_tools_override: config.base_tools_override,
            session_start_event: config.session_start_event.unwrap_or(SessionStartEvent {
                reason: SessionReason::Startup,
                initial_model_provenance: None,
                previous_session_file: None,
            }),
            auto_title_sessions: config.auto_title_sessions.unwrap_or(false),
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
        };
        let session = Self {
            agent,
            session_manager: Mutex::new(config.session_manager),
            settings_manager: Mutex::new(config.settings_manager),
            model_runtime,
            state: Mutex::new(state),
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
        &self.model_runtime
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

    fn test_model() -> Model {
        serde_json::from_value(serde_json::json!({
            "id": "faux-1", "name": "faux-1", "api": "faux", "provider": "faux",
            "baseUrl": "", "reasoning": false, "input": [], "contextWindow": 128000, "maxTokens": 4096,
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }
        }))
        .expect("model")
    }
}
