//! Port of senpi packages/coding-agent/src/core/settings-retired-keys.ts.

use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

use serde_json::{Map, Value};

use crate::hidden_stdout_log::append_debug_log_entry;
use crate::settings_manager::{SettingsScope, SettingsStorage, parse_settings_json};

/// Settings keys whose feature no longer exists. Each entry is `(parent, key)`: the key is deleted from the
/// parent object, and the parent is dropped once nothing is left in it.
const RETIRED_NESTED_KEYS: &[(&str, &str)] = &[("experimental", "sharedHost")];

/// Deletes every retired key from a raw parsed settings object in place; true when one was present.
pub fn remove_retired_settings_keys(raw: &mut Map<String, Value>) -> bool {
    let mut removed = false;
    for (parent, key) in RETIRED_NESTED_KEYS {
        let Some(Value::Object(container)) = raw.get_mut(*parent) else {
            continue;
        };
        if container.remove(*key).is_none() {
            continue;
        }
        if container.is_empty() {
            raw.remove(*parent);
        }
        removed = true;
    }
    removed
}

/// Settings files (or path-less storages, keyed by scope) whose retired-key rewrite already failed in this
/// process: later managers over the same file neither retry the locked write nor log it again.
static FAILED_REWRITES: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// Rewrites one scope's file as its raw parsed content with the retired keys removed, without running the
/// settings migration, so every other key keeps its parsed value. The edit is re-derived from the content
/// seen under the storage lock. A failed write never fails the load; it is logged once per file.
pub fn write_raw_scoped_settings(storage: &dyn SettingsStorage, scope: SettingsScope) {
    let target = storage
        .select_source(scope)
        .map(|source| source.path)
        .unwrap_or_else(|| {
            format!(
                "storage:{:p}:{scope:?}",
                storage as *const dyn SettingsStorage as *const ()
            )
        });
    let mut failed = FAILED_REWRITES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if failed.contains(&target) {
        return;
    }
    let result = storage.with_lock(scope, &mut |current| {
        let current = current?;
        let mut raw = parse_settings_json(current).ok()?;
        remove_retired_settings_keys(&mut raw)
            .then(|| serde_json::to_string_pretty(&Value::Object(raw)).ok())
            .flatten()
    });
    if let Err(error) = result {
        failed.insert(target);
        let scope_name = match scope {
            SettingsScope::Global => "global",
            SettingsScope::Project => "project",
        };
        append_debug_log_entry(
            &format!("settings: could not remove retired {scope_name} settings keys"),
            &error,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn object(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            _ => Map::new(),
        }
    }

    #[test]
    fn removes_a_retired_key_and_drops_the_emptied_parent() {
        let mut raw = object(json!({"experimental": {"sharedHost": true}, "theme": "dark"}));
        assert!(remove_retired_settings_keys(&mut raw));
        assert_eq!(Value::Object(raw), json!({"theme": "dark"}));
        let mut raw = object(json!({"experimental": {"sharedHost": false, "other": 1}}));
        assert!(remove_retired_settings_keys(&mut raw));
        assert_eq!(Value::Object(raw), json!({"experimental": {"other": 1}}));
        let mut raw = object(json!({"experimental": ["sharedHost"]}));
        assert!(!remove_retired_settings_keys(&mut raw));
    }

    struct Memory(Mutex<Option<String>>);

    impl SettingsStorage for Memory {
        fn with_lock(
            &self,
            _scope: SettingsScope,
            update: &mut dyn FnMut(Option<&str>) -> Option<String>,
        ) -> Result<(), String> {
            let mut content = self.0.lock().map_err(|e| e.to_string())?;
            if let Some(next) = update(content.as_deref()) {
                *content = Some(next);
            }
            Ok(())
        }
    }

    #[test]
    fn rewrites_the_raw_content_without_the_retired_key() {
        let storage = Memory(Mutex::new(Some(
            "{\"experimental\":{\"sharedHost\":true},\"x\":1}".into(),
        )));
        write_raw_scoped_settings(&storage, SettingsScope::Global);
        let content = storage
            .0
            .lock()
            .ok()
            .and_then(|c| c.clone())
            .unwrap_or_default();
        assert_eq!(
            serde_json::from_str::<Value>(&content).ok(),
            Some(json!({"x": 1}))
        );
    }
}
