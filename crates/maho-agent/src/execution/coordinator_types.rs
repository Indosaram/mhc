//! Port of senpi `packages/agent/src/execution/coordinator-types.ts`.

use std::sync::Arc;

use maho_ai::types::BoxFuture;
use serde::{Deserialize, Serialize};

use crate::durable::store::StoreValue;

use super::ledger_types::{ExecutionOutcome, InterruptedReason};

/// One model call of an assistant message, in source order, as the brain plans it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchCallPlan {
    pub execution_id: String,
    pub executor: String,
    pub name: String,
    pub args: StoreValue,
    pub sequential: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchPlan {
    pub batch_id: String,
    pub scope: String,
    pub workspace: String,
    pub generation: u64,
    pub admitted_at: i64,
    pub lease_until: i64,
    pub calls: Vec<BatchCallPlan>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestCall {
    pub execution_id: String,
    pub executor: String,
    pub name: String,
    pub args: StoreValue,
    pub sequential: bool,
    pub source_index: usize,
    pub depends_on: Vec<usize>,
    pub wave: usize,
    pub request_digest: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchManifest {
    pub batch_id: String,
    pub scope: String,
    pub workspace: String,
    pub generation: u64,
    pub admitted_at: i64,
    pub lease_until: i64,
    pub issuer: String,
    pub manifest_digest: String,
    pub calls: Vec<ManifestCall>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareCallPayload {
    pub name: String,
    pub args: StoreValue,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareRequest {
    pub issuer: String,
    pub execution_id: String,
    pub generation: u64,
    pub batch_id: String,
    pub manifest_digest: String,
    pub scope: String,
    pub workspace: String,
    pub admitted_at: i64,
    pub lease_until: i64,
    pub request_digest: String,
    pub call: PrepareCallPayload,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorized_digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionRef {
    pub issuer: String,
    pub execution_id: String,
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PrepareReply {
    #[serde(rename_all = "camelCase")]
    Prepared {
        frozen_digest: String,
    },
    #[serde(rename_all = "camelCase")]
    PreparedAwaitingConsent {
        frozen_digest: String,
    },
    Blocked {
        result: StoreValue,
    },
    BatchAbort,
    Settled {
        outcome: ExecutionOutcome,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SettleReply {
    Settled { outcome: ExecutionOutcome },
    UnknownExecution,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConsentStepReply {
    Prepared,
    Settled { outcome: ExecutionOutcome },
    UnknownExecution,
}

impl From<SettleReply> for ConsentStepReply {
    fn from(value: SettleReply) -> Self {
        match value {
            SettleReply::Settled { outcome } => Self::Settled { outcome },
            SettleReply::UnknownExecution => Self::UnknownExecution,
        }
    }
}

pub trait BatchExecutor: Send + Sync {
    fn prepare(&self, request: PrepareRequest) -> BoxFuture<'_, Result<PrepareReply, String>>;
    fn release(&self, exec_ref: ExecutionRef) -> BoxFuture<'_, Result<SettleReply, String>>;
    fn abort(&self, exec_ref: ExecutionRef) -> BoxFuture<'_, Result<SettleReply, String>>;
    fn consent(
        &self,
        _exec_ref: ExecutionRef,
        _frozen_digest: &str,
    ) -> Option<BoxFuture<'_, Result<ConsentStepReply, String>>> {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConsentAnswer {
    Approved,
    Denied,
    Expired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConsentGate {
    Raw,
    Frozen,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsentRequest {
    pub issuer: String,
    pub execution_id: String,
    pub generation: u64,
    pub gate: ConsentGate,
    pub digest: String,
    pub name: String,
    pub lease_until: i64,
}

pub trait BatchConsent: Send + Sync {
    fn needs_consent(&self, name: &str, args: &StoreValue) -> bool;
    fn approve(&self, request: ConsentRequest) -> BoxFuture<'_, Result<ConsentAnswer, String>>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsentRecord {
    pub gate: ConsentGate,
    pub digest: String,
    pub answer: ConsentAnswer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum BatchDecision {
    Release,
    Abort,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotRunCause {
    BatchAbort,
    InterruptedPreparation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeInterruptionReason {
    Cancelled,
    LeaseExpired,
    HandRestarted,
    Uncertain,
    UnknownExecution,
}

impl From<InterruptedReason> for OutcomeInterruptionReason {
    fn from(value: InterruptedReason) -> Self {
        match value {
            InterruptedReason::Cancelled => Self::Cancelled,
            InterruptedReason::LeaseExpired => Self::LeaseExpired,
            InterruptedReason::HandRestarted => Self::HandRestarted,
            InterruptedReason::Uncertain => Self::Uncertain,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CallOutcome {
    Result {
        value: StoreValue,
    },
    Blocked {
        result: StoreValue,
    },
    NotRun {
        cause: NotRunCause,
    },
    Interrupted {
        reason: OutcomeInterruptionReason,
        uncertain: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchRecord {
    pub manifest: BatchManifest,
    pub prepares: Vec<Option<PrepareReply>>,
    pub decision: Option<BatchDecision>,
    pub released: Vec<bool>,
    pub outcomes: Vec<Option<CallOutcome>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consents: Option<Vec<Option<ConsentRecord>>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BatchResult {
    pub decision: BatchDecision,
    pub outcomes: Vec<CallOutcome>,
}

pub type BatchExecutorFactory = Arc<dyn Fn(&str) -> Arc<dyn BatchExecutor> + Send + Sync>;
