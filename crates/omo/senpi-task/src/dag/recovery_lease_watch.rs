//! Polls the previous holder of a paused DAG run until its process exits.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Once, OnceLock, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

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

/// One shared timer thread serves every default-timer watch: each poll tick re-arms a queue entry
/// instead of spawning a thread, and clearing a timer removes its entry so nothing sleeps on.
struct DefaultTimers {
    queue: Mutex<std::collections::BTreeMap<(Instant, DagLeaseWatchTimerHandle), DagLeaseWatchCallback>>,
    changed: Condvar,
}

fn default_timers() -> &'static DefaultTimers {
    static TIMERS: OnceLock<DefaultTimers> = OnceLock::new();
    static WORKER: Once = Once::new();
    let timers = TIMERS.get_or_init(|| DefaultTimers { queue: Mutex::new(std::collections::BTreeMap::new()), changed: Condvar::new() });
    WORKER.call_once(|| {
        let _ = thread::Builder::new().name("dag-lease-watch".into()).spawn(move || run_default_timers(timers));
    });
    timers
}

fn run_default_timers(timers: &'static DefaultTimers) {
    let mut queue = lock(&timers.queue);
    loop {
        let next = queue.first_key_value().map(|(&key, _)| key);
        let Some((deadline, handle)) = next else {
            queue = timers.changed.wait(queue).unwrap_or_else(PoisonError::into_inner);
            continue;
        };
        let now = Instant::now();
        if deadline > now {
            queue = timers.changed.wait_timeout(queue, deadline - now).unwrap_or_else(PoisonError::into_inner).0;
            continue;
        }
        let callback = queue.remove(&(deadline, handle));
        drop(queue);
        if let Some(callback) = callback {
            // A faulting callback must not take down the shared timer thread for other watches.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback()));
        }
        queue = lock(&timers.queue);
    }
}

fn default_set_timer(callback: DagLeaseWatchCallback, delay_ms: u64) -> DagLeaseWatchTimerHandle {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let handle = NEXT.fetch_add(1, Ordering::Relaxed);
    let timers = default_timers();
    lock(&timers.queue).insert((Instant::now() + Duration::from_millis(delay_ms), handle), callback);
    timers.changed.notify_all();
    handle
}

fn default_clear_timer(handle: DagLeaseWatchTimerHandle) {
    let timers = default_timers();
    lock(&timers.queue).retain(|&(_, queued), _| queued != handle);
    timers.changed.notify_all();
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "recovery_lease_watch_tests.rs"]
mod tests;
