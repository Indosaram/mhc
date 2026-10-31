//! Port of senpi packages/coding-agent/src/core/memory-report/memory-report-registry.ts.
//!
//! Process-wide: the RPC handler and the CLI report must see the sessions the session runtime
//! registered. Sessions are keyed by an owner id the caller chooses (upstream keys by object identity).

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use crate::session_resident_store_size::ResidentStoreSize;

/// `SENPI_MEMORY_REPORT=1` installs the on-demand report; nothing is installed or kept without it.
pub const MEMORY_REPORT_ENV: &str = "SENPI_MEMORY_REPORT";
/// `SENPI_MEMORY_REPORT_SNAPSHOT=1` adds a heap snapshot to each report (itself a large allocation).
pub const MEMORY_REPORT_SNAPSHOT_ENV: &str = "SENPI_MEMORY_REPORT_SNAPSHOT";

/// Keys the report owns; an extension reporter cannot shadow them.
pub use maho_ext_api::RESERVED_MEMORY_REPORT_KEYS;

pub fn memory_report_enabled(env: &HashMap<String, String>) -> bool {
    crate::brand::env_value("MEMORY_REPORT", env).as_deref() == Some("1")
}

pub fn memory_report_snapshot_enabled(env: &HashMap<String, String>) -> bool {
    crate::brand::env_value("MEMORY_REPORT_SNAPSHOT", env).as_deref() == Some("1")
}

/// What a live session contributes to its report; read only when a report is requested.
pub trait MemoryReportSessionSource: Send + Sync {
    fn session_id(&self) -> String;
    fn session_file(&self) -> Option<String>;
    fn resident_store(&self) -> ResidentStoreSize;
    fn reporters(&self) -> Vec<maho_ext_api::NamedMemoryReporter>;
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TuiRenderCacheTotals {
    pub components: usize,
    pub cached_lines: usize,
    pub images: usize,
    /// Cards that finished and keep a retained `result` (senpi#1960).
    pub finished_cards: usize,
    /// Byte cost of every live card's cached rendered lines.
    pub cached_lines_bytes: usize,
    /// Serialized size of the retained `result` over every finished card, computed once at finalize.
    pub result_bytes: usize,
}

type Source = Arc<dyn MemoryReportSessionSource>;
type TuiSource = Arc<dyn Fn() -> TuiRenderCacheTotals + Send + Sync>;

#[derive(Default)]
struct State {
    sessions: Vec<(u64, Source)>,
    tui_render_cache: Option<TuiSource>,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::default()));

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Lists `owner`'s session for reports while the flag is on; the returned function removes it (only if
/// it is still the registered source).
pub fn register_memory_report_session(
    owner: u64,
    source: Source,
    env: &HashMap<String, String>,
) -> Box<dyn FnOnce() + Send> {
    if !memory_report_enabled(env) {
        return Box::new(|| {});
    }
    {
        let mut state = state();
        state.sessions.retain(|(key, _)| *key != owner);
        state.sessions.push((owner, Arc::clone(&source)));
    }
    Box::new(move || {
        state()
            .sessions
            .retain(|(key, current)| !(*key == owner && Arc::ptr_eq(current, &source)))
    })
}

pub fn memory_report_session_for(owner: u64) -> Option<Source> {
    state()
        .sessions
        .iter()
        .find(|(key, _)| *key == owner)
        .map(|(_, source)| Arc::clone(source))
}

pub fn memory_report_sessions() -> Vec<Source> {
    state()
        .sessions
        .iter()
        .map(|(_, source)| Arc::clone(source))
        .collect()
}

/// Called by the terminal UI: only a process that renders tool cards reports their render cache.
pub fn register_tui_render_cache_source(read: TuiSource) {
    state().tui_render_cache = Some(read);
}

pub fn tui_render_cache_totals() -> Option<TuiRenderCacheTotals> {
    let read = state().tui_render_cache.clone()?;
    Some(read())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(&'static str);

    impl MemoryReportSessionSource for Fixed {
        fn session_id(&self) -> String {
            self.0.to_owned()
        }
        fn session_file(&self) -> Option<String> {
            None
        }
        fn resident_store(&self) -> ResidentStoreSize {
            ResidentStoreSize {
                entries: 0,
                approx_bytes: 0,
            }
        }
        fn reporters(&self) -> Vec<maho_ext_api::NamedMemoryReporter> {
            Vec::new()
        }
    }

    #[test]
    fn registers_only_with_the_flag_and_unregisters_its_own_source() {
        let off = HashMap::new();
        let _ = register_memory_report_session(91, Arc::new(Fixed("off")), &off);
        assert!(memory_report_session_for(91).is_none());

        let on = HashMap::from([(MEMORY_REPORT_ENV.to_owned(), "1".to_owned())]);
        let first = register_memory_report_session(92, Arc::new(Fixed("a")), &on);
        let _second = register_memory_report_session(92, Arc::new(Fixed("b")), &on);
        first();
        assert_eq!(
            memory_report_session_for(92)
                .map(|s| s.session_id())
                .as_deref(),
            Some("b")
        );
        assert!(RESERVED_MEMORY_REPORT_KEYS.contains(&"heapSnapshot"));
    }
}
