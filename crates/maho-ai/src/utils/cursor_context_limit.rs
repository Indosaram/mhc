//! Port of senpi packages/ai/src/utils/cursor-context-limit.ts together with the in-memory store
//! it wraps (senpi packages/ai/src/cursor/context-limit-store.ts). The cursor/ module belongs to
//! todo 13, so the observed-limit map lives here; brand env `SENPI_CODING_AGENT_DIR` and the
//! `~/.senpi/agent` default become `MAHO_CODING_AGENT_DIR` and `~/.maho/agent` (D-M7).

use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex, MutexGuard};

/// Env lookup seam (`NodeJS.ProcessEnv`); `None` reads the process environment.
pub type EnvLookup<'a> = Option<&'a BTreeMap<String, String>>;

fn env_var(env: EnvLookup<'_>, key: &str) -> Option<String> {
    match env {
        Some(env) => env.get(key).cloned(),
        None => std::env::var(key).ok(),
    }
}

fn trim_trailing_slash(path: &str) -> &str {
    path.strip_suffix('/').unwrap_or(path)
}

pub fn resolve_cursor_context_limit_store_path(env: EnvLookup<'_>) -> String {
    if let Some(path) = env_var(env, "CURSOR_CONTEXT_LIMIT_STORE").filter(|p| !p.is_empty()) {
        return path;
    }
    let agent_dir = env_var(env, "MAHO_CODING_AGENT_DIR").or_else(|| env_var(env, "CODING_AGENT_DIR")).unwrap_or_else(|| {
        let home = env_var(env, "HOME").unwrap_or_else(|| ".".to_owned());
        format!("{}/.maho/agent", trim_trailing_slash(&home))
    });
    format!("{}/cursor-context-limits.json", trim_trailing_slash(&agent_dir))
}

struct Store {
    observed: indexmap::IndexMap<String, f64>,
    hydrated: bool,
    saving_enabled: bool,
}

static STORE: LazyLock<Mutex<Store>> =
    LazyLock::new(|| Mutex::new(Store { observed: indexmap::IndexMap::new(), hydrated: false, saving_enabled: true }));

fn store() -> MutexGuard<'static, Store> {
    STORE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
static TEST_STORE_PATH: Mutex<Option<String>> = Mutex::new(None);

fn store_path() -> String {
    #[cfg(test)]
    if let Some(path) = TEST_STORE_PATH.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone() {
        return path;
    }
    resolve_cursor_context_limit_store_path(None)
}

fn load() -> Vec<(String, f64)> {
    let Ok(text) = std::fs::read_to_string(store_path()) else { return Vec::new() };
    let Ok(Value::Object(parsed)) = serde_json::from_str::<Value>(&text) else { return Vec::new() };
    parsed
        .into_iter()
        .filter_map(|(model_id, max)| max.as_f64().filter(|m| m.is_finite() && *m > 0.0).map(|m| (model_id, m)))
        .collect()
}

fn hydrate(store: &mut Store) {
    if store.hydrated {
        return;
    }
    store.hydrated = true;
    for (model_id, max_tokens) in load() {
        store.observed.entry(model_id).or_insert(max_tokens);
    }
}

fn save(store: &mut Store) {
    if !store.saving_enabled {
        return;
    }
    let path = PathBuf::from(store_path());
    let record: Map<String, Value> = store.observed.iter().map(|(k, v)| (k.clone(), crate::utils::js::json_number(*v))).collect();
    let result = (|| -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temporary = PathBuf::from(format!("{}.{}.tmp", path.display(), std::process::id()));
        let json = serde_json::to_string_pretty(&Value::Object(record)).map_err(std::io::Error::other)?;
        std::fs::write(&temporary, format!("{json}\n"))?;
        std::fs::rename(&temporary, &path)
    })();
    if result.is_err() {
        store.saving_enabled = false;
    }
}

pub fn record_cursor_context_limit(model_id: &str, max_tokens: Option<f64>) {
    let Some(max_tokens) = max_tokens.filter(|m| m.is_finite() && *m > 0.0) else { return };
    let mut store = store();
    hydrate(&mut store);
    if store.observed.get(model_id) == Some(&max_tokens) {
        return;
    }
    store.observed.insert(model_id.to_owned(), max_tokens);
    save(&mut store);
}

pub fn get_cursor_context_limit(model_id: &str) -> Option<f64> {
    let mut store = store();
    hydrate(&mut store);
    store.observed.get(model_id).copied()
}

pub fn resolve_cursor_context_window(model_id: &str, catalog_window: f64) -> f64 {
    get_cursor_context_limit(model_id).unwrap_or(catalog_window)
}

pub fn reset_cursor_context_limit_store_for_test() {
    let mut store = store();
    store.saving_enabled = true;
    store.observed.clear();
    store.hydrated = false;
}

/// senpi packages/ai/test/cursor-context-limit-store.test.ts uses a fresh `mkdtempSync` dir and a
/// per-test `resetCursorContextLimitStoreForTest()` around a process-global store; this harness
/// mirrors that isolation with a process-wide test mutex plus `TEST_STORE_PATH` since Rust's
/// `#[test]` runs share the same `STORE` static within one test binary.
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex as StdMutex, OnceLock};

    /// Serializes access to the process-global `STORE`/`TEST_STORE_PATH` statics across tests in
    /// this module, the way senpi's per-test temp dir plus `resetCursorContextLimitStoreForTest`
    /// isolates each `it()` from the others in the same process.
    fn test_lock() -> &'static StdMutex<()> {
        static LOCK: OnceLock<StdMutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| StdMutex::new(()))
    }

    struct Harness {
        _dir: tempfile::TempDir,
        path: PathBuf,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl Harness {
        fn new() -> Self {
            let guard = test_lock().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("cursor-context-limits.json");
            *TEST_STORE_PATH.lock().expect("path") = Some(path.display().to_string());
            reset_cursor_context_limit_store_for_test();
            Self { _dir: dir, path, _guard: guard }
        }

        fn exists(&self) -> bool {
            self.path.exists()
        }

        fn remove(&self) {
            let _ = std::fs::remove_file(&self.path);
        }

        fn write(&self, contents: &str) {
            std::fs::write(&self.path, contents).expect("write");
        }

        fn read(&self) -> String {
            std::fs::read_to_string(&self.path).expect("saved")
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            reset_cursor_context_limit_store_for_test();
            *TEST_STORE_PATH.lock().expect("path") = None;
        }
    }

    #[test]
    fn serves_a_recorded_ceiling_to_a_process_that_lost_its_in_memory_state() {
        let _h = Harness::new();
        // Given: a ceiling observed by an earlier process.
        record_cursor_context_limit("kimi-k3", Some(200_000.0));

        // When: in-memory state is dropped the way a restart drops it.
        reset_cursor_context_limit_store_for_test();

        // Then: the persisted observation is still the model's ceiling.
        assert_eq!(get_cursor_context_limit("kimi-k3"), Some(200_000.0));
        assert_eq!(resolve_cursor_context_window("kimi-k3", 1_048_576.0), 200_000.0);
    }

    #[test]
    fn falls_back_to_the_catalog_window_for_a_model_that_was_never_observed() {
        let _h = Harness::new();
        // Given: a store holding another model only.
        record_cursor_context_limit("kimi-k3", Some(200_000.0));

        // When/Then: an unobserved model keeps its catalog window.
        assert_eq!(get_cursor_context_limit("gemini-3.5-flash"), None);
        assert_eq!(resolve_cursor_context_window("gemini-3.5-flash", 1_048_576.0), 1_048_576.0);
    }

    #[test]
    fn treats_a_corrupt_store_file_as_empty() {
        let h = Harness::new();
        // Given: a truncated store file.
        h.write(r#"{"kimi-k3": 200"#);

        // When: a fresh read hydrates from it.
        reset_cursor_context_limit_store_for_test();

        // Then: nothing is observed and the catalog window wins.
        assert_eq!(get_cursor_context_limit("kimi-k3"), None);
        assert_eq!(resolve_cursor_context_window("kimi-k3", 1_048_576.0), 1_048_576.0);
    }

    #[test]
    fn ignores_the_non_positive_ceilings_the_first_checkpoint_of_a_turn_reports() {
        let h = Harness::new();
        // Given/When: the values Cursor sends before it knows the conversation size.
        record_cursor_context_limit("kimi-k3", Some(0.0));
        record_cursor_context_limit("kimi-k3", Some(-1.0));
        record_cursor_context_limit("kimi-k3", Some(f64::NAN));
        record_cursor_context_limit("kimi-k3", None);

        // Then: nothing is observed and nothing is persisted.
        assert_eq!(get_cursor_context_limit("kimi-k3"), None);
        assert!(!h.exists());
    }

    #[test]
    fn persists_only_when_the_observed_ceiling_changes() {
        let h = Harness::new();
        // Given: an already persisted ceiling whose file was removed.
        record_cursor_context_limit("kimi-k3", Some(200_000.0));
        h.remove();

        // When: the same ceiling is observed again.
        record_cursor_context_limit("kimi-k3", Some(200_000.0));

        // Then: no rewrite happened; a different ceiling does write.
        assert!(!h.exists());
        record_cursor_context_limit("kimi-k3", Some(262_144.0));
        assert!(h.exists());
        reset_cursor_context_limit_store_for_test();
        assert_eq!(get_cursor_context_limit("kimi-k3"), Some(262_144.0));
    }

    #[test]
    fn resolves_the_store_path_the_way_the_conversation_rotation_store_does() {
        let env = |pairs: &[(&str, &str)]| pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect::<BTreeMap<_, _>>();
        assert_eq!(
            resolve_cursor_context_limit_store_path(Some(&env(&[("CURSOR_CONTEXT_LIMIT_STORE", "/tmp/explicit.json")]))),
            "/tmp/explicit.json"
        );
        assert_eq!(
            resolve_cursor_context_limit_store_path(Some(&env(&[
                ("MAHO_CODING_AGENT_DIR", "/tmp/senpi-agent"),
                ("CODING_AGENT_DIR", "/tmp/x")
            ]))),
            "/tmp/senpi-agent/cursor-context-limits.json"
        );
        assert_eq!(
            resolve_cursor_context_limit_store_path(Some(&env(&[("CODING_AGENT_DIR", "/tmp/legacy-agent/")]))),
            "/tmp/legacy-agent/cursor-context-limits.json"
        );
        assert_eq!(
            resolve_cursor_context_limit_store_path(Some(&env(&[("HOME", "/home/tester")]))),
            "/home/tester/.maho/agent/cursor-context-limits.json"
        );
    }

    /// Extra Rust-only regression covering the round-trip through a rewritten file with mixed
    /// valid/invalid entries, not present as its own senpi `it()` but exercising `load()`'s
    /// filter (finite, positive) beyond what the ported cases above already hit.
    #[test]
    fn rehydrates_from_a_hand_edited_file_ignoring_invalid_entries() {
        let h = Harness::new();
        h.write(r#"{"m": 5, "k": 7, "bad": -1, "s": "x"}"#);
        reset_cursor_context_limit_store_for_test();
        assert_eq!(get_cursor_context_limit("k"), Some(7.0));
        assert_eq!(get_cursor_context_limit("bad"), None);
        assert_eq!(resolve_cursor_context_window("m", 1.0), 5.0);
        assert!(h.read().contains("\"k\""));
    }
}
