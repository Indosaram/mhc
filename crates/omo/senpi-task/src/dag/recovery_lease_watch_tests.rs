use super::*;
use std::collections::BTreeMap;

#[derive(Default)]
struct ManualTimers {
    next: AtomicU64,
    callbacks: Mutex<BTreeMap<u64, DagLeaseWatchCallback>>,
}

impl ManualTimers {
    fn timers(self: &Arc<Self>) -> DagLeaseWatchTimers {
        let set_timers = Arc::clone(self);
        let clear_timers = Arc::clone(self);
        DagLeaseWatchTimers {
            set: Some(Arc::new(move |callback, _delay| {
                let handle = set_timers.next.fetch_add(1, Ordering::Relaxed) + 1;
                lock(&set_timers.callbacks).insert(handle, callback);
                handle
            })),
            clear: Some(Arc::new(move |handle| {
                lock(&clear_timers.callbacks).remove(&handle);
            })),
        }
    }

    fn pending(&self) -> usize {
        lock(&self.callbacks).len()
    }

    fn tick(&self) {
        let due = std::mem::take(&mut *lock(&self.callbacks));
        for callback in due.into_values() {
            callback();
        }
    }
}

#[test]
fn given_live_holder_when_it_exits_between_polls_then_callback_fires_once() {
    let timers = Arc::new(ManualTimers::default());
    let alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let alive_probe = Arc::clone(&alive);
    let watch = create_dag_lease_watch(DagLeaseWatchOptions {
        is_process_alive: Some(Arc::new(move |_| alive_probe.load(Ordering::SeqCst))),
        timers: Some(timers.timers()),
        interval_ms: Some(250),
    });
    let fired = Arc::new(AtomicU64::new(0));
    let fired_callback = Arc::clone(&fired);
    watch.watch(4242, Arc::new(move || { fired_callback.fetch_add(1, Ordering::SeqCst); }));

    timers.tick();
    assert_eq!(fired.load(Ordering::SeqCst), 0);
    assert_eq!(timers.pending(), 1);
    alive.store(false, Ordering::SeqCst);
    timers.tick();
    timers.tick();
    assert_eq!(fired.load(Ordering::SeqCst), 1);
    assert_eq!(timers.pending(), 0);
}

#[test]
fn given_armed_watch_when_cancelled_then_no_callback_or_timer_remains() {
    let timers = Arc::new(ManualTimers::default());
    let alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let watch = create_dag_lease_watch(DagLeaseWatchOptions {
        is_process_alive: Some(Arc::new(move |_| alive.load(Ordering::SeqCst))),
        timers: Some(timers.timers()),
        interval_ms: Some(250),
    });
    let fired = Arc::new(AtomicU64::new(0));
    let fired_callback = Arc::clone(&fired);
    let cancel = watch.watch(4242, Arc::new(move || { fired_callback.fetch_add(1, Ordering::SeqCst); }));

    cancel.cancel();
    timers.tick();
    assert_eq!(fired.load(Ordering::SeqCst), 0);
    assert_eq!(timers.pending(), 0);
}

#[test]
fn given_multiple_armed_watches_when_disposed_then_every_timer_is_cleared() {
    let timers = Arc::new(ManualTimers::default());
    let watch = create_dag_lease_watch(DagLeaseWatchOptions {
        is_process_alive: Some(Arc::new(|_| false)),
        timers: Some(timers.timers()),
        interval_ms: Some(250),
    });
    let fired = Arc::new(AtomicU64::new(0));
    for pid in [1, 2] {
        let fired_callback = Arc::clone(&fired);
        watch.watch(pid, Arc::new(move || { fired_callback.fetch_add(1, Ordering::SeqCst); }));
    }
    assert_eq!(timers.pending(), 2);

    watch.dispose();
    timers.tick();
    assert_eq!(fired.load(Ordering::SeqCst), 0);
    assert_eq!(timers.pending(), 0);
}
