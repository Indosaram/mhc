//! Steering port and outcome types (`steering/types.ts`).

use std::sync::Arc;

use crate::host::HostError;
use crate::lifecycle::DestroyCause;
use crate::manager::ManagedChildHandle;
use crate::state::{DeliverAs, TaskRunStats, TaskStatus};
use crate::store::{StoreError, TaskRecordStore};

/// Destruction is delegated exclusively to lifecycle's single-writer port.
pub trait DestructionPort: Send + Sync {
    fn destroy_resident_task(&self, task_id: &str, cause: DestroyCause) -> Result<(), HostError>;
}

pub type RunStatsSnapshotFn = dyn Fn(&str) -> Option<TaskRunStats> + Send + Sync;
pub type LiveHandleFn = dyn Fn(&str) -> Option<Arc<dyn ManagedChildHandle>> + Send + Sync;

/// What the engine needs from the manager.
#[derive(Clone)]
pub struct SteeringPort {
    pub store: TaskRecordStore,
    pub live_handle: Arc<LiveHandleFn>,
    /// Removes a queued (never-launched) child from the concurrency queue.
    pub dequeue_pending: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    /// Re-acquires a residency slot for a revived terminal child.
    pub reacquire_for_revive: Arc<dyn Fn(&str) + Send + Sync>,
    pub destruction: Arc<dyn DestructionPort>,
    pub run_stats_snapshot: Arc<RunStatsSnapshotFn>,
    /// Epoch milliseconds.
    pub now: Arc<dyn Fn() -> i64 + Send + Sync>,
}

pub const DEFAULT_SEND_DELIVERY: DeliverAs = DeliverAs::FollowUp;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SendInput {
    pub id_or_name: String,
    pub message: String,
    pub deliver_as: Option<DeliverAs>,
    pub caller_session_id: Option<String>,
    pub all_scope: bool,
}

impl SendInput {
    pub fn new(id_or_name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            id_or_name: id_or_name.into(),
            message: message.into(),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendOutcome {
    Steered {
        task_id: String,
        status: TaskStatus,
        delivered: DeliverAs,
    },
    Revived {
        task_id: String,
        run_epoch: i64,
    },
    Queued {
        task_id: String,
        queue_position: usize,
    },
    NotContinuable {
        task_id: String,
        reason: String,
        suggestion: String,
    },
    OneShotAgent {
        task_id: String,
        agent: String,
        message: String,
    },
    ScopeDenied {
        task_id: String,
        owning_session_id: String,
        reason: String,
    },
    NotFound {
        reason: String,
        suggestion: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InterruptOutcome {
    Interrupted {
        task_id: String,
        previous_status: TaskStatus,
    },
    Noop {
        task_id: String,
        status: TaskStatus,
        reason: String,
    },
    NotFound {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CancelOutcome {
    Cancelled {
        task_id: String,
        previous_status: TaskStatus,
    },
    Noop {
        task_id: String,
        status: TaskStatus,
        reason: String,
    },
    NotFound {
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CancelAbort {
    #[default]
    Request,
    Skip,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CancelOptions {
    pub abort: CancelAbort,
}

#[derive(Debug, thiserror::Error)]
pub enum SteeringError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Host(#[from] HostError),
}
