//! Port of senpi packages/coding-agent/src/core/settings-state.ts.
//!
//! Bookkeeping keys (tip history, changelog markers, per-model thinking levels and service tiers)
//! live in `<agentDir>/state.json`, not in the user's global settings file. Loading moves any copy
//! still in the settings file into the state file, then overlays the state file onto the settings.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use serde_json::Value;

use crate::hidden_stdout_log::append_debug_log_entry;
use crate::settings_manager::{
    Settings, SettingsScope, SettingsStorage, parse_settings_json, serialize_settings,
};

pub const STATE_KEYS: [&str; 6] = [
    "tipsHistory",
    "changelogSeen",
    "lastChangelogVersion",
    "modelThinkingLevels",
    "modelLastOnThinkingLevels",
    "modelServiceTiers",
];

pub fn is_state_key(key: &str) -> bool {
    STATE_KEYS.contains(&key)
}

fn failed_migrations() -> &'static Mutex<HashSet<String>> {
    static FAILED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    FAILED.get_or_init(Mutex::default)
}

fn reported_state_errors() -> &'static Mutex<HashSet<String>> {
    static REPORTED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    REPORTED.get_or_init(Mutex::default)
}

fn target(storage: &dyn SettingsStorage) -> String {
    storage
        .select_source(SettingsScope::Global)
        .map(|source| source.path)
        .unwrap_or_else(|| format!("{:p}", storage as *const dyn SettingsStorage as *const ()))
}

pub fn log_state_error(storage: &dyn SettingsStorage, error: &str) {
    let target = target(storage);
    if !reported_state_errors()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(target)
    {
        return;
    }
    append_debug_log_entry(
        "settings: could not read or save state.json; using empty bookkeeping",
        error,
    );
}

/// State is disposable bookkeeping: malformed bytes never disable config loading or saving.
pub fn parse_state_settings(storage: &dyn SettingsStorage, content: Option<&str>) -> Settings {
    let mut raw = match content.map(parse_settings_json).transpose() {
        Ok(raw) => raw.unwrap_or_default(),
        Err(error) => {
            log_state_error(storage, &error);
            return Settings::new();
        }
    };
    for field in [
        "modelThinkingLevels",
        "modelLastOnThinkingLevels",
        "modelServiceTiers",
    ] {
        let Some(Value::Object(map)) = raw.get(field) else {
            continue;
        };
        let mut normalized = serde_json::Map::new();
        for (key, value) in map {
            let canonical = maho_ai::legacy_provider_ids::normalize_model_ref(key);
            if *key == canonical || !map.contains_key(&canonical) {
                normalized.insert(canonical, value.clone());
            }
        }
        raw.insert(field.to_owned(), Value::Object(normalized));
    }
    raw
}

/// Move raw global bookkeeping only after its state write succeeds. Re-read both files under
/// their locks, preserving existing state keys and every unrelated config key's value/order.
pub fn migrate_settings_state(storage: &dyn SettingsStorage) {
    if !storage.has_state_store() {
        return;
    }
    let target = target(storage);
    if failed_migrations()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains(&target)
    {
        return;
    }
    let mut failure: Option<String> = None;
    let result = storage.with_lock(SettingsScope::Global, &mut |current| {
        let current = current?;
        let mut raw = match parse_settings_json(current) {
            Ok(raw) => raw,
            Err(error) => {
                failure = Some(error);
                return None;
            }
        };
        if !STATE_KEYS.iter().any(|key| raw.contains_key(*key)) {
            return None;
        }
        let mut state_failure: Option<String> = None;
        let state_result = storage.with_state_lock(&mut |state_content| {
            let mut state = match state_content.map(parse_settings_json).transpose() {
                Ok(state) => state.unwrap_or_default(),
                Err(error) => {
                    state_failure = Some(error);
                    return None;
                }
            };
            let mut changed = false;
            for key in STATE_KEYS {
                if let Some(value) = raw.get(key)
                    && !state.contains_key(key)
                {
                    state.insert(key.to_owned(), value.clone());
                    changed = true;
                }
            }
            changed.then(|| serialize_settings(&state))
        });
        if let Some(Err(error)) = state_result {
            state_failure = Some(error);
        }
        if let Some(error) = state_failure {
            failure = Some(error);
            return None;
        }
        for key in STATE_KEYS {
            raw.shift_remove(key);
        }
        Some(serialize_settings(&raw))
    });
    if let Err(error) = result {
        failure = Some(error);
    }
    if let Some(error) = failure {
        failed_migrations()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(target);
        append_debug_log_entry("settings: could not migrate global state keys", &error);
    }
}

/// Only the six bookkeeping keys overlay config; arbitrary state-file keys are never settings.
pub fn read_settings_state(storage: &dyn SettingsStorage) -> Settings {
    let mut raw = Settings::new();
    let result = storage.with_state_lock(&mut |current| {
        raw = parse_state_settings(storage, current);
        None
    });
    if let Some(Err(error)) = result {
        log_state_error(storage, &error);
        return Settings::new();
    }
    STATE_KEYS
        .iter()
        .filter_map(|key| {
            raw.get(*key)
                .map(|value| ((*key).to_owned(), value.clone()))
        })
        .collect()
}
