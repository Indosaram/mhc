//! Port of senpi packages/agent/src/harness/session/jsonl/codec.ts.

use serde_json::Value;

use crate::harness::session::types::JsonValue;

use super::types::{JSONL_FORMAT_VERSION, JsonlStorageHeader};

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyV3SessionHeader {
    #[serde(rename = "type")]
    pub record_type: String,
    pub version: u32,
    pub id: String,
    pub timestamp: String,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session: Option<String>,
}

fn is_record(value: &Value) -> bool {
    value.is_object()
}

fn is_safe_integer_at_least(value: &Value, minimum: i64) -> bool {
    value.as_i64().is_some_and(|number| number >= minimum)
}

pub fn is_legacy_v3_session_header(value: &Value) -> bool {
    if !is_record(value) {
        return false;
    }
    value.get("type").and_then(Value::as_str) == Some("session")
        && value.get("version").and_then(Value::as_u64) == Some(3)
        && value.get("id").and_then(Value::as_str).is_some()
        && value.get("cwd").and_then(Value::as_str).is_some()
        && value
            .get("timestamp")
            .and_then(Value::as_str)
            .is_some_and(is_parsable_timestamp)
        && value
            .get("parentSession")
            .is_none_or(|parent| parent.as_str().is_some())
}

fn is_parsable_timestamp(value: &str) -> bool {
    chrono_like_parse(value)
}

/// ISO-8601 instants accepted by `Date.parse` for the v3 header.
fn chrono_like_parse(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() < 20 {
        return false;
    }
    let digits = |slice: &[u8]| slice.iter().all(u8::is_ascii_digit);
    digits(&bytes[0..4])
        && bytes[4] == b'-'
        && digits(&bytes[5..7])
        && bytes[7] == b'-'
        && digits(&bytes[8..10])
        && (bytes[10] == b'T' || bytes[10] == b' ')
        && digits(&bytes[11..13])
        && bytes[13] == b':'
        && digits(&bytes[14..16])
        && bytes[16] == b':'
        && digits(&bytes[17..19])
}

pub fn is_jsonl_storage_header(value: &Value) -> bool {
    if !is_record(value) {
        return false;
    }
    value.get("kind").and_then(Value::as_str) == Some("header")
        && value.get("v").and_then(Value::as_u64) == Some(JSONL_FORMAT_VERSION as u64)
        && value.get("id").and_then(Value::as_str).is_some()
        && value.get("cwd").and_then(Value::as_str).is_some()
        && value
            .get("storageVersion")
            .is_some_and(|version| is_safe_integer_at_least(version, 1))
        && value
            .get("createdAt")
            .is_some_and(|created| is_safe_integer_at_least(created, 0))
        && value
            .get("nextSeq")
            .is_none_or(|next| is_safe_integer_at_least(next, 1))
        && value
            .get("parentSessionId")
            .is_none_or(|parent| parent.as_str().is_some())
        && value
            .get("legacyParentSessionPath")
            .is_none_or(|parent| parent.as_str().is_some())
}

#[derive(Debug, Clone, PartialEq)]
pub enum JsonlParsedSessionHeader {
    V4(JsonlStorageHeader),
    V3Legacy(LegacyV3SessionHeader),
}

pub fn parse_jsonl_session_header(line: &str) -> Result<JsonlParsedSessionHeader, String> {
    let Ok(value) = serde_json::from_str::<JsonValue>(line) else {
        return Err("Invalid JSONL session header: not valid JSON".to_owned());
    };
    if is_jsonl_storage_header(&value) {
        return serde_json::from_value::<JsonlStorageHeader>(value)
            .map(JsonlParsedSessionHeader::V4)
            .map_err(|_| "Invalid JSONL session header: not valid JSON".to_owned());
    }
    if is_legacy_v3_session_header(&value) {
        return serde_json::from_value::<LegacyV3SessionHeader>(value)
            .map(JsonlParsedSessionHeader::V3Legacy)
            .map_err(|_| "Invalid JSONL session header: not valid JSON".to_owned());
    }
    Err("Unsupported JSONL session header".to_owned())
}
