//! Port of senpi packages/coding-agent/src/core/resolved-paths-memo.ts.
//!
//! Host-scoped memo for package resolution: the product depends only on the agent dir, cwd, the
//! settings content and the CLI-supplied extension sources, so it is memoized on exactly those
//! inputs. senpi stores the pending Promise so N concurrent opens share one resolution; Rust cannot
//! share an in-flight future here, so this port memoizes the resolved value and documents that a
//! caller which starts a second resolution before the first settles recomputes instead of joining.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

const MAX_ENTRIES: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPathsMemoKeyInput {
    pub agent_dir: String,
    pub cwd: String,
    pub global_settings: serde_json::Value,
    pub project_settings: serde_json::Value,
    pub additional_extension_paths: Vec<String>,
}

fn stable(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "null".to_owned(),
        serde_json::Value::Bool(value) => value.to_string(),
        serde_json::Value::Number(value) => value.to_string(),
        serde_json::Value::String(value) => serde_json::Value::String(value.clone()).to_string(),
        serde_json::Value::Array(items) => {
            format!("[{}]", items.iter().map(stable).collect::<Vec<_>>().join(","))
        }
        serde_json::Value::Object(entries) => {
            let mut pairs: Vec<(String, String)> = entries
                .iter()
                .filter(|(_, value)| !value.is_null())
                .map(|(key, value)| (key.clone(), stable(value)))
                .collect();
            pairs.sort();
            format!(
                "{{{}}}",
                pairs
                    .into_iter()
                    .map(|(key, value)| format!("{}:{}", serde_json::Value::String(key).to_string(), value))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
    }
}

pub fn resolved_paths_memo_key(input: &ResolvedPathsMemoKeyInput) -> String {
    let mut object = serde_json::Map::new();
    object.insert("agentDir".into(), serde_json::Value::String(input.agent_dir.clone()));
    object.insert("cwd".into(), serde_json::Value::String(input.cwd.clone()));
    object.insert("globalSettings".into(), input.global_settings.clone());
    object.insert("projectSettings".into(), input.project_settings.clone());
    object.insert(
        "additionalExtensionPaths".into(),
        serde_json::Value::Array(input.additional_extension_paths.iter().cloned().map(serde_json::Value::String).collect()),
    );
    stable(&serde_json::Value::Object(object))
}

type MemoStore = BTreeMap<String, serde_json::Value>;

fn store() -> &'static Mutex<MemoStore> {
    static MEMO: OnceLock<Mutex<MemoStore>> = OnceLock::new();
    MEMO.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn lock() -> std::sync::MutexGuard<'static, MemoStore> {
    store().lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Returns the memoized value for a key, computing and storing it on a miss. A refresh bypasses and
/// overwrites the entry (senpi's re-load signal that disk may have changed).
pub fn memoize_resolved_paths<T, E>(
    key: &str,
    compute: impl FnOnce() -> Result<T, E>,
    refresh: bool,
) -> Result<T, E>
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    if !refresh
        && let Some(cached) = lock().get(key).cloned()
        && let Ok(value) = serde_json::from_value(cached.clone())
    {
        return Ok(value);
    }
    let computed = compute()?;
    let encoded = serde_json::to_value(&computed).map_err(|_| ()).ok();
    let mut guard = lock();
    if let Some(encoded) = encoded {
        if guard.len() >= MAX_ENTRIES
            && !guard.contains_key(key)
            && let Some(oldest) = guard.keys().next().cloned()
        {
            guard.remove(&oldest);
        }
        guard.insert(key.to_owned(), encoded);
    }
    Ok(computed)
}

pub fn clear_resolved_paths_memo() {
    lock().clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn input() -> ResolvedPathsMemoKeyInput {
        ResolvedPathsMemoKeyInput {
            agent_dir: "/agent".into(),
            cwd: "/work".into(),
            global_settings: json!({ "a": 1, "b": 2 }),
            project_settings: json!({}),
            additional_extension_paths: vec!["/x".into()],
        }
    }

    #[test]
    fn the_key_is_stable_regardless_of_object_order() {
        let mut first = input();
        first.global_settings = json!({ "a": 1, "b": 2 });
        let mut second = input();
        second.global_settings = json!({ "b": 2, "a": 1 });
        assert_eq!(resolved_paths_memo_key(&first), resolved_paths_memo_key(&second));
    }

    #[test]
    fn a_different_input_yields_a_different_key() {
        let mut other = input();
        other.cwd = "/elsewhere".into();
        assert_ne!(resolved_paths_memo_key(&input()), resolved_paths_memo_key(&other));
    }

    #[test]
    fn a_hit_does_not_recompute() {
        clear_resolved_paths_memo();
        let calls = AtomicUsize::new(0);
        let compute = || {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok::<_, ()>(vec!["a".to_owned()])
        };
        let key = "k";
        assert_eq!(memoize_resolved_paths(key, compute, false).expect("first"), vec!["a".to_owned()]);
        assert_eq!(memoize_resolved_paths(key, compute, false).expect("second"), vec!["a".to_owned()]);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_refresh_bypasses_the_cache() {
        clear_resolved_paths_memo();
        let key = "refresh";
        let _ = memoize_resolved_paths(key, || Ok::<_, ()>(1u32), false);
        let value = memoize_resolved_paths(key, || Ok::<_, ()>(2u32), true).expect("refresh");
        assert_eq!(value, 2);
    }

    #[test]
    fn the_memo_is_capped_at_sixteen_entries() {
        clear_resolved_paths_memo();
        for index in 0..20 {
            let _ = memoize_resolved_paths(&format!("k{index}"), || Ok::<_, ()>(index), false);
        }
        assert!(lock().len() <= MAX_ENTRIES);
    }
}
