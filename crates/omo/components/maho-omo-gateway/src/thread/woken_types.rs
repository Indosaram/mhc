//! Woken session lifecycle types (`woken-types.ts`).

use serde::{Deserialize, Serialize};

use super::types::ProcessIdentity;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WokenSessionState {
    Requested,
    Opening,
    Open,
    Closing,
}

impl WokenSessionState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::Opening => "opening",
            Self::Open => "open",
            Self::Closing => "closing",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "requested" => Some(Self::Requested),
            "opening" => Some(Self::Opening),
            "open" => Some(Self::Open),
            "closing" => Some(Self::Closing),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WokenSessionRow {
    pub durable_id: String,
    pub session_file: String,
    pub host_instance: Option<String>,
    pub state: WokenSessionState,
    pub requested_at: i64,
    pub opened_at: Option<i64>,
    pub first_waiting_at: Option<i64>,
    pub lease_owner: Option<ProcessIdentity>,
    pub lease_expires_at: Option<i64>,
    pub generation: i64,
    pub attempts: i64,
    pub next_attempt_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WakeRequestOutcome {
    pub row: WokenSessionRow,
    pub created: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WakeStartOutcome {
    Started { row: WokenSessionRow },
    NotYet { next_attempt_at: i64 },
    InProgress { row: WokenSessionRow },
    NotFound,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WakeTakeOverOutcome {
    TakenOver {
        row: WokenSessionRow,
        previous_owner_alive: bool,
    },
    LeaseLive {
        row: WokenSessionRow,
    },
    NotLeased {
        row: WokenSessionRow,
    },
    NotFound,
    LostLease,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum WakeRetryOutcome {
    Deferred { row: WokenSessionRow },
    Failed { attempts: i64 },
    LostLease,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WokenStateUpdate {
    pub durable_id: String,
    pub generation: i64,
    pub state: WokenSessionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_instance: Option<String>,
}
