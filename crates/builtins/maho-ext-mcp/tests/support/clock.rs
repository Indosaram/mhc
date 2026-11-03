//! Deterministic time for MCP engine tests: a manual `McpClock` (port of the upstream fixture `Clock` in
//! test/mcp/fixtures/mrtr-lifecycle.ts) plus a paused tokio clock that never auto-advances.
#![allow(dead_code)]

use maho_ext_mcp::wrap::McpClock;
use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, mpsc as std_mpsc},
    time::Duration,
};
use tokio::sync::{Notify, oneshot};

/// Upstream fixture start time: deadlines are computed from a nonzero instant.
pub const CLOCK_START: f64 = 1_000_000.0;

#[derive(Default)]
struct State {
    time: f64,
    sequence: u64,
    timers: BTreeMap<u64, (f64, oneshot::Sender<()>)>,
    schedules: Vec<(f64, oneshot::Sender<()>)>,
}

/// Virtual milliseconds that move only on `advance`; dropping a sleep cancels its timer (upstream `cancel`).
#[derive(Clone)]
pub struct ManualClock {
    state: Arc<Mutex<State>>,
    removed: Arc<Notify>,
}

impl Default for ManualClock {
    fn default() -> Self {
        Self::new()
    }
}

impl ManualClock {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                time: CLOCK_START,
                ..State::default()
            })),
            removed: Arc::new(Notify::new()),
        }
    }
    /// Upstream `nextScheduled(ms)`: resolves when a timer of exactly `ms` is scheduled. Register it before the
    /// action that schedules the timer.
    pub fn next_scheduled(&self, ms: f64) -> impl Future<Output = ()> + Send + 'static {
        let (sender, receiver) = oneshot::channel();
        self.state.lock().unwrap().schedules.push((ms, sender));
        async move {
            receiver.await.expect("clock dropped before the timer was scheduled");
        }
    }
    /// Upstream `advance(ms)`: fires every due timer in deadline order, then lands on `now + ms`.
    pub fn advance(&self, ms: f64) {
        let mut state = self.state.lock().unwrap();
        let until = state.time + ms;
        loop {
            let next = state
                .timers
                .iter()
                .filter(|(_, (at, _))| *at <= until)
                .min_by(|left, right| left.1.0.total_cmp(&right.1.0).then(left.0.cmp(right.0)))
                .map(|(id, _)| *id);
            let Some(id) = next else {
                break;
            };
            let (at, fire) = state.timers.remove(&id).expect("due timer");
            state.time = at;
            let _ = fire.send(());
        }
        state.time = until;
        drop(state);
        self.removed.notify_waiters();
    }
    /// Pending timer deadlines as offsets from `CLOCK_START`, in deadline order.
    pub fn pending(&self) -> Vec<f64> {
        let mut pending = self
            .state
            .lock()
            .unwrap()
            .timers
            .values()
            .map(|(at, _)| at - CLOCK_START)
            .collect::<Vec<_>>();
        pending.sort_by(f64::total_cmp);
        pending
    }
    /// Upstream `clock.timers.size === 0`: resolves once every timer fired or was cancelled. A cancelled timer
    /// is removed when its aborted task is dropped, so this waits on removals instead of sampling once.
    pub async fn settled(&self) {
        loop {
            let removed = self.removed.notified();
            tokio::pin!(removed);
            removed.as_mut().enable();
            if self.state.lock().unwrap().timers.is_empty() {
                return;
            }
            removed.await;
        }
    }
}

struct Timer {
    state: Arc<Mutex<State>>,
    removed: Arc<Notify>,
    id: u64,
}
impl Drop for Timer {
    fn drop(&mut self) {
        let removed = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .timers
            .remove(&self.id)
            .is_some();
        if removed {
            self.removed.notify_waiters();
        }
    }
}

impl McpClock for ManualClock {
    fn now(&self) -> f64 {
        self.state.lock().unwrap().time
    }
    fn sleep(&self, ms: f64) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let (fire, fired) = oneshot::channel();
        let id = {
            let mut state = self.state.lock().unwrap();
            state.sequence += 1;
            let id = state.sequence;
            let at = state.time + ms;
            state.timers.insert(id, (at, fire));
            for (_, listener) in state.schedules.extract_if(.., |(wanted, _)| *wanted == ms) {
                let _ = listener.send(());
            }
            id
        };
        let timer = Timer {
            state: self.state.clone(),
            removed: self.removed.clone(),
            id,
        };
        Box::pin(async move {
            let _timer = timer;
            if fired.await.is_err() {
                std::future::pending::<()>().await;
            }
        })
    }
}

/// Keeps a paused tokio clock frozen: while a blocking task runs, the current-thread runtime does not
/// auto-advance to the next timer when it waits on real I/O (tokio `time::pause`, "Preventing auto-advance").
/// Timers then fire only on an explicit `tokio::time::advance`, like upstream's fake timers.
pub struct FrozenTime {
    release: Option<std_mpsc::Sender<()>>,
}
pub fn freeze_time() -> FrozenTime {
    let (release, wait) = std_mpsc::channel::<()>();
    drop(tokio::task::spawn_blocking(move || {
        let _ = wait.recv();
    }));
    FrozenTime {
        release: Some(release),
    }
}
impl Drop for FrozenTime {
    fn drop(&mut self) {
        self.release.take();
    }
}

/// Awaits `work`, failing the test if it has not completed after ten real seconds. The bound runs on a blocking
/// thread, so it is independent of the frozen tokio clock and only ever trips on a genuine hang.
pub async fn within<T>(what: &str, work: impl Future<Output = T>) -> T {
    let (done, wait) = std_mpsc::channel::<()>();
    let mut expired = tokio::task::spawn_blocking(move || {
        matches!(
            wait.recv_timeout(Duration::from_secs(10)),
            Err(std_mpsc::RecvTimeoutError::Timeout)
        )
    });
    tokio::pin!(work);
    tokio::select! {
        biased;
        value = &mut work => {
            drop(done);
            let _ = expired.await;
            value
        }
        timed_out = &mut expired => {
            assert!(!timed_out.unwrap_or(false), "{what} did not happen within 10s");
            work.await
        }
    }
}

/// A session-binding identity no other test shares: the consent queue is process-wide and keyed by binding
/// (upstream keys it by a fresh `{}` per fixture), so parallel tests must not queue behind each other.
pub fn unique_binding() -> usize {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1 << 20);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}
