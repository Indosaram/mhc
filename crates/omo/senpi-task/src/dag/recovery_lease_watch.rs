//! Polls the previous holder of a paused DAG run until its process exits.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;

pub const DAG_LEASE_WATCH_INTERVAL_MS: u64 = 1_000;

pub type DagLeaseWatchTimerHandle = u64;
pub type DagLeaseWatchCallback = Arc<dyn Fn() + Send + Sync>;
pub type DagLeaseWatchSetTimer = Arc<dyn Fn(DagLeaseWatchCallback, u64) -> DagLeaseWatchTimerHandle + Send + Sync>;
pub type DagLeaseWatchClearTimer = Arc<dyn Fn(DagLeaseWatchTimerHandle) + Send + Sync>;
pub type DagLeaseWatchIsProcessAlive = Arc<dyn Fn(i64) -> bool + Send + Sync>;

#[derive(Clone, Default)]
pub struct DagLeaseWatchTimers {
    pub set: Option<DagLeaseWatchSetTimer>,
    pub clear: Option<DagLeaseWatchClearTimer>,
}

#[derive(Clone, Default)]
pub struct DagLeaseWatchOptions {
    pub is_process_alive: Option<DagLeaseWatchIsProcessAlive>,
    pub timers: Option<DagLeaseWatchTimers>,
    pub interval_ms: Option<u64>,
}

#[derive(Clone)]
struct TimerEntry {
    handle: DagLeaseWatchTimerHandle,
    alive: Arc<std::sync::atomic::AtomicBool>,
}

#[derive(Clone)]
pub struct DagLeaseWatch {
    state: Arc<WatchState>,
}

struct WatchState {
    is_process_alive: DagLeaseWatchIsProcessAlive,
    timers: DagLeaseWatchTimers,
    interval_ms: u64,
    pending: Mutex<HashMap<u64, TimerEntry>>,
    next_key: AtomicU64,
}

impl DagLeaseWatch {
    /// Polls `holder_pid` until it stops being alive, then invokes `on_exit` once.
    pub fn watch(&self, holder_pid: i64, on_exit: DagLeaseWatchCallback) -> DagLeaseWatchCancel {
        let key = self.state.next_key.fetch_add(1, Ordering::Relaxed);
        let state = Arc::clone(&self.state);
        let poll: DagLeaseWatchCallback = Arc::new(move || {
            let entry = lock(&state.pending).remove(&key);
            let Some(entry) = entry else { return };
            entry.alive.store(false, Ordering::SeqCst);
            if (state.is_process_alive)(holder_pid) {
                arm_poll(Arc::clone(&state), key, holder_pid, Arc::clone(&on_exit));
            } else {
                on_exit();
            }
        });
        arm_with_callback(Arc::clone(&self.state), key, poll);
        DagLeaseWatchCancel { state: Arc::clone(&self.state), key }
    }

    pub fn dispose(&self) {
        let keys: Vec<u64> = lock(&self.state.pending).keys().copied().collect();
        for key in keys {
            cancel_watch(&self.state, key);
        }
    }
}

pub struct DagLeaseWatchCancel {
    state: Arc<WatchState>,
    key: u64,
}

impl DagLeaseWatchCancel {
    pub fn cancel(&self) {
        cancel_watch(&self.state, self.key);
    }
}

pub fn create_dag_lease_watch(options: DagLeaseWatchOptions) -> DagLeaseWatch {
    let timers = options.timers.unwrap_or_default();
    let timers = DagLeaseWatchTimers {
        set: Some(timers.set.unwrap_or_else(|| Arc::new(default_set_timer))),
        clear: Some(timers.clear.unwrap_or_else(|| Arc::new(default_clear_timer))),
    };
    DagLeaseWatch {
        state: Arc::new(WatchState {
            is_process_alive: options.is_process_alive.unwrap_or_else(|| Arc::new(crate::dag::store::default_is_process_alive)),
            timers,
            interval_ms: options.interval_ms.unwrap_or(DAG_LEASE_WATCH_INTERVAL_MS),
            pending: Mutex::new(HashMap::new()),
            next_key: AtomicU64::new(1),
        }),
    }
}

fn arm_poll(state: Arc<WatchState>, key: u64, holder_pid: i64, on_exit: DagLeaseWatchCallback) {
    let next_state = Arc::clone(&state);
    let callback_state = Arc::clone(&state);
    let next: DagLeaseWatchCallback = Arc::new(move || {
        let entry = lock(&callback_state.pending).remove(&key);
        let Some(entry) = entry else { return };
        entry.alive.store(false, Ordering::SeqCst);
        if (callback_state.is_process_alive)(holder_pid) {
                arm_poll(Arc::clone(&next_state), key, holder_pid, Arc::clone(&on_exit));
        } else {
            on_exit();
        }
    });
    arm_with_callback(state, key, next);
}

fn arm_with_callback(state: Arc<WatchState>, key: u64, callback: DagLeaseWatchCallback) {
    let alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let active = Arc::clone(&alive);
    let guarded: DagLeaseWatchCallback = Arc::new(move || {
        if active.load(Ordering::SeqCst) { callback(); }
    });
    let handle = state.timers.set.as_ref().expect("timer set is initialized")(guarded, state.interval_ms);
    lock(&state.pending).insert(key, TimerEntry { handle, alive });
}

fn cancel_watch(state: &Arc<WatchState>, key: u64) {
    if let Some(entry) = lock(&state.pending).remove(&key) {
        entry.alive.store(false, Ordering::SeqCst);
        state.timers.clear.as_ref().expect("timer clear is initialized")(entry.handle);
    }
}

fn default_set_timer(callback: DagLeaseWatchCallback, delay_ms: u64) -> DagLeaseWatchTimerHandle {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let handle = NEXT.fetch_add(1, Ordering::Relaxed);
    let _ = thread::Builder::new().name("dag-lease-watch".into()).spawn(move || {
        thread::park_timeout(Duration::from_millis(delay_ms));
        callback();
    });
    handle
}

fn default_clear_timer(_handle: DagLeaseWatchTimerHandle) {
    // Thread timers cannot be interrupted; the watch's active guard makes cancellation inert.
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
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
}
