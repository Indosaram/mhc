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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_path_precedence() {
        let env = |pairs: &[(&str, &str)]| pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect::<BTreeMap<_, _>>();
        assert_eq!(resolve_cursor_context_limit_store_path(Some(&env(&[("CURSOR_CONTEXT_LIMIT_STORE", "/x.json"), ("HOME", "/h")]))), "/x.json");
        assert_eq!(resolve_cursor_context_limit_store_path(Some(&env(&[("CODING_AGENT_DIR", "/a/"), ("HOME", "/h")]))), "/a/cursor-context-limits.json");
        assert_eq!(resolve_cursor_context_limit_store_path(Some(&env(&[("HOME", "/h/")]))), "/h/.maho/agent/cursor-context-limits.json");
        assert_eq!(resolve_cursor_context_limit_store_path(Some(&env(&[]))), "./.maho/agent/cursor-context-limits.json");
    }

    /// The only test touching the process-global store.
    #[test]
    fn records_persists_and_rehydrates_limits() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested/limits.json");
        *TEST_STORE_PATH.lock().expect("path") = Some(path.display().to_string());
        reset_cursor_context_limit_store_for_test();
        assert_eq!(resolve_cursor_context_window("m", 1000.0), 1000.0);
        record_cursor_context_limit("m", Some(200_000.0));
        record_cursor_context_limit("m", Some(f64::NAN));
        record_cursor_context_limit("n", Some(0.0));
        assert_eq!(std::fs::read_to_string(&path).expect("saved"), "{\n  \"m\": 200000\n}\n");
        std::fs::write(&path, r#"{"m": 5, "k": 7, "bad": -1, "s": "x"}"#).expect("write");
        reset_cursor_context_limit_store_for_test();
        assert_eq!((get_cursor_context_limit("k"), get_cursor_context_limit("bad")), (Some(7.0), None));
        assert_eq!(resolve_cursor_context_window("m", 1.0), 5.0);
        reset_cursor_context_limit_store_for_test();
        *TEST_STORE_PATH.lock().expect("path") = None;
    }
}
