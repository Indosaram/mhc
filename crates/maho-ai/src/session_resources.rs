//! Port of senpi packages/ai/src/session-resources.ts.

use std::sync::{Arc, Mutex};

pub type SessionResourceCleanup = Arc<dyn Fn(Option<&str>) -> Result<(), String> + Send + Sync>;

static CLEANUPS: Mutex<Vec<(u64, SessionResourceCleanup)>> = Mutex::new(Vec::new());
static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Failed to cleanup session resources")]
pub struct SessionCleanupError {
    pub errors: Vec<String>,
}

/// Registers a cleanup; the returned closure unregisters it.
pub fn register_session_resource_cleanup(cleanup: SessionResourceCleanup) -> impl FnOnce() {
    let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    CLEANUPS.lock().unwrap_or_else(|p| p.into_inner()).push((id, cleanup));
    move || CLEANUPS.lock().unwrap_or_else(|p| p.into_inner()).retain(|(entry, _)| *entry != id)
}

pub fn cleanup_session_resources(session_id: Option<&str>) -> Result<(), SessionCleanupError> {
    let cleanups: Vec<SessionResourceCleanup> =
        CLEANUPS.lock().unwrap_or_else(|p| p.into_inner()).iter().map(|(_, c)| c.clone()).collect();
    let errors: Vec<String> = cleanups.iter().filter_map(|cleanup| cleanup(session_id).err()).collect();
    if errors.is_empty() { Ok(()) } else { Err(SessionCleanupError { errors }) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_all_cleanups_and_aggregates_errors() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = seen.clone();
        let unregister_ok = register_session_resource_cleanup(Arc::new(move |id| {
            record.lock().unwrap_or_else(|p| p.into_inner()).push(id.map(str::to_owned));
            Ok(())
        }));
        let unregister_err = register_session_resource_cleanup(Arc::new(|_| Err("boom".into())));
        let result = cleanup_session_resources(Some("s1"));
        assert!(result.as_ref().is_err_and(|e| e.errors.contains(&"boom".to_owned())));
        assert_eq!(result.map_err(|e| e.to_string()), Err("Failed to cleanup session resources".into()));
        unregister_err();
        unregister_ok();
        assert!(seen.lock().unwrap_or_else(|p| p.into_inner()).contains(&Some("s1".into())));
    }
}
