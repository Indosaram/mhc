//! Persisted long-context refusal state (senpi `long-context.ts`).
use std::{
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex, PoisonError},
};

/// How long a refusal holds before the 1M window is tried again.
pub const LONG_CONTEXT_REFUSAL_TTL_MS: u64 = 6 * 60 * 60 * 1_000;
pub const STATE_FILE: &str = "anthropic-subscription-long-context.json";
/// Refusals before version 2 were recorded for native-1M models too (senpi#3801); such a record is dropped on first read.
pub const STATE_VERSION: u64 = 2;

type Listener = Box<dyn Fn(bool) + Send + Sync>;

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

static STATE: LazyLock<Mutex<LongContextState>> =
    LazyLock::new(|| Mutex::new(LongContextState::new(None)));
static LISTENERS: LazyLock<Mutex<Vec<Listener>>> = LazyLock::new(Default::default);

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

/// Whether the subscription lane may request the 1M window.
pub fn long_context_available() -> bool {
    STATE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .available(now_ms())
}

pub fn mark_long_context_refused() {
    let was_available = STATE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .mark_refused(now_ms());
    if was_available {
        for listener in LISTENERS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
        {
            listener(false);
        }
    }
}

pub fn on_long_context_availability_change(listener: impl Fn(bool) + Send + Sync + 'static) {
    LISTENERS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(Box::new(listener));
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
}
