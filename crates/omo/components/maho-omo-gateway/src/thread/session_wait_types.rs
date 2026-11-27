//! Woken session wait states and intake tracking types (`session-wait-types.ts`).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionWaitKind {
    Held,
    Waiting,
}

impl SessionWaitKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Held => "held",
            Self::Waiting => "waiting",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "held" => Some(Self::Held),
            "waiting" => Some(Self::Waiting),
            _ => None,
        }
    }
}

/// A session the carrier found held or waiting for a person (A2.6, A3.2.8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionWaitState {
    pub durable_id: String,
    pub wait: SessionWaitKind,
    pub hold_kinds: Vec<String>,
    pub file_size: i64,
    pub written_at: i64,
}

/// Holds released automatically when a delivery is admitted do not keep a woken session waiting.
pub fn wait_blocks_delivery(state: &SessionWaitState) -> bool {
    state.wait != SessionWaitKind::Held || state.hold_kinds.iter().any(|kind| kind != "unattended")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueuedWaitKind {
    Hold,
    Approval,
    Wake,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedWaitingOn {
    pub kind: QueuedWaitKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntakePair {
    pub sender_device: String,
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntakeMark {
    pub sender_device: String,
    pub session_id: String,
    pub last_command_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenIntakeSession {
    pub durable_id: String,
    pub devices: Vec<String>,
    pub local: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "drained")]
pub enum DrainedOutcome {
    #[serde(rename = "true")]
    Drained { last_command_id: String },
    #[serde(rename = "false")]
    NotDrained,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionWork {
    pub queued: i64,
    pub claimed: i64,
    pub unapplied: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteSessionWaitRequest {
    pub durable_id: String,
    pub wait: SessionWaitKind,
    pub hold_kinds: Vec<String>,
    pub file_size: i64,
}
