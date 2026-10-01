//! Port of senpi packages/agent/src/harness/utils/adaptive-publisher.ts.

use std::sync::{Arc, Mutex};

/// The clock and timer a publisher uses. Injectable so tests are deterministic.
pub trait PublisherClock: Send + Sync {
    /// `Date.now()`.
    fn now_ms(&self) -> i64;
    /// `setTimeout(callback, wait)`.
    fn set_timeout(&self, wait_ms: u64, callback: Box<dyn FnOnce() + Send>);
    /// `clearTimeout(handle)`.
    fn clear_timeout(&self);
}

/// Wall-clock clock backed by the system time and tokio timers.
#[derive(Default)]
pub struct SystemPublisherClock;

impl PublisherClock for SystemPublisherClock {
    fn now_ms(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis() as i64)
            .unwrap_or(0)
    }

    fn set_timeout(&self, wait_ms: u64, callback: Box<dyn FnOnce() + Send>) {
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(wait_ms)).await;
            callback();
        });
    }

    fn clear_timeout(&self) {}
}

/// `snapshot()` of the published value.
pub type SnapshotFn<TValue> = Box<dyn Fn() -> TValue + Send + Sync>;

/// `update(previous, current)`.
pub type UpdateFn<TValue, TUpdate> = Box<dyn Fn(Option<&TValue>, &TValue) -> Option<TUpdate> + Send + Sync>;

/// `measure(update)`.
pub type MeasureFn<TUpdate> = Box<dyn Fn(&TUpdate) -> u64 + Send + Sync>;

/// `publish(update)`.
pub type PublishFn<TUpdate> = Box<dyn Fn(TUpdate) + Send + Sync>;

/// `onError(error)`.
pub type ErrorFn = Box<dyn Fn(String) + Send + Sync>;

/// `AdaptivePublisherOptions<TValue, TUpdate>`.
pub struct AdaptivePublisherOptions<TValue, TUpdate> {
    pub snapshot: SnapshotFn<TValue>,
    pub update: UpdateFn<TValue, TUpdate>,
    pub measure: MeasureFn<TUpdate>,
    pub publish: PublishFn<TUpdate>,
    pub on_error: ErrorFn,
    pub min_interval_ms: Option<u64>,
    pub target_bytes_per_second: Option<u64>,
}

struct PublisherState<TValue> {
    published: Option<TValue>,
    dirty: bool,
    next_emit_at: i64,
    disposed: bool,
    timer_armed: bool,
}

struct Inner<TValue, TUpdate> {
    options: AdaptivePublisherOptions<TValue, TUpdate>,
    min_interval_ms: u64,
    target_bytes_per_second: u64,
    clock: Arc<dyn PublisherClock>,
    state: Mutex<PublisherState<TValue>>,
}

/// `AdaptivePublisher`: publishes the latest state without queuing intermediate mutations.
pub struct AdaptivePublisher<TValue, TUpdate> {
    inner: Arc<Inner<TValue, TUpdate>>,
}

impl<TValue, TUpdate> Clone for AdaptivePublisher<TValue, TUpdate> {
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone() }
    }
}

impl<TValue: Clone + Send + Sync + 'static, TUpdate: Send + 'static> AdaptivePublisher<TValue, TUpdate> {
    pub fn new(options: AdaptivePublisherOptions<TValue, TUpdate>, clock: Arc<dyn PublisherClock>) -> Self {
        let min_interval_ms = options.min_interval_ms.unwrap_or(100);
        let target_bytes_per_second = options.target_bytes_per_second.unwrap_or(100 * 1024);
        Self {
            inner: Arc::new(Inner {
                options,
                min_interval_ms,
                target_bytes_per_second,
                clock,
                state: Mutex::new(PublisherState {
                    published: None,
                    dirty: false,
                    next_emit_at: 0,
                    disposed: false,
                    timer_armed: false,
                }),
            }),
        }
    }

    /// `markDirty()`.
    pub fn mark_dirty(&self) {
        let wait = {
            let mut state = self.inner.state.lock().expect("publisher state poisoned");
            if state.disposed {
                return;
            }
            state.dirty = true;
            state.next_emit_at - self.inner.clock.now_ms()
        };
        if wait <= 0 {
            self.flush(false);
            return;
        }
        self.arm_timer(wait as u64);
    }

    /// `flush(force)`.
    pub fn flush(&self, force: bool) {
        let now = self.inner.clock.now_ms();
        let current = {
            let mut state = self.inner.state.lock().expect("publisher state poisoned");
            if state.disposed || !state.dirty {
                return;
            }
            if !force && now < state.next_emit_at {
                let wait = (state.next_emit_at - now) as u64;
                drop(state);
                self.arm_timer(wait);
                return;
            }
            self.inner.clock.clear_timeout();
            state.timer_armed = false;
            (self.inner.options.snapshot)()
        };

        let previous = self.inner.state.lock().expect("publisher state poisoned").published.clone();
        let update = (self.inner.options.update)(previous.as_ref(), &current);

        let Some(update) = update else {
            let mut state = self.inner.state.lock().expect("publisher state poisoned");
            state.published = Some(current);
            state.dirty = false;
            return;
        };

        let encoded_bytes = (self.inner.options.measure)(&update);
        {
            let mut state = self.inner.state.lock().expect("publisher state poisoned");
            state.published = Some(current);
            state.dirty = false;
            let delay = self
                .inner
                .min_interval_ms
                .max(encoded_bytes.saturating_mul(1000) / self.inner.target_bytes_per_second);
            state.next_emit_at = now + delay as i64;
        }
        // Commit before delivery. A consumer may apply the update and then throw or reenter the
        // producer; retaining the old baseline would duplicate that delta.
        (self.inner.options.publish)(update);
    }

    /// `dispose()`.
    pub fn dispose(&self) {
        self.inner.clock.clear_timeout();
        let mut state = self.inner.state.lock().expect("publisher state poisoned");
        state.timer_armed = false;
        state.disposed = true;
    }

    fn arm_timer(&self, wait_ms: u64) {
        {
            let mut state = self.inner.state.lock().expect("publisher state poisoned");
            if state.timer_armed || state.disposed {
                return;
            }
            state.timer_armed = true;
        }
        let inner = self.inner.clone();
        self.inner.clock.set_timeout(
            wait_ms,
            Box::new(move || {
                {
                    let mut state = inner.state.lock().expect("publisher state poisoned");
                    state.timer_armed = false;
                }
                let result =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| AdaptivePublisher { inner: inner.clone() }.flush(false)));
                if result.is_err() {
                    (inner.options.on_error)("adaptive publisher flush panicked".to_string());
                }
            }),
        );
    }
}
