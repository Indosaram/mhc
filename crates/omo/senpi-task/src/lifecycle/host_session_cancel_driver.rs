//! Retry driver for no-handle host-session cancels (`lifecycle/host-session-cancel-driver.ts`, `#11765`).

use std::sync::Arc;

use serde_json::json;

use super::context::LifecycleContext;
use super::deferred_revival::retries_stopped;
use super::host_session::{HostSessionCancelSink, HostSessionEnd};
use super::host_session_end::end_host_session_child;
use crate::state::TaskRecord;

struct SettleGuard {
    sink: Arc<dyn HostSessionCancelSink>,
    settle_on_exit: bool,
}

impl Drop for SettleGuard {
    fn drop(&mut self) {
        if self.settle_on_exit {
            self.sink.settle();
        }
    }
}

/// Re-drives a no-handle host-session cancel that did not end on its first pass.
/// Every exit except an engine dispose settles `sink` once.
pub fn drive_host_session_cancel(
    context: LifecycleContext,
    record: TaskRecord,
    sink: Arc<dyn HostSessionCancelSink>,
) {
    let task_id = record.task_id.clone();
    let result = std::thread::Builder::new()
        .name(format!("host-cancel-{task_id}"))
        .spawn(move || run_driver(&context, &record, sink));
    if let Err(error) = result {
        utils::logger::log(
            "senpi-task host session cancel driver spawn failed",
            Some(&json!({ "taskId": task_id, "error": error.to_string() })),
        );
    }
}

fn run_driver(
    context: &LifecycleContext,
    record: &TaskRecord,
    sink: Arc<dyn HostSessionCancelSink>,
) {
    let mut guard = SettleGuard {
        sink: Arc::clone(&sink),
        settle_on_exit: true,
    };
    let mut end = end_host_session_child(context, record);
    for &backoff_ms in &context.host_retry.deferred_retry_backoff_ms {
        if matches!(end, HostSessionEnd::Ended { .. }) {
            sink.ended();
            return;
        }
        (context.host_retry_wait)(backoff_ms);
        if context.scoped_retries.is_disposed() {
            guard.settle_on_exit = false;
            return;
        }
        if retries_stopped(context, &record.parent_session_id) || !still_ours(context, record) {
            return;
        }
        end = end_host_session_child(context, record);
    }
    if matches!(end, HostSessionEnd::Ended { .. }) {
        sink.ended();
    }
}

fn still_ours(context: &LifecycleContext, record: &TaskRecord) -> bool {
    let Ok(Some(fresh)) = context.store.load(&record.task_id) else {
        return false;
    };
    if fresh.status.is_terminal() || fresh.killed == Some(true) {
        return false;
    }
    fresh.notification.run_epoch == record.notification.run_epoch
        && fresh.residency_claim == record.residency_claim
}
