use std::future::Future;

use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
use std::time::Duration;
use idle_injection::IdleInjectionSource;
use maho_omo::{DeferredScheduler, IdleInjectionQueue, TurnBarrier, runtime_macrotask, runtime_microtask};

#[test]
fn custom_spawn_seam_captures_one_pending_drain() {
    let captured=Arc::new(AtomicUsize::new(0));let count=captured.clone();
    let scheduler=DeferredScheduler::with_spawn(TurnBarrier::new(),Arc::new(move |future| {count.fetch_add(1,Ordering::SeqCst);drop(future);}));
    scheduler.schedule(Box::new(||{}));
    assert_eq!(captured.load(Ordering::SeqCst),1);assert_eq!(scheduler.pending_count(),1);
}

#[tokio::test]
async fn runtime_drain_serializes_passes() {
    let active=Arc::new(AtomicUsize::new(0));let maximum=Arc::new(AtomicUsize::new(0));
    let scheduler=DeferredScheduler::runtime(TurnBarrier::new(),None);
    let (sender,receipt)=tokio::sync::oneshot::channel();let sender=Arc::new(std::sync::Mutex::new(Some(sender)));
    for index in 0..3 {
        let active=active.clone();let maximum=maximum.clone();let sender=sender.clone();
        scheduler.schedule(Box::new(move || {
            let current=active.fetch_add(1,Ordering::SeqCst)+1;maximum.fetch_max(current,Ordering::SeqCst);
            active.fetch_sub(1,Ordering::SeqCst);
            if index==2 {sender.lock().expect("sender").take().expect("last receipt").send(()).expect("receipt");}
        }));
    }
    tokio::time::timeout(Duration::from_secs(5),receipt).await.expect("bounded drain").expect("receipt");
    assert_eq!(maximum.load(Ordering::SeqCst),1);
}

#[tokio::test]
async fn runtime_deferred_helpers_signal_after_the_current_task_yields() {
    let observed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let microtask_observed = Arc::clone(&observed);
    let macrotask_observed = Arc::clone(&observed);
    let (microtask_sender, microtask_receipt) = tokio::sync::oneshot::channel();
    let (macrotask_sender, macrotask_receipt) = tokio::sync::oneshot::channel();

    runtime_microtask()(Box::new(move || {
        microtask_observed.store(true, Ordering::SeqCst);
        let _ = microtask_sender.send(());
    }));
    runtime_macrotask()(Box::new(move || {
        macrotask_observed.store(true, Ordering::SeqCst);
        let _ = macrotask_sender.send(());
    }));

    assert!(!observed.load(Ordering::SeqCst));
    tokio::time::timeout(Duration::from_secs(5), async {
        microtask_receipt.await.unwrap();
        macrotask_receipt.await.unwrap();
    })
    .await
    .unwrap();
}

#[test]
fn runtime_microtask_fallback_runs_off_the_callers_stack() {
    let (sender, receipt) = std::sync::mpsc::channel();
    let deferred = runtime_microtask();
    let caller = std::thread::current().id();

    deferred(Box::new(move || {
        let _ = sender.send(std::thread::current().id());
    }));

    assert_ne!(receipt.recv_timeout(Duration::from_secs(5)).unwrap(), caller);
}

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
async fn deferred_pass_is_not_blocked_by_awaited_handler_work() {
    let barrier = TurnBarrier::new();
    let scheduler = DeferredScheduler::runtime(Arc::clone(&barrier), None);
    let (started, started_receipt) = tokio::sync::oneshot::channel();
    let (release, release_receipt) = tokio::sync::oneshot::channel();
    let handler_barrier = Arc::clone(&barrier);
    let handler = tokio::spawn(async move {
        handler_barrier.first_poll(async move {
            started.send(()).expect("started receipt");
            release_receipt.await.expect("handler release");
        }).await;
    });
    started_receipt.await.expect("handler first poll");

    let (delivered, delivery_receipt) = tokio::sync::oneshot::channel();
    scheduler.schedule(Box::new(move || delivered.send(()).expect("delivery receipt")));
    tokio::time::timeout(Duration::from_secs(5), delivery_receipt).await.expect("bounded delivery").expect("delivery");

    release.send(()).expect("release handler");
    handler.await.expect("handler task");
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
        use idle_injection::IdleInjectionCoordinator;
        queue.enqueue(crate::support::injection("ulw", IdleInjectionSource::UlwContinuation, "continue"));
        queue.schedule_flush();
        queue.enqueue(crate::support::injection("task", IdleInjectionSource::TaskCompletion, "complete"));
        assert!(log.calls().is_empty());
    });
    tokio::time::timeout(Duration::from_secs(5), receipt).await.unwrap().unwrap();
    assert_eq!(log.calls().len(), 1);
    assert_eq!(log.calls()[0].message.content, "complete\n\ncontinue");
}

#[tokio::test(start_paused = true)]
async fn runtime_scheduler_waits_for_the_configured_batch_window() {
    let barrier = TurnBarrier::new();
    let scheduler = DeferredScheduler::runtime(Arc::clone(&barrier), Some(Duration::from_millis(10)));
    let (sender, mut receipt) = tokio::sync::oneshot::channel();
    scheduler.schedule(Box::new(move || {
        let _ = sender.send(());
    }));

    tokio::task::yield_now().await;
    assert_eq!(scheduler.pending_count(), 0, "the batch has been taken and its delay registered");
    tokio::time::advance(Duration::from_millis(9)).await;
    tokio::task::yield_now().await;
    assert!(matches!(receipt.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Empty)));

    tokio::time::advance(Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    tokio::time::timeout(Duration::from_secs(1), receipt).await.unwrap().unwrap();
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
