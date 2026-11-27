//! One schema per inbound RPC record type, and the validator built from it (senpi#4324).
//!
//! Exhaustive over [`crate::method_classes::RPC_METHOD_CLASS_ENTRIES`]. Unknown fields stay accepted,
//! as they are on the local connection, so additive fields keep working against older hosts. Typed
//! fields are bounded ([`RPC_INBOUND_LIMITS`]); the whole record is bounded by [`MAX_RPC_LINE_CHARACTERS`].

use crate::jsonl::MAX_RPC_LINE_CHARACTERS;
pub use crate::method_classes_schema_fields::{
    RPC_INBOUND_LIMITS, RPC_QUESTION_ANSWER_LIMITS, Schema, is_env_name,
};
use crate::method_classes_schema_host::host_record_schemas;
use crate::method_classes_schema_recovery::recovery_record_schemas;
use crate::method_classes_schema_session::session_record_schemas;
use crate::rpc_input_validation::{
    retry_fallback_error, rpc_command_payload_error, rpc_command_shape_error,
    rpc_message_length_error, session_context_error,
};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::OnceLock;

const MAX_REASON_TYPE_CHARACTERS: usize = 64;
const MAX_ECHOED_NAME_LENGTH: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RpcInboundValidation {
    Ok,
    Refused(String),
}

impl RpcInboundValidation {
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok)
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Ok => None,
            Self::Refused(reason) => Some(reason),
        }
    }
}

pub fn rpc_inbound_schemas() -> &'static HashMap<&'static str, Schema> {
    static SCHEMAS: OnceLock<HashMap<&'static str, Schema>> = OnceLock::new();
    SCHEMAS.get_or_init(|| {
        let mut map = HashMap::new();
        for (k, v) in host_record_schemas()
            .into_iter()
            .chain(session_record_schemas())
            .chain(recovery_record_schemas())
        {
            map.insert(k, v);
        }
        map
    })
}

fn echoed_name(name: &str) -> String {
    if name.chars().count() > MAX_ECHOED_NAME_LENGTH {
        let prefix: String = name.chars().take(MAX_ECHOED_NAME_LENGTH).collect();
        format!("{prefix}...")
    } else {
        name.to_owned()
    }
}

/// Detail for an `open_session.env` the host refuses by shape alone, or `None` when absent or well formed
/// (senpi `sessionEnvShapeError`).
pub fn session_env_shape_error(value: Option<&Value>) -> Option<String> {
    let value = value?;
    let Some(entries) = value.as_object() else {
        return Some("env must be an object of strings.".to_owned());
    };
    if entries.len() > RPC_INBOUND_LIMITS.env_entries {
        return Some(format!(
            "env allows at most {} entries.",
            RPC_INBOUND_LIMITS.env_entries
        ));
    }
    for (name, entry) in entries {
        if !is_env_name(name) {
            return Some(format!(
                "env name \"{}\" must match ^[A-Z_][A-Z0-9_]*$.",
                echoed_name(name)
            ));
        }
        let Some(text) = entry.as_str() else {
            return Some(format!("env.{} must be a string.", echoed_name(name)));
        };
        if text.contains('\0') {
            return Some(format!("env.{} must not contain NUL.", echoed_name(name)));
        }
        if text.encode_utf16().count() > RPC_INBOUND_LIMITS.env_value_characters {
            return Some(format!(
                "env.{} must be at most {} characters.",
                echoed_name(name),
                RPC_INBOUND_LIMITS.env_value_characters
            ));
        }
    }
    None
}

/// Bounds the schema leaves to the host's own field validators, so both paths refuse the same values.
fn field_bounds_error(record: &Value, object: &Map<String, Value>) -> Option<String> {
    if let Some(err) = rpc_command_payload_error(record) {
        return Some(err.to_owned());
    }
    if let Some(err) = rpc_message_length_error(record) {
        return Some(err);
    }
    let record_type = object.get("type").and_then(Value::as_str)?;
    if matches!(
        record_type,
        "open_session" | "warm" | "park_session" | "claim_session"
    ) && let Some(err) = session_context_error(object.get("context"))
    {
        return Some(err);
    }
    if matches!(record_type, "open_session" | "claim_session") {
        if let Some(err) = retry_fallback_error(object.get("retryFallback")) {
            return Some(err);
        }
        if let Some(err) = session_env_shape_error(object.get("env")) {
            return Some(err);
        }
    }
    if record_type == "set_retry_fallback" {
        return retry_fallback_error(object.get("retryFallback"));
    }
    None
}

/// Whether a caller other than the local interactive user may pass `record` on: an object whose `type`
/// has a schema, that matches it, and whose fields are inside the bounds the host enforces.
pub fn validate_rpc_inbound_record(record: &Value) -> RpcInboundValidation {
    if let Some(shape) = rpc_command_shape_error(record) {
        return RpcInboundValidation::Refused(shape.to_owned());
    }
    let Ok(serialized) = serde_json::to_string(record) else {
        return RpcInboundValidation::Refused("the record is not JSON-serializable.".to_owned());
    };
    let size = if serialized.is_ascii() {
        serialized.len()
    } else {
        serialized.encode_utf16().count()
    };
    if size > MAX_RPC_LINE_CHARACTERS {
        return RpcInboundValidation::Refused(format!(
            "the record is {size} characters of JSON, at most {MAX_RPC_LINE_CHARACTERS}."
        ));
    }
    let Some(object) = record.as_object() else {
        return RpcInboundValidation::Refused("RPC command must be a JSON object.".to_owned());
    };
    let Some(record_type) = object.get("type").and_then(Value::as_str) else {
        return RpcInboundValidation::Refused("type must be a string.".to_owned());
    };
    let Some(schema) = rpc_inbound_schemas().get(record_type) else {
        let shown = if record_type.chars().count() > MAX_REASON_TYPE_CHARACTERS {
            let prefix: String = record_type
                .chars()
                .take(MAX_REASON_TYPE_CHARACTERS)
                .collect();
            format!("{prefix}...")
        } else {
            record_type.to_owned()
        };
        return RpcInboundValidation::Refused(format!(
            "unknown record type: {}.",
            serde_json::json!(shown)
        ));
    };
    if let Err(failure) = schema.validate(record, "") {
        let at = if failure.path.is_empty() {
            String::new()
        } else {
            format!(" at {}", failure.path)
        };
        return RpcInboundValidation::Refused(format!(
            "{record_type}: does not match its schema{at}: {}.",
            failure.message
        ));
    }
    match field_bounds_error(record, object) {
        None => RpcInboundValidation::Ok,
        Some(err) => RpcInboundValidation::Refused(format!("{record_type}: {err}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::method_classes::RPC_METHOD_CLASS_ENTRIES;
    use serde_json::json;
    use std::collections::BTreeSet;

    fn refused(record: &Value) -> String {
        validate_rpc_inbound_record(record)
            .reason()
            .expect("expected refusal")
            .to_owned()
    }

    #[test]
    fn has_exactly_one_schema_per_method_class_table_entry() {
        let schema_keys: BTreeSet<&str> = rpc_inbound_schemas().keys().copied().collect();
        let method_keys: BTreeSet<&str> =
            RPC_METHOD_CLASS_ENTRIES.iter().map(|(k, _)| *k).collect();
        assert_eq!(schema_keys, method_keys);
    }

    #[test]
    fn set_session_settled_mirrors_thread_settled_by() {
        let settle = json!({"type": "set_session_settled", "sessionId": "s", "settled": true});
        assert_eq!(
            validate_rpc_inbound_record(&json!({
                "type": "set_session_settled",
                "sessionId": "s",
                "settled": true,
                "by": {"kind": "pr-merged", "pullRequestNumber": 3}
            })),
            RpcInboundValidation::Ok
        );
        assert_eq!(
            validate_rpc_inbound_record(&json!({
                "type": "set_session_settled",
                "sessionId": "s",
                "settled": true,
                "by": {"kind": "inactivity", "days": 0}
            })),
            RpcInboundValidation::Ok
        );
        let _ = settle;
        assert!(
            refused(&json!({
                "type": "set_session_settled",
                "sessionId": "s",
                "settled": true,
                "by": {"kind": "goal-complete"}
            }))
            .contains("/by")
        );
        assert!(
            refused(&json!({
                "type": "set_session_settled",
                "sessionId": "s",
                "settled": true,
                "by": {"kind": "inactivity"}
            }))
            .contains("/by")
        );
    }

    #[test]
    fn refuses_bound_classes_past_limits_and_accepts_at_limits() {
        let long_id = "s".repeat(RPC_INBOUND_LIMITS.identifier_characters + 1);
        assert!(
            refused(&json!({"type": "get_state", "sessionId": long_id})).contains("/sessionId")
        );
        assert!(
            refused(&json!({"type": "bash", "command": "x".repeat(RPC_INBOUND_LIMITS.text_characters + 1)}))
                .contains("/command")
        );
        assert!(
            refused(&json!({"type": "switch_session", "sessionPath": "/".repeat(RPC_INBOUND_LIMITS.path_characters + 1)}))
                .contains("/sessionPath")
        );
        assert!(
            refused(&json!({"type": "login_api_key", "provider": "p", "key": "k".repeat(RPC_INBOUND_LIMITS.secret_characters + 1)}))
                .contains("/key")
        );
        assert!(refused(&json!({"type": "get_entries", "limit": -5})).contains("/limit"));
        assert!(
            refused(&json!({"type": "get_media", "toolCallId": "t", "contentIndex": 1.5}))
                .contains("/contentIndex")
        );
        assert!(
            refused(&json!({"type": "get_entries", "tail": RPC_INBOUND_LIMITS.count + 2}))
                .contains("/tail")
        );
        assert!(refused(&json!({"type": "set_client_info", "width": -1e308})).contains("/width"));

        // Accepted at exact limits
        assert_eq!(
            validate_rpc_inbound_record(&json!({
                "type": "get_state",
                "sessionId": "s".repeat(RPC_INBOUND_LIMITS.identifier_characters),
                "note": "additive field"
            })),
            RpcInboundValidation::Ok
        );
        assert_eq!(
            validate_rpc_inbound_record(&json!({
                "type": "account_pin",
                "provider": "p",
                "name": null
            })),
            RpcInboundValidation::Ok
        );
        assert_eq!(
            validate_rpc_inbound_record(&json!({
                "type": "record_bash_result",
                "command": "x",
                "result": {"output": "", "exitCode": -9, "cancelled": false, "truncated": false}
            })),
            RpcInboundValidation::Ok
        );
    }

    #[test]
    fn bounds_question_answers_and_whole_record_line_limit() {
        let mut answers = serde_json::Map::new();
        for i in 0..RPC_QUESTION_ANSWER_LIMITS.answers {
            let selected: Vec<Value> = (0..RPC_QUESTION_ANSWER_LIMITS.selected_per_answer)
                .map(|j| Value::String(format!("label-{j}")))
                .collect();
            answers.insert(format!("q{i}"), json!({"selected": selected}));
        }
        assert_eq!(
            validate_rpc_inbound_record(&json!({
                "type": "extension_ui_progress",
                "id": "u",
                "answers": answers
            })),
            RpcInboundValidation::Ok
        );

        let mut too_many = answers;
        too_many.insert("q_extra".into(), json!({"selected": ["a"]}));
        assert!(
            refused(&json!({"type": "extension_ui_progress", "id": "u", "answers": too_many}))
                .contains("at /answers:")
        );

        let wrapper = serde_json::to_string(&json!({"type": "get_state", "pad": ""}))
            .expect("json")
            .len();
        let at_limit =
            json!({"type": "get_state", "pad": "x".repeat(MAX_RPC_LINE_CHARACTERS - wrapper)});
        assert_eq!(
            validate_rpc_inbound_record(&at_limit),
            RpcInboundValidation::Ok
        );

        let over_limit =
            json!({"type": "get_state", "pad": "x".repeat(MAX_RPC_LINE_CHARACTERS - wrapper + 1)});
        assert_eq!(
            refused(&over_limit),
            format!(
                "the record is {} characters of JSON, at most {MAX_RPC_LINE_CHARACTERS}.",
                MAX_RPC_LINE_CHARACTERS + 1
            )
        );
    }

    #[test]
    fn refuses_non_objects_missing_or_unknown_types_and_nul_env_values() {
        for bad in [Value::Null, json!([]), json!("get_state"), json!(7)] {
            assert_eq!(refused(&bad), "RPC command must be a JSON object.");
        }
        assert_eq!(refused(&json!({})), "type must be a string.");
        assert_eq!(
            refused(&json!({"type": "unknown_cmd"})),
            "unknown record type: \"unknown_cmd\"."
        );
        assert!(
            refused(&json!({"type": "open_session", "env": {"OMOCAT_LANE": "a\0b"}}))
                .contains("must not contain NUL")
        );
    }
}
