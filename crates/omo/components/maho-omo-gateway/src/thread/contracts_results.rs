//! Discriminated thread result unions.
//!
//! Upstream counterpart: `thread/contracts/results.ts`. The error branch is shared data, never an
//! exception.

use serde::{Deserialize, Serialize};

use super::errors::ThreadToolFailure;
use super::store_relay_ops::{BindingRecord, OutboxRow};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ThreadDataError {
    Error { error: ThreadToolFailure },
}

impl ThreadDataError {
    pub fn of(error: ThreadToolFailure) -> Self {
        Self::Error { error }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadStatus {
    Live,
    Resumable,
}

impl ThreadStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Resumable => "resumable",
        }
    }
}

/// The endpoint serving a thread: a host (with the session's routing id) or a terminal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadEndpointSummary {
    pub kind: String,
    pub socket: String,
    pub routing_id: Option<String>,
    pub durable_session_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadSummary {
    pub thread_id: String,
    pub name: String,
    pub status: ThreadStatus,
    pub cwd: String,
    pub created_at: String,
    pub updated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<ThreadEndpointSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alive: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controls: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
}

/// A thread as a tool result names it; `link` is `[title](omo-thread://v1/<thread_id>)`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadResultSummary {
    #[serde(flatten)]
    pub summary: ThreadSummary,
    pub link: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ThreadDelivery {
    Steered { turn_id: String },
    Started { turn_id: String },
    Queued { queue_position: i64 },
    QueuedOffline { queue_position: i64 },
}

/// What a send through the session gateway adds to a delivery result.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadGatewayDeliveryFacts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadAddressResolution {
    Id,
    ExactName,
    Fuzzy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadReadSource {
    LiveHost,
    SessionJsonl,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadTranscriptItem {
    pub seq: i64,
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadCreateOk {
    pub thread: ThreadResultSummary,
    pub deduplicated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadListOk {
    pub threads: Vec<ThreadResultSummary>,
    pub scope: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadReadOk {
    pub thread_id: String,
    pub link: String,
    pub items: Vec<ThreadTranscriptItem>,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub source: ThreadReadSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_incomplete: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadSendOk {
    pub thread_id: String,
    pub link: String,
    pub delivery: ThreadDelivery,
    pub message_seq: i64,
    pub deduplicated: bool,
    #[serde(flatten)]
    pub gateway: ThreadGatewayDeliveryFacts,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadInterruptOk {
    pub thread_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    pub interrupted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadHandoffOk {
    pub thread: ThreadResultSummary,
    pub resolved_by: ThreadAddressResolution,
    pub delivery: ThreadDelivery,
    pub message_seq: i64,
    pub deduplicated: bool,
    #[serde(flatten)]
    pub gateway: ThreadGatewayDeliveryFacts,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadRenameOk {
    pub thread_id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadSetModelOk {
    pub thread_id: String,
    pub model: ThreadModelRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadModelRef {
    pub provider: String,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadSetReasoningOk {
    pub thread_id: String,
    pub level: String,
    pub scope: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadBindOk {
    pub binding: BindingRecord,
    pub deduplicated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadUnbindOk {
    pub binding: BindingRecord,
    pub already_closed: bool,
    pub in_flight: Vec<String>,
    pub deduplicated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadRebindOk {
    pub binding: BindingRecord,
    pub closed: Vec<String>,
    pub deduplicated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadBindingsOk {
    pub bindings: Vec<BindingRecord>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadReportOk {
    pub binding_id: String,
    pub revision: i64,
    pub event: String,
    pub cursor: Option<i64>,
    pub reply_token: Option<String>,
    pub armed: bool,
    pub deduplicated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadOutboxOk {
    pub binding_id: String,
    pub revision: i64,
    pub status: String,
    pub rows: Vec<OutboxRow>,
    pub next_cursor: i64,
    pub acked_cursor: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadOutboxAckOk {
    pub binding_id: String,
    pub acked_cursor: i64,
    pub changed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadAnswerOk {
    pub binding_id: String,
    pub cursor: i64,
    pub session_durable_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered_by: Option<super::types::ExternalAuthor>,
}

/// `[title](omo-thread://v1/<thread_id>)`, pasted when telling the user about the thread.
pub fn thread_link(thread_id: &str, title: &str) -> String {
    format!("[{title}](omo-thread://v1/{thread_id})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thread_link_names_the_durable_thread() {
        assert_eq!(thread_link("t-1", "Build"), "[Build](omo-thread://v1/t-1)");
    }

    #[test]
    fn an_error_result_serializes_its_failure() {
        let error = ThreadDataError::of(super::super::errors::thread_tool_failure("not_found", "gone", "call thread_list", None));
        let json = serde_json::to_value(&error).unwrap_or_default();
        assert_eq!(json["kind"], "error");
        assert_eq!(json["error"]["code"], "not_found");
    }
}
