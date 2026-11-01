pub type OnDeliveryFailedCallback = Option<Arc<dyn Fn(&[String], String) + Send + Sync>>;
use std::sync::{Arc, Mutex, PoisonError};
use idle_injection::{IdleInjection, IdleInjectionSource};
use maho_ext_api::{CustomMessage, DeliverAs, ExtensionActions, SendMessageOptions, ToolContent};
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

struct BatchSettlement {
    remaining: usize,
    last_error: Option<HostError>,
}

pub struct TaskParentNotifier {
    pub actions: Arc<dyn ExtensionActions>,
    pub coordinator: Option<Arc<dyn CompletionCoordinator>>,
    pub is_streaming: Arc<dyn Fn() -> bool + Send + Sync>,
    /// Per-completion delivery failure: the engine keeps `notified_epoch` unpersisted for exactly
    /// the tasks named here, so the post-reload reconcile can redeliver them.
    pub on_delivery_failed: OnDeliveryFailedCallback,
}

impl ParentNotifier for TaskParentNotifier {
    fn enqueue_with_callbacks(&self, message: &ParentNotifierMessage, callbacks: DeliveryCallbacks) -> Result<(), HostError> {
        if let Some(coordinator) = &self.coordinator {
            if message.details.is_empty() {
                callbacks.delivered();
                return Ok(());
            }
            let total = message.details.len();
            let batch_state = Arc::new(Mutex::new(BatchSettlement {
                remaining: total,
                last_error: None,
            }));
            let mut accepted_count = 0;

            for detail in &message.details {
                let failure_hook = self.on_delivery_failed.clone();
                let task_id = detail.task_id.clone();
                let caller = callbacks.clone();
                let batch = Arc::clone(&batch_state);
                let receipt = DeliveryCallbacks::new(move |result: Result<(), HostError>| {
                    if let Err(error) = &result && let Some(failed) = &failure_hook {
                        failed(std::slice::from_ref(&task_id), error.message.clone());
                    }
                    let to_settle = {
                        let mut state = batch.lock().unwrap_or_else(PoisonError::into_inner);
                        if let Err(error) = &result {
                            state.last_error = Some(error.clone());
                        }
                        state.remaining -= 1;
                        if state.remaining == 0 {
                            Some(state.last_error.take())
                        } else {
                            None
                        }
                    };
                    if let Some(last_error) = to_settle {
                        match last_error {
                            Some(error) => caller.failed(error),
                            None => caller.delivered(),
                        }
                    }
                });
                let single = build_completion_message(std::slice::from_ref(detail));
                let enqueue_result = coordinator.enqueue(
                    &format!("task-completion:{}", detail.task_id),
                    "task-completion",
                    &single,
                    receipt.clone(),
                );
                let error = match enqueue_result {
                    Ok(true) => {
                        accepted_count += 1;
                        None
                    }
                    Ok(false) => Some(HostError {
                        message: "the idle-injection coordinator is retired".into(),
                    }),
                    Err(err) => Some(err),
                };
                if let Some(error) = error {
                    if accepted_count == 0 {
                        if let Some(failed) = &self.on_delivery_failed {
                            failed(std::slice::from_ref(&detail.task_id), error.message.clone());
                        }
                        return Err(error);
                    }
                    receipt.failed(error);
                }
            }

            // Mid-turn: collect in the batch window. Idle: flush on the next microtask so same-tick
            // completions batch but delivery is immediate.
            if (self.is_streaming)() { coordinator.schedule_flush(); } else { coordinator.flush_soon(); }
            return Ok(());
        }
        let details = serde_json::to_value(&message.details).map_err(|error| HostError { message: error.to_string() })?;
        let receipt = self.actions.send_message_with_receipt(CustomMessage { custom_type: message.custom_type.into(), content: vec![ToolContent::text(&message.content)], display: message.display, details: Some(details) }, SendMessageOptions { trigger_turn: true, deliver_as: Some(DeliverAs::Steer) }).map_err(|error| HostError { message: error.to_string() })?;
        receipt.on_settled(move |outcome| match outcome {
            Ok(()) => callbacks.delivered(),
            Err(message) => callbacks.failed(HostError { message }),
        });
        Ok(())
    }
}

/// The injection a single completion becomes when the coordinator owns delivery.
pub fn completion_injection(detail: &senpi_task::completion::CompletionDetails) -> IdleInjection {
    let single = build_completion_message(std::slice::from_ref(detail));
    IdleInjection { key: format!("task-completion:{}", detail.task_id), source: IdleInjectionSource::TaskCompletion, custom_type: Some(single.custom_type.into()), content: single.content, display: Some(single.display), details: serde_json::to_value(&single.details).ok(), passive: None, on_flushed: None, on_delivery_failed: None }
}
