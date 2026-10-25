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
pub struct LazyLifecycleApi {
    resolve: Arc<dyn Fn() -> Result<Arc<dyn LifecycleApi>, ExtensionFailure> + Send + Sync>,
}
pub fn create_lifecycle_api(resolve: impl Fn() -> Result<Arc<dyn LifecycleApi>, ExtensionFailure> + Send + Sync + 'static) -> LazyLifecycleApi {
    LazyLifecycleApi { resolve: Arc::new(resolve) }
}
impl LifecycleApi for LazyLifecycleApi {
    fn obligations(&self) -> Result<Arc<dyn WorkObligations>, ExtensionFailure> { (self.resolve)()?.obligations() }
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
}
