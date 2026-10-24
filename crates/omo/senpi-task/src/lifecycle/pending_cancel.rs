//! Port of `lifecycle/pending-cancel.ts`.
//!
//! A task_cancel accepted while the child was unreachable outlives the process that accepted it
//! (omo#9403): its parent shut down, or its host shard crashed, before the stop landed. Whichever
//! revival reaches the record next - session-start reconcile, daemon-loss retry, a send - finishes the
//! cancel instead: the session is ended on its host, the record is cancelled, and nothing runs.
//!
//! The session is ended FIRST. A close the host refuses or does not confirm in time returns `None`
//! and leaves the cancel pending on the record, so the next revival retries it: a cancelled record whose
//! session still runs would let the child keep working until the TTL sweep caught it. A host that answers
//! but cannot list its sessions leaves the cancel pending the same way (omo#9450).

use serde_json::{Map, Value, json};

use super::context::LifecycleContext;
use super::destroy::{destroy_resident_task, spawn_spec_cwd};
use super::errors::LifecycleError;
use super::host_session::{HostSessionLiveness, is_host_session_record};
use super::host_session_close::close_host_session_confirmed;
use super::port::DestroyCause;
use super::types::{ReconcileOutcome, ReconcileOutcomeKind};
use crate::state::{TaskRecord, TaskTransition};
use crate::store::PersistedTaskEvent;

pub fn finish_pending_cancel(
    context: &LifecycleContext,
    record: &TaskRecord,
) -> Result<Option<ReconcileOutcome>, LifecycleError> {
    if is_host_session_record(record)
        && context.registry.get(&record.task_id).is_none()
        && let Some(host_session) = record.host_session.as_ref()
    {
        context
            .host_session_probe
            .refresh(Some(&host_session.socket));
        match context.host_session_probe.session_liveness(host_session) {
            HostSessionLiveness::Unknown => return Ok(None),
            HostSessionLiveness::Live => {
                let cwd = spawn_spec_cwd(record);
                if !close_host_session_confirmed(
                    context,
                    &record.task_id,
                    host_session,
                    cwd.as_deref(),
                ) {
                    return Ok(None);
                }
            }
            HostSessionLiveness::Gone => {}
        }
    }
    let reason = record
        .cancel_requested
        .as_ref()
        .and_then(|request| request.reason.clone());
    let result = context.store.transition(
        &record.task_id,
        &TaskTransition::Cancel {
            timestamp: context.now_iso(),
            error_message: reason.clone(),
            run_stats: None,
        },
    )?;
    if result.applied {
        let mut payload = Map::new();
        payload.insert("previous_status".to_string(), json!(record.status.as_str()));
        payload.insert("finished_on".to_string(), json!("revival"));
        if let Some(reason) = reason {
            payload.insert("reason".to_string(), Value::String(reason));
        }
        context.store.append_event(
            &record.task_id,
            &PersistedTaskEvent {
                event_type: "cancelled".to_string(),
                payload: Value::Object(payload),
            },
        )?;
    }
    destroy_resident_task(context, &record.task_id, DestroyCause::Cancel, None)?;
    Ok(Some(ReconcileOutcome::new(
        &record.task_id,
        ReconcileOutcomeKind::Resumed,
        Some("pending cancel finished"),
    )))
}
