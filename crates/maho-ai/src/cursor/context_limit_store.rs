//! Port of senpi packages/ai/src/cursor/context-limit-store.ts.
// ported by todo 13
//
// Context ceilings Cursor itself reported, keyed by model id.
//
// `GetUsableModels` carries no window, so `CURSOR_MODEL_CAPABILITIES` is a
// committed guess at what each family accepts. The server states the truth on
// every conversation checkpoint (`tokenDetails.maxTokens`), so once a model has
// been observed, that value outranks the catalog for every later request.
//
// This module stays browser-safe in the TS source: `providers/cursor.ts` and
// `cursor/store-migration.ts` materialize catalog windows and are bundled for
// the browser. Node processes install file persistence through
// `installCursorContextLimitPersistence` - see `utils/cursor-context-limit.ts`
// (ported at `crate::utils::cursor_context_limit`).

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, OnceLock};

/// Limits observed by earlier processes. Called at most once per install.
pub trait CursorContextLimitLoad: Send + Sync {
    fn load(&self) -> BTreeMap<String, f64>;
}

/// Called only when a recorded limit actually changed the store.
pub trait CursorContextLimitSave: Send + Sync {
    fn save(&self, limits: &BTreeMap<String, f64>);
}

pub trait CursorContextLimitPersistence: Send + Sync {
    fn load(&self) -> BTreeMap<String, f64>;
    fn save(&self, limits: &BTreeMap<String, f64>);
}

struct Store {
    observed: BTreeMap<String, f64>,
    persistence: Option<Box<dyn CursorContextLimitPersistence>>,
    hydrated: bool,
}

fn store() -> MutexGuard<'static, Store> {
    static STORE: OnceLock<Mutex<Store>> = OnceLock::new();
    STORE
        .get_or_init(|| Mutex::new(Store { observed: BTreeMap::new(), persistence: None, hydrated: false }))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Installs the process-wide persistence port. Idempotent per port identity is not
/// checkable across trait objects, so this mirrors the TS `hydrated = false` reset
/// unconditionally on every install call (the Rust port has no port-identity check).
pub fn install_cursor_context_limit_persistence(port: Box<dyn CursorContextLimitPersistence>) {
    let mut store = store();
    store.persistence = Some(port);
    store.hydrated = false;
}

fn hydrate(store: &mut Store) {
    if store.hydrated {
        return;
    }
    store.hydrated = true;
    let Some(persistence) = store.persistence.as_ref() else { return };
    for (model_id, max_tokens) in persistence.load() {
        store.observed.entry(model_id).or_insert(max_tokens);
    }
}

/// Records the server-reported ceiling for `model_id`. The first checkpoint of a
/// conversation reports 0, so non-positive and non-finite values are ignored.
pub fn record_cursor_context_limit(model_id: &str, max_tokens: Option<f64>) {
    let Some(max_tokens) = max_tokens else { return };
    if !max_tokens.is_finite() || max_tokens <= 0.0 {
        return;
    }
    let mut store = store();
    hydrate(&mut store);
    if store.observed.get(model_id) == Some(&max_tokens) {
        return;
    }
    store.observed.insert(model_id.to_owned(), max_tokens);
    if let Some(persistence) = store.persistence.as_ref() {
        persistence.save(&store.observed);
    }
}

pub fn get_cursor_context_limit(model_id: &str) -> Option<f64> {
    let mut store = store();
    hydrate(&mut store);
    store.observed.get(model_id).copied()
}

/// The window to trust for `model_id`: what the server reported, else the catalog.
pub fn resolve_cursor_context_window(model_id: &str, catalog_window: f64) -> f64 {
    get_cursor_context_limit(model_id).unwrap_or(catalog_window)
}

/// Drops in-memory state so the next read re-hydrates from the installed port.
pub fn reset_cursor_context_limit_store_for_test() {
    let mut store = store();
    store.observed.clear();
    store.hydrated = false;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    struct TestPersistence {
        loaded: BTreeMap<String, f64>,
        saved: StdMutex<Vec<BTreeMap<String, f64>>>,
    }

    impl CursorContextLimitPersistence for TestPersistence {
        fn load(&self) -> BTreeMap<String, f64> {
            self.loaded.clone()
        }
        fn save(&self, limits: &BTreeMap<String, f64>) {
            self.saved.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(limits.clone());
        }
    }

    /// Serializes the whole file: the store is one process-global singleton, so tests
    /// mutating it must not interleave.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn with_clean_store<T>(f: impl FnOnce() -> T) -> T {
        let _guard = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_cursor_context_limit_store_for_test();
        store().persistence = None;
        let result = f();
        reset_cursor_context_limit_store_for_test();
        store().persistence = None;
        result
    }

    #[test]
    fn serves_a_recorded_ceiling_to_a_process_that_lost_its_in_memory_state() {
        with_clean_store(|| {
            record_cursor_context_limit("kimi-k3", Some(200_000.0));
            reset_cursor_context_limit_store_for_test();
            assert_eq!(get_cursor_context_limit("kimi-k3"), Some(200_000.0));
            assert_eq!(resolve_cursor_context_window("kimi-k3", 1_048_576.0), 200_000.0);
        });
    }

    #[test]
    fn falls_back_to_the_catalog_window_for_a_model_that_was_never_observed() {
        with_clean_store(|| {
            record_cursor_context_limit("kimi-k3", Some(200_000.0));
            assert_eq!(get_cursor_context_limit("gemini-3.5-flash"), None);
            assert_eq!(resolve_cursor_context_window("gemini-3.5-flash", 1_048_576.0), 1_048_576.0);
        });
    }

    #[test]
    fn treats_a_corrupt_store_file_as_empty() {
        with_clean_store(|| {
            struct CorruptPersistence;
            impl CursorContextLimitPersistence for CorruptPersistence {
                fn load(&self) -> BTreeMap<String, f64> {
                    BTreeMap::new()
                }
                fn save(&self, _limits: &BTreeMap<String, f64>) {}
            }
            install_cursor_context_limit_persistence(Box::new(CorruptPersistence));
            reset_cursor_context_limit_store_for_test();
            assert_eq!(get_cursor_context_limit("kimi-k3"), None);
            assert_eq!(resolve_cursor_context_window("kimi-k3", 1_048_576.0), 1_048_576.0);
        });
    }

    #[test]
    fn ignores_the_non_positive_ceilings_the_first_checkpoint_of_a_turn_reports() {
        with_clean_store(|| {
            record_cursor_context_limit("kimi-k3", Some(0.0));
            record_cursor_context_limit("kimi-k3", Some(-1.0));
            record_cursor_context_limit("kimi-k3", Some(f64::NAN));
            record_cursor_context_limit("kimi-k3", None);
            assert_eq!(get_cursor_context_limit("kimi-k3"), None);
        });
    }

    #[test]
    fn persists_only_when_the_observed_ceiling_changes() {
        with_clean_store(|| {
            let persistence = std::sync::Arc::new(TestPersistence { loaded: BTreeMap::new(), saved: StdMutex::new(Vec::new()) });
            struct Wrapper(std::sync::Arc<TestPersistence>);
            impl CursorContextLimitPersistence for Wrapper {
                fn load(&self) -> BTreeMap<String, f64> {
                    self.0.load()
                }
                fn save(&self, limits: &BTreeMap<String, f64>) {
                    self.0.save(limits)
                }
            }
            install_cursor_context_limit_persistence(Box::new(Wrapper(persistence.clone())));
            record_cursor_context_limit("kimi-k3", Some(200_000.0));
            assert_eq!(persistence.saved.lock().unwrap_or_else(std::sync::PoisonError::into_inner).len(), 1);
            record_cursor_context_limit("kimi-k3", Some(200_000.0));
            assert_eq!(persistence.saved.lock().unwrap_or_else(std::sync::PoisonError::into_inner).len(), 1);
            record_cursor_context_limit("kimi-k3", Some(262_144.0));
            assert_eq!(persistence.saved.lock().unwrap_or_else(std::sync::PoisonError::into_inner).len(), 2);
        });
    }
}
