//! `runners/rpc/ui-auto-answer.ts`: safe deny/cancel defaults so a headless child never blocks on
//! human input. Requests and responses are the host's RPC JSON wire values.

use serde_json::{Value, json};

/// `None` for display-only requests (notify/setStatus/setWidget/setTitle/...), which expect no reply.
pub fn build_auto_ui_response(request: &Value) -> Option<Value> {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    match request.get("method").and_then(Value::as_str) {
        Some("confirm") => {
            Some(json!({ "type": "extension_ui_response", "id": id, "confirmed": false }))
        }
        Some("select" | "input" | "editor") => {
            Some(json!({ "type": "extension_ui_response", "id": id, "cancelled": true }))
        }
        _ => None,
    }
}
