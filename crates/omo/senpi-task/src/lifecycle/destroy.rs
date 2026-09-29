//! The single-writer destruction port (`lifecycle/destroy.ts`).

use std::sync::Arc;

use serde_json::json;

use super::context::LifecycleContext;
use super::errors::LifecycleError;
use super::port::{DestroyCause, OrphanSignal, ResidentHandle, ResidentKind};
use crate::host::HostError;
use crate::state::TaskTransition;
use crate::store::PersistedTaskEvent;

pub fn destroy_resident_task(
    context: &LifecycleContext,
    task_id: &str,
    cause: DestroyCause,
    orphan_pid: Option<i64>,
) -> Result<(), LifecycleError> {
    if let Some(handle) = context.registry.get(task_id) {
        teardown_handle(&handle, cause == DestroyCause::CancelWithoutAbort)?;
        if cause != DestroyCause::FallbackHandoff {
            context.registry.forget(task_id);
        }
    } else if matches!(cause, DestroyCause::ReconcileLost | DestroyCause::Ttl) {
        terminate_orphan(context, task_id, orphan_pid)?;
    }
    if cause != DestroyCause::FallbackHandoff {
        record_residency(context, task_id, cause)?;
    }
    Ok(())
}

fn teardown_handle(
    handle: &Arc<dyn ResidentHandle>,
    skip_in_process_abort: bool,
) -> Result<(), HostError> {
    match handle.kind() {
        ResidentKind::InProcess => {
            if !skip_in_process_abort {
                best_effort(handle.task_id(), "abort", handle.abort(), "teardown");
            }
        }
        ResidentKind::Rpc => {
            best_effort(
                handle.task_id(),
                "terminate",
                handle.terminate(),
                "teardown",
            );
        }
    }
    handle.dispose()
}

/// Logs a rejected pre-dispose step; dispose must still run.
pub(crate) fn best_effort(task_id: &str, step: &str, result: Result<(), HostError>, phase: &str) {
    if let Err(error) = result {
        utils::logger::log(
            &format!("senpi-task {phase} pre-dispose step rejected"),
            Some(&json!({ "taskId": task_id, "step": step, "error": error.to_string() })),
        );
    }
}

/// SIGTERM, wait the orphan-kill delay, then SIGKILL if still alive. Returns whether it died.
pub(crate) fn term_then_kill(
    context: &LifecycleContext,
    task_id: &str,
    pid: i64,
) -> Result<bool, LifecycleError> {
    if !context.signaller.is_alive(pid) {
        return Ok(true);
    }
    signal_and_record(context, task_id, pid, OrphanSignal::Term)?;
    context.delay_orphan_kill();
    if context.signaller.is_alive(pid) {
        signal_and_record(context, task_id, pid, OrphanSignal::Kill)?;
    }
    Ok(!context.signaller.is_alive(pid))
}

fn signal_and_record(
    context: &LifecycleContext,
    task_id: &str,
    pid: i64,
    signal: OrphanSignal,
) -> Result<(), LifecycleError> {
    context.signaller.signal(pid, signal);
    context.store.append_event(
        task_id,
        &PersistedTaskEvent {
            event_type: "reconcile_terminated".to_string(),
            payload: json!({ "pid": pid, "signal": signal.as_str() }),
        },
    )?;
    Ok(())
}

fn terminate_orphan(
    context: &LifecycleContext,
    task_id: &str,
    orphan_pid: Option<i64>,
) -> Result<(), LifecycleError> {
    let pid = match context.store.load(task_id)? {
        None => orphan_pid,
        Some(record) if record.execution_mode == "process" => record.pid,
        Some(_) => None,
    };
    if let Some(pid) = pid {
        term_then_kill(context, task_id, pid)?;
    }
    Ok(())
}

fn record_residency(
    context: &LifecycleContext,
    task_id: &str,
    cause: DestroyCause,
) -> Result<(), LifecycleError> {
    if context.store.load(task_id)?.is_none() {
        return Ok(());
    }
    let timestamp = context.now_iso();
    let (transition, event_type) = if cause == DestroyCause::Evict {
        (TaskTransition::Evict { timestamp }, "evicted")
    } else {
        (TaskTransition::Dispose { timestamp }, "destroyed")
    };
    context.store.transition(task_id, &transition)?;
    context.store.append_event(
        task_id,
        &PersistedTaskEvent {
            event_type: event_type.to_string(),
            payload: json!({ "cause": cause.as_str() }),
        },
    )?;
    Ok(())
}
