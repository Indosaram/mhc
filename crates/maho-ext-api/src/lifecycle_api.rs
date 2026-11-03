//! Lazy session lifecycle contract, resolved only when an extension invokes it.
use std::sync::Arc;
use crate::types::{ExtensionFailure, JsonValue};

pub type DeliveryTicketId = String;
pub type ChildLifecycleReport = JsonValue;
pub trait ObligationToken: Send + Sync {
    fn transfer(&self, next_key: &str, kind: Option<&str>) -> Result<Arc<dyn ObligationToken>, ExtensionFailure>;
    fn ticket(&self) -> Result<DeliveryTicketId, ExtensionFailure>;
    fn held_as_notice(&self, notice_id: &str) -> Result<(), ExtensionFailure>;
    fn close(&self, reason: &str) -> Result<(), ExtensionFailure>;
}
pub trait WorkObligations: Send + Sync {
    fn open(&self, key: &str, kind: &str, detail: Option<&str>) -> Result<Arc<dyn ObligationToken>, ExtensionFailure>;
}
pub trait LifecycleApi: Send + Sync {
    fn obligations(&self) -> Result<Arc<dyn WorkObligations>, ExtensionFailure>;
    fn report_child(&self, report: ChildLifecycleReport) -> Result<(), ExtensionFailure>;
    fn mark_run_completed(&self, run_id: &str) -> Result<(), ExtensionFailure>;
}
/// Custody key prefixes only the engine opens (senpi `ENGINE_CUSTODY_KEY_PREFIXES`, senpi#3986).
pub const ENGINE_DELIVERY_KEY_PREFIX: &str = "delivery:";
pub const WAKE_SOURCE_KEY_PREFIX: &str = "wake-source:";
pub const ENGINE_CUSTODY_KEY_PREFIXES: [&str; 2] = [ENGINE_DELIVERY_KEY_PREFIX, WAKE_SOURCE_KEY_PREFIX];
/// `ExtensionFailure::class` of a refused reserved key (senpi `ReservedCustodyKey`).
pub const RESERVED_CUSTODY_KEY_CLASS: &str = "ReservedCustodyKey";

/// An extension asked for a custody key under a prefix only the engine opens (senpi#3986).
pub fn reserved_custody_key(key: &str) -> ExtensionFailure {
    ExtensionFailure::classified(
        format!("Custody key is reserved for the engine: {key}"),
        Some(RESERVED_CUSTODY_KEY_CLASS.to_owned()),
        None,
    )
}
fn assert_extension_key(key: &str) -> Result<(), ExtensionFailure> {
    if ENGINE_CUSTODY_KEY_PREFIXES.iter().any(|prefix| key.starts_with(prefix)) {
        return Err(reserved_custody_key(key));
    }
    Ok(())
}
struct ExtensionToken(Arc<dyn ObligationToken>);
impl ObligationToken for ExtensionToken {
    fn transfer(&self, next_key: &str, kind: Option<&str>) -> Result<Arc<dyn ObligationToken>, ExtensionFailure> {
        assert_extension_key(next_key)?;
        Ok(Arc::new(ExtensionToken(self.0.transfer(next_key, kind)?)))
    }
    fn ticket(&self) -> Result<DeliveryTicketId, ExtensionFailure> { self.0.ticket() }
    fn held_as_notice(&self, notice_id: &str) -> Result<(), ExtensionFailure> { self.0.held_as_notice(notice_id) }
    fn close(&self, reason: &str) -> Result<(), ExtensionFailure> { self.0.close(reason) }
}
struct ExtensionObligations(Arc<dyn WorkObligations>);
impl WorkObligations for ExtensionObligations {
    fn open(&self, key: &str, kind: &str, detail: Option<&str>) -> Result<Arc<dyn ObligationToken>, ExtensionFailure> {
        assert_extension_key(key)?;
        Ok(Arc::new(ExtensionToken(self.0.open(key, kind, detail)?)))
    }
}
pub struct LazyLifecycleApi {
    resolve: Arc<dyn Fn() -> Result<Arc<dyn LifecycleApi>, ExtensionFailure> + Send + Sync>,
}
pub fn create_lifecycle_api(resolve: impl Fn() -> Result<Arc<dyn LifecycleApi>, ExtensionFailure> + Send + Sync + 'static) -> LazyLifecycleApi {
    LazyLifecycleApi { resolve: Arc::new(resolve) }
}
impl LifecycleApi for LazyLifecycleApi {
    fn obligations(&self) -> Result<Arc<dyn WorkObligations>, ExtensionFailure> { Ok(Arc::new(ExtensionObligations((self.resolve)()?.obligations()?))) }
    fn report_child(&self, report: ChildLifecycleReport) -> Result<(), ExtensionFailure> { (self.resolve)()?.report_child(report) }
    fn mark_run_completed(&self, run_id: &str) -> Result<(), ExtensionFailure> { (self.resolve)()?.mark_run_completed(run_id) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Lifecycle(Arc<AtomicUsize>);
    impl LifecycleApi for Lifecycle {
        fn obligations(&self) -> Result<Arc<dyn WorkObligations>, ExtensionFailure> { Err(ExtensionFailure::new("not requested")) }
        fn report_child(&self, _: ChildLifecycleReport) -> Result<(), ExtensionFailure> { self.0.fetch_add(1, Ordering::SeqCst); Ok(()) }
        fn mark_run_completed(&self, _: &str) -> Result<(), ExtensionFailure> { self.0.fetch_add(1, Ordering::SeqCst); Ok(()) }
    }
    #[test]
    fn every_invocation_resolves_the_live_session_lazily() {
        let calls = Arc::new(AtomicUsize::new(0));
        let resolutions = Arc::new(AtomicUsize::new(0));
        let (count, resolved) = (calls.clone(), resolutions.clone());
        let api = create_lifecycle_api(move || { resolved.fetch_add(1, Ordering::SeqCst); Ok(Arc::new(Lifecycle(count.clone()))) });
        assert_eq!(resolutions.load(Ordering::SeqCst), 0);
        api.report_child(serde_json::json!({"taskId":"task"})).expect("report");
        api.mark_run_completed("run").expect("complete");
        assert_eq!(resolutions.load(Ordering::SeqCst), 2);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    struct Token(Arc<std::sync::Mutex<Vec<String>>>);
    impl ObligationToken for Token {
        fn transfer(&self, next_key: &str, _: Option<&str>) -> Result<Arc<dyn ObligationToken>, ExtensionFailure> {
            self.0.lock().expect("log").push(format!("transfer:{next_key}"));
            Ok(Arc::new(Token(self.0.clone())))
        }
        fn ticket(&self) -> Result<DeliveryTicketId, ExtensionFailure> { Ok("t".into()) }
        fn held_as_notice(&self, _: &str) -> Result<(), ExtensionFailure> { Ok(()) }
        fn close(&self, _: &str) -> Result<(), ExtensionFailure> { Ok(()) }
    }
    struct Obligations(Arc<std::sync::Mutex<Vec<String>>>);
    impl WorkObligations for Obligations {
        fn open(&self, key: &str, _: &str, _: Option<&str>) -> Result<Arc<dyn ObligationToken>, ExtensionFailure> {
            self.0.lock().expect("log").push(format!("open:{key}"));
            Ok(Arc::new(Token(self.0.clone())))
        }
    }
    struct WithObligations(Arc<std::sync::Mutex<Vec<String>>>);
    impl LifecycleApi for WithObligations {
        fn obligations(&self) -> Result<Arc<dyn WorkObligations>, ExtensionFailure> { Ok(Arc::new(Obligations(self.0.clone()))) }
        fn report_child(&self, _: ChildLifecycleReport) -> Result<(), ExtensionFailure> { Ok(()) }
        fn mark_run_completed(&self, _: &str) -> Result<(), ExtensionFailure> { Ok(()) }
    }
    #[test]
    fn extensions_cannot_open_or_transfer_to_engine_custody_keys() {
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = log.clone();
        let api = create_lifecycle_api(move || Ok(Arc::new(WithObligations(captured.clone()))));
        let obligations = api.obligations().expect("obligations");
        for key in ["delivery:x", "wake-source:y"] {
            let error = obligations.open(key, "kind", None).err().expect("refused");
            assert_eq!(error.class.as_deref(), Some(RESERVED_CUSTODY_KEY_CLASS));
        }
        let token = obligations.open("ext:ok", "kind", None).expect("open");
        let refused = token.transfer("delivery:z", None).err().expect("refused");
        assert_eq!(refused.class.as_deref(), Some(RESERVED_CUSTODY_KEY_CLASS));
        let moved = token.transfer("ext:next", None).expect("transfer");
        moved.transfer("wake-source:q", None).err().expect("wrapped token still guarded");
        assert_eq!(*log.lock().expect("log"), vec!["open:ext:ok", "transfer:ext:next"]);
    }
}
