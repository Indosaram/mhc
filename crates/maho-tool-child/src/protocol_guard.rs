//! Boundary check for a decoded frame (`protocol-guard.ts`): a known `type`, every required field of the right kind,
//! no unknown field. Works on the raw `serde_json::Value` so the check is independent of the typed decode.

use serde_json::{Map, Value};

use crate::protocol::{
    HOST_CALL_KINDS, LAUNCH_APIS, LAUNCH_CONTAINMENTS, LAUNCH_LIFETIMES, LAUNCH_ORIGINS,
    REF_NACK_REASONS, TRACE_RESERVED_KEYS,
};

#[derive(Clone, Copy)]
enum FieldKind {
    String,
    Number,
    Boolean,
    Json,
    Ref,
    Entries,
    Detail,
    NumberOrNull,
    Strings,
    OneOf(&'static [&'static str]),
}

#[derive(Clone, Copy)]
struct FieldRule {
    kind: FieldKind,
    optional: bool,
}

const fn req(kind: FieldKind) -> FieldRule {
    FieldRule {
        kind,
        optional: false,
    }
}
const fn opt(kind: FieldKind) -> FieldRule {
    FieldRule {
        kind,
        optional: true,
    }
}

type Shape = &'static [(&'static str, FieldRule)];

/// One entry per message type, in the upstream `SHAPES` order.
const SHAPES: &[(&str, Shape)] = &[
    ("hello_ack", &[("generation", req(FieldKind::Number))]),
    (
        "job",
        &[
            ("jobId", req(FieldKind::String)),
            ("kind", req(FieldKind::String)),
            ("parentJobId", opt(FieldKind::String)),
            ("args", opt(FieldKind::Json)),
            ("argsRef", opt(FieldKind::Ref)),
        ],
    ),
    ("cancel", &[("jobId", req(FieldKind::String))]),
    (
        "host_reply",
        &[
            ("callId", req(FieldKind::String)),
            ("value", opt(FieldKind::Json)),
            ("valueRef", opt(FieldKind::Ref)),
            ("error", opt(FieldKind::String)),
        ],
    ),
    (
        "host_call_refused",
        &[
            ("callId", req(FieldKind::String)),
            ("code", req(FieldKind::String)),
        ],
    ),
    ("launch_ack", &[("launchId", req(FieldKind::String))]),
    ("wake_ack", &[("wakeId", req(FieldKind::String))]),
    ("slot_query", &[("launchId", req(FieldKind::String))]),
    ("ping", &[("seq", req(FieldKind::Number))]),
    (
        "update_ack",
        &[
            ("jobId", req(FieldKind::String)),
            ("seq", req(FieldKind::Number)),
        ],
    ),
    ("shutdown", &[]),
    (
        "hello",
        &[
            ("generation", req(FieldKind::Number)),
            ("pid", req(FieldKind::Number)),
            ("protocol", req(FieldKind::Number)),
            ("jobKinds", req(FieldKind::Strings)),
        ],
    ),
    ("heartbeat", &[("seq", req(FieldKind::Number))]),
    ("pong", &[("seq", req(FieldKind::Number))]),
    ("cancel_ack", &[("jobId", req(FieldKind::String))]),
    (
        "result",
        &[
            ("jobId", req(FieldKind::String)),
            ("value", req(FieldKind::Json)),
        ],
    ),
    (
        "result_ref",
        &[
            ("jobId", req(FieldKind::String)),
            ("ref", req(FieldKind::Ref)),
        ],
    ),
    (
        "job_error",
        &[
            ("jobId", req(FieldKind::String)),
            ("code", req(FieldKind::String)),
            ("message", req(FieldKind::String)),
        ],
    ),
    (
        "job_refused",
        &[
            ("jobId", req(FieldKind::String)),
            ("code", req(FieldKind::String)),
        ],
    ),
    (
        "host_call",
        &[
            ("callId", req(FieldKind::String)),
            ("jobId", req(FieldKind::String)),
            ("kind", req(FieldKind::OneOf(&HOST_CALL_KINDS))),
            ("name", opt(FieldKind::String)),
            ("args", opt(FieldKind::Json)),
            ("argsRef", opt(FieldKind::Ref)),
        ],
    ),
    (
        "launch",
        &[
            ("launchId", req(FieldKind::String)),
            ("jobId", req(FieldKind::String)),
            ("origin", req(FieldKind::OneOf(&LAUNCH_ORIGINS))),
            ("api", req(FieldKind::OneOf(&LAUNCH_APIS))),
            ("containment", req(FieldKind::OneOf(&LAUNCH_CONTAINMENTS))),
            ("lifetime", req(FieldKind::OneOf(&LAUNCH_LIFETIMES))),
            ("pid", opt(FieldKind::Number)),
            ("pgid", opt(FieldKind::Number)),
        ],
    ),
    (
        "launch_proof",
        &[
            ("launchId", req(FieldKind::String)),
            ("ackTick", req(FieldKind::NumberOrNull)),
            ("invokeTick", req(FieldKind::Number)),
        ],
    ),
    (
        "launch_done",
        &[
            ("launchId", req(FieldKind::String)),
            ("exitCode", opt(FieldKind::Number)),
            ("signal", opt(FieldKind::String)),
        ],
    ),
    (
        "command_started",
        &[
            ("launchId", req(FieldKind::String)),
            ("pid", req(FieldKind::Number)),
            ("startedAtMs", req(FieldKind::Number)),
            ("creatorInJob", req(FieldKind::Boolean)),
            ("breakaway", req(FieldKind::Boolean)),
        ],
    ),
    (
        "slot_state",
        &[
            ("launchId", req(FieldKind::String)),
            ("ackTick", req(FieldKind::NumberOrNull)),
            ("invokeTick", req(FieldKind::NumberOrNull)),
            ("invocations", req(FieldKind::Number)),
        ],
    ),
    (
        "wake",
        &[
            ("wakeId", req(FieldKind::String)),
            ("source", req(FieldKind::String)),
            ("bytes", req(FieldKind::Number)),
            ("value", opt(FieldKind::Json)),
        ],
    ),
    (
        "batch",
        &[
            ("jobId", req(FieldKind::String)),
            ("entries", req(FieldKind::Entries)),
            ("done", req(FieldKind::Boolean)),
        ],
    ),
    (
        "update",
        &[
            ("jobId", req(FieldKind::String)),
            ("seq", req(FieldKind::Number)),
            ("value", opt(FieldKind::Json)),
            ("valueRef", opt(FieldKind::Ref)),
        ],
    ),
    (
        "trace",
        &[
            ("atMs", req(FieldKind::Number)),
            ("event", req(FieldKind::String)),
            ("detail", opt(FieldKind::Detail)),
        ],
    ),
    ("ref_ack", &[("refId", req(FieldKind::String))]),
    (
        "ref_nack",
        &[
            ("refId", req(FieldKind::String)),
            ("reason", req(FieldKind::OneOf(&REF_NACK_REASONS))),
        ],
    ),
];

/// `TOOL_CHILD_MESSAGE_TYPES`.
pub fn tool_child_message_types() -> Vec<&'static str> {
    SHAPES.iter().map(|(name, _)| *name).collect()
}

const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// `Number.isSafeInteger`, over a JSON number (an integral float such as `3.0` counts, as it does in JS).
fn safe_integer(value: &Value) -> Option<i64> {
    let number = value.as_number()?;
    if let Some(integer) = number.as_i64() {
        return (integer.unsigned_abs() <= MAX_SAFE_INTEGER as u64).then_some(integer);
    }
    if let Some(integer) = number.as_u64() {
        return (integer <= MAX_SAFE_INTEGER as u64).then_some(integer as i64);
    }
    let float = number.as_f64()?;
    (float.is_finite() && float.fract() == 0.0 && float.abs() <= MAX_SAFE_INTEGER)
        .then_some(float as i64)
}

fn is_count(value: &Value) -> bool {
    safe_integer(value).is_some_and(|integer| integer >= 0)
}

/// `isRefId`: `/^[A-Za-z0-9_-]{1,64}$/`.
pub fn is_ref_id(value: &Value) -> bool {
    value.as_str().is_some_and(is_ref_id_str)
}

/// `isRefId` for an already-typed string.
pub fn is_ref_id_str(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

/// `/^[0-9a-f]{64}$/`.
fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn as_record(value: &Value) -> Option<&Map<String, Value>> {
    value.as_object()
}

fn is_payload_ref(value: &Value) -> bool {
    let Some(record) = as_record(value) else {
        return false;
    };
    record.get("refId").is_some_and(is_ref_id)
        && record.get("generation").is_some_and(is_count)
        && record.get("bytes").is_some_and(is_count)
        && record
            .get("sha256")
            .and_then(Value::as_str)
            .is_some_and(is_sha256)
}

fn is_entry_meta(value: &Value) -> bool {
    let Some(record) = as_record(value) else {
        return false;
    };
    record.get("id").is_some_and(Value::is_string)
        && record
            .get("parentId")
            .is_some_and(|parent| parent.is_null() || parent.is_string())
        && record.get("lineOffset").is_some_and(is_count)
        && record.get("lineBytes").is_some_and(is_count)
        && record
            .get("lineDelta")
            .is_some_and(|delta| safe_integer(delta).is_some())
}

fn is_trace_detail(value: &Value) -> bool {
    let Some(record) = as_record(value) else {
        return false;
    };
    record.iter().all(|(key, item)| {
        !TRACE_RESERVED_KEYS.contains(&key.as_str())
            && (item.is_string() || item.is_number() || item.is_boolean())
    })
}

fn matches(kind: FieldKind, value: &Value) -> bool {
    match kind {
        FieldKind::OneOf(options) => value.as_str().is_some_and(|text| options.contains(&text)),
        FieldKind::String => value.is_string(),
        FieldKind::Number => value.is_number(),
        FieldKind::Boolean => value.is_boolean(),
        FieldKind::Json => true,
        FieldKind::Ref => is_payload_ref(value),
        FieldKind::Entries => value
            .as_array()
            .is_some_and(|items| items.iter().all(is_entry_meta)),
        FieldKind::Detail => is_trace_detail(value),
        FieldKind::NumberOrNull => value.is_null() || value.is_number(),
        FieldKind::Strings => value
            .as_array()
            .is_some_and(|items| items.iter().all(Value::is_string)),
    }
}

fn shape_of(message_type: &str) -> Option<Shape> {
    SHAPES
        .iter()
        .find(|(name, _)| *name == message_type)
        .map(|(_, shape)| *shape)
}

/// Boundary check for a decoded frame: a known `type`, every required field of the right kind, no unknown field.
/// An unknown field is refused rather than ignored, so a `trace` frame can never smuggle a result, proof or tick.
pub fn is_tool_child_message(value: &Value) -> bool {
    let Some(record) = as_record(value) else {
        return false;
    };
    let Some(rules) = record
        .get("type")
        .and_then(Value::as_str)
        .and_then(shape_of)
    else {
        return false;
    };
    if record
        .keys()
        .any(|key| key != "type" && !rules.iter().any(|(name, _)| name == key))
    {
        return false;
    }
    rules.iter().all(|(key, rule)| match record.get(*key) {
        None => rule.optional,
        Some(field) => matches(rule.kind, field),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn rejects_unknown_fields_and_types() {
        assert!(is_tool_child_message(&json!({"type": "shutdown"})));
        assert!(!is_tool_child_message(&json!({"type": "shutdown", "x": 1})));
        assert!(!is_tool_child_message(&json!({"type": "nope"})));
        assert!(!is_tool_child_message(&json!([1])));
    }

    #[test]
    fn ref_id_matches_upstream_pattern() {
        assert!(is_ref_id(&json!("r-1_A")));
        assert!(!is_ref_id(&json!("")));
        assert!(!is_ref_id(&json!("a".repeat(65))));
        assert!(!is_ref_id(&json!("../x")));
        assert!(!is_ref_id(&json!(3)));
    }

    #[test]
    fn json_field_accepts_null_but_not_absence() {
        assert!(is_tool_child_message(
            &json!({"type": "result", "jobId": "j", "value": null})
        ));
        assert!(!is_tool_child_message(
            &json!({"type": "result", "jobId": "j"})
        ));
    }
}
