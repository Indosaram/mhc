//! senpi `modes/rpc/compact-call-identity.ts`: RULING-123 (#5346) call-named records and
//! unvouched identity fail-safe helper.

use crate::compact_call_model::{CALL_UNKNOWN, CallIdentity};
use serde_json::Value;

/// RULING-123 (#5346): the execution and hook records whose compact copy names the call they
/// belong to.
pub const CALL_NAMED_RECORDS: [&str; 4] = [
    "tool_execution_start",
    "tool_execution_update",
    "tool_execution_end",
    "tool_hook_status",
];

pub fn is_call_named_record(record_type: &str) -> bool {
    CALL_NAMED_RECORDS.contains(&record_type)
}

fn stream_event(record: &Value) -> Option<&Value> {
    record.get("assistantMessageEvent")
}

/// A `toolcall_start`, `toolcall_delta` or `toolcall_end` update: it streams one call's block of
/// its message.
fn streams_call(record: &Value) -> bool {
    record.get("type").and_then(Value::as_str) == Some("message_update")
        && stream_event(record)
            .and_then(|ev| ev.get("type"))
            .and_then(Value::as_str)
            .is_some_and(|kind| kind.starts_with("toolcall_"))
}

/// The content index of the block a `toolcall_*` update streams; `None` for every other record.
pub fn toolcall_index(record: &Value) -> Option<usize> {
    if !streams_call(record) {
        return None;
    }
    stream_event(record)
        .and_then(|ev| ev.get("contentIndex"))
        .and_then(Value::as_u64)
        .and_then(|idx| usize::try_from(idx).ok())
}

/// What a compact peer's copy of `record` says when the lane holds no identity for it.
pub fn unvouched_call(record: &Value) -> Option<CallIdentity> {
    let named = record
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(is_call_named_record);
    if named || streams_call(record) {
        Some(CALL_UNKNOWN)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn identifies_named_and_streaming_call_records() {
        assert_eq!(
            unvouched_call(&json!({"type": "tool_execution_start"})),
            Some(CALL_UNKNOWN)
        );
        assert_eq!(
            unvouched_call(&json!({
                "type": "message_update",
                "assistantMessageEvent": {"type": "toolcall_delta", "contentIndex": 2}
            })),
            Some(CALL_UNKNOWN)
        );
        assert_eq!(
            toolcall_index(&json!({
                "type": "message_update",
                "assistantMessageEvent": {"type": "toolcall_end", "contentIndex": 3}
            })),
            Some(3)
        );
        assert_eq!(
            unvouched_call(&json!({
                "type": "message_update",
                "assistantMessageEvent": {"type": "text_delta", "contentIndex": 0}
            })),
            None
        );
    }
}
