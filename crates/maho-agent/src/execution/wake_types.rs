//! Port of senpi `packages/agent/src/execution/wake-types.ts`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::durable::store::StoreValue;

/// Design 5.6.4: an un-acked wake lives until its scope closes plus this long, then it is dropped as `wake_expired`.
pub const WAKE_RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// One durable wake from a hand to the brain that owns its scope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WakeEvent {
    pub event_id: String,
    pub scope: String,
    pub source: String,
    pub payload: StoreValue,
}

/// Input when offering a new wake (before `event_id` is minted).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WakeOffer {
    pub scope: String,
    pub source: String,
    pub payload: StoreValue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeRefusal {
    NotScopeHand,
    UnknownScope,
    ScopeClosed,
    NotScopeSession,
}

impl WakeRefusal {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NotScopeHand => "not_scope_hand",
            Self::UnknownScope => "unknown_scope",
            Self::ScopeClosed => "scope_closed",
            Self::NotScopeSession => "not_scope_session",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "not_scope_hand" => Some(Self::NotScopeHand),
            "unknown_scope" => Some(Self::UnknownScope),
            "scope_closed" => Some(Self::ScopeClosed),
            "not_scope_session" => Some(Self::NotScopeSession),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WakeDropReason {
    Overflow,
    Expired,
    Refused,
    Malformed,
    Unrecorded,
}

impl WakeDropReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Overflow => "overflow",
            Self::Expired => "expired",
            Self::Refused => "refused",
            Self::Malformed => "malformed",
            Self::Unrecorded => "unrecorded",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "overflow" => Some(Self::Overflow),
            "expired" => Some(Self::Expired),
            "refused" => Some(Self::Refused),
            "malformed" => Some(Self::Malformed),
            "unrecorded" => Some(Self::Unrecorded),
            _ => None,
        }
    }
}

pub const WAKE_DROPPED_SOURCE: &str = "wake.dropped";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WakeLimits {
    pub per_scope: usize,
    pub total: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum WakeReceipt {
    #[serde(rename_all = "camelCase")]
    Ack { event_id: String, duplicate: bool },
    #[serde(rename_all = "camelCase")]
    Refused {
        event_id: String,
        reason: WakeRefusal,
    },
}

impl WakeReceipt {
    pub fn event_id(&self) -> &str {
        match self {
            Self::Ack { event_id, .. } | Self::Refused { event_id, .. } => event_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxRefusalEntry {
    pub reason: WakeRefusal,
    pub at: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WakeOutboxRecord {
    pub pending: Vec<WakeEvent>,
    pub drops: Vec<WakeEvent>,
    pub refused: BTreeMap<String, OutboxRefusalEntry>,
    pub closed: BTreeMap<String, i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WakeInboxRecord {
    pub seen: BTreeMap<String, Vec<String>>,
    pub pending: Vec<WakeEvent>,
    pub closed: BTreeMap<String, i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failures: Option<BTreeMap<String, usize>>,
}
