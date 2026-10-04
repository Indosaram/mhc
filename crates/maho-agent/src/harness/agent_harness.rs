//! Port of senpi `packages/agent/src/harness/agent-harness.ts` (declaration slice consumed by
//! the runtime lane; the full port is owned by the sibling todo 15 lane).

use std::sync::Arc;

use maho_ai::types::DeferredHandle;

use crate::harness::compaction::compaction::CompactionSettings;
use crate::harness::session::session::SessionError;
use crate::harness::session::types::{JsonValue, OperationKind, OperationResultRecord, Session, ToolExecutionMode};
use crate::harness::types::{AgentHarnessStreamOptions, AgentHarnessTool};
use crate::types::QueueMode;

use super::runtime::lane::OperationMismatch;
use super::runtime::types::SystemPromptFn;

pub type Resources = crate::harness::types::AgentHarnessResources;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveOptions {
    pub operation_id: String,
    pub wait_for_retry: Option<bool>,
    pub poll_deferred: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DriveOutcome {
    Settled { outcome: OperationResultRecord },
    WaitingRetry { operation_id: String, not_before: i64 },
    WaitingDeferred { operation_id: String, deferred: DeferredHandle },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelIdentity {
    pub provider: String,
    pub model_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationStatus {
    Running,
    Open,
    Aborting,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CurrentOperationInfo {
    pub id: String,
    pub kind: OperationKind,
    pub started_at: i64,
    pub status: OperationStatus,
    pub captured_model: Option<ModelIdentity>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LaneExecutionInfo {
    pub lane: String,
    pub tip_id: Option<String>,
    pub configured_model: ModelIdentity,
    pub current: Option<CurrentOperationInfo>,
    pub last_operation_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HarnessEvent {
    pub event_type: String,
    pub lane: Option<String>,
    pub recovery: Option<bool>,
    pub payload: JsonValue,
}

/// The pinned `Context`-taking constructor lives in `runtime::harness`.
///
/// `AgentHarnessOptions<TContext>` for the process-local (`()` context) runtime lane.
///
/// Mirrors pinned `createAgentHarness(options, context)`: the constructor seeds the lane
/// configuration from `model`/`thinkingLevel`/`activeToolNames`, installs the process-local
/// `Config` (tools, resources, stream options, retry, compaction, queue modes, tool execution,
/// system prompt) and restores every durable lane into the new `Harness`.
pub struct AgentHarnessOptions {
    pub session: Arc<dyn Session>,
    pub models: maho_ai::models::Models,
    pub model: maho_ai::model::Model,
    pub thinking_level: Option<maho_ai::types::ModelThinkingLevel>,
    pub active_tool_names: Option<Vec<String>>,
    pub tools: Vec<Arc<AgentHarnessTool<()>>>,
    pub system_prompt: Option<SystemPromptFn>,
    pub resources: Option<Resources>,
    pub stream_options: Option<AgentHarnessStreamOptions>,
    pub retry: Option<maho_ai::utils::retry::RetryPolicy>,
    pub compaction: Option<CompactionSettings>,
    pub steering_mode: Option<QueueMode>,
    pub follow_up_mode: Option<QueueMode>,
    pub tool_execution: Option<ToolExecutionMode>,
}

/// Failure of one `Lane::drive` call: the pinned `Result<DriveOutcome, OperationMismatch | Closed>`.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum DriveOptionsError {
    #[error("{0}")]
    Session(SessionError),
    #[error("{0}")]
    Drive(String),
    #[error("Operation {0:?} does not own its lane")]
    Mismatch(OperationMismatch),
}

impl From<SessionError> for DriveOptionsError {
    fn from(error: SessionError) -> Self {
        Self::Session(error)
    }
}
