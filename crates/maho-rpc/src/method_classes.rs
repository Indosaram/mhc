//! The engine's own classification of every inbound RPC record (senpi#4319).
//!
//! A client that speaks for someone other than the local user (a forwarder, a remote viewer, an
//! automation) needs to know which records it may accept from that someone. The table lives here
//! and is exhaustive over all known inbound RPC record types. An entry is either a fixed class
//! or a versioned rule (a pure function of the record) when the class depends on its fields.
//!
//! At runtime a record the table does not classify (unknown type, non-string type, not an object)
//! is treated as more dangerous than every class: [`rpc_method_permitted`] refuses it whatever is
//! granted.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RpcMethodClass {
    Observe,
    Drive,
    Answer,
    Thread,
    DeviceAdmin,
    ConnectionLocal,
}

impl RpcMethodClass {
    pub const ALL: [Self; 6] = [
        Self::Observe,
        Self::Drive,
        Self::Answer,
        Self::Thread,
        Self::DeviceAdmin,
        Self::ConnectionLocal,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Observe => "observe",
            Self::Drive => "drive",
            Self::Answer => "answer",
            Self::Thread => "thread",
            Self::DeviceAdmin => "device_admin",
            Self::ConnectionLocal => "connection_local",
        }
    }
}

/// Bumped when the meaning of a class changes; a reassignment changes only the digest.
pub const RPC_METHOD_CLASSES_VERSION: u32 = 1;

/// How the table description names an entry whose class depends on the record.
pub const DYNAMIC_CLASS_LABEL: &str = "dynamic";

type RuleFn = fn(&Map<String, Value>) -> RpcMethodClass;

#[derive(Debug, Clone, Copy)]
pub enum RpcMethodClassEntry {
    Static(RpcMethodClass),
    Dynamic { classify: RuleFn, version: u32 },
}

const KNOWN_ENGINE_ENTRIES: [&str; 12] = [
    "thinking_level_change",
    "configuration_update",
    "model_change",
    "model_change_rejected",
    "usage",
    "compaction",
    "branch_summary",
    "custom",
    "custom_message",
    "context_edit",
    "label",
    "session_info",
];

/// A user-authored message entry drives the session; any other entry is a forged record of engine
/// work, the same class as `edit_assistant_message` and `send_custom_message`.
fn append_entry_class(record: &Map<String, Value>) -> RpcMethodClass {
    let Some(entry) = record.get("entry").and_then(Value::as_object) else {
        return RpcMethodClass::DeviceAdmin;
    };
    let Some(entry_type) = entry.get("type").and_then(Value::as_str) else {
        return RpcMethodClass::DeviceAdmin;
    };
    if entry_type != "message" {
        let _ = KNOWN_ENGINE_ENTRIES;
        return RpcMethodClass::DeviceAdmin;
    }
    let Some(message) = entry.get("message").and_then(Value::as_object) else {
        return RpcMethodClass::DeviceAdmin;
    };
    if message.get("role").and_then(Value::as_str) == Some("user") {
        RpcMethodClass::Drive
    } else {
        RpcMethodClass::DeviceAdmin
    }
}

/// A field that names a host path or a permission preset; present means any key present on the JSON
/// object (`null` included).
fn names_any(record: &Map<String, Value>, fields: &[&str]) -> bool {
    fields.iter().any(|field| record.contains_key(*field))
}

/// Opening by id is a thread operation; naming a session file, a cwd, a permission preset, a
/// browser engine, or an environment for the session's tool subprocesses (`env`, version 2) acts
/// on the device.
fn open_session_class(record: &Map<String, Value>) -> RpcMethodClass {
    if names_any(
        record,
        &[
            "sessionPath",
            "cwd",
            "permissionPreset",
            "browserEngine",
            "env",
        ],
    ) {
        RpcMethodClass::DeviceAdmin
    } else {
        RpcMethodClass::Thread
    }
}

/// `scope: "turn"` changes only this turn; any other scope remembers the model's level device-wide.
fn thinking_level_class(record: &Map<String, Value>) -> RpcMethodClass {
    if record.get("scope").and_then(Value::as_str) == Some("turn") {
        RpcMethodClass::Drive
    } else {
        RpcMethodClass::DeviceAdmin
    }
}

/// `scope: "session"` switches only this session's model; any other scope persists the device's default model.
fn set_model_class(record: &Map<String, Value>) -> RpcMethodClass {
    if record.get("scope").and_then(Value::as_str) == Some("session") {
        RpcMethodClass::Drive
    } else {
        RpcMethodClass::DeviceAdmin
    }
}

/// An effect looked up by session id is a read; one looked up in a named session file opens a host path.
fn effect_status_class(record: &Map<String, Value>) -> RpcMethodClass {
    if names_any(record, &["sessionPath"]) {
        RpcMethodClass::DeviceAdmin
    } else {
        RpcMethodClass::Observe
    }
}

/// Prompt text is `drive`; text that can dispatch a registered slash command is `device_admin`.
fn prompt_class(record: &Map<String, Value>) -> RpcMethodClass {
    let Some(message) = record.get("message").and_then(Value::as_str) else {
        return RpcMethodClass::DeviceAdmin;
    };
    if record.get("expandPromptTemplates") == Some(&Value::Bool(false)) {
        return RpcMethodClass::Drive;
    }
    if message.starts_with('/') {
        RpcMethodClass::DeviceAdmin
    } else {
        RpcMethodClass::Drive
    }
}

/// `steer`/`follow_up` with `whenIdle: "start"` take the prompt path; anything else is `drive`.
fn idle_start_class(record: &Map<String, Value>) -> RpcMethodClass {
    if record.get("whenIdle").and_then(Value::as_str) != Some("start") {
        return RpcMethodClass::Drive;
    }
    let Some(message) = record.get("message").and_then(Value::as_str) else {
        return RpcMethodClass::DeviceAdmin;
    };
    if message.starts_with('/') {
        RpcMethodClass::DeviceAdmin
    } else {
        RpcMethodClass::Drive
    }
}

/// A new session is a thread operation; naming a parent session file reads a host path.
fn new_session_class(record: &Map<String, Value>) -> RpcMethodClass {
    if names_any(record, &["parentSession"]) {
        RpcMethodClass::DeviceAdmin
    } else {
        RpcMethodClass::Thread
    }
}

const fn s(class: RpcMethodClass) -> RpcMethodClassEntry {
    RpcMethodClassEntry::Static(class)
}

const fn d(classify: RuleFn, version: u32) -> RpcMethodClassEntry {
    RpcMethodClassEntry::Dynamic { classify, version }
}

/// Complete table of inbound RPC method types and their class or versioned rule.
pub const RPC_METHOD_CLASS_ENTRIES: &[(&str, RpcMethodClassEntry)] = &[
    // observe
    ("get_protocol_info", s(RpcMethodClass::Observe)),
    ("get_subscription_limits", s(RpcMethodClass::Observe)),
    ("list_sessions", s(RpcMethodClass::Observe)),
    ("list_hands", s(RpcMethodClass::Observe)),
    ("get_state", s(RpcMethodClass::Observe)),
    ("get_snapshot", s(RpcMethodClass::Observe)),
    ("get_entries", s(RpcMethodClass::Observe)),
    ("get_messages", s(RpcMethodClass::Observe)),
    ("get_tree", s(RpcMethodClass::Observe)),
    ("get_fork_messages", s(RpcMethodClass::Observe)),
    ("get_last_assistant_text", s(RpcMethodClass::Observe)),
    ("get_session_stats", s(RpcMethodClass::Observe)),
    ("get_effect_status", d(effect_status_class, 1)),
    ("get_pending_inputs", s(RpcMethodClass::Observe)),
    ("get_follow_up_messages", s(RpcMethodClass::Observe)),
    ("get_steering_messages", s(RpcMethodClass::Observe)),
    ("get_commands", s(RpcMethodClass::Observe)),
    ("get_loaded_surfaces", s(RpcMethodClass::Observe)),
    ("get_project_trust", s(RpcMethodClass::Observe)),
    ("get_available_models", s(RpcMethodClass::Observe)),
    ("get_available_thinking_levels", s(RpcMethodClass::Observe)),
    ("get_model_thinking_level", s(RpcMethodClass::Observe)),
    ("get_fast_mode", s(RpcMethodClass::Observe)),
    ("get_media", s(RpcMethodClass::Observe)),
    ("memory_report", s(RpcMethodClass::Observe)),
    ("eval_cells.list", s(RpcMethodClass::Observe)),
    ("eval_cells.peek", s(RpcMethodClass::Observe)),
    // drive
    ("prompt", d(prompt_class, 1)),
    ("steer", d(idle_start_class, 1)),
    ("follow_up", d(idle_start_class, 1)),
    ("abort", s(RpcMethodClass::Drive)),
    ("interrupt", s(RpcMethodClass::Drive)),
    ("abort_retry", s(RpcMethodClass::Drive)),
    ("abort_compaction", s(RpcMethodClass::Drive)),
    ("abort_branch_summary", s(RpcMethodClass::Drive)),
    ("compact", s(RpcMethodClass::Drive)),
    ("clear_queue", s(RpcMethodClass::Drive)),
    ("dequeue_message", s(RpcMethodClass::Drive)),
    ("edit_queued_message", s(RpcMethodClass::Drive)),
    ("hold_input", s(RpcMethodClass::Drive)),
    ("withdraw_held_input", s(RpcMethodClass::Drive)),
    ("append_user_message", s(RpcMethodClass::Drive)),
    ("edit_user_message", s(RpcMethodClass::Drive)),
    ("continue_from_leaf", s(RpcMethodClass::Drive)),
    ("navigate_tree", s(RpcMethodClass::Drive)),
    ("set_label", s(RpcMethodClass::Drive)),
    ("set_session_name", s(RpcMethodClass::Drive)),
    ("set_session_archived", s(RpcMethodClass::Drive)),
    ("set_session_settled", s(RpcMethodClass::Drive)),
    ("set_thinking_level", d(thinking_level_class, 1)),
    ("set_model", d(set_model_class, 1)),
    ("set_auto_compaction", s(RpcMethodClass::Drive)),
    ("set_permission_preset", s(RpcMethodClass::DeviceAdmin)),
    ("set_retry_fallback", s(RpcMethodClass::Drive)),
    ("pause_for_run", s(RpcMethodClass::Drive)),
    ("resume_for_run", s(RpcMethodClass::Drive)),
    ("wake", s(RpcMethodClass::Drive)),
    ("admit_external_message", s(RpcMethodClass::Drive)),
    ("release_tool_barrier", s(RpcMethodClass::Drive)),
    ("request_self_settle", s(RpcMethodClass::Drive)),
    ("eval_cells.stop", s(RpcMethodClass::Drive)),
    ("append_session_entry", d(append_entry_class, 1)),
    // answer
    ("extension_ui_response", s(RpcMethodClass::Answer)),
    ("extension_ui_progress", s(RpcMethodClass::Answer)),
    // thread
    ("new_session", d(new_session_class, 1)),
    ("open_session", d(open_session_class, 2)),
    ("claim_session", s(RpcMethodClass::DeviceAdmin)),
    ("park_session", s(RpcMethodClass::DeviceAdmin)),
    ("resume_session", s(RpcMethodClass::Thread)),
    ("fork", s(RpcMethodClass::Thread)),
    ("clone", s(RpcMethodClass::Thread)),
    ("close_session", s(RpcMethodClass::Thread)),
    ("unload_session", s(RpcMethodClass::Thread)),
    ("set_session_context", s(RpcMethodClass::Thread)),
    ("attach_session", s(RpcMethodClass::Thread)),
    // device_admin
    ("select_hand", s(RpcMethodClass::DeviceAdmin)),
    ("bash", s(RpcMethodClass::DeviceAdmin)),
    ("abort_bash", s(RpcMethodClass::DeviceAdmin)),
    ("record_bash_result", s(RpcMethodClass::DeviceAdmin)),
    ("cleanup_bash_output", s(RpcMethodClass::DeviceAdmin)),
    ("extension_request", s(RpcMethodClass::DeviceAdmin)),
    ("reload", s(RpcMethodClass::DeviceAdmin)),
    ("check_reload_veto", s(RpcMethodClass::DeviceAdmin)),
    ("resolve_path", s(RpcMethodClass::DeviceAdmin)),
    ("find_session", s(RpcMethodClass::DeviceAdmin)),
    ("login_start", s(RpcMethodClass::DeviceAdmin)),
    ("login_cancel", s(RpcMethodClass::DeviceAdmin)),
    ("login_api_key", s(RpcMethodClass::DeviceAdmin)),
    ("logout", s(RpcMethodClass::DeviceAdmin)),
    ("verify_api_key", s(RpcMethodClass::DeviceAdmin)),
    ("get_auth_providers", s(RpcMethodClass::DeviceAdmin)),
    ("get_provider_accounts", s(RpcMethodClass::DeviceAdmin)),
    ("account_order", s(RpcMethodClass::DeviceAdmin)),
    ("account_pin", s(RpcMethodClass::DeviceAdmin)),
    ("account_remove", s(RpcMethodClass::DeviceAdmin)),
    ("account_reset", s(RpcMethodClass::DeviceAdmin)),
    ("test_provider_account", s(RpcMethodClass::DeviceAdmin)),
    ("set_favorite_models", s(RpcMethodClass::DeviceAdmin)),
    ("set_scoped_models", s(RpcMethodClass::DeviceAdmin)),
    ("cycle_model", s(RpcMethodClass::DeviceAdmin)),
    ("cycle_thinking_level", s(RpcMethodClass::DeviceAdmin)),
    ("set_fast_mode", s(RpcMethodClass::DeviceAdmin)),
    ("set_steering_mode", s(RpcMethodClass::DeviceAdmin)),
    ("set_follow_up_mode", s(RpcMethodClass::DeviceAdmin)),
    ("set_auto_retry", s(RpcMethodClass::DeviceAdmin)),
    ("set_project_trust", s(RpcMethodClass::DeviceAdmin)),
    ("get_project_settings", s(RpcMethodClass::DeviceAdmin)),
    ("set_project_setting", s(RpcMethodClass::DeviceAdmin)),
    ("read_project_file", s(RpcMethodClass::DeviceAdmin)),
    ("write_project_file", s(RpcMethodClass::DeviceAdmin)),
    ("get_project_mcp_servers", s(RpcMethodClass::DeviceAdmin)),
    ("set_project_mcp_servers", s(RpcMethodClass::DeviceAdmin)),
    ("read_project_skill_file", s(RpcMethodClass::DeviceAdmin)),
    ("write_project_skill_file", s(RpcMethodClass::DeviceAdmin)),
    ("read_user_skill_file", s(RpcMethodClass::DeviceAdmin)),
    ("write_user_skill_file", s(RpcMethodClass::DeviceAdmin)),
    ("edit_assistant_message", s(RpcMethodClass::DeviceAdmin)),
    ("send_custom_message", s(RpcMethodClass::DeviceAdmin)),
    ("switch_session", s(RpcMethodClass::DeviceAdmin)),
    ("import_jsonl", s(RpcMethodClass::DeviceAdmin)),
    ("export_html", s(RpcMethodClass::DeviceAdmin)),
    ("export_jsonl", s(RpcMethodClass::DeviceAdmin)),
    // connection_local
    ("set_client_info", s(RpcMethodClass::ConnectionLocal)),
    ("subscribe_sessions", s(RpcMethodClass::ConnectionLocal)),
    ("unsubscribe_sessions", s(RpcMethodClass::ConnectionLocal)),
    ("record_user_activity", s(RpcMethodClass::ConnectionLocal)),
    ("detach_connection", s(RpcMethodClass::ConnectionLocal)),
    ("release_session", s(RpcMethodClass::ConnectionLocal)),
    ("warm", s(RpcMethodClass::ConnectionLocal)),
    ("host_response", s(RpcMethodClass::ConnectionLocal)),
    ("release_attachment", s(RpcMethodClass::ConnectionLocal)),
    ("reclaim_session", s(RpcMethodClass::ConnectionLocal)),
    ("move_session", s(RpcMethodClass::ConnectionLocal)),
    ("get_recovery_page", s(RpcMethodClass::ConnectionLocal)),
    ("release_recovery", s(RpcMethodClass::ConnectionLocal)),
    ("cancel_attempt", s(RpcMethodClass::ConnectionLocal)),
    ("reset_stream", s(RpcMethodClass::ConnectionLocal)),
];

pub fn lookup_method_class_entry(method_type: &str) -> Option<RpcMethodClassEntry> {
    RPC_METHOD_CLASS_ENTRIES
        .iter()
        .find(|(name, _)| *name == method_type)
        .map(|(_, entry)| *entry)
}

/// The class of an inbound record, or `None` when the engine cannot classify it.
pub fn classify_rpc_method(record: &Value) -> Option<RpcMethodClass> {
    let object = record.as_object()?;
    let method_type = object.get("type").and_then(Value::as_str)?;
    let entry = lookup_method_class_entry(method_type)?;
    Some(match entry {
        RpcMethodClassEntry::Static(class) => class,
        RpcMethodClassEntry::Dynamic { classify, .. } => classify(object),
    })
}

/// Whether a caller granted `granted` may send `record`. Fails closed: an unclassified record is
/// refused whatever is granted, `connection_local` is never permitted, and every other class only
/// when the caller is granted that class.
pub fn rpc_method_permitted(record: &Value, granted: &HashSet<RpcMethodClass>) -> bool {
    let Some(method_class) = classify_rpc_method(record) else {
        return false;
    };
    method_class != RpcMethodClass::ConnectionLocal && granted.contains(&method_class)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcMethodClassesDescription {
    pub version: u32,
    pub digest: String,
    pub classes: BTreeMap<String, String>,
    pub dynamic_versions: BTreeMap<String, u32>,
}

/// sha256 (hex) of the sorted `type=value\n` lines, where `value` is the class, or `dynamic@<version>`
/// for a record-dependent entry.
pub fn rpc_method_classes_digest(table: &BTreeMap<String, String>) -> String {
    let mut lines: Vec<String> = table
        .iter()
        .map(|(method_type, method_class)| format!("{method_type}={method_class}\n"))
        .collect();
    lines.sort();
    let mut hasher = Sha256::new();
    hasher.update(lines.join("").as_bytes());
    hex::encode(hasher.finalize())
}

fn digest_value(entry: RpcMethodClassEntry) -> String {
    match entry {
        RpcMethodClassEntry::Static(class) => class.as_str().to_owned(),
        RpcMethodClassEntry::Dynamic { version, .. } => {
            format!("{DYNAMIC_CLASS_LABEL}@{version}")
        }
    }
}

pub fn rpc_method_classes_digest_value() -> &'static str {
    static DIGEST: OnceLock<String> = OnceLock::new();
    DIGEST.get_or_init(|| {
        let map: BTreeMap<String, String> = RPC_METHOD_CLASS_ENTRIES
            .iter()
            .map(|(method_type, entry)| ((*method_type).to_owned(), digest_value(*entry)))
            .collect();
        rpc_method_classes_digest(&map)
    })
}

/// The whole table as data: what a client compares against its own copy.
pub fn describe_rpc_method_classes() -> RpcMethodClassesDescription {
    let mut classes = BTreeMap::new();
    let mut dynamic_versions = BTreeMap::new();
    for &(method_type, entry) in RPC_METHOD_CLASS_ENTRIES {
        match entry {
            RpcMethodClassEntry::Static(class) => {
                classes.insert(method_type.to_owned(), class.as_str().to_owned());
            }
            RpcMethodClassEntry::Dynamic { version, .. } => {
                classes.insert(method_type.to_owned(), DYNAMIC_CLASS_LABEL.to_owned());
                dynamic_versions.insert(method_type.to_owned(), version);
            }
        }
    }
    RpcMethodClassesDescription {
        version: RPC_METHOD_CLASSES_VERSION,
        digest: rpc_method_classes_digest_value().to_owned(),
        classes,
        dynamic_versions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn table_has_unique_entries_and_consistent_digest() {
        let mut seen = HashSet::new();
        for (name, _) in RPC_METHOD_CLASS_ENTRIES {
            assert!(seen.insert(*name), "duplicate method class entry: {name}");
        }
        let desc = describe_rpc_method_classes();
        assert_eq!(desc.version, 1);
        assert_eq!(desc.classes.len(), RPC_METHOD_CLASS_ENTRIES.len());
        assert_eq!(desc.dynamic_versions.get("open_session"), Some(&2));
        assert_eq!(desc.digest, rpc_method_classes_digest_value());
        assert_eq!(desc.digest.len(), 64);
    }

    #[test]
    fn refuses_unclassified_records_even_with_every_class_granted() {
        let all: HashSet<RpcMethodClass> = RpcMethodClass::ALL.into_iter().collect();
        for bad in [
            json!({"type": "zz_unclassified_probe"}),
            json!({"type": "__proto__"}),
            json!({"type": "constructor"}),
            json!({"type": 7}),
            json!({}),
            json!("prompt"),
            json!(7),
            Value::Null,
            json!([{"type": "prompt"}]),
            json!({"type": "authenticate", "client_proof": "p"}),
        ] {
            assert_eq!(classify_rpc_method(&bad), None);
            assert!(!rpc_method_permitted(&bad, &all));
        }
    }

    #[test]
    fn never_permits_connection_local_and_gates_every_other_static_class() {
        let all: HashSet<RpcMethodClass> = RpcMethodClass::ALL.into_iter().collect();
        for &(method_type, entry) in RPC_METHOD_CLASS_ENTRIES {
            let RpcMethodClassEntry::Static(class) = entry else {
                continue;
            };
            let record = json!({"type": method_type});
            let others: HashSet<_> = all.iter().copied().filter(|c| *c != class).collect();
            assert!(!rpc_method_permitted(&record, &others), "{method_type}");
            let only = HashSet::from([class]);
            assert_eq!(
                rpc_method_permitted(&record, &only),
                class != RpcMethodClass::ConnectionLocal,
                "{method_type}"
            );
        }
    }

    #[test]
    fn dynamic_rules_classify_fields_accurately() {
        assert_eq!(
            classify_rpc_method(&json!({"type": "prompt", "message": "hello"})),
            Some(RpcMethodClass::Drive)
        );
        assert_eq!(
            classify_rpc_method(&json!({"type": "prompt", "message": "/mcp"})),
            Some(RpcMethodClass::DeviceAdmin)
        );
        assert_eq!(
            classify_rpc_method(
                &json!({"type": "prompt", "message": "/mcp", "expandPromptTemplates": false})
            ),
            Some(RpcMethodClass::Drive)
        );
        assert_eq!(
            classify_rpc_method(&json!({"type": "steer", "whenIdle": "start", "message": "/sh"})),
            Some(RpcMethodClass::DeviceAdmin)
        );
        assert_eq!(
            classify_rpc_method(&json!({"type": "steer", "whenIdle": "queue", "message": "/sh"})),
            Some(RpcMethodClass::Drive)
        );
        assert_eq!(
            classify_rpc_method(&json!({"type": "open_session", "sessionId": "s"})),
            Some(RpcMethodClass::Thread)
        );
        assert_eq!(
            classify_rpc_method(&json!({"type": "open_session", "env": {"OMOCAT_LANE": "a"}})),
            Some(RpcMethodClass::DeviceAdmin)
        );
        assert_eq!(
            classify_rpc_method(&json!({"type": "new_session"})),
            Some(RpcMethodClass::Thread)
        );
        assert_eq!(
            classify_rpc_method(&json!({"type": "new_session", "parentSession": null})),
            Some(RpcMethodClass::DeviceAdmin)
        );
        assert_eq!(
            classify_rpc_method(&json!({"type": "get_effect_status", "effectId": "e"})),
            Some(RpcMethodClass::Observe)
        );
        assert_eq!(
            classify_rpc_method(
                &json!({"type": "get_effect_status", "effectId": "e", "sessionPath": "/x"})
            ),
            Some(RpcMethodClass::DeviceAdmin)
        );
        assert_eq!(
            classify_rpc_method(
                &json!({"type": "set_thinking_level", "level": "high", "scope": "turn"})
            ),
            Some(RpcMethodClass::Drive)
        );
        assert_eq!(
            classify_rpc_method(&json!({"type": "set_thinking_level", "level": "high"})),
            Some(RpcMethodClass::DeviceAdmin)
        );
        assert_eq!(
            classify_rpc_method(
                &json!({"type": "set_model", "provider": "a", "modelId": "b", "scope": "session"})
            ),
            Some(RpcMethodClass::Drive)
        );
        assert_eq!(
            classify_rpc_method(&json!({"type": "set_model", "provider": "a", "modelId": "b"})),
            Some(RpcMethodClass::DeviceAdmin)
        );
        assert_eq!(
            classify_rpc_method(
                &json!({"type": "append_session_entry", "entry": {"type": "message", "message": {"role": "user"}}})
            ),
            Some(RpcMethodClass::Drive)
        );
        assert_eq!(
            classify_rpc_method(
                &json!({"type": "append_session_entry", "entry": {"type": "message", "message": {"role": "assistant"}}})
            ),
            Some(RpcMethodClass::DeviceAdmin)
        );
    }
}
