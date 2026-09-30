//! Port of senpi packages/coding-agent/src/core/credential-pool/rotation-events.ts.

use serde_json::Value;

/// True when a stream event starts a rotation: the committed output of the previous slot must not
/// be replayed.
pub fn is_rotation_stream_start(event: &Value) -> bool {
    matches!(event.get("type").and_then(Value::as_str), Some("start"))
}

pub fn is_committed_rotation_output(event: &Value) -> bool {
    matches!(
        event.get("type").and_then(Value::as_str),
        Some("text_delta") | Some("thinking_delta") | Some("toolcall_delta") | Some("toolcall_end")
    )
}

/// The provider failure carried by a terminal rotation event, if any: the event's own error value,
/// so partial content, usage and transport diagnostics stay intact for the caller.
pub fn rotation_error_from_event(event: &Value) -> Option<Value> {
    match event.get("type").and_then(Value::as_str) {
        Some("error") => Some(event.get("error").cloned().unwrap_or_else(|| Value::from("provider error"))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn classifies_stream_events() {
        assert!(is_rotation_stream_start(&json!({ "type": "start" })));
        assert!(!is_rotation_stream_start(&json!({ "type": "text_delta" })));
        assert!(is_committed_rotation_output(&json!({ "type": "text_delta" })));
        assert!(is_committed_rotation_output(&json!({ "type": "toolcall_end" })));
        assert!(!is_committed_rotation_output(&json!({ "type": "start" })));
    }

    #[test]
    fn reads_the_error_text_from_a_terminal_event() {
        assert_eq!(
            rotation_error_from_event(&json!({ "type": "error", "error": { "errorMessage": "boom" } })),
            Some(json!({ "errorMessage": "boom" }))
        );
        assert_eq!(rotation_error_from_event(&json!({ "type": "error" })), Some(json!("provider error")));
        assert_eq!(rotation_error_from_event(&json!({ "type": "done" })), None);
    }
}
