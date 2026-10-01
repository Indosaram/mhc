//! Shared types for the lead mailbox poller.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use team_core::TeamCoreError;
use team_core::TeamModeConfig;
use team_core::team_mailbox::DeliveryReservation;
use team_core::types::Message;

use crate::team::messaging::delivery_journal::LeadDeliveryJournal;
use crate::team::messaging::session_marker_index::SessionMarkerIndex;

/// Source tag carried by every lead injection produced from a team message.
pub const LEAD_INJECTION_SOURCE: &str = "team-message";

pub type OnFlushed = Box<dyn FnOnce() + Send>;

pub struct LeadInjection {
    pub key: String,
    pub source: &'static str,
    pub content: String,
    pub on_flushed: Option<OnFlushed>,
}

pub trait LeadInjectionSink: Send + Sync {
    fn enqueue(&self, injection: LeadInjection);
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LeadPollFilter {
    pub from: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum LeadPollError {
    #[error(transparent)]
    Core(#[from] TeamCoreError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    InvalidReserved(#[from] InvalidReservedLeadMessageError),
}

pub trait LeadPoller {
    fn poll_once(&self, filter: Option<&LeadPollFilter>) -> Result<(), LeadPollError>;
    fn shutdown(&self);
}

/// A task event emitted by the team messaging layer (`type` + JSON `payload`).
#[derive(Debug, Clone, PartialEq)]
pub struct TeamTaskEvent {
    pub event_type: String,
    pub payload: Value,
}

pub type AppendEventFn = Box<dyn Fn(&str, TeamTaskEvent) + Send + Sync>;
pub type EventTaskIdFn = Box<dyn Fn(&Message) -> Option<String> + Send + Sync>;
pub type LeadSessionFileFn = Box<dyn Fn() -> Option<PathBuf> + Send + Sync>;

pub struct LeadPollerDeps {
    pub team_run_id: String,
    pub config: TeamModeConfig,
    pub coordinator: Arc<dyn LeadInjectionSink>,
    pub delivery_journal: Option<Arc<LeadDeliveryJournal>>,
    pub append_event: Option<AppendEventFn>,
    pub event_task_id: EventTaskIdFn,
    pub lead_session_file: Option<LeadSessionFileFn>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PendingPhase {
    AwaitingFlush,
    AwaitingPersistence,
    Recovery,
}

impl PendingPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AwaitingFlush => "awaiting_flush",
            Self::AwaitingPersistence => "awaiting_persistence",
            Self::Recovery => "recovery",
        }
    }
}

pub struct PendingDelivery {
    pub message: Message,
    pub reservation: DeliveryReservation,
    pub phase: PendingPhase,
}

pub struct LeadPollState {
    pub pending: Mutex<HashMap<String, PendingDelivery>>,
    pub is_stopped: Box<dyn Fn() -> bool + Send + Sync>,
    pub marker_index: SessionMarkerIndex,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Invalid reserved lead team message: {path}")]
pub struct InvalidReservedLeadMessageError {
    pub path: String,
}

impl InvalidReservedLeadMessageError {
    pub fn new(path: impl Into<String>) -> Self {
        Self { path: path.into() }
    }

    pub fn name(&self) -> &'static str {
        "InvalidReservedLeadMessageError"
    }
}
