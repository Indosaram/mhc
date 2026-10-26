//! Port of `packages/omo-senpi/src/components/telemetry/session-summary-purge.ts`.
//!
//! The opted-out side of `session_summary`: turning telemetry off must delete what earlier runs queued, and
//! this module reads and deletes only, never writes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::session_summary_store::ConsentScope;

/// Env keys that pick which config a process reads (a profile, a config dir, a home): per-process choices.
pub const CONFIG_SELECTION_ENV_KEYS: [&str; 5] = [
    "OMO_PROFILE",
    "OCX_PROFILE",
    "OPENCODE_CONFIG_DIR",
    "HOME",
    "USERPROFILE",
];

/// The config selection a record's writer consented under. Stored with the record's scope so that a reader
/// judges the record by the writer's own config (its profile included), never by the reader's.
pub type ConfigSelection = BTreeMap<String, String>;

pub fn config_selection_of(env: &telemetry_core::TelemetryEnv) -> ConfigSelection {
    let mut selection = ConfigSelection::new();
    for key in CONFIG_SELECTION_ENV_KEYS {
        if let Some(value) = env.get(key).filter(|value| !value.is_empty()) {
            selection.insert(key.to_string(), value.clone());
        }
    }
    selection
}

pub fn parse_config_selection(value: &Value) -> Option<ConfigSelection> {
    let object = value.as_object()?;
    let mut selection = ConfigSelection::new();
    for key in CONFIG_SELECTION_ENV_KEYS {
        let Some(entry) = object.get(key) else {
            continue;
        };
        let entry = entry.as_str().filter(|entry| !entry.is_empty())?;
        selection.insert(key.to_string(), entry.to_string());
    }
    Some(selection)
}

/// `env` with its config selection replaced by a record's: the keys of [`CONFIG_SELECTION_ENV_KEYS`] are
/// taken from `selection` only, every other key (the env opt-outs included) stays this process's own.
pub fn with_config_selection(
    env: &telemetry_core::TelemetryEnv,
    selection: &ConfigSelection,
) -> telemetry_core::TelemetryEnv {
    let mut out = env.clone();
    for key in CONFIG_SELECTION_ENV_KEYS {
        out.remove(key);
    }
    for (key, value) in selection {
        out.insert(key.clone(), value.clone());
    }
    out
}

/// `<state>/session-summary`: windows at the top, events awaiting delivery under `pending/`.
pub fn summary_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("session-summary")
}

#[cfg(test)]
pub fn has_queued_summary_files(state_dir: &Path) -> bool {
    !names(&summary_dir(state_dir)).is_empty()
}

#[cfg(test)]
pub fn pending_dir(state_dir: &Path) -> PathBuf {
    summary_dir(state_dir).join("pending")
}

/// Deletes every queued window and pending event that may never be sent: no `enabled` consent mark, or a
/// scope whose PERSISTED consent (the config files every process reads), read now, is off.
///
/// An opt-out set by environment variable belongs to one process, so it is never passed in here: other
/// sessions that consented keep their windows and pending events.
pub fn purge_unconsented_queue(
    state_dir: &Path,
    is_scope_consented: &(dyn Fn(&ConsentScope) -> bool + Send + Sync),
    drop_unreadable: bool,
) {
    let dir = summary_dir(state_dir);
    // One persisted-setting read per scope per pass: a queue of many records from one project loads its
    // config once.
    let verdicts = std::sync::Mutex::new(BTreeMap::<String, bool>::new());
    let consented = |scope: &ConsentScope| -> bool {
        let key = scope_key(scope);
        let mut verdicts = verdicts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(known) = verdicts.get(&key) {
            return *known;
        }
        let verdict = is_scope_consented(scope);
        verdicts.insert(key, verdict);
        verdict
    };
    for folder in [dir.clone(), dir.join("pending")] {
        for name in names(&folder) {
            if !name.ends_with(".json") {
                continue;
            }
            let path = folder.join(&name);
            let verdict = sendable(&read(&path), &consented);
            if verdict == Some(false) || (verdict.is_none() && drop_unreadable) {
                remove(&path);
            }
        }
    }
}

fn scope_key(scope: &ConsentScope) -> String {
    let config = scope.config.as_ref().map_or_else(String::new, |config| {
        CONFIG_SELECTION_ENV_KEYS
            .iter()
            .map(|name| config.get(*name).cloned().unwrap_or_else(|| "null".into()))
            .collect::<Vec<_>>()
            .join(",")
    });
    format!("{}\u{0}{config}", scope.cwd.clone().unwrap_or_default())
}

/// A record parses to its consent mark and scope. `None` means unreadable or an unknown version: it is
/// deleted when the caller's own persisted setting is off, else left for the recorder's sweep.
fn sendable(
    value: &Option<Value>,
    is_scope_consented: &(dyn Fn(&ConsentScope) -> bool + Send + Sync),
) -> Option<bool> {
    let value = value.as_ref()?;
    if value.get("v").and_then(Value::as_u64) != Some(2) {
        return None;
    }
    if value.get("consent").and_then(Value::as_str) != Some("enabled") {
        return Some(false);
    }
    let Some(scope) = value.get("scope").and_then(Value::as_object) else {
        return Some(false);
    };
    // A record that names no cwd fails closed, as at send time: consent read without a cwd would be this
    // process's default, not the writer's.
    let Some(cwd) = scope.get("cwd").and_then(Value::as_str).filter(|cwd| !cwd.is_empty()) else {
        return Some(false);
    };
    let Some(raw_config) = scope.get("config") else {
        return Some(is_scope_consented(&ConsentScope {
            cwd: Some(cwd.to_string()),
            config: None,
        }));
    };
    // A selection that cannot be read back cannot be judged: leave the record to the recorder's own reading.
    parse_config_selection(raw_config).map(|config| {
        is_scope_consented(&ConsentScope {
            cwd: Some(cwd.to_string()),
            config: Some(config),
        })
    })
}

fn names(dir: &Path) -> Vec<String> {
    crate::session_summary_files::list_names(dir)
        .into_iter()
        .filter(|name| name.ends_with(".json"))
        .collect()
}

fn read(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn remove(path: &Path) {
    // Already gone, or claimed by another process: nothing of it is sent either way.
    let _ignored = std::fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;

    fn env(pairs: &[(&str, &str)]) -> telemetry_core::TelemetryEnv {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    fn queue(state: &Path, name: &str, value: Value) {
        let dir = summary_dir(state);
        std::fs::create_dir_all(dir.join("pending")).unwrap();
        std::fs::write(dir.join(name), value.to_string()).unwrap();
    }

    fn record(consent: &str, scope: Value) -> Value {
        json!({"v":2,"consent":consent,"scope":scope,"sessionHash":"h","owner":{"pid":1,"bootId":"b"},"seq":0,"sampleRate":0.05,"windowId":null,"window":null})
    }

    #[test]
    fn config_selection_round_trips_only_its_own_keys() {
        let selection = config_selection_of(&env(&[
            ("OMO_PROFILE", "work"),
            ("HOME", "/home/user"),
            ("DO_NOT_TRACK", "1"),
            ("OCX_PROFILE", ""),
        ]));
        assert_eq!(selection.len(), 2);
        assert_eq!(selection.get("OMO_PROFILE").map(String::as_str), Some("work"));
        assert_eq!(parse_config_selection(&json!(selection)), Some(selection.clone()));
        assert_eq!(parse_config_selection(&json!({"HOME": ""})), None);
        assert_eq!(parse_config_selection(&json!("nope")), None);

        let merged = with_config_selection(
            &env(&[("HOME", "/reader"), ("OMO_PROFILE", "reader"), ("DO_NOT_TRACK", "1")]),
            &selection,
        );
        assert_eq!(merged.get("HOME").map(String::as_str), Some("/home/user"));
        assert_eq!(merged.get("OMO_PROFILE").map(String::as_str), Some("work"));
        assert_eq!(merged.get("DO_NOT_TRACK").map(String::as_str), Some("1"));
        assert_eq!(merged.get("OCX_PROFILE"), None);
    }

    #[test]
    fn purge_deletes_opted_out_scopes_and_keeps_consented_ones() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path();
        queue(state, "opted.json", record("enabled", json!({"cwd": "/opt/out"})));
        queue(state, "kept.json", record("enabled", json!({"cwd": "/kept"})));
        queue(state, "unmarked.json", record("unconsented", json!({"cwd": "/kept"})));
        queue(
            state,
            "pending/opted.json",
            record("enabled", json!({"cwd": "/opt/out"})),
        );
        queue(
            state,
            "pending/kept.json",
            record("enabled", json!({"cwd": "/kept"})),
        );
        let consented = |scope: &ConsentScope| scope.cwd.as_deref() == Some("/kept");
        purge_unconsented_queue(state, &consented, false);
        let mut remaining = names(&summary_dir(state));
        remaining.sort();
        assert_eq!(remaining, vec!["kept.json".to_string()]);
        let mut pending = names(&pending_dir(state));
        pending.sort();
        assert_eq!(pending, vec!["kept.json".to_string()]);
    }

    #[test]
    fn unreadable_records_are_kept_unless_the_writers_setting_is_off() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path();
        queue(state, "corrupt.json", json!("not an object"));
        queue(state, "old.json", json!({"v":1}));
        queue(state, "no-cwd.json", record("enabled", json!({})));
        let consented = |_: &ConsentScope| true;
        purge_unconsented_queue(state, &consented, false);
        let mut remaining = names(&summary_dir(state));
        remaining.sort();
        assert_eq!(remaining, vec!["corrupt.json".to_string(), "old.json".to_string()]);
        purge_unconsented_queue(state, &consented, true);
        assert!(names(&summary_dir(state)).is_empty());
    }

    #[test]
    fn writer_selection_judges_the_record_not_the_reader() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path();
        queue(
            state,
            "profile-a.json",
            record(
                "enabled",
                json!({"cwd": "/repo", "config": {"OMO_PROFILE": "a"}}),
            ),
        );
        queue(
            state,
            "profile-b.json",
            record(
                "enabled",
                json!({"cwd": "/repo", "config": {"OMO_PROFILE": "b"}}),
            ),
        );
        let consented = |scope: &ConsentScope| {
            scope
                .config
                .as_ref()
                .and_then(|config| config.get("OMO_PROFILE"))
                .map(String::as_str)
                == Some("a")
        };
        purge_unconsented_queue(state, &consented, false);
        assert_eq!(names(&summary_dir(state)), vec!["profile-a.json".to_string()]);
    }

    #[test]
    fn verdicts_are_cached_once_per_scope() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path();
        for index in 0..4 {
            queue(
                state,
                &format!("w{index}.json"),
                record("enabled", json!({"cwd": "/repo"})),
            );
        }
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = Arc::clone(&calls);
        let consented = move |_: &ConsentScope| {
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            true
        };
        purge_unconsented_queue(state, &consented, false);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(names(&summary_dir(state)).len(), 4);
    }

    #[test]
    fn queued_files_detection() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!has_queued_summary_files(dir.path()));
        queue(dir.path(), "w.json", json!({}));
        assert!(has_queued_summary_files(dir.path()));
    }
}
