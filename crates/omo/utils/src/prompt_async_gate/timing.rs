use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::types::BoxFuture;

pub const DEFAULT_PROMPT_ASYNC_POST_DISPATCH_HOLD_MS: u64 = 2_000;
pub const DEFAULT_PROMPT_SEMANTIC_DEDUPE_HOLD_MS: u64 = 15_000;
pub const DEFAULT_PROMPT_DISPATCH_TIMEOUT_MS: u64 = 30_000;
pub const DEFAULT_PROMPT_GATE_MESSAGES_FETCH_TIMEOUT_MS: u64 = 5_000;
pub const DEFAULT_PROMPT_QUEUE_RETRY_MS: u64 = 250;

pub trait Clock: Send + Sync {
    fn now_ms(&self) -> u64;
    fn sleep_ms(&self, ms: u64) -> BoxFuture<'static, ()>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(0)
    }

    fn sleep_ms(&self, ms: u64) -> BoxFuture<'static, ()> {
        Box::pin(tokio::time::sleep(Duration::from_millis(ms)))
    }
}

#[derive(Debug, Default)]
pub struct MockClock {
    now: AtomicU64,
}

impl MockClock {
    pub fn new(initial_now_ms: u64) -> Self {
        Self {
            now: AtomicU64::new(initial_now_ms),
        }
    }

    pub fn set_now_ms(&self, now_ms: u64) {
        self.now.store(now_ms, Ordering::SeqCst);
    }

    pub fn advance_ms(&self, ms: u64) {
        self.now.fetch_add(ms, Ordering::SeqCst);
    }
}

impl Clock for MockClock {
    fn now_ms(&self) -> u64 {
        self.now.load(Ordering::SeqCst)
    }

    fn sleep_ms(&self, ms: u64) -> BoxFuture<'static, ()> {
        Box::pin(tokio::time::sleep(Duration::from_millis(ms)))
    }
}

static DEFAULT_CLOCK: LazyLock<Arc<dyn Clock>> = LazyLock::new(|| Arc::new(SystemClock));
static GLOBAL_CLOCK: LazyLock<RwLock<Option<Arc<dyn Clock>>>> = LazyLock::new(|| RwLock::new(None));

pub fn current_clock() -> Arc<dyn Clock> {
    GLOBAL_CLOCK
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
        .cloned()
        .unwrap_or_else(|| DEFAULT_CLOCK.clone())
}

pub fn set_clock_for_testing(clock: Option<Arc<dyn Clock>>) {
    *GLOBAL_CLOCK
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = clock;
}

static PROMPT_GATE_MESSAGES_FETCH_TIMEOUT_MS: LazyLock<RwLock<Option<u64>>> =
    LazyLock::new(|| RwLock::new(None));

pub fn _set_prompt_gate_messages_fetch_timeout_ms_for_testing(value: Option<u64>) {
    *PROMPT_GATE_MESSAGES_FETCH_TIMEOUT_MS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = value;
}

pub fn get_prompt_gate_messages_fetch_timeout_ms() -> u64 {
    PROMPT_GATE_MESSAGES_FETCH_TIMEOUT_MS
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .unwrap_or(DEFAULT_PROMPT_GATE_MESSAGES_FETCH_TIMEOUT_MS)
}

pub fn reset_prompt_gate_timing_for_testing() {
    _set_prompt_gate_messages_fetch_timeout_ms_for_testing(None);
    set_clock_for_testing(None);
}

pub async fn with_dispatch_timeout<F, T>(
    operation: F,
    dispatch_timeout_ms: u64,
    operation_name: &str,
    clock: &dyn Clock,
) -> Result<T, String>
where
    F: Future<Output = Result<T, String>>,
{
    if dispatch_timeout_ms == 0 {
        return operation.await;
    }

    tokio::select! {
        res = operation => res,
        _ = clock.sleep_ms(dispatch_timeout_ms) => {
            Err(format!("{operation_name} timed out after {dispatch_timeout_ms}ms"))
        }
    }
}
