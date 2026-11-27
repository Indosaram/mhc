//! Port of senpi `packages/agent/src/execution/ledger-types.ts`.

use serde::{Deserialize, Serialize};

use crate::durable::store::StoreValue;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionKey {
    pub issuer: String,
    pub command_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionAdmission {
    pub issuer: String,
    pub command_id: String,
    pub executor: String,
    pub workspace: String,
    pub scope: String,
    pub generation: u64,
    pub request_digest: String,
    pub admitted_at: i64,
    pub lease_until: i64,
    pub reconcilable: bool,
}

impl ExecutionAdmission {
    pub fn key(&self) -> ExecutionKey {
        ExecutionKey {
            issuer: self.issuer.clone(),
            command_id: self.command_id.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrozenCall {
    pub name: String,
    pub args: StoreValue,
    pub digest: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterruptedReason {
    Cancelled,
    LeaseExpired,
    HandRestarted,
    Uncertain,
}

impl InterruptedReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::LeaseExpired => "lease_expired",
            Self::HandRestarted => "hand_restarted",
            Self::Uncertain => "uncertain",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum ExecutionOutcome {
    #[serde(rename_all = "camelCase")]
    Result {
        value: StoreValue,
        hooks_ran: bool,
        uncertain: bool,
    },
    #[serde(rename_all = "camelCase")]
    Blocked {
        value: StoreValue,
        hooks_ran: bool,
        uncertain: bool,
    },
    #[serde(rename_all = "camelCase")]
    Cancelled { hooks_ran: bool, uncertain: bool },
    #[serde(rename_all = "camelCase")]
    LeaseExpired { hooks_ran: bool, uncertain: bool },
    #[serde(rename_all = "camelCase")]
    HandRestarted { hooks_ran: bool, uncertain: bool },
    #[serde(rename_all = "camelCase")]
    Uncertain { hooks_ran: bool, uncertain: bool },
}

impl ExecutionOutcome {
    pub fn reason_str(&self) -> &'static str {
        match self {
            Self::Result { .. } => "result",
            Self::Blocked { .. } => "blocked",
            Self::Cancelled { .. } => "cancelled",
            Self::LeaseExpired { .. } => "lease_expired",
            Self::HandRestarted { .. } => "hand_restarted",
            Self::Uncertain { .. } => "uncertain",
        }
    }

    pub fn from_reason(reason: InterruptedReason, hooks_ran: bool, uncertain: bool) -> Self {
        match reason {
            InterruptedReason::Cancelled => Self::Cancelled {
                hooks_ran,
                uncertain,
            },
            InterruptedReason::LeaseExpired => Self::LeaseExpired {
                hooks_ran,
                uncertain,
            },
            InterruptedReason::HandRestarted => Self::HandRestarted {
                hooks_ran,
                uncertain,
            },
            InterruptedReason::Uncertain => Self::Uncertain {
                hooks_ran,
                uncertain,
            },
        }
    }

    pub fn is_uncertain(&self) -> bool {
        match self {
            Self::Result { uncertain, .. }
            | Self::Blocked { uncertain, .. }
            | Self::Cancelled { uncertain, .. }
            | Self::LeaseExpired { uncertain, .. }
            | Self::HandRestarted { uncertain, .. }
            | Self::Uncertain { uncertain, .. } => *uncertain,
        }
    }

    pub fn hooks_ran(&self) -> bool {
        match self {
            Self::Result { hooks_ran, .. }
            | Self::Blocked { hooks_ran, .. }
            | Self::Cancelled { hooks_ran, .. }
            | Self::LeaseExpired { hooks_ran, .. }
            | Self::HandRestarted { hooks_ran, .. }
            | Self::Uncertain { hooks_ran, .. } => *hooks_ran,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ExecutionProgress {
    Admitted,
    Preparing,
    Prepared { call: FrozenCall },
    PreparedAwaitingConsent { call: FrozenCall },
    Released { call: FrozenCall },
    Dispatched { call: FrozenCall },
    Reconciling { call: FrozenCall, until: i64 },
    Settled { outcome: ExecutionOutcome },
    Acked,
}

impl ExecutionProgress {
    pub fn state(&self) -> &'static str {
        self.state_str()
    }

    pub fn state_str(&self) -> &'static str {
        match self {
            Self::Admitted => "admitted",
            Self::Preparing => "preparing",
            Self::Prepared { .. } => "prepared",
            Self::PreparedAwaitingConsent { .. } => "prepared_awaiting_consent",
            Self::Released { .. } => "released",
            Self::Dispatched { .. } => "dispatched",
            Self::Reconciling { .. } => "reconciling",
            Self::Settled { .. } => "settled",
            Self::Acked => "acked",
        }
    }

    pub fn call(&self) -> Option<&FrozenCall> {
        match self {
            Self::Prepared { call }
            | Self::PreparedAwaitingConsent { call }
            | Self::Released { call }
            | Self::Dispatched { call }
            | Self::Reconciling { call, .. } => Some(call),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionEntry {
    pub issuer: String,
    pub command_id: String,
    pub executor: String,
    pub workspace: String,
    pub scope: String,
    pub generation: u64,
    pub request_digest: String,
    pub admitted_at: i64,
    pub lease_until: i64,
    pub reconcilable: bool,
    pub initial_lease_until: i64,
    pub renew_seq: u64,
    pub effect_possible: bool,
    pub body_dispatched: bool,
    pub progress: ExecutionProgress,
}

impl ExecutionEntry {
    pub fn key(&self) -> ExecutionKey {
        ExecutionKey {
            issuer: self.issuer.clone(),
            command_id: self.command_id.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExecutionEvent {
    Prepare,
    Effect,
    Consent,
    Release,
    Dispatch,
    Cancel,
    Abort,
    Ack,
    Expire,
    Restart,
    #[serde(rename_all = "camelCase")]
    Freeze {
        call: FrozenCall,
        awaiting_consent: bool,
    },
    Result {
        value: StoreValue,
    },
    Block {
        value: StoreValue,
    },
    #[serde(rename_all = "camelCase")]
    Renew {
        seq: u64,
        lease_until: i64,
    },
}

impl ExecutionEvent {
    pub fn kind_str(&self) -> &'static str {
        match self {
            Self::Prepare => "prepare",
            Self::Effect => "effect",
            Self::Consent => "consent",
            Self::Release => "release",
            Self::Dispatch => "dispatch",
            Self::Cancel => "cancel",
            Self::Abort => "abort",
            Self::Ack => "ack",
            Self::Expire => "expire",
            Self::Restart => "restart",
            Self::Freeze { .. } => "freeze",
            Self::Result { .. } => "result",
            Self::Block { .. } => "block",
            Self::Renew { .. } => "renew",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionActor {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_workspace: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionCommand {
    pub key: ExecutionKey,
    pub generation: u64,
    pub actor: ExecutionActor,
    pub now: i64,
    pub event: ExecutionEvent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LedgerRefusal {
    NotFound,
    IdentityConflict,
    InvalidAdmission,
    StaleGeneration,
    Unauthorized,
    InvalidTransition,
    InvalidRenewal,
    InvalidTime,
    Terminal,
    EpochFenced,
}

impl LedgerRefusal {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::IdentityConflict => "identity_conflict",
            Self::InvalidAdmission => "invalid_admission",
            Self::StaleGeneration => "stale_generation",
            Self::Unauthorized => "unauthorized",
            Self::InvalidTransition => "invalid_transition",
            Self::InvalidRenewal => "invalid_renewal",
            Self::InvalidTime => "invalid_time",
            Self::Terminal => "terminal",
            Self::EpochFenced => "epoch_fenced",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LedgerReply {
    Applied { entry: ExecutionEntry },
    Replayed { entry: ExecutionEntry },
    Refused { reason: LedgerRefusal },
    ResultExpired,
}

pub type Actor = ExecutionActor;
pub type RefusalReason = LedgerRefusal;

pub const MAX_EXECUTION_LIFETIME_MS: i64 = 86_400_000;
pub const RECONCILIATION_GRACE_MS: i64 = 30_000;

pub fn refused(reason: LedgerRefusal) -> LedgerReply {
    LedgerReply::Refused { reason }
}

pub fn replayed(entry: ExecutionEntry) -> LedgerReply {
    if matches!(entry.progress, ExecutionProgress::Acked) {
        LedgerReply::ResultExpired
    } else {
        LedgerReply::Replayed { entry }
    }
}

pub fn applied(entry: ExecutionEntry) -> LedgerReply {
    LedgerReply::Applied { entry }
}
