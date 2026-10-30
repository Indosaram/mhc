//! Port of `components/task/host-liveness-notice.ts` (omo v5.1.52): session availability is a status
//! notice, not a task completion and not permission to re-dispatch.
//!
//! OPEN (wiring): upstream's engine registers this through `createTaskManager({ onChildLiveness })`.
//! The native `TaskManagerOptions` (senpi-task lane) has no such hook yet, and `HostLivenessStatus` /
//! `hostLivenessText` live in senpi-task's `state/host-liveness.ts` port. Until that lane lands them,
//! this module owns a byte-identical copy of the two so the notice is complete and tested; swap the
//! local items for the senpi-task ones and call [`notify_host_liveness`] from the manager hook.

use std::sync::Arc;

use idle_injection::{IdleInjection, IdleInjectionCoordinator, IdleInjectionSource};
use maho_ext_api::{
    CustomMessage, DeliverAs, ExtensionActions, ExtensionFailure, SendMessageOptions, ToolContent,
};
use serde_json::{Value, json};

pub const TASK_STATUS_MESSAGE_TYPE: &str = "senpi-task.status";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostLivenessStatus {
    Running,
    Unresponsive { since: i64 },
    TerminalPending { since: i64 },
}

fn iso(millis: i64) -> String {
    chrono::DateTime::from_timestamp_millis(millis).map_or_else(String::new, |time| {
        time.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
    })
}

pub fn host_liveness_text(status: HostLivenessStatus) -> String {
    match status {
        HostLivenessStatus::Running => {
            "running: the original session is responding again".to_owned()
        }
        HostLivenessStatus::Unresponsive { since } => {
            format!(
                "unresponsive since {}, retrying; the session may still be running",
                iso(since)
            )
        }
        HostLivenessStatus::TerminalPending { since } => format!(
            "unresponsive since {}, close pending; retrying until the host confirms the session ended",
            iso(since)
        ),
    }
}

fn status_details(status: HostLivenessStatus) -> Value {
    match status {
        HostLivenessStatus::Running => json!({ "state": "running" }),
        HostLivenessStatus::Unresponsive { since } => {
            json!({ "state": "unresponsive", "since": since })
        }
        HostLivenessStatus::TerminalPending { since } => {
            json!({ "state": "terminal-pending", "since": since })
        }
    }
}

/// Queues (or, without a coordinator, steers) the status notice for one task.
pub fn notify_host_liveness(
    actions: &dyn ExtensionActions,
    coordinator: Option<&Arc<dyn IdleInjectionCoordinator>>,
    task_id: &str,
    run_epoch: i64,
    status: HostLivenessStatus,
    is_streaming: impl Fn() -> bool,
) -> Result<(), ExtensionFailure> {
    let content = format!(
        "Task {task_id}: {}. Keep the original task; do not re-dispatch it.",
        host_liveness_text(status)
    );
    let mut details = status_details(status);
    if let Some(object) = details.as_object_mut() {
        object.insert("task_id".into(), json!(task_id));
        object.insert("run_epoch".into(), json!(run_epoch));
    }
    let Some(coordinator) = coordinator else {
        return actions.send_message(
            CustomMessage {
                custom_type: TASK_STATUS_MESSAGE_TYPE.into(),
                content: vec![ToolContent::text(&content)],
                display: true,
                details: Some(details),
            },
            SendMessageOptions {
                trigger_turn: true,
                deliver_as: Some(DeliverAs::Steer),
            },
        );
    };
    coordinator.enqueue(IdleInjection {
        key: format!("task-status:{task_id}"),
        source: IdleInjectionSource::TaskStatus,
        custom_type: Some(TASK_STATUS_MESSAGE_TYPE.into()),
        content,
        display: Some(true),
        details: Some(details),
        passive: None,
        on_flushed: None,
        on_delivery_failed: None,
    });
    if is_streaming() {
        coordinator.schedule_flush();
    } else {
        coordinator.flush_soon();
    }
    Ok(())
}
