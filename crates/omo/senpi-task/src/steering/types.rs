//! Steering port and outcome types (`steering/types.ts`).

use std::sync::Arc;

use crate::host::HostError;
use crate::lifecycle::DestroyCause;
use crate::manager::ManagedChildHandle;
use crate::state::{DeliverAs, TaskRecord, TaskRunStats, TaskStatus};
use crate::store::{StoreError, TaskRecordStore};

/// Destruction is delegated exclusively to lifecycle's single-writer port.
pub trait DestructionPort: Send + Sync {
    fn destroy_resident_task(&self, task_id: &str, cause: DestroyCause) -> Result<(), HostError>;
}

pub type RunStatsSnapshotFn = dyn Fn(&str) -> Option<TaskRunStats> + Send + Sync;
pub type LiveHandleFn = dyn Fn(&str) -> Option<Arc<dyn ManagedChildHandle>> + Send + Sync;

/// What the engine needs from the manager.
/// Called with a task id when its stop is requested or settled.
pub type StopHookFn = dyn Fn(&str) + Send + Sync;

pub type HeldSendFn =
    dyn Fn(&TaskRecord, &str) -> Option<Result<SendOutcome, SteeringError>> + Send + Sync;

#[derive(Clone)]
pub struct SteeringPort {
    pub store: TaskRecordStore,
    pub live_handle: Arc<LiveHandleFn>,
    pub send_while_run_held: Option<Arc<HeldSendFn>>,
    /// Removes a queued (never-launched) child from the concurrency queue.
    pub dequeue_pending: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    /// Re-acquires a residency slot for a revived terminal child.
    pub reacquire_for_revive: Arc<dyn Fn(&str) + Send + Sync>,
    pub destruction: Arc<dyn DestructionPort>,
    pub run_stats_snapshot: Arc<RunStatsSnapshotFn>,
    /// A cancel that must wait for the child's lost connection: until `stop_settled`, only the cancel
    /// ends that run, so no failure the stop itself causes is written over it.
    pub stop_requested: Option<Arc<StopHookFn>>,
    pub stop_settled: Option<Arc<StopHookFn>>,
    /// Epoch milliseconds.
    pub now: Arc<dyn Fn() -> i64 + Send + Sync>,
}

/// Why a cancel is still pending (TS `CANCEL_PENDING_REASON`).
pub const CANCEL_PENDING_REASON: &str = "cancel requested, child unreachable";

pub const DEFAULT_SEND_DELIVERY: DeliverAs = DeliverAs::FollowUp;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SendInput {
    pub id_or_name: String,
    pub message: String,
    pub deliver_as: Option<DeliverAs>,
    pub caller_session_id: Option<String>,
    pub all_scope: bool,
    /// Deliver only if the task is still on this run epoch.
    pub expected_run_epoch: Option<i64>,
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
    Stale {
        task_id: String,
        run_epoch: i64,
        reason: String,
    },
    Steered {
        task_id: String,
        status: TaskStatus,
        delivered: DeliverAs,
    },
    Revived {
        task_id: String,
        run_epoch: i64,
    },
    Held {
        task_id: String,
        pause_seq: u64,
        entry_id: String,
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
    /// The child's connection is down: the stop waits on its host and the record carries
    /// `cancel_requested`.
    CancelPending {
        task_id: String,
        previous_status: TaskStatus,
        reason: String,
    },
    Stale {
        task_id: String,
        status: TaskStatus,
        run_epoch: i64,
        reason: String,
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
    /// Cancel only if the task is still on this run epoch.
    pub expected_run_epoch: Option<i64>,
}

#[derive(Debug, thiserror::Error)]
pub enum SteeringError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Host(#[from] HostError),
}
