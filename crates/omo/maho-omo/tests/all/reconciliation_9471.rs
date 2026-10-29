use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use maho_omo::{Delivery, DeferredScheduler, IdleInjectionQueue, TurnBarrier};

#[test]
fn a_successful_delivery_reports_flushed() {
    use maho_ext_api::{IdleInjectionCoordinator, IdleInjectionSource};
    let scheduler = DeferredScheduler::manual(TurnBarrier::new());
    let queue = IdleInjectionQueue::new(crate::support::DeliveryLog::new().delivery(), scheduler.clone(), scheduler);
    let flushed = Arc::new(AtomicUsize::new(0));
    let receipt = Arc::clone(&flushed);
    let mut injection = crate::support::injection("success", IdleInjectionSource::TaskCompletion, "complete");
    injection.on_flushed = Some(Arc::new(move || { receipt.fetch_add(1, Ordering::SeqCst); }));
    queue.enqueue(injection);
    assert_eq!(queue.flush_on_idle(), 1);
    assert_eq!(flushed.load(Ordering::SeqCst), 1);
}

#[test]
fn a_failed_delivery_reports_failure_without_reporting_flushed() {
    use maho_ext_api::{IdleInjectionCoordinator, IdleInjectionSource};
    let scheduler = DeferredScheduler::manual(TurnBarrier::new());
    let queue = IdleInjectionQueue::new(Arc::new(|_, _| Delivery::Failed("unavailable".to_owned())), scheduler.clone(), scheduler);
    let failures = Arc::new(AtomicUsize::new(0));
    let failed = Arc::clone(&failures);
    let flushed = Arc::new(AtomicUsize::new(0));
    let flushed_receipt = Arc::clone(&flushed);
    let mut injection = crate::support::injection("failure", IdleInjectionSource::TaskCompletion, "complete");
    injection.on_flushed = Some(Arc::new(move || { flushed_receipt.fetch_add(1, Ordering::SeqCst); }));
    injection.on_delivery_failed = Some(Arc::new(move |reason| {
        assert_eq!(reason, "unavailable");
        failed.fetch_add(1, Ordering::SeqCst);
    }));
    queue.enqueue(injection);
    assert_eq!(queue.flush_on_idle(), 1);
    assert_eq!(failures.load(Ordering::SeqCst), 1);
    assert_eq!(flushed.load(Ordering::SeqCst), 0);
}
