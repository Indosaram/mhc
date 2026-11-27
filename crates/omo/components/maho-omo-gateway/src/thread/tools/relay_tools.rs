//! Relay tool parameter schemas & helpers (`thread/tools/relay-tools.ts`).

use serde_json::{Value, json};

pub fn relay_tool_schemas() -> [(&'static str, Value); 4] {
    [
        (
            "thread_report",
            json!({
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "enum": ["milestone", "report", "question", "completion"] },
                    "text": { "type": "string" },
                    "binding_id": { "type": "string" },
                    "request_id": { "type": "string" },
                    "request_kind": { "type": "string", "enum": ["question", "select", "confirm", "input", "editor"] },
                    "idempotency_key": { "type": "string" }
                },
                "required": ["kind", "text"]
            }),
        ),
        (
            "thread_outbox",
            json!({
                "type": "object",
                "properties": {
                    "binding_id": { "type": "string" },
                    "after_cursor": { "type": "integer", "minimum": 0 },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 200 }
                },
                "required": ["binding_id"]
            }),
        ),
        (
            "thread_outbox_ack",
            json!({
                "type": "object",
                "properties": {
                    "binding_id": { "type": "string" },
                    "cursor": { "type": "integer", "minimum": 1 }
                },
                "required": ["binding_id", "cursor"]
            }),
        ),
        (
            "thread_answer",
            json!({
                "type": "object",
                "properties": {
                    "binding_id": { "type": "string" },
                    "reply_token": { "type": "string" },
                    "answer": { "type": "string" }
                },
                "required": ["binding_id", "reply_token", "answer"]
            }),
        ),
    ]
}
