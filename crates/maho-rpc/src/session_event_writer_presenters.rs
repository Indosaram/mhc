//! Port of senpi `packages/coding-agent/src/modes/rpc/session-event-writer-presenters.ts`.
//!
//! Private MCP form and sealed-secret presenter routing on `SessionEventWriter`.

use crate::jsonl::serialize_json_line;
use serde_json::{Map, Value, json};

pub fn is_private_presenter_record(value: &Value) -> bool {
    let Some(kind) = value.get("type").and_then(Value::as_str) else {
        return false;
    };
    kind.starts_with("mcp_form_")
        || kind == "mcp_presenter_context"
        || kind == "mcp_presenter_secret_answer"
}

pub fn format_mcp_form_reply(
    request_id: Option<&str>,
    reply: &Value,
) -> Result<String, serde_json::Error> {
    let mut out = match reply.as_object() {
        Some(map) => map.clone(),
        None => Map::new(),
    };
    out.insert("type".to_owned(), json!("mcp_form_reply"));
    if let Some(id) = request_id {
        out.insert("id".to_owned(), json!(id));
    }
    serialize_json_line(&Value::Object(out))
}

pub fn format_secret_reply(
    request_id: Option<&str>,
    reply: &Value,
) -> Result<String, serde_json::Error> {
    let mut out = match reply.as_object() {
        Some(map) => map.clone(),
        None => Map::new(),
    };
    out.insert("type".to_owned(), json!("mcp_presenter_secret_reply"));
    if let Some(id) = request_id {
        out.insert("id".to_owned(), json!(id));
    }
    serialize_json_line(&Value::Object(out))
}
