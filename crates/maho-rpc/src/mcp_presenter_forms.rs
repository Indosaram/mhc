//! Port of senpi `packages/coding-agent/src/modes/rpc/mcp-presenter-forms.ts`.
//!
//! Private `mcp_form_*` inbound frame validation and dispatch helpers.

use serde_json::Value;

pub const ALLOWED_FORM_FIELDS: [&str; 9] = [
    "type",
    "id",
    "sessionId",
    "bindingEpoch",
    "attachmentEpoch",
    "grantId",
    "dialogId",
    "dialogToken",
    "formRevision",
];

/// Validates that an inbound `mcp_form_*` frame carries only allowed fields and required tokens.
pub fn validate_form_frame_shape(value: &Value) -> Result<&str, &'static str> {
    let Some(obj) = value.as_object() else {
        return Err("invalid_form_response");
    };
    let Some(kind) = obj.get("type").and_then(Value::as_str) else {
        return Err("invalid_form_response");
    };
    for key in obj.keys() {
        let ok = ALLOWED_FORM_FIELDS.contains(&key.as_str())
            || (kind == "mcp_form_response" && matches!(key.as_str(), "action" | "content"));
        if !ok {
            return Err("invalid_form_response");
        }
    }
    if kind == "mcp_form_sync" {
        return Ok(kind);
    }
    if !obj.get("dialogId").is_some_and(Value::is_string)
        || !obj.get("dialogToken").is_some_and(Value::is_string)
        || !obj.get("formRevision").is_some_and(Value::is_string)
    {
        return Err("invalid_form_response");
    }
    if kind == "mcp_form_response" && !obj.get("action").is_some_and(Value::is_string) {
        return Err("invalid_form_response");
    }
    if matches!(kind, "mcp_form_presented" | "mcp_form_response") {
        Ok(kind)
    } else {
        Err("invalid_form_response")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validates_sync_presented_and_response_shapes() {
        assert_eq!(
            validate_form_frame_shape(&json!({"type":"mcp_form_sync","sessionId":"s1"})),
            Ok("mcp_form_sync")
        );
        assert_eq!(
            validate_form_frame_shape(&json!({
                "type":"mcp_form_response",
                "dialogId":"d1",
                "dialogToken":"t1",
                "formRevision":"r1",
                "action":"accept"
            })),
            Ok("mcp_form_response")
        );
        assert_eq!(
            validate_form_frame_shape(&json!({"type":"mcp_form_sync","extra":1})),
            Err("invalid_form_response")
        );
    }
}
