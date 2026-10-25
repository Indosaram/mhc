//! The host status a send answers with, decided from the run the engine acted on
//! (`eval-handles/run-after-engine.ts`).

use super::errors::{EvalHandleErrorCode, EvalHandleHostError};
use super::host::{HandlePhase, HandleRef, HandleSnapshot};
use super::task_refs::{TaskRecord, settled, task_snapshot};

/// The run ended while the message was delivered; a follow-up turn would be a newer epoch, so
/// re-fetch to follow it.
pub const SEND_HOST_STATUS_DELIVERED_AFTER_END: &str = "delivered_after_run_ended";
/// The message was durably accepted locally but the host has not received it yet: never a delivery.
pub const SEND_HOST_STATUS_QUEUED: &str = "queued";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Live,
    Moved,
    RolledBack,
}

/// The ref the engine acted on, or the snapshot that answers for it outright.
enum SettledRun {
    At(HandleRef),
    Answered(HandleSnapshot),
}

#[derive(Debug, Clone, Copy)]
pub struct RunAfterEngine {
    pub observed_epoch: i64,
    pub expected_epoch: i64,
    pub verdict: Verdict,
}

impl RunAfterEngine {
    pub fn new(observed_epoch: i64, expected_epoch: i64) -> Self {
        let verdict = if observed_epoch == expected_epoch {
            Verdict::Live
        } else if observed_epoch > expected_epoch {
            Verdict::Moved
        } else {
            Verdict::RolledBack
        };
        Self {
            observed_epoch,
            expected_epoch,
            verdict,
        }
    }

    pub fn ref_for(&self, reference: &HandleRef) -> HandleRef {
        HandleRef {
            run_epoch: self.observed_epoch,
            ..reference.clone()
        }
    }

    pub fn phase(&self) -> HandlePhase {
        HandlePhase::Pending
    }

    /// The reply for a message the engine delivered (or revived the task with).
    pub fn delivered(
        &self,
        reference: &HandleRef,
        record: &TaskRecord,
        delivered_status: Option<&str>,
    ) -> Result<HandleSnapshot, EvalHandleHostError> {
        let target = match self.settled_run(reference, record)? {
            SettledRun::Answered(snapshot) => return Ok(snapshot),
            SettledRun::At(target) => target,
        };
        if let Some(status) = delivered_status {
            return Ok(task_snapshot(record, target, status));
        }
        // The run ended while the message was delivered: a follow-up turn would be a newer epoch
        // this ref cannot follow.
        if settled(record) {
            return Ok(task_snapshot(
                record,
                target,
                SEND_HOST_STATUS_DELIVERED_AFTER_END,
            ));
        }
        Ok(task_snapshot(record, target, match record.status {
            super::host::TaskStatus::Pending => "pending",
            super::host::TaskStatus::Running => "running",
            super::host::TaskStatus::Paused => "paused",
            super::host::TaskStatus::Completed => "completed",
            super::host::TaskStatus::Error => "error",
            super::host::TaskStatus::Cancelled => "cancelled",
            super::host::TaskStatus::Interrupted => "interrupted",
            super::host::TaskStatus::Lost => "lost",
        }))
    }

    /// The reply for a message only queued locally: it stays `queued` even when the run ended
    /// before this read.
    pub fn queued(
        &self,
        reference: &HandleRef,
        record: &TaskRecord,
    ) -> Result<HandleSnapshot, EvalHandleHostError> {
        let target = match self.settled_run(reference, record)? {
            SettledRun::Answered(snapshot) => return Ok(snapshot),
            SettledRun::At(target) => target,
        };
        Ok(task_snapshot(record, target, SEND_HOST_STATUS_QUEUED))
    }

    /// Refuses a rolled-back run, names a moved run by its newer ref, else yields the ref at the
    /// epoch the engine acted on.
    fn settled_run(
        &self,
        reference: &HandleRef,
        record: &TaskRecord,
    ) -> Result<SettledRun, EvalHandleHostError> {
        match self.verdict {
            Verdict::RolledBack => Err(EvalHandleHostError::new(
                EvalHandleErrorCode::SendRefused,
                format!(
                    "{}: the run the message started was rolled back; fetch its current handle and send again",
                    reference.id
                ),
            )),
            Verdict::Moved => {
                let current = record.run_epoch;
                Ok(SettledRun::Answered(task_snapshot(
                    record,
                    HandleRef {
                        run_epoch: current,
                        ..reference.clone()
                    },
                    format!("continued as epoch {current}"),
                )))
            }
            Verdict::Live => Ok(SettledRun::At(HandleRef {
                run_epoch: self.expected_epoch,
                ..reference.clone()
            })),
        }
    }
}
