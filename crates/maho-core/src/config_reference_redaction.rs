//! Port of senpi packages/coding-agent/src/core/config-reference-redaction.ts.
//!
//! Every value a configuration reference resolves to is remembered for the process lifetime and redacted,
//! raw or encoded, before text or JSON-shaped data reaches a sink outside the process.

use std::collections::{BTreeSet, HashSet};
use std::sync::{LazyLock, Mutex};

use serde_json::Value;

const MIN_REDACTED_LENGTH: usize = 4;

#[derive(Default)]
struct ResolvedValues {
    values: HashSet<String>,
    /// Longest first; `None` until the next redaction rebuilds it.
    forms: Option<Vec<String>>,
}

static RESOLVED: LazyLock<Mutex<ResolvedValues>> =
    LazyLock::new(|| Mutex::new(ResolvedValues::default()));

fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}

/// Every value a reference resolves to is kept for the process lifetime, including rotated ones: no cap, no eviction.
pub fn remember_resolved_reference_value(value: &str) {
    if utf16_len(value) < MIN_REDACTED_LENGTH {
        return;
    }
    let mut resolved = RESOLVED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if resolved.values.insert(value.to_owned()) {
        resolved.forms = None;
    }
}

fn has_resolved_values() -> bool {
    !RESOLVED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .values
        .is_empty()
}

/// JS `encodeURIComponent`: everything but `A-Z a-z 0-9 - _ . ! ~ * ' ( )` is percent-encoded as UTF-8.
fn encode_uri_component(value: &str) -> String {
    encode_with(value, |byte| {
        byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte)
    })
}

/// JS `encodeURI`: additionally keeps the reserved URI characters `;,/?:@&=+$#`.
fn encode_uri(value: &str) -> String {
    encode_with(value, |byte| {
        byte.is_ascii_alphanumeric() || b"-_.!~*'();,/?:@&=+$#".contains(&byte)
    })
}

/// `URLSearchParams` form encoding: `application/x-www-form-urlencoded` (space as `+`).
fn encode_form(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte == b' ' {
            out.push('+');
        } else if byte.is_ascii_alphanumeric() || b"*-._".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn encode_with(value: &str, keep: impl Fn(u8) -> bool) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if keep(byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// `JSON.stringify(value).slice(1, -1)`.
fn json_inner(value: &str) -> String {
    let quoted = serde_json::to_string(value).unwrap_or_default();
    quoted[1..quoted.len() - 1].to_owned()
}

fn encodings(value: &str) -> [String; 5] {
    [
        value.to_owned(),
        json_inner(value),
        encode_uri_component(value),
        encode_uri(value),
        encode_form(value),
    ]
}

fn forms() -> Vec<String> {
    let mut resolved = RESOLVED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if resolved.forms.is_none() {
        let unique: BTreeSet<String> = resolved
            .values
            .iter()
            .flat_map(|value| encodings(value))
            .filter(|form| utf16_len(form) >= MIN_REDACTED_LENGTH)
            .collect();
        let mut forms: Vec<String> = unique.into_iter().collect();
        forms.sort_by_key(|form| std::cmp::Reverse(utf16_len(form)));
        resolved.forms = Some(forms);
    }
    resolved.forms.clone().unwrap_or_default()
}

/// Replaces every resolved reference value, raw or encoded, before text reaches a sink outside the process.
pub fn redact_resolved_reference_values(text: &str, replacement: &str) -> String {
    let mut redacted = text.to_owned();
    for form in forms() {
        if redacted.contains(&form) {
            redacted = redacted
                .split(form.as_str())
                .collect::<Vec<_>>()
                .join(replacement);
        }
    }
    redacted
}

/// MCP content-block and resource kinds plus JSON Schema types.
const PROTOCOL_TYPES: &[&str] = &[
    "text",
    "image",
    "audio",
    "resource",
    "resource_link",
    "object",
    "array",
    "string",
    "number",
    "integer",
    "boolean",
    "null",
];

/// JSON Schema string formats.
const SCHEMA_FORMATS: &[&str] = &[
    "date-time",
    "date",
    "time",
    "duration",
    "email",
    "idn-email",
    "hostname",
    "idn-hostname",
    "ipv4",
    "ipv6",
    "uri",
    "uri-reference",
    "iri",
    "iri-reference",
    "uri-template",
    "uuid",
    "json-pointer",
    "relative-json-pointer",
    "regex",
    "byte",
    "binary",
    "password",
];

/// Media types MCP servers send in content blocks and resources.
const MEDIA_TYPES: &[&str] = &[
    "text/plain",
    "text/markdown",
    "text/html",
    "text/css",
    "text/csv",
    "text/javascript",
    "text/xml",
    "text/x-python",
    "text/x-typescript",
    "application/json",
    "application/xml",
    "application/yaml",
    "application/javascript",
    "application/typescript",
    "application/pdf",
    "application/zip",
    "application/gzip",
    "application/octet-stream",
    "application/x-ndjson",
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "image/svg+xml",
    "image/bmp",
    "image/avif",
    "image/heic",
    "image/tiff",
    "audio/wav",
    "audio/mpeg",
    "audio/mp3",
    "audio/ogg",
    "audio/webm",
    "audio/flac",
    "audio/aac",
    "video/mp4",
    "video/webm",
];

/// The published JSON Schema draft identifiers.
const SCHEMA_URLS: &[&str] = &[
    "http://json-schema.org/draft-04/schema#",
    "http://json-schema.org/draft-06/schema#",
    "http://json-schema.org/draft-07/schema#",
    "http://json-schema.org/schema#",
    "https://json-schema.org/draft/2019-09/schema",
    "https://json-schema.org/draft/2020-12/schema",
];

/// Each protocol key keeps a value only when it EXACTLY equals an entry of its allowlist (no open patterns, so
/// no secret can hide inside a kept value); any other value is redacted as usual (fail closed). An allowlisted
/// value is a public protocol token, so it is kept even when a remembered value happens to equal it.
fn is_protocol_value(key: &str, value: &str) -> bool {
    let allowed: &[&str] = match key {
        "type" => PROTOCOL_TYPES,
        "mimeType" => MEDIA_TYPES,
        "format" => SCHEMA_FORMATS,
        "jsonrpc" => &["2.0"],
        "role" => &["user", "assistant"],
        "$schema" => SCHEMA_URLS,
        _ => return false,
    };
    allowed.contains(&value)
}

/// Copies JSON-shaped data, redacting string leaves only; never mutates the wire-facing original.
pub fn redact_resolved_reference_data(value: &Value) -> Value {
    if !has_resolved_values() {
        return value.clone();
    }
    let forms = forms();
    let redact = |text: &str| {
        let mut redacted = text.to_owned();
        for form in &forms {
            if redacted.contains(form.as_str()) {
                redacted = redacted
                    .split(form.as_str())
                    .collect::<Vec<_>>()
                    .join("<redacted>");
            }
        }
        redacted
    };
    fn copy(value: &Value, redact: &dyn Fn(&str) -> String) -> Value {
        match value {
            Value::String(text) => Value::String(redact(text)),
            Value::Array(items) => {
                Value::Array(items.iter().map(|item| copy(item, redact)).collect())
            }
            Value::Object(entries) => Value::Object(
                entries
                    .iter()
                    .map(|(key, child)| {
                        let kept = match child {
                            Value::String(text) if is_protocol_value(key, text) => child.clone(),
                            other => copy(other, redact),
                        };
                        (key.clone(), kept)
                    })
                    .collect(),
            ),
            other => other.clone(),
        }
    }
    copy(value, &redact)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// senpi test/mcp/3858-reference-disclosure.test.ts: raw and encoded forms are redacted at the sink.
    #[test]
    fn resolved_values_are_redacted_raw_and_encoded() {
        remember_resolved_reference_value("ref secret/value+1");
        let text =
            "raw ref secret/value+1 uri ref%20secret%2Fvalue%2B1 form ref+secret%2Fvalue%2B1";
        let redacted = redact_resolved_reference_values(text, "<redacted>");
        assert!(!redacted.contains("ref secret/value+1"), "{redacted}");
        assert!(!redacted.contains("ref%20secret%2Fvalue%2B1"), "{redacted}");
        assert!(!redacted.contains("ref+secret%2Fvalue%2B1"), "{redacted}");
    }

    #[test]
    fn short_values_are_not_remembered() {
        remember_resolved_reference_value("abc");
        assert_eq!(
            redact_resolved_reference_values("abc abc", "<x>"),
            "abc abc"
        );
    }

    /// senpi test/mcp/3858-reference-structure.test.ts: protocol tokens survive only on their exact allowlist.
    #[test]
    fn data_keeps_allowlisted_protocol_values_only() {
        // A remembered value that equals an allowlisted token stays readable only under that key.
        remember_resolved_reference_value("uri-template");
        remember_resolved_reference_value("structured-secret-xyz");
        let data = json!({
            "format": "uri-template",
            "description": "uri-template structured-secret-xyz",
            "type": "structured-secret-xyz",
            "nested": [{"value": "structured-secret-xyz"}]
        });
        let redacted = redact_resolved_reference_data(&data);
        assert_eq!(redacted["format"], "uri-template");
        assert_eq!(redacted["description"], "<redacted> <redacted>");
        assert_eq!(redacted["type"], "<redacted>");
        assert_eq!(redacted["nested"][0]["value"], "<redacted>");
        assert_eq!(data["nested"][0]["value"], "structured-secret-xyz");
    }
}
