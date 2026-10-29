//! Cancellation cascade for a deliberately stopped session owner.
use super::{
    DestroyCause, LifecycleContext, LifecycleError, SuspendFailure, SuspendInput, SuspendSummary,
};
use crate::state::{TaskRecord, TaskStatus, TaskTransition};
use crate::store::PersistedTaskEvent;
use crate::tools::control::cancel::with_dropped_steering_notice;
use serde_json::json;

pub fn stopped_owner_task(
    context: &LifecycleContext,
    session_id: &str,
    own_task_id: Option<&str>,
) -> Result<Option<TaskRecord>, LifecycleError> {
    Ok(context.store.list()?.records.into_iter().find(|record| {
        (record.child_session_id.as_deref() == Some(session_id)
            || own_task_id == Some(record.task_id.as_str()))
            && (record.killed == Some(true)
                || matches!(record.status, TaskStatus::Cancelled | TaskStatus::Lost)
                || record.cancel_requested.is_some())
    }))
}

pub fn stop_owned_children(
    context: &LifecycleContext,
    input: &SuspendInput,
    owner: &TaskRecord,
) -> Result<SuspendSummary, LifecycleError> {
    let mut summary = SuspendSummary {
        stopped_owner: Some(owner.task_id.clone()),
        ..Default::default()
    };
    let reason = format!(
        "parent task {} was {}",
        owner.task_id,
        if owner.killed == Some(true) {
            "killed"
        } else if owner.status == TaskStatus::Running {
            "cancelled"
        } else {
            owner.status.as_str()
        }
    );
    for record in context.store.list()?.records {
        if record.parent_session_id != input.parent_session_id
            || record.host_pid != Some(context.host_pid)
            || (context.registry.get(&record.task_id).is_none()
                && record.status != TaskStatus::Pending)
        {
            continue;
        }
        let result = (|| -> Result<(), LifecycleError> {
            if record.status == TaskStatus::Pending {
                (context.dequeue_pending)(&record.task_id);
            }
            if matches!(record.status, TaskStatus::Pending | TaskStatus::Running) {
                cancel_child(context, &record.task_id, &reason)?;
            }
            if let Some(handle) = context.registry.get(&record.task_id) {
                handle.mark_stopping();
            }
            super::destroy::destroy_resident_task(
                context,
                &record.task_id,
                DestroyCause::Cancel,
                None,
            )
        })();
        match result {
            Ok(()) => summary.disposed += 1,
            Err(error) => summary.failures.push(SuspendFailure {
                task_id: record.task_id,
                error: error.to_string(),
            }),
        }
    }
    Ok(summary)
}

fn cancel_child(
    context: &LifecycleContext,
    task_id: &str,
    reason: &str,
) -> Result<(), LifecycleError> {
    let Some(before) = context.store.load(task_id)? else {
        return Ok(());
    };
    let result = context.store.transition(
        task_id,
        &TaskTransition::Cancel {
            timestamp: context.now_iso(),
            error_message: Some(reason.to_string()),
            run_stats: None,
        },
    )?;
    if !result.applied {
        return Ok(());
    }
    let mut dropped = 0;
    context.store.mutate(task_id, &mut |fresh| {
        let mut next = fresh.clone();
        dropped = next.pending_steering.as_ref().map_or(0, Vec::len);
        if dropped > 0 {
            next.pending_steering = None;
            next.error_message = Some(with_dropped_steering_notice(reason, dropped));
        }
        next
    })?;
    if dropped > 0 {
        context.store.append_event(
            task_id,
            &PersistedTaskEvent {
                event_type: "steer_dropped".into(),
                payload: json!({"count": dropped, "reason": "cancelled"}),
            },
        )?;
    }
    context.store.append_event(
        task_id,
        &PersistedTaskEvent {
            event_type: "cancelled".into(),
            payload: json!({"previous_status": before.status, "reason": reason}),
        },
    )?;
    Ok(())
}
