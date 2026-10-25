use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::manager::types::{ManagerStartSpec, ResolvedChildPlan};

pub type Json = Value;
pub type PoolId = String;
pub type ItemId = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkpoolMode { Fresh, KeepAlive }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WorkpoolAgent {
    Category { prompt: String, model: Option<String>, category: String },
    SubagentType { prompt: String, model: Option<String>, subagent_type: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkpoolCreate {
    pub name: String,
    pub agent: WorkpoolAgent,
    pub mode: Option<WorkpoolMode>,
    pub tools: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkpoolCaller { pub session_id: String, pub root_session_id: String, pub depth: u32, pub cwd: String }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkpoolInput { pub key: String, pub input: Json }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WorkpoolYieldValue {
    Data { key: String, data: Json },
    Error { key: String, error: WorkpoolYieldError },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkpoolYieldError { pub code: String, pub message: String }

pub const WORKPOOL_ERROR_CODES: &[&str] = &[
    "invalid_input", "invalid_pool_id", "pool_not_found", "scope_denied", "pool_name_conflict", "pool_closed",
    "duplicate_key_conflict", "tools_unavailable", "admission_refused", "spawn_failed", "policy_denied",
    "worker_not_continuable", "worker_unassigned", "stale_assignment", "yield_unavailable", "store_corrupt", "cancelled",
    "yield_conflict", "item_missing_yield", "delivery_uncertain", "cwd_unavailable", "config_generation_mismatch",
    "reserved_tool_name", "tool_name_collision", "kernel_tool_missing", "kernel_tool_stale",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkpoolErrorCode {
    InvalidInput, InvalidPoolId, PoolNotFound, ScopeDenied, PoolNameConflict, PoolClosed,
    DuplicateKeyConflict, ToolsUnavailable, AdmissionRefused, SpawnFailed, PolicyDenied,
    WorkerNotContinuable, WorkerUnassigned, StaleAssignment, YieldUnavailable, StoreCorrupt, Cancelled,
    YieldConflict, ItemMissingYield, DeliveryUncertain, CwdUnavailable, ConfigGenerationMismatch,
    ReservedToolName, ToolNameCollision, KernelToolMissing, KernelToolStale,
}

impl WorkpoolErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input", Self::InvalidPoolId => "invalid_pool_id", Self::PoolNotFound => "pool_not_found",
            Self::ScopeDenied => "scope_denied", Self::PoolNameConflict => "pool_name_conflict", Self::PoolClosed => "pool_closed",
            Self::DuplicateKeyConflict => "duplicate_key_conflict", Self::ToolsUnavailable => "tools_unavailable", Self::AdmissionRefused => "admission_refused",
            Self::SpawnFailed => "spawn_failed", Self::PolicyDenied => "policy_denied", Self::WorkerNotContinuable => "worker_not_continuable",
            Self::WorkerUnassigned => "worker_unassigned", Self::StaleAssignment => "stale_assignment", Self::YieldUnavailable => "yield_unavailable",
            Self::StoreCorrupt => "store_corrupt", Self::Cancelled => "cancelled", Self::YieldConflict => "yield_conflict",
            Self::ItemMissingYield => "item_missing_yield", Self::DeliveryUncertain => "delivery_uncertain", Self::CwdUnavailable => "cwd_unavailable",
            Self::ConfigGenerationMismatch => "config_generation_mismatch", Self::ReservedToolName => "reserved_tool_name",
            Self::ToolNameCollision => "tool_name_collision", Self::KernelToolMissing => "kernel_tool_missing", Self::KernelToolStale => "kernel_tool_stale",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkpoolError { pub code: WorkpoolErrorCode, pub message: String }
impl WorkpoolError { pub fn new(code: WorkpoolErrorCode, message: impl Into<String>) -> Self { Self { code, message: message.into() } } }
impl fmt::Display for WorkpoolError { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "{}: {}", self.code.as_str(), self.message) } }
impl std::error::Error for WorkpoolError {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkpoolItem {
    pub key: String, pub input: Json, pub item_id: ItemId,
    pub status: WorkpoolItemStatus, pub binding: Option<WorkpoolBinding>, pub delivery: Option<WorkpoolDelivery>,
    pub data: Option<Json>, pub error: Option<WorkpoolYieldError>, pub yield_sha256: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkpoolItemStatus { Queued, Assigned, Completed, Error, Cancelled }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkpoolBinding { pub task_id: String, pub run_epoch: u32, pub generation: u32 }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkpoolDelivery { pub phase: WorkpoolDeliveryPhase, pub message_sha256: Option<String>, #[serde(default, skip_serializing_if = "Option::is_none")] pub entry_ids: Option<Vec<String>> }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkpoolDeliveryPhase { Queued, Dispatching, Acknowledged }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkpoolWorker { pub task_id: String, pub run_epoch: u32, pub status: WorkpoolWorkerStatus, pub completed_turns: u32, pub idle_since: u64 }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkpoolWorkerStatus { Busy, Idle, Paused }

#[derive(Clone)]
pub struct WorkpoolSpec { pub start: ManagerStartSpec, pub plan: ResolvedChildPlan }
#[derive(Clone)]
pub struct WorkpoolRecord {
    pub version: u8, pub pool_id: PoolId, pub name: String, pub parent_session_id: String, pub root_session_id: String,
    pub generation: u32, pub revision: u32, pub mode: WorkpoolMode, pub agent: WorkpoolAgent, pub worker_spec: WorkpoolSpec,
    pub status: WorkpoolStatus, pub kernel_tool_names: Option<Vec<String>>, pub items: Vec<WorkpoolItem>, pub workers: Vec<WorkpoolWorker>,
    pub hold: Option<WorkpoolHold>,
    pub aggregate: Option<WorkpoolAggregate>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkpoolHold { pub reason: WorkpoolHoldReason, #[serde(rename = "pauseSeq")] pub pause_seq: u64 }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkpoolHoldReason { UserStop }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkpoolStatus { Open, Closing, Cancelled }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkpoolAggregate { pub generation: u32, pub delivered: bool, pub accepted: Option<bool> }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkpoolEvent { pub pool_id: PoolId, pub kind: WorkpoolEventKind, pub item_id: Option<ItemId>, pub task_id: Option<String>, pub run_epoch: Option<u32>, pub error: Option<WorkpoolEventError> }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkpoolEventKind { Queued, Waiting, Granted, Dispatched, AdmissionFailed, Cancelled, WorkerIdle, ItemResult, AggregateFailed }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkpoolEventError { pub code: WorkpoolErrorCode, pub message: String }

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn error_code_vocabulary_is_stable() { assert_eq!(WORKPOOL_ERROR_CODES.len(), 26); assert_eq!(WorkpoolErrorCode::InvalidPoolId.as_str(), "invalid_pool_id"); }
    #[test] fn agent_variants_are_mutually_exclusive() { let value = serde_json::to_value(WorkpoolAgent::Category { prompt: "p".into(), model: None, category: "c".into() }).expect("json"); assert!(value.get("category").is_some()); assert!(value.get("subagent_type").is_none()); }
}
