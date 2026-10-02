use std::sync::{Arc, Mutex, PoisonError};
use senpi_task::{completion::{CompletionNotifier, FlushInput, FlushResult, TransitionReason}, store::StoreError};
use crate::runtime_context::TaskRuntimeContext;

pub struct SessionTransitionBridge {
    runtime: Arc<Mutex<TaskRuntimeContext>>,
    notifier: CompletionNotifier,
    transitioning_session_id: Option<String>,
}
impl SessionTransitionBridge {
    pub fn new(runtime: Arc<Mutex<TaskRuntimeContext>>, notifier: CompletionNotifier) -> Self { Self { runtime, notifier, transitioning_session_id: None } }
    pub fn mark(&mut self, reason: TransitionReason, session_id: Option<&str>) {
        self.runtime.lock().unwrap_or_else(PoisonError::into_inner).set_transition(Some(reason));
        self.transitioning_session_id = session_id.map(str::to_owned);
    }
    pub fn resolve(&mut self, current_session_id: Option<&str>) -> Result<Option<FlushResult>, StoreError> {
        self.runtime.lock().unwrap_or_else(PoisonError::into_inner).set_transition(None);
        let Some(buffered) = self.transitioning_session_id.take() else { return Ok(None) };
        let replaced = current_session_id != Some(buffered.as_str());
        self.notifier.flush_buffered(&FlushInput { session_id: buffered, replaced }).map(Some)
    }
}
