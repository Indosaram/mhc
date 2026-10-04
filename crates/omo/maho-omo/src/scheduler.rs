//! Deferred-pass scheduling for the omo composition (todo 47).
//!
//! Upstream defers with `queueMicrotask` (the coordinator's default `scheduleFlush`, its
//! `flushSoon` hand-off, every promise continuation) and with `setTimeout` (the composition's
//! 200 ms batch window, and the `setTimeout(0)` macrotask `init-deep-advisor` uses before its
//! dialog).
//!
//! # Required ordering (pinned source, `idle-injection-coordinator.test.ts`)
//!
//! 1. A deferred pass never runs inside the caller's synchronous section. `scheduleFlush()` must
//!    return with nothing delivered; only the deferred pass may deliver.
//! 2. A synchronous `flushOnIdle()` that drains the queue first makes the pending deferred pass a
//!    no-op (guaranteed by clearing the queue before delivery, not by timing).
//! 3. Repeated `scheduleFlush()` requests before the deferred pass runs coalesce to one pass
//!    (guaranteed by the coordinator's `flush_scheduled` flag, not by the executor).
//!
//! In JS all three hold because the event loop runs the microtask after the whole synchronous turn.
//! Tokio has no microtask queue, and `tokio::spawn` alone does NOT reproduce rule 1: on a
//! multi-thread runtime the spawned task can be polled on another worker while the caller is still
//! inside `scheduleFlush`.
//!
//! # Why a per-method guard is not enough
//!
//! Guarding each coordinator method only says "no pass runs *inside one method*". A host turn is
//! usually several consecutive synchronous calls:
//!
//! ```text
//! coordinator.schedule_flush();   // returns, depth back to 0
//! coordinator.enqueue(b);         // still the SAME host turn
//! ```
//!
//! Between those two calls the depth is momentarily zero, so a drain that only waits for quiet can
//! run and deliver *before* `enqueue(b)`, splitting one upstream turn into two injections. The
//! cross-call boundary therefore has to be held by the host: the dispatch that owns the turn wraps
//! its whole synchronous section in [`TurnBarrier::turn`] (or [`DeferredScheduler::run_in_turn`]),
//! and the drain waits for that scope to close. This is the native equivalent of the JS turn
//! boundary and it is the ordering mechanism; the optional `delay` is only the coalescing window,
//! never a correctness guarantee.
//!
//! The barrier is a counting guard rather than a lock so a callback that re-enters the coordinator
//! from inside a turn cannot deadlock.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use maho_ext_api::DeferredMacrotask;

pub type DeferredTask = Box<dyn FnOnce() + Send + 'static>;

type DrainFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

pub type SpawnSeam = Arc<dyn Fn(DrainFuture) + Send + Sync>;

/// Counts open synchronous turn scopes; a deferred pass waits for the count to reach zero.
#[derive(Default)]
pub struct TurnBarrier {
    depth: AtomicUsize,
    notify: tokio::sync::Notify,
}

impl TurnBarrier {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn enter(self: &Arc<Self>) -> TurnGuard {
        self.depth.fetch_add(1, Ordering::AcqRel);
        TurnGuard { barrier: Arc::clone(self) }
    }

    pub fn depth(&self) -> usize {
        self.depth.load(Ordering::Acquire)
    }

    /// Runs `body` with the turn held, so no deferred pass can run inside it.
    pub fn turn<R>(self: &Arc<Self>, body: impl FnOnce() -> R) -> R {
        let _guard = self.enter();
        body()
    }

    /// Resolves once no turn scope is open. `enable()` registers the waiter before the check, so a
    /// release between the check and the await cannot be lost.
    pub async fn wait_for_quiet(&self) {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.depth() == 0 {
                return;
            }
            notified.await;
        }
    }
}

pub struct TurnGuard {
    barrier: Arc<TurnBarrier>,
}

impl Drop for TurnGuard {
    fn drop(&mut self) {
        if self.barrier.depth.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.barrier.notify.notify_waiters();
        }
    }
}

struct Inner {
    barrier: Arc<TurnBarrier>,
    spawn: SpawnSeam,
    delay: Option<Duration>,
    pending: Mutex<VecDeque<DeferredTask>>,
    draining: AtomicBool,
    manual: bool,
}

#[derive(Clone)]
pub struct DeferredScheduler {
    inner: Arc<Inner>,
}

impl DeferredScheduler {
    pub fn runtime(barrier: Arc<TurnBarrier>, delay: Option<Duration>) -> Self {
        Self::build(barrier, Arc::new(spawn_tokio), delay, false)
    }

    pub fn manual(barrier: Arc<TurnBarrier>) -> Self {
        Self::build(barrier, Arc::new(|_| {}), None, true)
    }

    pub fn with_spawn(barrier: Arc<TurnBarrier>, spawn: SpawnSeam) -> Self {
        Self::build(barrier, spawn, None, false)
    }

    fn build(barrier: Arc<TurnBarrier>, spawn: SpawnSeam, delay: Option<Duration>, manual: bool) -> Self {
        Self {
            inner: Arc::new(Inner {
                barrier,
                spawn,
                delay,
                pending: Mutex::new(VecDeque::new()),
                draining: AtomicBool::new(false),
                manual,
            }),
        }
    }

    pub fn barrier(&self) -> Arc<TurnBarrier> {
        Arc::clone(&self.inner.barrier)
    }

    pub fn enter_turn(&self) -> TurnGuard {
        self.inner.barrier.enter()
    }

    /// Holds the turn across the whole closure so consecutive coordinator calls inside it cannot be
    /// split by a deferred pass.
    pub fn run_in_turn<R>(&self, body: impl FnOnce() -> R) -> R {
        let _guard = self.enter_turn();
        body()
    }

    pub fn schedule(&self, task: DeferredTask) {
        self.inner.pending.lock().unwrap_or_else(PoisonError::into_inner).push_back(task);
        if !self.inner.manual {
            self.ensure_drain();
        }
    }

    pub fn pending_count(&self) -> usize {
        self.inner.pending.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    pub fn is_empty(&self) -> bool {
        self.pending_count() == 0
    }

    pub fn run_pending(&self) -> usize {
        let batch: Vec<DeferredTask> =
            std::mem::take(&mut *self.inner.pending.lock().unwrap_or_else(PoisonError::into_inner)).into();
        let count = batch.len();
        for task in batch {
            task();
        }
        count
    }

    fn ensure_drain(&self) {
        if self.inner.draining.swap(true, Ordering::AcqRel) {
            return;
        }
        let inner = Arc::clone(&self.inner);
        (self.inner.spawn)(Box::pin(async move { drain(inner).await }));
    }
}

async fn drain(inner: Arc<Inner>) {
    loop {
        inner.barrier.wait_for_quiet().await;
        let batch: Vec<DeferredTask> = {
            let mut pending = inner.pending.lock().unwrap_or_else(PoisonError::into_inner);
            if pending.is_empty() {
                inner.draining.store(false, Ordering::Release);
                return;
            }
            pending.drain(..).collect()
        };
        if let Some(delay) = inner.delay {
            tokio::time::sleep(delay).await;
        }
        inner.barrier.wait_for_quiet().await;
        for task in batch {
            task();
        }
    }
}

fn spawn_tokio(future: DrainFuture) {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn(future);
        }
        Err(_) => {
            std::thread::spawn(move || {
                if let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() {
                    runtime.block_on(future);
                }
            });
        }
    }
}

fn defer_once(task: DeferredTask) {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn(async move {
                tokio::task::yield_now().await;
                task();
            });
        }
        Err(_) => {
            std::thread::spawn(task);
        }
    }
}

pub fn runtime_microtask() -> Arc<dyn Fn(DeferredTask) + Send + Sync> {
    Arc::new(defer_once)
}

pub fn runtime_macrotask() -> DeferredMacrotask {
    Arc::new(defer_once)
}
