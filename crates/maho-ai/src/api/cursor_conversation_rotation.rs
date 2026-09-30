//! Port of senpi packages/ai/src/api/cursor-conversation-rotation.ts.
// ported by todo 12

use fancy_regex::Regex;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use uuid::Uuid;

pub const MAX_CURSOR_CONVERSATION_ROTATIONS: u32 = 3;
pub const MAX_CURSOR_CONVERSATION_ROTATION_RECORDS: usize = 512;
pub const CURSOR_CONVERSATION_POISONED_MESSAGE: &str =
    "Cursor conversation is poisoned for this session; use another provider";

static RESOURCE_EXHAUSTED_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)resource.?exhausted").expect("static pattern is valid"));

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationRotationRecord {
    pub wire_id: String,
    pub poison_count: u32,
    pub skip: bool,
    pub surfaced: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PoisonDecision {
    Rotated { wire_id: String },
    Exhausted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MutableRecord {
    #[serde(rename = "wireId")]
    wire_id: String,
    #[serde(rename = "poisonCount")]
    poison_count: u32,
    skip: bool,
    surfaced: bool,
}

pub fn is_zero_token_resource_exhausted(error_message: &str, saw_token_delta: bool) -> bool {
    !saw_token_delta && RESOURCE_EXHAUSTED_PATTERN.is_match(error_message).unwrap_or(false)
}

pub struct ConversationRotationStoreOptions<F: Fn() -> String> {
    pub persist_path: PathBuf,
    pub random_id: F,
    pub max_records: Option<usize>,
}

pub struct ConversationRotationStore<F: Fn() -> String> {
    persist_path: PathBuf,
    random_id: F,
    max_records: usize,
    records: Mutex<IndexMap<String, MutableRecord>>,
}

fn default_random_id() -> String {
    Uuid::new_v4().to_string()
}

impl ConversationRotationStore<fn() -> String> {
    pub fn new(persist_path: PathBuf) -> Self {
        Self::with_options(ConversationRotationStoreOptions {
            persist_path,
            random_id: default_random_id,
            max_records: None,
        })
    }
}

impl<F: Fn() -> String> ConversationRotationStore<F> {
    pub fn with_options(options: ConversationRotationStoreOptions<F>) -> Self {
        let max_records = options.max_records.unwrap_or(MAX_CURSOR_CONVERSATION_ROTATION_RECORDS);
        let records = load_records(&options.persist_path);
        Self {
            persist_path: options.persist_path,
            random_id: options.random_id,
            max_records,
            records: Mutex::new(records),
        }
    }

    fn with_records<T>(&self, f: impl FnOnce(&mut IndexMap<String, MutableRecord>) -> T) -> T {
        let mut records = self.records.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        f(&mut records)
    }

    fn trim_and_persist(&self, records: &mut IndexMap<String, MutableRecord>) {
        while records.len() > self.max_records {
            records.shift_remove_index(0);
        }
        persist_records(&self.persist_path, records);
    }

    pub fn get_wire_id(&self, base_id: &str) -> String {
        self.with_records(|records| {
            let Some(existing) = records.get(base_id) else {
                return base_id.to_string();
            };
            if !existing.skip {
                return existing.wire_id.clone();
            }
            let wire_id = (self.random_id)();
            let entry = records.get_mut(base_id).expect("checked above");
            entry.wire_id = wire_id.clone();
            entry.skip = false;
            entry.poison_count = 0;
            entry.surfaced = false;
            self.trim_and_persist(records);
            wire_id
        })
    }

    pub fn should_skip(&self, base_id: &str) -> bool {
        self.with_records(|records| records.get(base_id).is_some_and(|record| record.skip))
    }

    pub fn should_surface_before_rotating(&self, base_id: &str) -> bool {
        self.with_records(|records| !records.get(base_id).is_some_and(|record| record.surfaced))
    }

    pub fn mark_surfaced(&self, base_id: &str, current_wire_id: &str) {
        self.with_records(|records| {
            let entry = records.entry(base_id.to_string()).or_insert_with(|| MutableRecord {
                wire_id: current_wire_id.to_string(),
                poison_count: 0,
                skip: false,
                surfaced: false,
            });
            if entry.surfaced {
                return;
            }
            entry.surfaced = true;
            self.trim_and_persist(records);
        });
    }

    pub fn record_count(&self) -> usize {
        self.with_records(|records| records.len())
    }

    pub fn record_zero_token_poison(&self, base_id: &str, current_wire_id: &str) -> PoisonDecision {
        self.with_records(|records| {
            if !records.contains_key(base_id) {
                records.insert(
                    base_id.to_string(),
                    MutableRecord {
                        wire_id: current_wire_id.to_string(),
                        poison_count: 0,
                        skip: false,
                        surfaced: false,
                    },
                );
            }
            let entry = records.get_mut(base_id).expect("inserted above");
            if entry.skip || entry.poison_count >= MAX_CURSOR_CONVERSATION_ROTATIONS {
                entry.skip = true;
                entry.wire_id = current_wire_id.to_string();
                self.trim_and_persist(records);
                return PoisonDecision::Exhausted;
            }
            let wire_id = (self.random_id)();
            entry.wire_id = wire_id.clone();
            entry.poison_count += 1;
            self.trim_and_persist(records);
            PoisonDecision::Rotated { wire_id }
        })
    }
}

pub fn resolve_conversation_rotation_persist_path(
    env: &std::collections::HashMap<String, String>,
) -> String {
    if let Some(store) = env.get("CURSOR_CONVERSATION_ID_STORE") {
        return store.clone();
    }
    let agent_dir = env
        .get("SENPI_CODING_AGENT_DIR")
        .or_else(|| env.get("CODING_AGENT_DIR"))
        .cloned()
        .unwrap_or_else(|| {
            let home = env.get("HOME").map(String::as_str).unwrap_or(".");
            format!("{}/.senpi/agent", home.trim_end_matches('/'))
        });
    format!("{}/cursor-conversation-ids.json", agent_dir.trim_end_matches('/'))
}

fn load_records(persist_path: &Path) -> IndexMap<String, MutableRecord> {
    let Ok(contents) = std::fs::read_to_string(persist_path) else {
        return IndexMap::new();
    };
    let Ok(serde_json::Value::Object(parsed)) = serde_json::from_str::<serde_json::Value>(&contents) else {
        return IndexMap::new();
    };
    let mut records = IndexMap::new();
    for (base_id, value) in parsed {
        let serde_json::Value::Object(raw) = &value else { continue };
        let Some(serde_json::Value::String(wire_id)) = raw.get("wireId") else { continue };
        if wire_id.is_empty() {
            continue;
        }
        let poison_count = raw
            .get("poisonCount")
            .and_then(serde_json::Value::as_f64)
            .filter(|value| *value >= 0.0)
            .map(|value| value as u32)
            .unwrap_or(0);
        let skip = raw.get("skip").and_then(serde_json::Value::as_bool).unwrap_or(false);
        let surfaced = raw.get("surfaced").and_then(serde_json::Value::as_bool).unwrap_or(false);
        records.insert(base_id, MutableRecord { wire_id: wire_id.clone(), poison_count, skip, surfaced });
    }
    records
}

fn persist_records(persist_path: &Path, records: &IndexMap<String, MutableRecord>) {
    if let Some(parent) = persist_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(records) {
        let _ = std::fs::write(persist_path, format!("{json}\n"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use tempfile::tempdir;

    fn store_path(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("cursor-conversation-ids.json")
    }

    fn queue_random_id(ids: Vec<&'static str>) -> impl Fn() -> String {
        let queue = RefCell::new(ids.into_iter().map(str::to_string).collect::<Vec<_>>());
        move || {
            let mut queue = queue.borrow_mut();
            if queue.is_empty() { "overflow".to_string() } else { queue.remove(0) }
        }
    }

    #[test]
    fn given_zero_token_resource_exhausted_when_recorded_then_mints_new_wire_id() {
        let dir = tempdir().expect("tempdir");
        let persist_path = store_path(&dir);
        let store = ConversationRotationStore::with_options(ConversationRotationStoreOptions {
            persist_path,
            random_id: queue_random_id(vec!["rot-1", "rot-2", "rot-3"]),
            max_records: None,
        });
        assert_eq!(store.get_wire_id("session-a"), "session-a");
        let first = store.record_zero_token_poison("session-a", "session-a");
        assert_eq!(first, PoisonDecision::Rotated { wire_id: "rot-1".to_string() });
        assert_eq!(store.get_wire_id("session-a"), "rot-1");
    }

    #[test]
    fn given_persisted_store_when_reloaded_then_wire_id_survives_restart() {
        let dir = tempdir().expect("tempdir");
        let persist_path = store_path(&dir);
        let first = ConversationRotationStore::with_options(ConversationRotationStoreOptions {
            persist_path: persist_path.clone(),
            random_id: queue_random_id(vec!["persisted-wire"]),
            max_records: None,
        });
        first.record_zero_token_poison("session-b", "session-b");
        let reloaded = ConversationRotationStore::with_options(ConversationRotationStoreOptions {
            persist_path: persist_path.clone(),
            random_id: queue_random_id(vec!["unused"]),
            max_records: None,
        });
        assert_eq!(reloaded.get_wire_id("session-b"), "persisted-wire");
        let persisted: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&persist_path).expect("read")).expect("json");
        assert_eq!(persisted["session-b"]["wireId"], "persisted-wire");
    }

    #[test]
    fn given_three_rotations_when_exhausted_then_stops_minting_ids_and_ignores_token_evidence_re() {
        let dir = tempdir().expect("tempdir");
        let persist_path = store_path(&dir);
        let counter = RefCell::new(0u32);
        let random_id = move || {
            let mut count = counter.borrow_mut();
            *count += 1;
            format!("rot-{count}")
        };
        let store = ConversationRotationStore::with_options(ConversationRotationStoreOptions {
            persist_path,
            random_id,
            max_records: None,
        });
        assert_eq!(
            store.record_zero_token_poison("session-c", "session-c"),
            PoisonDecision::Rotated { wire_id: "rot-1".to_string() }
        );
        assert_eq!(
            store.record_zero_token_poison("session-c", "rot-1"),
            PoisonDecision::Rotated { wire_id: "rot-2".to_string() }
        );
        assert_eq!(
            store.record_zero_token_poison("session-c", "rot-2"),
            PoisonDecision::Rotated { wire_id: "rot-3".to_string() }
        );
        assert_eq!(store.record_zero_token_poison("session-c", "rot-3"), PoisonDecision::Exhausted);
        assert!(store.should_skip("session-c"));
        let reopened = store.get_wire_id("session-c");
        assert_ne!(reopened, "rot-3");
        assert!(!store.should_skip("session-c"));
        assert!(!is_zero_token_resource_exhausted("Connect error resource_exhausted: Error", true));
        assert!(is_zero_token_resource_exhausted("Connect error resource_exhausted: Error", false));
    }

    #[test]
    fn given_env_vars_when_resolving_persist_path_then_prefers_agent_dir_over_home() {
        let mut env = HashMap::new();
        env.insert("HOME".to_string(), "/home/u".to_string());
        assert_eq!(
            resolve_conversation_rotation_persist_path(&env),
            "/home/u/.senpi/agent/cursor-conversation-ids.json"
        );

        env.insert("CODING_AGENT_DIR".to_string(), "/home/u/.omo/agent".to_string());
        assert_eq!(
            resolve_conversation_rotation_persist_path(&env),
            "/home/u/.omo/agent/cursor-conversation-ids.json"
        );

        env.insert("CURSOR_CONVERSATION_ID_STORE".to_string(), "/tmp/store.json".to_string());
        assert_eq!(resolve_conversation_rotation_persist_path(&env), "/tmp/store.json");
    }

    #[test]
    fn given_more_records_than_max_when_recorded_then_oldest_are_trimmed() {
        let dir = tempdir().expect("tempdir");
        let persist_path = store_path(&dir);
        let counter = RefCell::new(0u32);
        let random_id = move || {
            let mut count = counter.borrow_mut();
            *count += 1;
            format!("wire-{count}")
        };
        let store = ConversationRotationStore::with_options(ConversationRotationStoreOptions {
            persist_path: persist_path.clone(),
            random_id,
            max_records: Some(3),
        });
        for base_id in ["rec-1", "rec-2", "rec-3", "rec-4", "rec-5"] {
            store.mark_surfaced(base_id, base_id);
        }
        assert_eq!(store.record_count(), 3);
        assert!(store.should_surface_before_rotating("rec-1"));
        assert!(store.should_surface_before_rotating("rec-2"));
        let persisted: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&persist_path).expect("read")).expect("json");
        let mut keys: Vec<String> =
            persisted.as_object().expect("object").keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, vec!["rec-3", "rec-4", "rec-5"]);
    }
}
