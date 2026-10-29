use std::sync::Arc;

use idle_injection::{IdleInjection, IdleInjectionCoordinator, IdleInjectionSource};
use maho_omo_task::{lead_poller_lifecycle::LeadInjectionCoordinator, parent_notifier::CompletionCoordinator};
use senpi_task::{completion::{DeliveryCallbacks, ParentNotifierMessage}, host::HostError, team::messaging::lead_poller_types::LeadInjection};

#[derive(Clone)]
pub struct TaskCoordinator(pub Arc<dyn IdleInjectionCoordinator>);

impl TaskCoordinator {
    pub fn enqueue_liveness(&self, key: &str, message: maho_ext_api::CustomMessage, callbacks: DeliveryCallbacks) -> Result<(), HostError> {
        let content = message.content.into_iter().map(|part| match part {
            maho_ext_api::ToolContent::Text { text, .. } => Ok(text),
            maho_ext_api::ToolContent::Image { .. } => Err(HostError { message: "team liveness must contain text".into() }),
        }).collect::<Result<Vec<_>, _>>()?.join("\n");
        let failed = callbacks.clone();
        let queued = self.0.enqueue(IdleInjection {
            key: key.into(), source: IdleInjectionSource::TeamLiveness,
            custom_type: Some(message.custom_type), content,
            display: Some(message.display), details: message.details,
            passive: Some(false),
            on_flushed: Some(Arc::new(move || callbacks.delivered())),
            on_delivery_failed: Some(Arc::new(move |error| failed.failed(HostError { message: error.into() }))),
        });
        if !queued {
            return Err(HostError { message: crate::coordinator::RETIRED_ERROR_MESSAGE.to_owned() });
        }
        Ok(())
    }
}

impl CompletionCoordinator for TaskCoordinator {
    fn enqueue(&self, key: &str, source: &str, message: &ParentNotifierMessage, callbacks: DeliveryCallbacks) -> Result<bool, HostError> {
        if source != "task-completion" {
            return Err(HostError { message: format!("invalid task completion source: {source}") });
        }
        let details = serde_json::to_value(&message.details).map_err(|error| HostError { message: error.to_string() })?;
        let failed = callbacks.clone();
        let queued = self.0.enqueue(IdleInjection {
            key: key.into(), source: IdleInjectionSource::TaskCompletion,
            custom_type: Some(message.custom_type.into()), content: message.content.clone(),
            display: Some(message.display), details: Some(details),
            passive: Some(false),
            on_flushed: Some(Arc::new(move || callbacks.delivered())),
            on_delivery_failed: Some(Arc::new(move |error| failed.failed(HostError { message: error.into() }))),
        });
        // A refused enqueue is reported, not thrown: the caller still owns the notification and
        // fails it durably so the post-reload reconcile can redeliver it.
        Ok(queued)
    }
    fn schedule_flush(&self) { self.0.schedule_flush(); }
    fn flush_soon(&self) { self.0.flush_soon(); }
}

impl LeadInjectionCoordinator for TaskCoordinator {
    fn enqueue(&self, injection: LeadInjection, custom_type: &str, display: bool) {
        let LeadInjection { key, source: _, content, run_ref, on_flushed, on_delivery_failed } = injection;
        let callbacks = DeliveryCallbacks::new(move |result| match result {
            Ok(()) => { if let Some(callback) = on_flushed { callback(); } }
            Err(error) => { if let Some(callback) = on_delivery_failed { callback(&error.message); } }
        });
        let failed = callbacks.clone();
        let refused = callbacks.clone();
        let queued = self.0.enqueue(IdleInjection {
            key, source: IdleInjectionSource::TeamMessage,
            custom_type: Some(custom_type.into()), content,
            display: Some(display),
            // The run reference rides the injection's details so a receiver can place the delivery.
            details: run_ref.map(|run| serde_json::json!({"runRef": run})),
            passive: Some(false),
            on_flushed: Some(Arc::new(move || callbacks.delivered())),
            on_delivery_failed: Some(Arc::new(move |error| failed.failed(HostError { message: error.into() }))),
        });
        if !queued {
            refused.failed(HostError { message: crate::coordinator::RETIRED_ERROR_MESSAGE.to_owned() });
        }
    }
    fn schedule_flush(&self) { self.0.schedule_flush(); }
    fn flush_soon(&self) { self.0.flush_soon(); }
}
