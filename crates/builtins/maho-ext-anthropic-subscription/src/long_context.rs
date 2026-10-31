//! Persisted long-context refusal state (senpi `long-context.ts`).
use std::{
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Mutex, PoisonError},
};

/// How long a refusal holds before the 1M window is tried again.
pub const LONG_CONTEXT_REFUSAL_TTL_MS: u64 = 6 * 60 * 60 * 1_000;
pub const STATE_FILE: &str = "anthropic-subscription-long-context.json";
/// Refusals before version 2 were recorded for native-1M models too (senpi#3801); such a record is dropped on first read.
pub const STATE_VERSION: u64 = 2;

type Listener = Arc<dyn Fn(bool) + Send + Sync>;

pub struct LongContextState {
    path: Option<PathBuf>,
    refused_until: Option<u64>,
    loaded: bool,
}

impl LongContextState {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self {
            path,
            refused_until: None,
            loaded: false,
        }
    }
    fn state_path(&self) -> PathBuf {
        self.path
            .clone()
            .unwrap_or_else(|| Path::new(&maho_core::config::get_agent_dir()).join(STATE_FILE))
    }
    fn load(&mut self) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        let path = self.state_path();
        // No state yet, or an unreadable file: the 1M window is assumed available and the next refusal records it.
        let Ok(text) = std::fs::read_to_string(&path) else {
            return;
        };
        let state =
            serde_json::from_str::<serde_json::Value>(&text).unwrap_or(serde_json::Value::Null);
        if state["version"].as_u64() != Some(STATE_VERSION) {
            let _ = std::fs::remove_file(&path);
            return;
        }
        self.refused_until = state["refusedUntil"]
            .as_f64()
            .filter(|until| until.is_finite() && *until >= 0.0)
            .map(|until| until as u64);
    }
    pub fn available(&mut self, now: u64) -> bool {
        self.load();
        self.refused_until.is_none_or(|until| now >= until)
    }
    /// Records a refusal; returns whether the window was available before it.
    pub fn mark_refused(&mut self, now: u64) -> bool {
        self.load();
        let was_available = self.available(now);
        let until = now + LONG_CONTEXT_REFUSAL_TTL_MS;
        self.refused_until = Some(until);
        let _ = self.persist(until);
        was_available
    }
    fn persist(&self, until: u64) -> std::io::Result<()> {
        let path = self.state_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temporary = PathBuf::from(format!("{}.{}.tmp", path.display(), std::process::id()));
        std::fs::write(
            &temporary,
            format!(
                "{}\n",
                serde_json::json!({"version":STATE_VERSION,"refusedUntil":until})
            ),
        )?;
        std::fs::rename(&temporary, &path)
    }
}

type Clock = Box<dyn Fn() -> u64 + Send + Sync>;

/// The refusal state, its listeners and the timer that offers the 1M window again (senpi `scheduleExpiry`).
pub struct LongContext {
    state: Mutex<LongContextState>,
    listeners: Mutex<Vec<Listener>>,
    expiry: Mutex<Option<tokio::task::JoinHandle<()>>>,
    now: Clock,
}

impl LongContext {
    pub fn new(path: Option<PathBuf>, now: impl Fn() -> u64 + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(LongContextState::new(path)),
            listeners: Mutex::default(),
            expiry: Mutex::default(),
            now: Box::new(now),
        })
    }
    pub fn available(self: &Arc<Self>) -> bool {
        let now = (self.now)();
        let (available, refused_until) = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            (state.available(now), state.refused_until)
        };
        // A refusal read from disk (another process recorded it) expires here too.
        let unscheduled = self
            .expiry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_none_or(tokio::task::JoinHandle::is_finished);
        if !available
            && unscheduled
            && let Some(until) = refused_until
        {
            self.schedule_expiry(until.saturating_sub(now));
        }
        available
    }
    pub fn mark_refused(self: &Arc<Self>) {
        let now = (self.now)();
        let was_available = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .mark_refused(now);
        self.schedule_expiry(LONG_CONTEXT_REFUSAL_TTL_MS);
        if was_available {
            self.notify(false);
        }
    }
    pub fn on_change(&self, listener: impl Fn(bool) + Send + Sync + 'static) {
        self.listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Arc::new(listener));
    }
    /// Offers the 1M window again when the refusal expires (the account may have enabled extra usage). Without a
    /// runtime nothing is scheduled; the next `available` call inside one schedules it.
    fn schedule_expiry(self: &Arc<Self>, delay_ms: u64) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let weak = Arc::downgrade(self);
        let timer = runtime.spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            if let Some(long_context) = weak.upgrade() {
                long_context.notify(true);
            }
        });
        if let Some(previous) = self
            .expiry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .replace(timer)
        {
            previous.abort();
        }
    }
    fn notify(&self, available: bool) {
        // Listeners run outside the lock: one may register again or read the state.
        let listeners = self
            .listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        for listener in listeners {
            listener(available);
        }
    }
}

static LONG_CONTEXT: LazyLock<Arc<LongContext>> = LazyLock::new(|| LongContext::new(None, now_ms));

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

/// Whether the subscription lane may request the 1M window.
pub fn long_context_available() -> bool {
    LONG_CONTEXT.available()
}

pub fn mark_long_context_refused() {
    LONG_CONTEXT.mark_refused();
}

/// Called with `false` when the 1M window is refused and with `true` when that refusal expires.
pub fn on_long_context_availability_change(listener: impl Fn(bool) + Send + Sync + 'static) {
    LONG_CONTEXT.on_change(listener);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refusal_is_versioned_atomic_and_expires() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join(STATE_FILE);
        let mut state = LongContextState::new(Some(path.clone()));
        assert!(state.available(10));
        assert!(state.mark_refused(10));
        assert!(!state.available(11));
        let stored: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("state")).expect("json");
        assert_eq!(
            stored,
            serde_json::json!({"version":2,"refusedUntil":10 + LONG_CONTEXT_REFUSAL_TTL_MS})
        );
        assert!(
            !dir.path()
                .join(format!("{STATE_FILE}.{}.tmp", std::process::id()))
                .exists()
        );
        let mut reloaded = LongContextState::new(Some(path));
        assert!(!reloaded.available(11));
        assert!(reloaded.available(10 + LONG_CONTEXT_REFUSAL_TTL_MS));
    }
    #[test]
    fn unversioned_state_is_removed_and_ignored() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join(STATE_FILE);
        std::fs::write(&path, "{\"refusedUntil\":99999999999999}").expect("seed");
        let mut state = LongContextState::new(Some(path.clone()));
        assert!(state.available(1));
        assert!(!path.exists());
    }
    /// The timer wheel rounds a deadline up to the next millisecond.
    fn assert_expired_at_ttl(started: tokio::time::Instant) {
        let ttl = std::time::Duration::from_millis(LONG_CONTEXT_REFUSAL_TTL_MS);
        let elapsed = started.elapsed();
        assert!(
            elapsed >= ttl && elapsed <= ttl + std::time::Duration::from_millis(1),
            "{elapsed:?}"
        );
    }
    /// Bounds a wait on the change channel so a missing expiry timer fails the test instead of hanging it.
    async fn next_change(changed: &mut tokio::sync::mpsc::UnboundedReceiver<bool>) -> Option<bool> {
        let bound = std::time::Duration::from_millis(LONG_CONTEXT_REFUSAL_TTL_MS * 2);
        tokio::time::timeout(bound, changed.recv())
            .await
            .expect("long-context change was not delivered")
    }
    #[tokio::test(start_paused = true)]
    async fn an_expired_refusal_offers_the_long_context_window_again() {
        let dir = tempfile::tempdir().expect("dir");
        let clock = Arc::new(std::sync::atomic::AtomicU64::new(1_000));
        let now = clock.clone();
        let long_context = LongContext::new(Some(dir.path().join(STATE_FILE)), move || {
            now.load(std::sync::atomic::Ordering::SeqCst)
        });
        let (changes, mut changed) = tokio::sync::mpsc::unbounded_channel();
        long_context.on_change(move |available| {
            let _ = changes.send(available);
        });
        let started = tokio::time::Instant::now();
        long_context.mark_refused();
        assert_eq!(next_change(&mut changed).await, Some(false));
        assert!(!long_context.available());
        // Paused time auto-advances to the expiry timer and no further.
        assert_eq!(next_change(&mut changed).await, Some(true));
        assert_expired_at_ttl(started);
        clock.fetch_add(
            LONG_CONTEXT_REFUSAL_TTL_MS,
            std::sync::atomic::Ordering::SeqCst,
        );
        assert!(long_context.available());
        // A refusal recorded by another process expires in this one too.
        let reloaded = LongContext::new(Some(dir.path().join(STATE_FILE)), || 1_000);
        let (changes, mut changed) = tokio::sync::mpsc::unbounded_channel();
        reloaded.on_change(move |available| {
            let _ = changes.send(available);
        });
        let started = tokio::time::Instant::now();
        assert!(!reloaded.available());
        assert_eq!(next_change(&mut changed).await, Some(true));
        assert_expired_at_ttl(started);
    }
}
