//! Port of senpi `packages/coding-agent/src/modes/rpc/session-event-writer-connections.ts`.
//!
//! Connection targeting classification, delegation tracking, and registration helpers.

use serde_json::Value;

/// Returns `true` when an outbound record belongs to one requesting connection rather than
/// broadcasting to every attached session peer.
pub fn is_targeted_record(record: &Value) -> bool {
    let kind = record.get("type").and_then(Value::as_str).unwrap_or("");
    if matches!(
        kind,
        "response" | "bash_execution_update" | "host_request" | "relocation_due"
    ) {
        return true;
    }
    if kind == "extension_ui_request"
        && record.get("audience").and_then(Value::as_str) != Some("session")
    {
        let method = record.get("method").and_then(Value::as_str).unwrap_or("");
        return matches!(method, "select" | "confirm" | "input" | "editor");
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn classifies_targeted_vs_broadcast_records() {
        assert!(is_targeted_record(&json!({"type":"response"})));
        assert!(is_targeted_record(
            &json!({"type":"extension_ui_request","method":"confirm"})
        ));
        assert!(!is_targeted_record(
            &json!({"type":"extension_ui_request","audience":"session","method":"confirm"})
        ));
        assert!(!is_targeted_record(&json!({"type":"agent_start"})));
    }
}
