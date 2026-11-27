//! Extended thread tool result contracts (`thread/contracts/results.ts`).

use serde::{Deserialize, Serialize};

pub use super::super::contracts_results::{
    ThreadAddressResolution, ThreadAnswerOk, ThreadBindOk, ThreadBindingsOk, ThreadCreateOk,
    ThreadDataError, ThreadDelivery, ThreadEndpointSummary, ThreadGatewayDeliveryFacts,
    ThreadHandoffOk, ThreadInterruptOk, ThreadListOk, ThreadModelRef, ThreadOutboxAckOk,
    ThreadOutboxOk, ThreadReadOk, ThreadReadSource, ThreadRebindOk, ThreadRenameOk, ThreadReportOk,
    ThreadResultSummary, ThreadSendOk, ThreadSetModelOk, ThreadSetReasoningOk, ThreadStatus,
    ThreadSummary, ThreadTranscriptItem, ThreadUnbindOk, thread_link,
};
pub use super::super::errors::ThreadToolFailure;
pub use super::state::{
    THREAD_ACTIVITIES, THREAD_STATES, ThreadActivity, ThreadModel, ThreadState,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadRetryWait {
    pub kind: String,
    pub reason: String,
    pub until: String,
    pub attempt: i64,
    pub max_attempts: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadBacklog {
    pub held: i64,
    pub held_by_session: i64,
    pub waiting_in_gateway: i64,
    pub oldest_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadWaitOutcome {
    pub outcome: String,
    pub status: String,
    pub waited_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadInterruptBackgroundJob {
    pub kind: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadInterruptBackgroundFailedJob {
    pub kind: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ThreadInterruptBackground {
    Stopped {
        stopped: Vec<ThreadInterruptBackgroundJob>,
        failed: Vec<ThreadInterruptBackgroundFailedJob>,
    },
    NotSupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadCloseGraceful {
    pub turn: i64,
    pub waited_ms: i64,
    pub stopped_wake_sources: Vec<String>,
    pub returned_user_messages: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadDevice {
    pub device: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub online: bool,
    pub last_seen: Option<String>,
    pub this_device: bool,
    pub listing: String,
    pub threads: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dropped: Option<usize>,
}
