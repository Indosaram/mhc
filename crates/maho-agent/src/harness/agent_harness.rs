//! Port of senpi `packages/agent/src/harness/agent-harness.ts` (declaration slice consumed by
//! the runtime lane; the full port is owned by the sibling todo 15 lane).

use std::sync::Arc;

use maho_ai::types::DeferredHandle;

use crate::harness::session::types::{JsonValue, OperationKind, OperationResultRecord};
use crate::harness::types::{AgentHarnessStreamOptions, AgentHarnessTool, PromptTemplate, Skill};

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

pub struct AgentHarnessOptions<TContext> {
    pub tools: Vec<Arc<AgentHarnessTool<TContext>>>,
    pub resources: Option<Resources>,
    pub stream_options: Option<AgentHarnessStreamOptions>,
    pub prompt_templates: Option<Vec<PromptTemplate>>,
    pub skills: Option<Vec<Skill>>,
}
