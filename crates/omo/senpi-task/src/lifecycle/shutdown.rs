//! Session-shutdown suspension (`lifecycle/shutdown.ts`).

use std::sync::Arc;

use serde_json::json;

use super::context::LifecycleContext;
use super::destroy::{best_effort, destroy_resident_task};
use super::errors::LifecycleError;
use super::port::{DestroyCause, ResidentHandle, ResidentKind};
use super::types::{SuspendFailure, SuspendInput, SuspendSummary};
use crate::state::{TaskRecord, TaskStatus, TaskTransition};
use crate::store::PersistedTaskEvent;

pub fn suspend_on_session_shutdown(
    context: &LifecycleContext,
    input: &SuspendInput,
) -> Result<SuspendSummary, LifecycleError> {
    if let Some(owner) = super::stopped_owner::stopped_owner_task(
        context,
        &input.parent_session_id,
        input.own_task_id.as_deref(),
    )? {
        return super::stopped_owner::stop_owned_children(context, input, &owner);
    }
    if !context.config.resume_children {
        return dispose_all_on_shutdown(context, input);
    }
    let mut summary = SuspendSummary::default();
    for handle in context.registry.entries() {
        let task_id = handle.task_id().to_string();
        let Some(record) = context.store.load(&task_id)? else {
            continue;
        };
        if !is_owned_child(&record, context, &input.parent_session_id) {
            continue;
        }
        if record.killed == Some(true)
            || matches!(record.status, TaskStatus::Cancelled | TaskStatus::Lost)
        {
            match destroy_resident_task(context, &task_id, DestroyCause::Cancel, None) {
                Ok(()) => summary.disposed += 1,
                Err(error) => summary.failures.push(failure(&task_id, &error)),
            }
            continue;
        }
        match suspend_handle(context, &handle, &input.reason) {
            Ok(()) if handle.kind() == ResidentKind::InProcess => summary.suspended_in_process += 1,
            Ok(()) => summary.suspended_rpc += 1,
            Err(error) => summary.failures.push(failure(&task_id, &error)),
        }
    }

    for record in context.store.list()?.records {
        if record.status != TaskStatus::Pending || record.killed == Some(true) {
            continue;
        }
        if !is_owned_child(&record, context, &input.parent_session_id)
            || context.registry.get(&record.task_id).is_some()
        {
            continue;
        }
        match suspend_pending(context, &record.task_id, &input.reason) {
            Ok(()) => summary.suspended_pending += 1,
            Err(error) => summary.failures.push(failure(&record.task_id, &error)),
        }
    }

    utils::logger::log(
        "senpi-task session shutdown suspend",
        Some(&json!({
            "parentSessionId": input.parent_session_id,
            "reason": input.reason,
            "suspended_in_process": summary.suspended_in_process,
            "suspended_rpc": summary.suspended_rpc,
            "suspended_pending": summary.suspended_pending,
            "disposed": summary.disposed,
            "failures": summary.failures.len(),
        })),
    );
    Ok(summary)
}

fn failure(task_id: &str, error: &LifecycleError) -> SuspendFailure {
    SuspendFailure {
        task_id: task_id.to_string(),
        error: error.to_string(),
    }
}

fn suspended_event(reason: &str) -> PersistedTaskEvent {
    PersistedTaskEvent {
        event_type: "suspended".to_string(),
        payload: json!({ "reason": reason }),
    }
}

fn suspend_pending(
    context: &LifecycleContext,
    task_id: &str,
    reason: &str,
) -> Result<(), LifecycleError> {
    (context.dequeue_pending)(task_id);
    context.store.transition(
        task_id,
        &TaskTransition::PersistOnly {
            timestamp: context.now_iso(),
        },
    )?;
    if reason == "quit" {
        super::host_session_record::mark_suspension_reason(
            context,
            task_id,
            crate::state::SuspensionReason::SessionQuit,
        )?;
    }
    context
        .store
        .append_event(task_id, &suspended_event(reason))?;
    Ok(())
}

pub(crate) fn suspend_handle(
    context: &LifecycleContext,
    handle: &Arc<dyn ResidentHandle>,
    reason: &str,
) -> Result<(), LifecycleError> {
    let hosted = handle.is_host_session()
        || context
            .store
            .load(handle.task_id())?
            .is_some_and(|r| super::host_session::is_host_session_record(&r));
    if hosted
        && let Some(record) = context.store.load(handle.task_id())?
            && record.status.is_terminal() {
                let publisher = handle.clone();
                let status = record.status;
                let (send, receive) = std::sync::mpsc::sync_channel(1);
                std::thread::spawn(move || {
                    let _ = send.send(publisher.publish_task_status(status));
                });
                let result = receive
                    .recv_timeout(std::time::Duration::from_millis(
                        context.teardown_step_deadline_ms,
                    ))
                    .unwrap_or_else(|error| {
                        Err(crate::host::HostError {
                            message: format!("publish_task_status: {error}"),
                        })
                    });
                best_effort(handle.task_id(), "publish_task_status", result, "suspend");
            }
    // Ownership drops BEFORE abort settles the turn so the outcome guard sees a forgotten child.
    context.registry.forget(handle.task_id());
    if !hosted {
        best_effort(handle.task_id(), "abort", handle.abort(), "suspend");
    }
    if handle.kind() == ResidentKind::Rpc && !hosted {
        best_effort(handle.task_id(), "terminate", handle.terminate(), "suspend");
    }
    handle.dispose()?;
    let timestamp = context.now_iso();
    let transition = if hosted {
        TaskTransition::DetachRpc { timestamp }
    } else {
        match handle.kind() {
            ResidentKind::InProcess => TaskTransition::PersistOnly { timestamp },
            ResidentKind::Rpc => TaskTransition::DetachRpc { timestamp },
        }
    };
    context.store.transition(handle.task_id(), &transition)?;
    if reason == "quit" {
        super::host_session_record::mark_suspension_reason(
            context,
            handle.task_id(),
            crate::state::SuspensionReason::SessionQuit,
        )?;
    }
    context
        .store
        .append_event(handle.task_id(), &suspended_event(reason))?;
    Ok(())
}

fn dispose_all_on_shutdown(
    context: &LifecycleContext,
    input: &SuspendInput,
) -> Result<SuspendSummary, LifecycleError> {
    let entries = context.registry.entries();
    let mut suspended_rpc = 0;
    let mut disposed = 0;
    for handle in &entries {
        if handle.is_host_session()
            || context
                .store
                .load(handle.task_id())?
                .is_some_and(|r| super::host_session::is_host_session_record(&r))
        {
            let Some(record) = context.store.load(handle.task_id())? else {
                continue;
            };
            if !is_owned_child(&record, context, &input.parent_session_id) {
                continue;
            }
            suspend_handle(context, handle, &input.reason)?;
            suspended_rpc += 1;
            continue;
        }
        destroy_resident_task(context, handle.task_id(), DestroyCause::Cancel, None)?;
        disposed += 1;
    }
    utils::logger::log(
        "senpi-task session shutdown dispose-all (resume_children=false)",
        Some(&json!({ "total": entries.len() })),
    );
    Ok(SuspendSummary {
        suspended_rpc,
        disposed,
        ..SuspendSummary::default()
    })
}

fn is_owned_child(
    record: &TaskRecord,
    context: &LifecycleContext,
    parent_session_id: &str,
) -> bool {
    record.parent_session_id == parent_session_id && record.host_pid == Some(context.host_pid)
}
