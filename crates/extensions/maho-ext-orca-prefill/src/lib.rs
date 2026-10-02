use maho_ext_api::{EventKind, EventResult, Extension, ExtensionApi, ExtensionEvent, SessionReason};
use std::sync::Arc;

pub trait PrefillEnvironment: Send + Sync {
    fn pane_key(&self) -> Option<String>;
    fn take_prefill(&self) -> Option<String>;
}

pub struct OrcaPrefill { pub environment: Arc<dyn PrefillEnvironment> }
impl Extension for OrcaPrefill {
    fn register(&self, api: &mut ExtensionApi) {
        let environment = Arc::clone(&self.environment);
        api.on(EventKind::SessionStart, Arc::new(move |event, ctx| {
            let environment = Arc::clone(&environment);
            Box::pin(async move {
                if !matches!(event, ExtensionEvent::SessionStart(start) if start.reason == SessionReason::Startup) { return Ok(EventResult::None); }
                if environment.pane_key().is_none_or(|key| key.is_empty()) { return Ok(EventResult::None); }
                if let Some(prefill) = environment.take_prefill().filter(|value| !value.is_empty()) { ctx.ui.set_editor_text(&prefill); }
                Ok(EventResult::None)
            })
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    struct Environment { value: Mutex<Option<String>> }
    impl PrefillEnvironment for Environment {
        fn pane_key(&self) -> Option<String> { Some("pane".into()) }
        fn take_prefill(&self) -> Option<String> { self.value.lock().unwrap().take() }
    }
    #[test]
    fn consumed_when_taken() {
        let environment = Environment { value: Mutex::new(Some("prompt".into())) };
        let value = environment.take_prefill();
        assert_eq!(value.as_deref(), Some("prompt"));
        assert_eq!(environment.take_prefill(), None);
    }
}
