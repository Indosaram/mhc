//! Port of `components/task/host-liveness-notice.ts` (omo v5.1.52): session availability is a status
//! notice, not a task completion and not permission to re-dispatch.

use std::sync::{Arc, Mutex, PoisonError};

use idle_injection::{IdleInjection, IdleInjectionCoordinator, IdleInjectionSource};
use maho_ext_api::{
    CustomMessage, DeliverAs, ExtensionActions, ExtensionFailure, SendMessageOptions, ToolContent,
};
pub use senpi_task::state::host_liveness::{HostLivenessStatus, host_liveness_text};
use senpi_task::{
    completion::ParentState, manager::types::ChildLivenessCallback, state::TaskRecord,
};
use serde_json::json;

use crate::runtime_context::TaskRuntimeContext;

pub const TASK_STATUS_MESSAGE_TYPE: &str = "senpi-task.status";

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
    let mut details = serde_json::to_value(status).unwrap_or_else(|_| json!({}));
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
    let failed_task = task_id.to_owned();
    let queued = coordinator.enqueue(IdleInjection {
        key: format!("task-status:{task_id}"),
        source: IdleInjectionSource::TaskStatus,
        custom_type: Some(TASK_STATUS_MESSAGE_TYPE.into()),
        content,
        display: Some(true),
        details: Some(details),
        passive: None,
        on_flushed: None,
        // The notice has no retry owner after enqueue: a coordinator retired while it is queued
        // reports through this callback instead of dropping the notice silently.
        on_delivery_failed: Some(Arc::new(move |error| {
            maho_ext_api::diagnostic(&format!(
                "task host liveness notice delivery failed: taskId={failed_task} error={error}"
            ));
        })),
    });
    if !queued {
        // A refused enqueue means the notice was never queued, so scheduling a flush of a queue it
        // never entered would be a lie. Report the refusal here and let the caller - the single
        // reporting site, which already diagnoses a returned `Err` - emit the diagnostic.
        return Err(ExtensionFailure::new(format!(
            "host liveness notice refused by the idle coordinator: taskId={task_id}"
        )));
    }
    if is_streaming() {
        coordinator.schedule_flush();
    } else {
        coordinator.flush_soon();
    }
    Ok(())
}

/// `engine.ts` `onChildLiveness`: only this session's own children notify it, and the flush follows
/// the parent's turn state.
pub fn child_liveness_notice(
    actions: Arc<dyn ExtensionActions>,
    coordinator: Option<Arc<dyn IdleInjectionCoordinator>>,
    runtime: Arc<Mutex<TaskRuntimeContext>>,
) -> ChildLivenessCallback {
    Arc::new(move |record: &TaskRecord, status| {
        let parent = runtime.lock().unwrap_or_else(PoisonError::into_inner);
        if parent.session_id() != Some(record.parent_session_id.as_str()) {
            return;
        }
        let streaming = parent.parent_state() == ParentState::Streaming;
        drop(parent);
        if let Err(error) = notify_host_liveness(
            actions.as_ref(),
            coordinator.as_ref(),
            &record.task_id,
            record.notification.run_epoch,
            status,
            || streaming,
        ) {
            maho_ext_api::diagnostic(&format!(
                "task host liveness notice failed: taskId={} error={error}",
                record.task_id
            ));
        }
    })
}
