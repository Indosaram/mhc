use maho_ext_api::{EventKind, EventResult, Extension, ExtensionApi, ExtensionEvent, SessionReason};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Name of the variable Orca sets for "launch with prompt" (`ORCA_PI_PREFILL`).
pub const PREFILL_ENV: &str = "ORCA_PI_PREFILL";

/// Process-global stand-in for upstream `delete process.env.ORCA_PI_PREFILL`: mutating the process
/// environment is `unsafe` in Rust, so the first take latches and every later read sees nothing.
static PREFILL_TAKEN: AtomicBool = AtomicBool::new(false);

/// Read the prefill once per process; `read` supplies the raw value on the first call only.
pub fn take_process_prefill(read: impl FnOnce() -> Option<String>) -> Option<String> {
    if PREFILL_TAKEN.swap(true, Ordering::SeqCst) { return None; }
    read()
}

/// Whether the prefill was consumed, so spawned children must not inherit it.
pub fn process_prefill_taken() -> bool { PREFILL_TAKEN.load(Ordering::SeqCst) }

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
    #[test]
    fn process_prefill_is_taken_once() {
        assert_eq!(take_process_prefill(|| Some("prompt".into())).as_deref(), Some("prompt"));
        assert!(process_prefill_taken());
        assert_eq!(take_process_prefill(|| Some("again".into())), None);
    }
}
