//! Live-parent suspended-run expiry. Queue counts are sampled by the clearing mutation.
use super::{DestroyCause, LifecycleContext, LifecycleError};
use crate::state::{TaskRecord, TaskStatus, TaskTransition};
use crate::store::PersistedTaskEvent;
use crate::tools::control::cancel::with_dropped_steering_notice;
use serde_json::json;

pub fn is_suspension_expiry(record: &TaskRecord) -> bool {
    record.killed == Some(true)
        && (record.failure_kind.as_deref() == Some("suspended_unresumable")
            || (record.failure_kind.as_deref() == Some("session_unavailable")
                && record
                    .error_message
                    .as_deref()
                    .is_some_and(|s| s.starts_with("suspended_unresumable:"))))
}

pub fn expire_suspended_child(
    context: &LifecycleContext,
    observed: &TaskRecord,
    parent_live: &dyn Fn() -> bool,
) -> Result<(), LifecycleError> {
    let token = uuid::Uuid::new_v4().to_string();
    let cause = observed
        .revival_deferred_reason
        .clone()
        .or_else(|| {
            observed.suspension_reason.map(|r| {
                serde_json::to_value(r)
                    .expect("reason")
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
        })
        .unwrap_or_else(|| "parent session restarted".into());
    let reason = format!("suspended_unresumable:{cause}");
    let mut claimed = false;
    let record = context.store.mutate(&observed.task_id, &mut |fresh| {
        if !parent_live()
            || !matches!(fresh.status, TaskStatus::Pending | TaskStatus::Running)
            || fresh.notification.run_epoch != observed.notification.run_epoch
            || fresh.residency_claim != observed.residency_claim
            || fresh.cancel_requested.is_some()
            || (fresh.killed == Some(true) && !is_suspension_expiry(fresh))
            || (fresh.residency_state == crate::state::ResidencyState::Resident
                && fresh.suspension_reason.is_none()
                && fresh.recovery_deadline_at.is_none()
                && !is_suspension_expiry(fresh))
        {
            return fresh.clone();
        }
        claimed = true;
        let mut next = fresh.clone();
        next.killed = Some(true);
        next.failure_kind = Some("suspended_unresumable".into());
        next.error_message = Some(reason.clone());
        next.host_pid = Some(context.host_pid);
        next.residency_claim = Some(token.clone());
        next
    })?;
    let Some(record) =
        record.filter(|r| claimed && parent_live() && r.residency_claim.as_deref() == Some(&token))
    else {
        return Ok(());
    };
    context.store.append_event(
        &record.task_id,
        &PersistedTaskEvent {
            event_type: "live_parent_expiry_claimed".into(),
            payload: json!({"cause": cause}),
        },
    )?;
    if record.execution_mode == "process" && !super::host_session::is_host_session_record(&record)
        && let Some(pid) = record.pid
            && !super::destroy::term_then_kill(context, &record.task_id, pid)? {
                return Ok(());
            }
    if let Some(host) = record.host_session.as_ref() {
        let cwd = super::destroy::spawn_spec_cwd(&record);
        if super::host_session_close::close_host_session(
            context,
            &record.task_id,
            host,
            cwd.as_deref(),
        ) != super::host_session_close::HostSessionCloseOutcome::Closed
        {
            return Ok(());
        }
    }
    super::destroy::destroy_resident_task(
        context,
        &record.task_id,
        DestroyCause::CancelWithoutAbort,
        None,
    )?;
    let mut undelivered = 0;
    let mut owned = false;
    context.store.mutate(&record.task_id, &mut |fresh| {
        if fresh.residency_claim.as_deref() != Some(&token)
            || fresh.notification.run_epoch != record.notification.run_epoch
            || !matches!(fresh.status, TaskStatus::Pending | TaskStatus::Running)
        {
            return fresh.clone();
        }
        owned = true;
        let mut next = fresh.clone();
        undelivered = next.pending_steering.take().map_or(0, |q| q.len());
        next
    })?;
    if !owned {
        return Ok(());
    }
    let message = with_dropped_steering_notice(&reason, undelivered);
    if undelivered > 0 {
        context.store.append_event(
            &record.task_id,
            &PersistedTaskEvent {
                event_type: "steer_dropped".into(),
                payload: json!({"count": undelivered, "reason": "target_gone"}),
            },
        )?;
    }
    (context.dequeue_pending)(&record.task_id);
    context.store.transition(
        &record.task_id,
        &TaskTransition::Dispose {
            timestamp: context.now_iso(),
        },
    )?;
    let result = context.store.transition(
        &record.task_id,
        &TaskTransition::Fail {
            timestamp: context.now_iso(),
            error_message: message,
            killed: true,
            run_stats: None,
        },
    )?;
    context.registry.forget(&record.task_id);
    if result.applied {
        context.store.append_event(&record.task_id, &PersistedTaskEvent { event_type: "suspended_unresumable".into(), payload: json!({"cause": cause, "confirmed_stop": true, "undelivered_messages": undelivered}) })?;
    }
    Ok(())
}
