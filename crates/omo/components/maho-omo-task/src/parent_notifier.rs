use std::sync::Arc;
use maho_ext_api::{CustomMessage, DeliverAs, ExtensionActions, SendMessageOptions, ToolContent};
use senpi_task::{completion::{ParentNotifier, ParentNotifierMessage}, host::HostError};

pub trait CompletionCoordinator: Send + Sync {
    fn enqueue(&self, key: &str, source: &str, message: &ParentNotifierMessage) -> Result<(), HostError>;
    fn schedule_flush(&self);
    fn flush_soon(&self);
}
pub struct TaskParentNotifier {
    pub actions: Arc<dyn ExtensionActions>,
    pub coordinator: Option<Arc<dyn CompletionCoordinator>>,
    pub is_streaming: Arc<dyn Fn() -> bool + Send + Sync>,
}
impl ParentNotifier for TaskParentNotifier {
    fn enqueue(&self, message: &ParentNotifierMessage) -> Result<(), HostError> {
        if let Some(coordinator) = &self.coordinator {
            let ids = message.details.iter().map(|detail| detail.task_id.as_str()).collect::<Vec<_>>().join(",");
            let key = if ids.is_empty() { "task-completion".into() } else { format!("task-completion:{ids}") };
            coordinator.enqueue(&key, "task-completion", message)?;
            if (self.is_streaming)() { coordinator.schedule_flush(); } else { coordinator.flush_soon(); }
            return Ok(())
        }
        let details = serde_json::to_value(&message.details).map_err(|error| HostError { message: error.to_string() })?;
        self.actions.send_message(CustomMessage { custom_type: message.custom_type.into(), content: vec![ToolContent::text(&message.content)], display: message.display, details: Some(details) }, SendMessageOptions { trigger_turn: true, deliver_as: Some(DeliverAs::Steer) }).map_err(|error| HostError { message: error.to_string() })
    }
}
