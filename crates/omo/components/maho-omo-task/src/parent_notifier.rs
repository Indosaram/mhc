use std::sync::Arc;
use maho_ext_api::{CustomMessage, DeliverAs, ExtensionActions, IdleInjection, IdleInjectionSource, SendMessageOptions, ToolContent};
use senpi_task::{completion::{DeliveryCallbacks, ParentNotifier, ParentNotifierMessage, build_completion_message}, host::HostError, pause::ports::{RunRef, RunRefKind}};

/// The senpi-task completion custom-message type; the component registers a renderer for it.
pub const TASK_COMPLETION_MESSAGE_TYPE: &str = "senpi-task.completion";

pub trait CompletionCoordinator: Send + Sync {
    /// Returns `false` when the coordinator is retired: the injection is NOT queued, so the caller
    /// still owns the notification and must fail it durably.
    fn enqueue(&self, key: &str, source: &str, message: &ParentNotifierMessage, callbacks: DeliveryCallbacks) -> Result<bool, HostError>;
    fn schedule_flush(&self);
    fn flush_soon(&self);
}

/// The run a completion belongs to: the engine's own reference, or the task it names.
pub fn completion_run_ref(detail: &senpi_task::completion::CompletionDetails) -> RunRef {
    detail.run_ref.clone().unwrap_or_else(|| RunRef::new(RunRefKind::Task, detail.task_id.clone()))
}

pub struct TaskParentNotifier {
    pub actions: Arc<dyn ExtensionActions>,
    pub coordinator: Option<Arc<dyn CompletionCoordinator>>,
    pub is_streaming: Arc<dyn Fn() -> bool + Send + Sync>,
    /// Per-completion delivery failure: the engine keeps `notified_epoch` unpersisted for exactly
    /// the tasks named here, so the post-reload reconcile can redeliver them.
    pub on_delivery_failed: Option<Arc<dyn Fn(&[String], String) + Send + Sync>>,
}

impl TaskParentNotifier {
    /// `createParentNotifier`: ONE individually keyed injection per completion. A batch of
    /// completions must never collapse into one key: two runs finishing together would then share a
    /// delivery receipt, and a failure of one would fail both.
    fn enqueue_one(&self, detail: &senpi_task::completion::CompletionDetails, message: &ParentNotifierMessage, callbacks: DeliveryCallbacks) -> Result<(), HostError> {
        let key = format!("task-completion:{}", detail.task_id);
        let single = build_completion_message(std::slice::from_ref(detail));
        if let Some(coordinator) = &self.coordinator {
            let accepted = coordinator.enqueue(&key, "task-completion", &single, callbacks)?;
            if !accepted { return Err(HostError { message: "the idle-injection coordinator is retired".into() }); }
            if (self.is_streaming)() { coordinator.schedule_flush(); } else { coordinator.flush_soon(); }
            return Ok(());
        }
        let _ = message;
        let details = serde_json::to_value(&single.details).map_err(|error| HostError { message: error.to_string() })?;
        let receipt = self.actions.send_message_with_receipt(CustomMessage { custom_type: single.custom_type.into(), content: vec![ToolContent::text(&single.content)], display: single.display, details: Some(details) }, SendMessageOptions { trigger_turn: true, deliver_as: Some(DeliverAs::Steer) }).map_err(|error| HostError { message: error.to_string() })?;
        receipt.on_settled(move |outcome| match outcome { Ok(()) => callbacks.delivered(), Err(message) => callbacks.failed(HostError { message }) });
        Ok(())
    }
}

impl ParentNotifier for TaskParentNotifier {
    fn enqueue_with_callbacks(&self, message: &ParentNotifierMessage, callbacks: DeliveryCallbacks) -> Result<(), HostError> {
        let last = message.details.len().saturating_sub(1);
        for (index, detail) in message.details.iter().enumerate() {
            let failure_hook = self.on_delivery_failed.clone();
            let task_id = detail.task_id.clone();
            // Only the final completion settles the caller's receipt; every one reports its own
            // failure, so a batch of two runs cannot make one run's failure fail the other.
            let settle = if index == last { Some(callbacks.clone()) } else { None };
            let receipt = DeliveryCallbacks::new(move |result: Result<(), HostError>| {
                if let Err(error) = &result && let Some(failed) = &failure_hook { failed(&[task_id.clone()], error.message.clone()); }
                match (result, settle) {
                    (Ok(()), Some(caller)) => caller.delivered(),
                    (Err(error), Some(caller)) => caller.failed(error),
                    _ => {}
                }
            });
            if let Err(error) = self.enqueue_one(detail, message, receipt) {
                if let Some(failed) = &self.on_delivery_failed { failed(std::slice::from_ref(&detail.task_id), error.message.clone()); }
                return Err(error);
            }
        }
        Ok(())
    }
}

/// The injection a single completion becomes when the coordinator owns delivery.
pub fn completion_injection(detail: &senpi_task::completion::CompletionDetails) -> IdleInjection {
    let single = build_completion_message(std::slice::from_ref(detail));
    IdleInjection { key: format!("task-completion:{}", detail.task_id), source: IdleInjectionSource::TaskCompletion, custom_type: Some(single.custom_type.into()), content: single.content, display: Some(single.display), details: serde_json::to_value(&single.details).ok(), passive: None, on_flushed: None, on_delivery_failed: None }
}
