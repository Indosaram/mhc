//! Port of senpi packages/agent/src/harness/session/types.ts.

use std::collections::{BTreeMap, BTreeSet};

use maho_ai::types::{AssistantMessage, StopReason, Usage};
use serde::{Deserialize, Serialize};

use maho_ai::types::ModelThinkingLevel as ThinkingLevel;

use super::session::SessionError;
use super::values::{ListElement, ListReadOptions, ListWrite, StoredValue, Value, ValueList, ValueWrite};
use crate::harness::compaction::compaction::CompactionSettings;
use crate::harness::context::Context;
use crate::harness::types::AgentHarnessStreamOptions;
use crate::types::{AgentMessage, QueueMode};

pub type JsonValue = serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryType {
    Message,
    Compaction,
    BranchSummary,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum EntryKind {
    Message {
        message: AgentMessage,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        terminate: Option<bool>,
    },
    Compaction {
        summary: String,
        retained_tail: Vec<AgentMessage>,
        tokens_before: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<JsonValue>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
        from_hook: bool,
    },
    BranchSummary {
        from_id: Option<String>,
        summary: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<JsonValue>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
        from_hook: bool,
    },
    Custom {
        custom_type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<JsonValue>,
    },
}

impl EntryKind {
    pub fn entry_type(&self) -> EntryType {
        match self {
            EntryKind::Message { .. } => EntryType::Message,
            EntryKind::Compaction { .. } => EntryType::Compaction,
            EntryKind::BranchSummary { .. } => EntryType::BranchSummary,
            EntryKind::Custom { .. } => EntryType::Custom,
        }
    }

    pub fn custom_type(&self) -> Option<&str> {
        match self {
            EntryKind::Custom { custom_type, .. } => Some(custom_type),
            _ => None,
        }
    }

    pub fn message(&self) -> Option<&AgentMessage> {
        match self {
            EntryKind::Message { message, .. } => Some(message),
            _ => None,
        }
    }

    pub fn summary(&self) -> Option<&str> {
        match self {
            EntryKind::Compaction { summary, .. } | EntryKind::BranchSummary { summary, .. } => Some(summary),
            _ => None,
        }
    }

    pub fn details(&self) -> Option<&JsonValue> {
        match self {
            EntryKind::Compaction { details, .. } | EntryKind::BranchSummary { details, .. } => details.as_ref(),
            _ => None,
        }
    }

    pub fn usage(&self) -> Option<&Usage> {
        match self {
            EntryKind::Compaction { usage, .. } | EntryKind::BranchSummary { usage, .. } => usage.as_ref(),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub id: String,
    pub parent_id: Option<String>,
    pub seq: i64,
    pub timestamp: i64,
    #[serde(flatten)]
    pub kind: EntryKind,
}

impl Entry {
    pub fn entry_type(&self) -> EntryType {
        self.kind.entry_type()
    }

    pub fn custom_type(&self) -> Option<&str> {
        self.kind.custom_type()
    }
}

/// Entry supplied to a transaction before storage assigns sequence and timestamp.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewEntry {
    pub id: String,
    pub parent_id: Option<String>,
    #[serde(flatten)]
    pub kind: EntryKind,
}

impl NewEntry {
    pub fn message(id: impl Into<String>, parent_id: Option<String>, message: AgentMessage) -> Self {
        Self {
            id: id.into(),
            parent_id,
            kind: EntryKind::Message {
                message,
                terminate: None,
            },
        }
    }

    pub fn custom(id: impl Into<String>, parent_id: Option<String>, custom_type: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            parent_id,
            kind: EntryKind::Custom {
                custom_type: custom_type.into(),
                data: None,
            },
        }
    }

    pub fn custom_with_data(
        id: impl Into<String>,
        parent_id: Option<String>,
        custom_type: impl Into<String>,
        data: JsonValue,
    ) -> Self {
        Self {
            id: id.into(),
            parent_id,
            kind: EntryKind::Custom {
                custom_type: custom_type.into(),
                data: Some(data),
            },
        }
    }

    pub fn compaction(
        id: impl Into<String>,
        parent_id: Option<String>,
        summary: impl Into<String>,
        retained_tail: Vec<AgentMessage>,
        tokens_before: i64,
        from_hook: bool,
    ) -> Self {
        Self {
            id: id.into(),
            parent_id,
            kind: EntryKind::Compaction {
                summary: summary.into(),
                retained_tail,
                tokens_before,
                details: None,
                usage: None,
                from_hook,
            },
        }
    }

    pub fn branch_summary(
        id: impl Into<String>,
        parent_id: Option<String>,
        from_id: Option<String>,
        summary: impl Into<String>,
        from_hook: bool,
    ) -> Self {
        Self {
            id: id.into(),
            parent_id,
            kind: EntryKind::BranchSummary {
                from_id,
                summary: summary.into(),
                details: None,
                usage: None,
                from_hook,
            },
        }
    }

    pub fn with_details(mut self, details: JsonValue) -> Self {
        match &mut self.kind {
            EntryKind::Compaction { details: slot, .. }
            | EntryKind::BranchSummary { details: slot, .. }
            | EntryKind::Custom { data: slot, .. } => *slot = Some(details),
            EntryKind::Message { .. } => {}
        }
        self
    }
}

/// Convert an application-defined custom entry into model context.
pub type EntryProjector = std::sync::Arc<
    dyn for<'a> Fn(&'a Entry, &'a Context) -> maho_ai::types::BoxFuture<'a, Option<Vec<AgentMessage>>>
        + Send
        + Sync,
>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaneModelRef {
    pub provider: String,
    pub model_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaneConfiguration {
    pub model: LaneModelRef,
    pub thinking_level: ThinkingLevel,
    pub active_tool_names: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationMeta {
    pub operation_id: String,
    pub lane: String,
    pub source_tip_id: Option<String>,
    pub started_at: i64,
    pub intent: OperationIntent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", rename_all_fields = "camelCase")]
pub enum OperationIntent {
    Run {
        prompt_entry_ids: Vec<String>,
    },
    Compaction {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        custom_instructions: Option<String>,
    },
    Navigation {
        target_id: Option<String>,
        summarize: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        custom_instructions: Option<String>,
    },
}

impl OperationIntent {
    pub fn kind(&self) -> OperationIntentKind {
        match self {
            OperationIntent::Run { .. } => OperationIntentKind::Run,
            OperationIntent::Compaction { .. } => OperationIntentKind::Compaction,
            OperationIntent::Navigation { .. } => OperationIntentKind::Navigation,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OperationIntentKind {
    Run,
    Compaction,
    Navigation,
}

/// `OperationIntent["kind"]`, the name the harness hooks and errors use for it.
pub type OperationKind = OperationIntentKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum Control {
    Running,
    CancelRequested { requested_at: i64 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationError {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonValue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TerminalStatus {
    Completed,
    Declined,
    Aborted,
    Failed,
}

/// Immutable lane-lived observation record written by one terminal transaction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationResultRecord {
    pub operation_id: String,
    pub kind: OperationIntentKind,
    pub status: TerminalStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<OperationError>,
    pub from_tip_id: Option<String>,
    pub tip_id: Option<String>,
    pub started_at: i64,
    pub ended_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum Continuation {
    NeedAssistant { overflow_recovery_used: bool },
    MayFinish { include_final_assistant: bool },
}

/// Checkpoint payload; the flat leaf literal replaces the old nested phase tag.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointData {
    pub continuation: Continuation,
    pub trigger_entry_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InboxItemKind {
    Steer,
    FollowUp,
    NextRun,
    Write,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxItem {
    pub entry_id: String,
    pub kind: InboxItemKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NormalizedRetryPolicy {
    pub max_attempts: u32,
    pub base_delay_ms: u64,
    pub max_agent_delay_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationContext {
    pub step_id: String,
    pub trigger_entry_id: String,
    pub configuration: LaneConfiguration,
    pub stream_options: AgentHarnessStreamOptions,
    pub retry_policy: NormalizedRetryPolicy,
    pub overflow_recovery_used: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolCallReplay {
    Never,
    Safe,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum ToolCall {
    Planned {
        source_index: usize,
        result_entry_id: String,
    },
    EffectPending {
        source_index: usize,
        result_entry_id: String,
        replay: ToolCallReplay,
    },
    OutcomeReady {
        source_index: usize,
        result_entry_id: String,
        terminate: bool,
    },
    Completed {
        source_index: usize,
        result_entry_id: String,
        terminate: bool,
    },
}

impl ToolCall {
    pub fn source_index(&self) -> usize {
        match self {
            ToolCall::Planned { source_index, .. }
            | ToolCall::EffectPending { source_index, .. }
            | ToolCall::OutcomeReady { source_index, .. }
            | ToolCall::Completed { source_index, .. } => *source_index,
        }
    }

    pub fn result_entry_id(&self) -> &str {
        match self {
            ToolCall::Planned { result_entry_id, .. }
            | ToolCall::EffectPending { result_entry_id, .. }
            | ToolCall::OutcomeReady { result_entry_id, .. }
            | ToolCall::Completed { result_entry_id, .. } => result_entry_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolBatch {
    pub assistant_entry_id: String,
    pub configuration: LaneConfiguration,
    pub turn_id: String,
    pub calls: Vec<ToolCall>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryContext {
    pub result_entry_id: String,
    pub configuration: LaneConfiguration,
    pub stream_options: AgentHarnessStreamOptions,
    pub retry_policy: NormalizedRetryPolicy,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cancellable {
    pub control: Control,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolExecutionMode {
    Sequential,
    Parallel,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunSettings {
    pub compaction: CompactionSettings,
    pub steering_mode: QueueMode,
    pub follow_up_mode: QueueMode,
    pub tool_execution: ToolExecutionMode,
}

/// Uniform scope carried by every operation leaf.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationScope {
    pub control: Control,
    pub settings: RunSettings,
    pub latest_assistant_entry_id: Option<String>,
}

/// Shared backoff data for every retry-wait leaf.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryWait {
    pub next_attempt: u32,
    pub not_before: i64,
    pub error_message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantGenerationScope {
    pub generation_context: GenerationContext,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum ResultBoundary {
    ResumeCheckpoint {
        resume_after: CheckpointData,
    },
    Finish,
    CommitNavigation {
        target_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SummaryTaskReason {
    Manual,
    Threshold,
    Overflow,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryTask {
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<SummaryTaskReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_instructions: Option<String>,
    pub boundary: ResultBoundary,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryGenerationScope {
    pub task: SummaryTask,
    pub summary_context: SummaryContext,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryGenerationReady {
    #[serde(flatten)]
    pub scope: SummaryGenerationScope,
    pub next_attempt: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryGenerationRequest {
    pub index: usize,
    pub usage_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryGenerationEffectPending {
    #[serde(flatten)]
    pub scope: SummaryGenerationScope,
    pub attempt: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<SummaryGenerationRequest>,
    pub usage_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryGenerationRetryWait {
    #[serde(flatten)]
    pub scope: SummaryGenerationScope,
    #[serde(flatten)]
    pub retry_wait: RetryWait,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeferredScope {
    #[serde(flatten)]
    pub operation: OperationScope,
    pub step_id: String,
    pub source_entry_id: String,
    pub poll: u64,
    pub configuration: LaneConfiguration,
    pub stream_options: AgentHarnessStreamOptions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OperationMarker {
    #[serde(rename = "starting")]
    Starting,
    #[serde(rename = "checkpoint")]
    Checkpoint,
    #[serde(rename = "assistant.ready")]
    AssistantReady,
    #[serde(rename = "assistant.effect_pending")]
    AssistantEffectPending,
    #[serde(rename = "assistant.retry_wait")]
    AssistantRetryWait,
    #[serde(rename = "tools")]
    Tools,
    #[serde(rename = "deferred.suspended")]
    DeferredSuspended,
    #[serde(rename = "deferred.effect_pending")]
    DeferredEffectPending,
    #[serde(rename = "summary.deciding")]
    SummaryDeciding,
    #[serde(rename = "summary.ready")]
    SummaryReady,
    #[serde(rename = "summary.effect_pending")]
    SummaryEffectPending,
    #[serde(rename = "summary.retry_wait")]
    SummaryRetryWait,
    #[serde(rename = "navigation.ready_to_commit")]
    NavigationReadyToCommit,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartingOperation {
    #[serde(flatten)]
    pub operation: OperationScope,
    pub at: OperationMarker,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointOperation {
    #[serde(flatten)]
    pub operation: OperationScope,
    #[serde(flatten)]
    pub checkpoint: CheckpointData,
    pub at: OperationMarker,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantReadyOperation {
    #[serde(flatten)]
    pub operation: OperationScope,
    #[serde(flatten)]
    pub assistant: AssistantGenerationScope,
    pub next_attempt: u32,
    pub at: OperationMarker,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantEffectPendingOperation {
    #[serde(flatten)]
    pub operation: OperationScope,
    #[serde(flatten)]
    pub assistant: AssistantGenerationScope,
    pub attempt: u32,
    pub response_entry_id: String,
    pub usage_id: String,
    pub intended_output_limit: u64,
    pub context_window: u64,
    pub at: OperationMarker,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantRetryWaitOperation {
    #[serde(flatten)]
    pub operation: OperationScope,
    #[serde(flatten)]
    pub assistant: AssistantGenerationScope,
    #[serde(flatten)]
    pub retry_wait: RetryWait,
    pub at: OperationMarker,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolsOperation {
    #[serde(flatten)]
    pub operation: OperationScope,
    pub batch: ToolBatch,
    pub at: OperationMarker,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeferredSuspendedOperation {
    #[serde(flatten)]
    pub deferred: DeferredScope,
    pub at: OperationMarker,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeferredEffectPendingOperation {
    #[serde(flatten)]
    pub deferred: DeferredScope,
    pub response_entry_id: String,
    pub usage_id: String,
    pub at: OperationMarker,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryDecidingOperation {
    #[serde(flatten)]
    pub operation: OperationScope,
    pub task: SummaryTask,
    pub at: OperationMarker,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryReadyOperation {
    #[serde(flatten)]
    pub operation: OperationScope,
    #[serde(flatten)]
    pub ready: SummaryGenerationReady,
    pub at: OperationMarker,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryEffectPendingOperation {
    #[serde(flatten)]
    pub operation: OperationScope,
    #[serde(flatten)]
    pub pending: SummaryGenerationEffectPending,
    pub at: OperationMarker,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryRetryWaitOperation {
    #[serde(flatten)]
    pub operation: OperationScope,
    #[serde(flatten)]
    pub retry: SummaryGenerationRetryWait,
    pub at: OperationMarker,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NavigationReadyToCommitOperation {
    #[serde(flatten)]
    pub operation: OperationScope,
    pub target_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub at: OperationMarker,
}

/// Flat durable operation state: exactly 13 family-neutral dispatcher leaves.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OperationState {
    Starting(StartingOperation),
    Checkpoint(CheckpointOperation),
    AssistantReady(AssistantReadyOperation),
    AssistantEffectPending(AssistantEffectPendingOperation),
    AssistantRetryWait(AssistantRetryWaitOperation),
    Tools(ToolsOperation),
    DeferredSuspended(DeferredSuspendedOperation),
    DeferredEffectPending(DeferredEffectPendingOperation),
    SummaryDeciding(SummaryDecidingOperation),
    SummaryReady(SummaryReadyOperation),
    SummaryEffectPending(SummaryEffectPendingOperation),
    SummaryRetryWait(SummaryRetryWaitOperation),
    NavigationReadyToCommit(NavigationReadyToCommitOperation),
}

impl OperationState {
    pub fn at(&self) -> OperationMarker {
        match self {
            OperationState::Starting(_) => OperationMarker::Starting,
            OperationState::Checkpoint(_) => OperationMarker::Checkpoint,
            OperationState::AssistantReady(_) => OperationMarker::AssistantReady,
            OperationState::AssistantEffectPending(_) => OperationMarker::AssistantEffectPending,
            OperationState::AssistantRetryWait(_) => OperationMarker::AssistantRetryWait,
            OperationState::Tools(_) => OperationMarker::Tools,
            OperationState::DeferredSuspended(_) => OperationMarker::DeferredSuspended,
            OperationState::DeferredEffectPending(_) => OperationMarker::DeferredEffectPending,
            OperationState::SummaryDeciding(_) => OperationMarker::SummaryDeciding,
            OperationState::SummaryReady(_) => OperationMarker::SummaryReady,
            OperationState::SummaryEffectPending(_) => OperationMarker::SummaryEffectPending,
            OperationState::SummaryRetryWait(_) => OperationMarker::SummaryRetryWait,
            OperationState::NavigationReadyToCommit(_) => OperationMarker::NavigationReadyToCommit,
        }
    }

    /// Copy only the uniform operation scope when constructing a successor leaf.
    pub fn operation_scope_of(&self) -> OperationScope {
        match self {
            OperationState::Starting(state) => state.operation.clone(),
            OperationState::Checkpoint(state) => state.operation.clone(),
            OperationState::AssistantReady(state) => state.operation.clone(),
            OperationState::AssistantEffectPending(state) => state.operation.clone(),
            OperationState::AssistantRetryWait(state) => state.operation.clone(),
            OperationState::Tools(state) => state.operation.clone(),
            OperationState::DeferredSuspended(state) => state.deferred.operation.clone(),
            OperationState::DeferredEffectPending(state) => state.deferred.operation.clone(),
            OperationState::SummaryDeciding(state) => state.operation.clone(),
            OperationState::SummaryReady(state) => state.operation.clone(),
            OperationState::SummaryEffectPending(state) => state.operation.clone(),
            OperationState::SummaryRetryWait(state) => state.operation.clone(),
            OperationState::NavigationReadyToCommit(state) => state.operation.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Operation {
    pub meta: OperationMeta,
    pub state: OperationState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaneState {
    pub current_operation_id: Option<String>,
    pub last_operation_id: Option<String>,
    pub inbox: Vec<InboxItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum PendingEntry {
    Message {
        payload: AgentMessage,
    },
    Custom {
        custom_type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload: Option<JsonValue>,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DurableFileOperations {
    pub read: Vec<String>,
    pub written: Vec<String>,
    pub edited: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum DurableStructuralPreparation {
    Compaction {
        messages_to_summarize: Vec<AgentMessage>,
        turn_prefix_messages: Vec<AgentMessage>,
        retained_tail: Vec<AgentMessage>,
        is_split_turn: bool,
        tokens_before: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        previous_summary: Option<String>,
        file_ops: DurableFileOperations,
        settings: CompactionSettings,
    },
    BranchSummary {
        messages: Vec<AgentMessage>,
        file_ops: DurableFileOperations,
        total_tokens: i64,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRow {
    pub id: String,
    pub seq: i64,
    pub usage: Usage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<String>,
    pub adjustment: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonValue>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewUsageRow {
    pub id: String,
    pub usage: Usage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<String>,
    pub adjustment: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonValue>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryWrite {
    pub entry: NewEntry,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageWrite {
    pub row: NewUsageRow,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Write {
    Entry(EntryWrite),
    Usage(UsageWrite),
    Value(ValueWrite),
    List(ListWrite),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitResult {
    pub first_seq: i64,
    pub seqs: Vec<i64>,
    pub timestamp: i64,
    /// Session totals immediately after this commit was applied.
    pub stats: SessionStats,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryStructure {
    pub id: String,
    pub parent_id: Option<String>,
    pub seq: i64,
    pub timestamp: i64,
    #[serde(rename = "type")]
    pub entry_type: EntryType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_type: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryCursor {
    pub seq: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BranchOrder {
    NewestFirst,
    OldestFirst,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScanOrder {
    Asc,
    Desc,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchScan {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_at_type: Option<EntryType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_at_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_type: Option<EntryType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<BranchOrder>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<EntryCursor>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageBranchScan {
    pub start: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_at_type: Option<EntryType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_at_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_type: Option<EntryType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<BranchOrder>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<EntryCursor>,
}

impl StorageBranchScan {
    pub fn new(start: impl Into<String>) -> Self {
        Self {
            start: start.into(),
            stop_at_type: None,
            stop_at_id: None,
            entry_type: None,
            custom_type: None,
            order: None,
            limit: None,
            cursor: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryScan {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_type: Option<EntryType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_seq: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_seq: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<ScanOrder>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageScan {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_seq: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_seq: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<ScanOrder>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStats {
    pub message_count: usize,
    pub usage: Usage,
}

pub trait Storage: Send + Sync {
    fn commit<'a>(
        &'a self,
        writes: Vec<Write>,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<CommitResult, SessionError>>;
    fn get_entries<'a>(
        &'a self,
        ids: Vec<String>,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<BTreeMap<String, Entry>, SessionError>>;
    fn get_value<'a>(
        &'a self,
        address: &'a Value,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<Option<StoredValue>, SessionError>>;
    fn scan_values<'a>(
        &'a self,
        prefix: &'a Value,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<Vec<StoredValue>, SessionError>>;
    fn read_list<'a>(
        &'a self,
        address: &'a ValueList,
        options: Option<ListReadOptions>,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<Vec<ListElement>, SessionError>>;
    fn scan_branch<'a>(
        &'a self,
        query: StorageBranchScan,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<Vec<Entry>, SessionError>>;
    fn scan_branch_structure<'a>(
        &'a self,
        query: StorageBranchScan,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<Vec<EntryStructure>, SessionError>>;
    fn scan_entries<'a>(
        &'a self,
        query: EntryScan,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<Vec<Entry>, SessionError>>;
    fn scan_usage<'a>(
        &'a self,
        query: UsageScan,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<Vec<UsageRow>, SessionError>>;
    fn get_stats<'a>(
        &'a self,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<SessionStats, SessionError>>;
    fn close<'a>(&'a self, context: &'a Context) -> maho_ai::types::BoxFuture<'a, ()>;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMetadata {
    pub id: String,
    pub created_at: i64,
    pub storage_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_parent_session_path: Option<String>,
}

pub type IdGenerator = std::sync::Arc<dyn Fn(Option<i64>) -> String + Send + Sync>;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_type: Option<EntryType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<ScanOrder>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<EntryCursor>,
}

pub trait SessionReader: Send + Sync {
    fn get_entries<'a>(
        &'a self,
        ids: Vec<String>,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<BTreeMap<String, Entry>, SessionError>>;
    fn get_stats<'a>(
        &'a self,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<SessionStats, SessionError>>;
    fn get_value<'a>(
        &'a self,
        address: &'a Value,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<Option<StoredValue>, SessionError>>;
    fn scan_values<'a>(
        &'a self,
        prefix: &'a Value,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<Vec<StoredValue>, SessionError>>;
    fn read_list<'a>(
        &'a self,
        address: &'a ValueList,
        options: Option<ListReadOptions>,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<Vec<ListElement>, SessionError>>;
    /// Scan a branch from an explicit entry while this reader capability remains valid.
    fn scan_branch<'a>(
        &'a self,
        query: StorageBranchScan,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<Vec<Entry>, SessionError>>;
}

/// Exclusive keyless mutation barrier for one Session.
pub trait SessionMutation: SessionReader {
    /// Exactly zero or one commit attempt. A second attempt rejects.
    fn commit<'a>(
        &'a self,
        writes: Vec<Write>,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<CommitResult, SessionError>>;
    /// Wait for any commit attempt, invalidate the capability, and release the barrier.
    fn end<'a>(&'a self, context: &'a Context) -> maho_ai::types::BoxFuture<'a, ()>;
}

/// Callback-scoped mutation capability without authority to release its Session barrier.
pub type SessionMutator = dyn SessionMutation;

pub type SessionMutationCallback = std::sync::Arc<
    dyn for<'a> Fn(
            &'a SessionMutator,
            &'a Context,
        ) -> maho_ai::types::BoxFuture<'a, Result<JsonValue, SessionError>>
        + Send
        + Sync,
>;

pub trait Branch: Send + Sync {
    fn name(&self) -> &str;
    fn get_tip_id<'a>(
        &'a self,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<Option<String>, SessionError>>;
    fn find_entries<'a>(
        &'a self,
        query: Option<BranchScan>,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<Vec<Entry>, SessionError>>;
    fn find_entry<'a>(
        &'a self,
        query: Option<BranchScan>,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<Option<Entry>, SessionError>>;
    fn append_message<'a>(
        &'a self,
        message: AgentMessage,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<String, SessionError>>;
    fn append_custom_entry<'a>(
        &'a self,
        custom_type: String,
        data: Option<JsonValue>,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<String, SessionError>>;
}

pub trait Session: SessionReader {
    fn metadata(&self) -> &SessionMetadata;
    fn id_generator(&self) -> &IdGenerator;
    fn get_entry<'a>(
        &'a self,
        id: &'a str,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<Option<Entry>, SessionError>>;
    fn get_name<'a>(
        &'a self,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<Option<String>, SessionError>>;
    fn get_label<'a>(
        &'a self,
        target_id: &'a str,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<Option<String>, SessionError>>;
    fn find_entries<'a>(
        &'a self,
        query: Option<EntryQuery>,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<Vec<Entry>, SessionError>>;
    fn find_entry<'a>(
        &'a self,
        query: Option<EntryQuery>,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<Option<Entry>, SessionError>>;
    fn branch<'a>(
        &'a self,
        name: &'a str,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<Option<Box<dyn Branch>>, SessionError>>;
    fn create_branch<'a>(
        &'a self,
        name: &'a str,
        at: Option<String>,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<Box<dyn Branch>, SessionError>>;
    fn begin_mutation<'a>(
        &'a self,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<Box<dyn SessionMutation>, SessionError>>;
    fn mutate<'a>(
        &'a self,
        mutation: SessionMutationCallback,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<JsonValue, SessionError>>;
    fn set_value<'a>(
        &'a self,
        address: &'a Value,
        next: JsonValue,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<(), SessionError>>;
    fn delete_value<'a>(
        &'a self,
        address: &'a Value,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<(), SessionError>>;
    fn append_list<'a>(
        &'a self,
        address: &'a ValueList,
        element: JsonValue,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<(), SessionError>>;
    fn delete_list<'a>(
        &'a self,
        address: &'a ValueList,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<(), SessionError>>;
    fn set_name<'a>(
        &'a self,
        name: Option<String>,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<(), SessionError>>;
    fn set_label<'a>(
        &'a self,
        target_id: &'a str,
        label: Option<String>,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<(), SessionError>>;
    fn close<'a>(&'a self, context: &'a Context) -> maho_ai::types::BoxFuture<'a, ()>;
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCreateOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ForkPosition {
    Before,
    At,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum ForkOptions {
    Branch {
        branch: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        entry_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position: Option<ForkPosition>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    Tree {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
}

impl ForkOptions {
    pub fn id(&self) -> Option<&str> {
        match self {
            ForkOptions::Branch { id, .. } | ForkOptions::Tree { id } => id.as_deref(),
        }
    }
}

pub trait SessionRepo: Send + Sync {
    fn create<'a>(
        &'a self,
        options: SessionCreateOptions,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<Box<dyn Session>, SessionError>>;
    fn open<'a>(
        &'a self,
        metadata: SessionMetadata,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<Box<dyn Session>, SessionError>>;
    fn list<'a>(
        &'a self,
        options: Option<JsonValue>,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<Vec<SessionMetadata>, SessionError>>;
    fn delete<'a>(
        &'a self,
        metadata: SessionMetadata,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<(), SessionError>>;
    fn fork<'a>(
        &'a self,
        source: SessionMetadata,
        options: ForkOptions,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<Box<dyn Session>, SessionError>>;
}

pub type SettledAssistantMessage = AssistantMessage;
pub type SettledStopReason = StopReason;
pub type EntryTypeSet = BTreeSet<EntryType>;

impl<T: SessionReader + ?Sized> SessionReader for std::sync::Arc<T> {
    fn get_entries<'a>(
        &'a self,
        ids: Vec<String>,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<BTreeMap<String, Entry>, SessionError>> {
        (**self).get_entries(ids, context)
    }

    fn get_stats<'a>(
        &'a self,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<SessionStats, SessionError>> {
        (**self).get_stats(context)
    }

    fn get_value<'a>(
        &'a self,
        address: &'a Value,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<Option<StoredValue>, SessionError>> {
        (**self).get_value(address, context)
    }

    fn scan_values<'a>(
        &'a self,
        prefix: &'a Value,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<Vec<StoredValue>, SessionError>> {
        (**self).scan_values(prefix, context)
    }

    fn read_list<'a>(
        &'a self,
        address: &'a ValueList,
        options: Option<ListReadOptions>,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<Vec<ListElement>, SessionError>> {
        (**self).read_list(address, options, context)
    }

    fn scan_branch<'a>(
        &'a self,
        query: StorageBranchScan,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<Vec<Entry>, SessionError>> {
        (**self).scan_branch(query, context)
    }
}

impl<T: Session + ?Sized> Session for std::sync::Arc<T> {
    fn metadata(&self) -> &SessionMetadata {
        (**self).metadata()
    }

    fn id_generator(&self) -> &IdGenerator {
        (**self).id_generator()
    }

    fn get_entry<'a>(
        &'a self,
        id: &'a str,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<Option<Entry>, SessionError>> {
        (**self).get_entry(id, context)
    }

    fn get_name<'a>(
        &'a self,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<Option<String>, SessionError>> {
        (**self).get_name(context)
    }

    fn get_label<'a>(
        &'a self,
        target_id: &'a str,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<Option<String>, SessionError>> {
        (**self).get_label(target_id, context)
    }

    fn find_entries<'a>(
        &'a self,
        query: Option<EntryQuery>,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<Vec<Entry>, SessionError>> {
        (**self).find_entries(query, context)
    }

    fn find_entry<'a>(
        &'a self,
        query: Option<EntryQuery>,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<Option<Entry>, SessionError>> {
        (**self).find_entry(query, context)
    }

    fn branch<'a>(
        &'a self,
        name: &'a str,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<Option<Box<dyn Branch>>, SessionError>> {
        (**self).branch(name, context)
    }

    fn create_branch<'a>(
        &'a self,
        name: &'a str,
        at: Option<String>,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<Box<dyn Branch>, SessionError>> {
        (**self).create_branch(name, at, context)
    }

    fn begin_mutation<'a>(
        &'a self,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<Box<dyn SessionMutation>, SessionError>> {
        (**self).begin_mutation(context)
    }

    fn mutate<'a>(
        &'a self,
        mutation: SessionMutationCallback,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<JsonValue, SessionError>> {
        (**self).mutate(mutation, context)
    }

    fn set_value<'a>(
        &'a self,
        address: &'a Value,
        next: JsonValue,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<(), SessionError>> {
        (**self).set_value(address, next, context)
    }

    fn delete_value<'a>(
        &'a self,
        address: &'a Value,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<(), SessionError>> {
        (**self).delete_value(address, context)
    }

    fn append_list<'a>(
        &'a self,
        address: &'a ValueList,
        element: JsonValue,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<(), SessionError>> {
        (**self).append_list(address, element, context)
    }

    fn delete_list<'a>(
        &'a self,
        address: &'a ValueList,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<(), SessionError>> {
        (**self).delete_list(address, context)
    }

    fn set_name<'a>(
        &'a self,
        name: Option<String>,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<(), SessionError>> {
        (**self).set_name(name, context)
    }

    fn set_label<'a>(
        &'a self,
        target_id: &'a str,
        label: Option<String>,
        context: &'a Context,
    ) -> maho_ai::types::BoxFuture<'a, Result<(), SessionError>> {
        (**self).set_label(target_id, label, context)
    }

    fn close<'a>(&'a self, context: &'a Context) -> maho_ai::types::BoxFuture<'a, ()> {
        (**self).close(context)
    }
}
