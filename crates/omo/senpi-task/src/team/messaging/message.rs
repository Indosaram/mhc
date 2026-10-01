//! Team message construction and the peer-message envelope.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use team_core::types::{Message, SchemaIssues};

use crate::team::messaging::types::SendTeamMessageInput;

#[derive(Default)]
pub struct BuildTeamMessageOptions<'a> {
    pub now: Option<&'a dyn Fn() -> i64>,
    pub new_message_id: Option<&'a dyn Fn() -> String>,
}

static UUID_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Random-looking RFC 4122 v4 UUID derived from time, pid and a process counter.
pub fn random_uuid() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let counter = UUID_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut hasher = Sha256::new();
    hasher.update(nanos.to_le_bytes());
    hasher.update(std::process::id().to_le_bytes());
    hasher.update(counter.to_le_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

/// Builds a `kind: "message"` team-core `Message` for a team send. `messageId`/`timestamp` are
/// injected (defaulting to a random UUID / the current epoch ms) so tests stay deterministic. `to` is
/// passed through verbatim (a member name, the "lead" sentinel, or "*");
/// `correlationId`/`references`/`color` are left unset.
pub fn build_team_message(
    input: &SendTeamMessageInput,
    options: &BuildTeamMessageOptions<'_>,
) -> Result<Message, SchemaIssues> {
    let timestamp = match options.now {
        Some(now) => now(),
        None => chrono::Utc::now().timestamp_millis(),
    };
    let message_id = match options.new_message_id {
        Some(new_message_id) => new_message_id(),
        None => random_uuid(),
    };
    let mut raw = json!({
        "version": 1,
        "messageId": message_id,
        "from": input.from,
        "to": input.to,
        "kind": "message",
        "body": input.body,
        "timestamp": timestamp,
    });
    if let (Some(summary), Some(record)) = (input.summary.as_ref(), raw.as_object_mut()) {
        record.insert("summary".to_string(), Value::String(summary.clone()));
    }
    Message::safe_parse(&raw)
}

fn string_field(record: &Map<String, Value>, key: &str) -> Option<String> {
    match record.get(key) {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => Some(text.clone()),
        Some(other) => Some(other.to_string()),
    }
}

/// Byte-for-byte mirror of team-core team-mailbox `build_envelope` (poll), replicated here because
/// that helper is only reachable via a forbidden deep subpath upstream.
pub fn build_peer_message_envelope(message: &Message) -> String {
    let record = match serde_json::to_value(message) {
        Ok(Value::Object(record)) => record,
        _ => Map::new(),
    };
    let get = |key: &str| string_field(&record, key).unwrap_or_default();

    let mut attributes = vec![
        format!("from=\"{}\"", escape_attribute_value(&get("from"))),
        format!("timestamp=\"{}\"", escape_attribute_value(&get("timestamp"))),
        format!("messageId=\"{}\"", escape_attribute_value(&get("messageId"))),
        format!("kind=\"{}\"", escape_attribute_value(&get("kind"))),
        format!("correlationId=\"{}\"", escape_attribute_value(&get("correlationId"))),
    ];
    if let Some(summary) = string_field(&record, "summary") {
        attributes.push(format!("summary=\"{}\"", escape_attribute_value(&summary)));
    }
    if let Some(references) = record.get("references").filter(|references| !references.is_null()) {
        attributes.push(format!(
            "references=\"{}\"",
            escape_attribute_value(&references.to_string())
        ));
    }
    format!("<peer_message {}>\n{}\n</peer_message>", attributes.join(" "), get("body"))
}

fn escape_attribute_value(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\'', "&apos;")
}
