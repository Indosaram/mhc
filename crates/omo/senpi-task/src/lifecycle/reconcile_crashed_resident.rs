//! Crash marking for a claimed resident (`lifecycle/reconcile-crashed-resident.ts`).

use serde_json::json;

use super::context::{LifecycleContext, is_terminal};
use crate::state::{
    LostReason, ResidencyState, TaskRecord, TaskStatus, mark_record_lost_for_reconciliation,
};
use crate::store::{PersistedTaskEvent, StoreError};

/// Marks a resident this process claimed as crashed: terminal results keep their status and are
/// flagged `killed`; live statuses become `lost`. Returns whether anything was written.
pub fn mark_crashed_resident(
    context: &LifecycleContext,
    record: &TaskRecord,
    message: &str,
) -> Result<bool, StoreError> {
    let mut applied = false;
    let timestamp = context.now_iso();
    context.store.mutate(&record.task_id, &mut |fresh| {
        if fresh.host_pid != Some(context.host_pid)
            || fresh.residency_state != ResidencyState::Resident
        {
            return fresh.clone();
        }
        if is_terminal(fresh.status) && fresh.status != TaskStatus::Lost {
            if fresh.killed == Some(true) && fresh.error_message.as_deref() == Some(message) {
                return fresh.clone();
            }
            applied = true;
            let mut next = fresh.clone();
            next.killed = Some(true);
            next.error_message = Some(message.to_string());
            next.updated_at.clone_from(&timestamp);
            return next;
        }
        let result =
            mark_record_lost_for_reconciliation(fresh, &timestamp, message, lost_reason(fresh));
        if !result.applied {
            return fresh.clone();
        }
        applied = true;
        result.record
    })?;
    if !applied {
        return Ok(false);
    }
    context
        .store
        .append_event(&record.task_id, &reconcile_lost_event(message))?;
    Ok(true)
}

pub(crate) fn lost_reason(record: &TaskRecord) -> LostReason {
    if record.status == TaskStatus::Lost {
        LostReason::Update
    } else {
        LostReason::KeepExisting
    }
}

pub(crate) fn reconcile_lost_event(message: &str) -> PersistedTaskEvent {
    PersistedTaskEvent {
        event_type: "reconcile_lost".to_string(),
        payload: json!({ "reason": message }),
    }
}
