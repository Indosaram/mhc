use std::future::Future;

use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
use std::time::Duration;
use maho_ext_api::IdleInjectionSource;
use maho_omo::{DeferredScheduler, IdleInjectionQueue, TurnBarrier};

#[tokio::test]
async fn deferred_pass_waits_for_the_whole_turn() {
    let barrier = TurnBarrier::new();
    let guard = barrier.enter();
    let (sender, receipt) = tokio::sync::oneshot::channel();
    let scheduler = DeferredScheduler::runtime(barrier, None);
    scheduler.schedule(Box::new(move || { sender.send(()).unwrap(); }));
    tokio::pin!(receipt);
    assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(receipt.as_mut().poll(cx).is_pending())).await);
    drop(guard);
    tokio::time::timeout(Duration::from_secs(5), receipt).await.unwrap().unwrap();
}

#[tokio::test]
async fn consecutive_producers_in_one_turn_deliver_one_batch() {
    let log = crate::support::DeliveryLog::new();
    let delivery = log.delivery();
    let (sender, receipt) = tokio::sync::oneshot::channel();
    let sender = std::sync::Mutex::new(Some(sender));
    let scheduler = DeferredScheduler::runtime(TurnBarrier::new(), None);
    let queue = IdleInjectionQueue::new(Arc::new(move |message, mode| {
        let result = delivery(message, mode);
        if let Some(sender) = sender.lock().unwrap().take() { sender.send(()).unwrap(); }
        result
    }), scheduler.clone(), scheduler);
    queue.run_in_turn(|| {
        use maho_ext_api::IdleInjectionCoordinator;
        queue.enqueue(crate::support::injection("ulw", IdleInjectionSource::UlwContinuation, "continue"));
        queue.schedule_flush();
        queue.enqueue(crate::support::injection("task", IdleInjectionSource::TaskCompletion, "complete"));
        assert!(log.calls().is_empty());
    });
    tokio::time::timeout(Duration::from_secs(5), receipt).await.unwrap().unwrap();
    assert_eq!(log.calls().len(), 1);
    assert_eq!(log.calls()[0].message.content, "complete\n\ncontinue");
}

#[test]
fn manual_scheduler_drains_one_snapshot_and_retains_reentrant_work() {
    let scheduler = DeferredScheduler::manual(TurnBarrier::new());
    let next = scheduler.clone();
    let count = Arc::new(AtomicUsize::new(0));
    let counter = count.clone();
    scheduler.schedule(Box::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
        next.schedule(Box::new(move || { counter.fetch_add(1, Ordering::SeqCst); }));
    }));
    assert_eq!(scheduler.run_pending(), 1);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(scheduler.run_pending(), 1);
    assert_eq!(count.load(Ordering::SeqCst), 2);
}
