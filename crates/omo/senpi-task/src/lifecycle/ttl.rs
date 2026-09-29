//! TTL expiry sweep (`lifecycle/ttl.ts`): two-phase tombstone then expunge.

use super::context::{LifecycleContext, is_terminal};
use super::destroy::destroy_resident_task;
use super::errors::LifecycleError;
use super::port::DestroyCause;
use super::types::CleanupResult;
use crate::state::{ResidencyState, TaskRecord, TaskStatus};
use crate::store::TombstoneResult;

pub fn cleanup_expired_records(
    context: &LifecycleContext,
) -> Result<CleanupResult, LifecycleError> {
    let mut result = CleanupResult::default();
    for task_id in context.store.list_expunging()? {
        context.store.complete_expunge(&task_id)?;
        result.deleted.push(task_id);
    }

    let cutoff = (context.now)() - context.config.ttl_ms;
    for record in context.store.list()?.records {
        if should_retain(context, &record, cutoff) {
            result.retained.push(record.task_id);
            continue;
        }
        let outcome = context
            .store
            .tombstone_if_expired(&record.task_id, &mut |fresh| {
                should_retain(context, fresh, cutoff)
            })?;
        let TombstoneResult::Tombstoned(tombstoned) = outcome else {
            result.retained.push(record.task_id);
            continue;
        };
        let orphan_pid = if tombstoned.execution_mode == "process" {
            tombstoned.pid
        } else {
            None
        };
        if let Some(pid) = orphan_pid
            && context.signaller.is_alive(pid)
        {
            destroy_resident_task(context, &record.task_id, DestroyCause::Ttl, Some(pid))?;
        }
        context.store.complete_expunge(&record.task_id)?;
        result.deleted.push(record.task_id);
    }
    Ok(result)
}

fn should_retain(context: &LifecycleContext, record: &TaskRecord, cutoff: i64) -> bool {
    if context.registry.get(&record.task_id).is_some() {
        return true;
    }
    if has_live_host_claim(context, record) || !is_terminal(record.status) {
        return true;
    }
    // An unparsable timestamp compares like NaN in TS: never "newer than the cutoff".
    if parse_iso_ms(&record.updated_at).is_some_and(|updated| updated > cutoff) {
        return true;
    }
    if has_undelivered_terminal_notification(record) {
        return true;
    }
    if record.status == TaskStatus::Lost && record.execution_mode == "process" {
        return record.pid.is_none_or(|pid| context.signaller.is_alive(pid));
    }
    false
}

fn has_live_host_claim(context: &LifecycleContext, record: &TaskRecord) -> bool {
    record.residency_state == ResidencyState::Resident
        && record
            .host_pid
            .is_some_and(|pid| context.signaller.is_alive(pid))
}

fn has_undelivered_terminal_notification(record: &TaskRecord) -> bool {
    let notification = &record.notification;
    if notification.notified_epoch >= notification.run_epoch {
        return false;
    }
    record.notify_on_terminal || notification.notification_failed_epoch.is_some()
}

pub(crate) fn parse_iso_ms(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|parsed| parsed.timestamp_millis())
}
