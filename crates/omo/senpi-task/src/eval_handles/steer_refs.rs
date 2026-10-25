//! Send and cancel replies for eval handles (`eval-handles/steer-refs.ts`).
//!
//! Only the parts of `sendRef` that decide the reply are ported here: the steering host calls
//! (`TaskControl::sendToTask`) and the pre-engine fence live in the manager. Each branch answers
//! from the run the engine acted on, so no reply can describe a run that moved or was undone.

use super::errors::{EvalHandleErrorCode, EvalHandleHostError};
use super::host::HandleRef;
use super::host::HandleSnapshot;
use super::run_after_engine::RunAfterEngine;
use super::task_refs::{TaskRecord, load_fenced};

pub const CANCEL_REASON: &str = "cancelled from an eval handle";

/// The host status a held send reports: the run is stopped and the message waits for Resume.
pub const HELD_UNTIL_RESUME: &str = "held until the run resumes";

pub fn agent_ref(reference: &HandleRef) -> Result<(), EvalHandleHostError> {
    if matches!(reference.kind, super::host::HandleKind::Agent) {
        Ok(())
    } else {
        Err(EvalHandleHostError::new(
            EvalHandleErrorCode::Unsupported,
            "operation is for agent handles",
        ))
    }
}

pub fn send_allowed(
    tasks: &[TaskRecord],
    reference: &HandleRef,
    owner: &str,
) -> Result<(), EvalHandleHostError> {
    agent_ref(reference)?;
    load_fenced(tasks, reference, owner).map(|_| ())
}

/// The reply for a send the host accepted but the run holds until it resumes: a delivery with the
/// held status, never a plain `steered`.
pub fn held(
    run: &RunAfterEngine,
    reference: &HandleRef,
    record: &TaskRecord,
) -> Result<HandleSnapshot, EvalHandleHostError> {
    run.delivered(reference, record, Some(HELD_UNTIL_RESUME))
}

/// The reply for a message only queued locally: the queued snapshot, not delivery semantics.
pub fn queued(
    run: &RunAfterEngine,
    reference: &HandleRef,
    record: &TaskRecord,
) -> Result<HandleSnapshot, EvalHandleHostError> {
    run.queued(reference, record)
}
