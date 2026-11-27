//! Port of senpi `packages/coding-agent/src/modes/rpc/session-worker-permits.ts`.
//!
//! Payload permits for session workers (S12 design section 3, RULING-63 Q3, RULING-69).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const FRAGMENT_MAX_BYTES: usize = 131_072;
pub const FRAGMENT_ENVELOPE_MAX_BYTES: usize = FRAGMENT_MAX_BYTES.div_ceil(3) * 4;
pub const TRANSFER_STAGING_MAX_BYTES: usize = 64 * 1024 * 1024;

pub const WORKER_PERMIT_BYTES: usize = FRAGMENT_ENVELOPE_MAX_BYTES;
pub const WORKER_CONTROL_SLOTS: usize = 1;

pub mod permit_word {
    pub const CREDIT: usize = 0;
    pub const HOST: usize = 1;
    pub const WORKER: usize = 2;
    pub const WAKE: usize = 3;
}

pub const PERMIT_BLOCK_BYTES: usize = 4 * std::mem::size_of::<i32>();

pub mod permit_tag {
    pub const IDLE: i32 = 0;
    pub const GRANTED: i32 = 1;
    pub const DENIED: i32 = 2;
    pub const CLOSED: i32 = 3;
    pub const WAITING: i32 = 4;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermitOutcome {
    Granted,
    Denied,
    Timeout,
    Closed,
}

impl PermitOutcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Granted => "granted",
            Self::Denied => "denied",
            Self::Timeout => "timeout",
            Self::Closed => "closed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PermitLedgerStatus {
    pub granted: usize,
    pub waiting: usize,
    pub ceiling: usize,
}

type GrantCallback = Arc<dyn Fn() + Send + Sync>;

struct LedgerInner {
    ceiling: usize,
    granted: usize,
    waiting: VecDeque<GrantCallback>,
}

/// The host's payload capacity for every session worker (S12 design section 3).
pub struct WorkerPermitLedger {
    inner: Mutex<LedgerInner>,
}

impl Default for WorkerPermitLedger {
    fn default() -> Self {
        Self::new(TRANSFER_STAGING_MAX_BYTES)
    }
}

impl WorkerPermitLedger {
    pub fn new(capacity: usize) -> Self {
        let ceiling = (capacity / WORKER_PERMIT_BYTES).saturating_sub(WORKER_CONTROL_SLOTS);
        Self {
            inner: Mutex::new(LedgerInner {
                ceiling,
                granted: 0,
                waiting: VecDeque::new(),
            }),
        }
    }

    pub fn ceiling(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .ceiling
    }

    pub fn request(&self, grant: GrantCallback) -> bool {
        let run = {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if inner.granted >= inner.ceiling {
                inner.waiting.push_back(grant);
                return false;
            }
            inner.granted += 1;
            grant
        };
        run();
        true
    }

    pub fn release(&self) {
        let next = {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            inner.granted = inner.granted.saturating_sub(1);
            let next = inner.waiting.pop_front();
            if next.is_some() {
                inner.granted += 1;
            }
            next
        };
        if let Some(cb) = next {
            cb();
        }
    }

    pub fn withdraw(&self, grant: &GrantCallback) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.waiting.retain(|w| !Arc::ptr_eq(w, grant));
    }

    pub fn status(&self) -> PermitLedgerStatus {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        PermitLedgerStatus {
            granted: inner.granted,
            waiting: inner.waiting.len(),
            ceiling: inner.ceiling,
        }
    }
}

static HOST_WORKER_PERMITS: OnceLock<Arc<WorkerPermitLedger>> = OnceLock::new();

pub fn host_worker_permits() -> Arc<WorkerPermitLedger> {
    HOST_WORKER_PERMITS
        .get_or_init(|| Arc::new(WorkerPermitLedger::default()))
        .clone()
}

/// Shared permit block replacing `SharedArrayBuffer` + `Atomics.wait`/`Atomics.notify`.
pub struct PermitBlock {
    words: [AtomicI32; 4],
    wait_lock: Mutex<()>,
    wake_cv: Condvar,
    worker_cv: Condvar,
}

impl Default for PermitBlock {
    fn default() -> Self {
        Self {
            words: [
                AtomicI32::new(0),
                AtomicI32::new(permit_tag::IDLE),
                AtomicI32::new(permit_tag::IDLE),
                AtomicI32::new(0),
            ],
            wait_lock: Mutex::new(()),
            wake_cv: Condvar::new(),
            worker_cv: Condvar::new(),
        }
    }
}

impl PermitBlock {
    pub fn load(&self, word: usize) -> i32 {
        self.words[word].load(Ordering::SeqCst)
    }

    pub fn store(&self, word: usize, value: i32) {
        self.words[word].store(value, Ordering::SeqCst);
    }

    pub fn fetch_add(&self, word: usize, delta: i32) -> i32 {
        self.words[word].fetch_add(delta, Ordering::SeqCst)
    }

    pub fn compare_exchange(&self, word: usize, current: i32, new: i32) -> Result<i32, i32> {
        self.words[word].compare_exchange(current, new, Ordering::SeqCst, Ordering::SeqCst)
    }

    pub fn notify_wake(&self) {
        let _guard = self
            .wait_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.wake_cv.notify_all();
    }

    pub fn notify_worker(&self) {
        let _guard = self
            .wait_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.worker_cv.notify_all();
    }

    pub fn wait_wake(&self, expected_wake: i32, timeout: Duration) {
        let guard = self
            .wait_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.load(permit_word::WAKE) == expected_wake {
            let _ = self.wake_cv.wait_timeout(guard, timeout);
        }
    }
}

struct HostPermitsInner {
    holding: bool,
    closed: bool,
}

/// The host half of one worker incarnation's permit block.
pub struct HostPermits {
    pub block: Arc<PermitBlock>,
    ledger: Arc<WorkerPermitLedger>,
    state: Mutex<HostPermitsInner>,
    grant_cb: Mutex<Option<GrantCallback>>,
}

impl HostPermits {
    pub fn new(ledger: Arc<WorkerPermitLedger>) -> Arc<Self> {
        let block = Arc::new(PermitBlock::default());
        let permits = Arc::new(Self {
            block: block.clone(),
            ledger,
            state: Mutex::new(HostPermitsInner {
                holding: false,
                closed: false,
            }),
            grant_cb: Mutex::new(None),
        });
        let weak = Arc::downgrade(&permits);
        let cb: GrantCallback = Arc::new(move || {
            let Some(this) = weak.upgrade() else {
                return;
            };
            {
                let mut st = this
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if st.closed {
                    return;
                }
                st.holding = true;
            }
            this.block
                .fetch_add(permit_word::CREDIT, WORKER_PERMIT_BYTES as i32);
            this.signal(permit_tag::GRANTED);
        });
        *permits
            .grant_cb
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(cb);
        permits.request();
        permits
    }

    pub fn returned(&self) {
        let should_release = {
            let mut st = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if st.closed
                || !st.holding
                || self.block.load(permit_word::CREDIT) >= WORKER_PERMIT_BYTES as i32
            {
                return;
            }
            st.holding = false;
            true
        };
        if should_release {
            self.ledger.release();
            self.request();
        }
    }

    pub fn close(&self) {
        let holding = {
            let mut st = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if st.closed {
                return;
            }
            st.closed = true;
            let was = st.holding;
            st.holding = false;
            was
        };
        self.signal(permit_tag::CLOSED);
        if holding {
            self.ledger.release();
        } else if let Some(cb) = self
            .grant_cb
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            self.ledger.withdraw(cb);
        }
    }

    fn request(&self) {
        let Some(cb) = self
            .grant_cb
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        else {
            return;
        };
        if !self.ledger.request(cb) {
            self.signal(permit_tag::DENIED);
        }
    }

    fn signal(&self, tag: i32) {
        self.block.store(permit_word::HOST, tag);
        self.block.fetch_add(permit_word::WAKE, 1);
        self.block.notify_wake();
    }
}

/// The worker half of a permit block: takes one permit before the worker builds any payload.
pub struct WorkerPermitGate {
    block: Arc<PermitBlock>,
    timeout: Duration,
}

impl WorkerPermitGate {
    pub fn new(block: Arc<PermitBlock>, timeout_ms: u64) -> Self {
        Self {
            block,
            timeout: Duration::from_millis(timeout_ms),
        }
    }

    pub fn acquire(&self) -> PermitOutcome {
        let deadline = Instant::now() + self.timeout;
        struct IdleOnDrop<'a>(&'a WorkerPermitGate);
        impl Drop for IdleOnDrop<'_> {
            fn drop(&mut self) {
                self.0.mark(permit_tag::IDLE);
            }
        }
        let _guard = IdleOnDrop(self);
        loop {
            let wake = self.block.load(permit_word::WAKE);
            if self.block.load(permit_word::HOST) == permit_tag::CLOSED {
                return PermitOutcome::Closed;
            }
            let credit = self.block.load(permit_word::CREDIT);
            if credit >= WORKER_PERMIT_BYTES as i32 {
                if self
                    .block
                    .compare_exchange(
                        permit_word::CREDIT,
                        credit,
                        credit - WORKER_PERMIT_BYTES as i32,
                    )
                    .is_ok()
                {
                    return PermitOutcome::Granted;
                }
                continue;
            }
            let now = Instant::now();
            if now >= deadline {
                return if self.block.load(permit_word::HOST) == permit_tag::DENIED {
                    PermitOutcome::Denied
                } else {
                    PermitOutcome::Timeout
                };
            }
            self.mark(permit_tag::WAITING);
            self.block
                .wait_wake(wake, deadline.saturating_duration_since(now));
        }
    }

    fn mark(&self, tag: i32) {
        self.block.store(permit_word::WORKER, tag);
        self.block.notify_worker();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permits_grant_queue_deny_and_close() {
        let ledger = Arc::new(WorkerPermitLedger::new(2 * WORKER_PERMIT_BYTES));
        assert_eq!(ledger.ceiling(), 1);
        let h1 = HostPermits::new(ledger.clone());
        let g1 = WorkerPermitGate::new(h1.block.clone(), 500);
        assert_eq!(g1.acquire(), PermitOutcome::Granted);

        let h2 = HostPermits::new(ledger.clone());
        let g2 = WorkerPermitGate::new(h2.block.clone(), 1);
        assert_eq!(g2.acquire(), PermitOutcome::Denied);

        let block_clone = h2.block.clone();
        let waiter = std::thread::spawn(move || {
            let g2_long = WorkerPermitGate::new(block_clone, 5_000);
            g2_long.acquire()
        });
        // Returning h1's spent permit grants h2 immediately.
        h1.returned();
        assert_eq!(waiter.join().expect("join"), PermitOutcome::Granted);

        h1.close();
        h2.close();
        assert_eq!(g1.acquire(), PermitOutcome::Closed);
    }
}
