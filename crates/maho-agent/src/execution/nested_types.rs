//! Port of senpi `packages/agent/src/execution/nested-types.ts`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::coordinator_types::ExecutionRef;

/// 0010's lifetime of an execution: a nested set never outlives its parent's admission by more than this.
pub const NESTED_LIFETIME_MS: i64 = 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NestedCapabilities {
    pub preparation: Vec<String>,
    pub body: Vec<String>,
}

/// The parent execution a nested set belongs to, as the brain recorded it when it sent the parent's prepare.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NestedParent {
    pub execution_id: String,
    pub generation: u64,
    pub executor: String,
    pub scope: String,
    pub admitted_at: i64,
    pub lease_until: i64,
    pub budget: u64,
    pub capabilities: NestedCapabilities,
}

/// A coordinator-owned background activity of a detached cell, registered before the detached result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NestedActivity {
    pub activity_id: String,
    pub parent: String,
    pub executor: String,
    pub scope: String,
    pub targets: Vec<String>,
    pub budget: u64,
    pub expires_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NestedRequest {
    pub sender: String,
    pub parent: String,
    pub parent_generation: u64,
    pub scope: String,
    pub child_request_id: String,
    pub target: String,
    pub cost: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NestedRefusal {
    UnknownParent,
    NotParentExecutor,
    StaleGeneration,
    ScopeMismatch,
    ParentTerminal,
    Expired,
    NotInNestedSet,
    BudgetExhausted,
    InvalidCost,
    UnknownActivity,
    ActivityEnded,
    ConflictingRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NestedChild {
    pub execution_id: String,
    pub target: String,
    pub cost: u64,
    pub origin: String,
    pub activity_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NestedAdmission {
    Admitted {
        child: ExecutionRef,
        target: String,
        replayed: bool,
    },
    Refused {
        reason: NestedRefusal,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityRefusal {
    ParentTerminal,
    UnknownParent,
    ExceedsParent,
    ConflictingActivity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityRecord {
    pub activity_id: String,
    pub parent: String,
    pub executor: String,
    pub scope: String,
    pub targets: Vec<String>,
    pub budget: u64,
    pub expires_at: i64,
    pub debited: u64,
    pub ended: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NestedPhase {
    Preparation,
    Body,
    Settled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CancelDeliveryState {
    Queued,
    Delivered,
}

/// The brain's durable record of one parent's nested work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NestedRecord {
    pub parent: NestedParent,
    pub phase: NestedPhase,
    pub cancelled: bool,
    pub expires_at: i64,
    pub debited: u64,
    pub children: BTreeMap<String, NestedChild>,
    pub activities: BTreeMap<String, ActivityRecord>,
    pub cancels: BTreeMap<String, CancelDeliveryState>,
}

/// Where a parent cancel is delivered: the child's executor, with 0010 cancel semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NestedCancelTarget {
    pub exec_ref: ExecutionRef,
    pub executor: String,
}
