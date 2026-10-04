use std::sync::{Arc, Mutex, PoisonError};

use maho_ext_api::{IdleInjection, IdleInjectionCoordinator, IdleInjectionSource};
use maho_omo_task::{lead_poller_lifecycle::LeadInjectionCoordinator, parent_notifier::CompletionCoordinator};
use senpi_task::{completion::ParentNotifierMessage, host::HostError, team::messaging::lead_poller_types::LeadInjection};

#[derive(Clone)]
pub struct TaskCoordinator(pub Arc<dyn IdleInjectionCoordinator>);

impl CompletionCoordinator for TaskCoordinator {
    fn enqueue(&self, key: &str, source: &str, message: &ParentNotifierMessage) -> Result<(), HostError> {
        if source != "task-completion" {
            return Err(HostError { message: format!("invalid task completion source: {source}") });
        }
        let details = serde_json::to_value(&message.details).map_err(|error| HostError { message: error.to_string() })?;
        self.0.enqueue(IdleInjection {
            key: key.into(), source: IdleInjectionSource::TaskCompletion,
            custom_type: Some(message.custom_type.into()), content: message.content.clone(),
            display: Some(message.display), details: Some(details),
            on_flushed: None, on_delivery_failed: None,
        });
        Ok(())
    }
    fn schedule_flush(&self) { self.0.schedule_flush(); }
    fn flush_soon(&self) { self.0.flush_soon(); }
}

impl LeadInjectionCoordinator for TaskCoordinator {
    fn enqueue(&self, injection: LeadInjection, custom_type: &str, display: bool) {
        let flushed = Arc::new(Mutex::new(injection.on_flushed));
        self.0.enqueue(IdleInjection {
            key: injection.key, source: IdleInjectionSource::TeamMessage,
            custom_type: Some(custom_type.into()), content: injection.content,
            display: Some(display), details: None,
            on_flushed: Some(Arc::new(move || {
                if let Some(callback) = flushed.lock().unwrap_or_else(PoisonError::into_inner).take() { callback(); }
            })),
            on_delivery_failed: None,
        });
    }
    fn schedule_flush(&self) { self.0.schedule_flush(); }
    fn flush_soon(&self) { self.0.flush_soon(); }
}
