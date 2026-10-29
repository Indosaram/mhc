//! RFC 8785 serialization, retaining ECMAScript binary64 number semantics.

use super::canonical_json_types::{CanonicalJsonError, CanonicalJsonValue, CanonicalJsonViolation};
use std::cmp::Ordering;

pub fn escape_canonical_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    push_escaped_canonical_string(&mut out, value);
    out
}

// PERF-DIVERGENCE(senpi canonical-json): upstream builds a fresh escaped string per key/value and then copies it into the output; this port escapes straight into the output buffer, copying unescaped runs as slices. Output bytes are identical.
fn push_escaped_canonical_string(out: &mut String, value: &str) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out.push('"');
    let mut run_start = 0;
    for (index, character) in value.char_indices() {
        let escape: Option<&str> = match character {
            '"' => Some("\\\""),
            '\\' => Some("\\\\"),
            '\u{08}' => Some("\\b"),
            '\t' => Some("\\t"),
            '\n' => Some("\\n"),
            '\u{0c}' => Some("\\f"),
            '\r' => Some("\\r"),
            c if c < '\u{20}' => None,
            _ => continue,
        };
        out.push_str(&value[run_start..index]);
        run_start = index + character.len_utf8();
        match escape {
            Some(text) => out.push_str(text),
            None => {
                let code = usize::from(u8::try_from(character).unwrap_or(0));
                out.push_str("\\u00");
                out.push(char::from(HEX[code / 16]));
                out.push(char::from(HEX[code % 16]));
            }
        }
    }
    out.push_str(&value[run_start..]);
    out.push('"');
}

pub fn compare_utf16(left: &str, right: &str) -> Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}

pub(crate) fn canonical_number(value: f64) -> String {
    // Unlike Rust Display or serde_json's formatter, ryu-js implements the
    // ECMAScript shortest-round-trip choices and fixed/exponential cutovers.
    if value == 0.0 {
        return String::from("0");
    }
    ryu_js::Buffer::new().format(value).to_owned()
}

const MAX_SERIALIZE_DEPTH: usize = 64;

fn serialize_value(
    value: &CanonicalJsonValue,
    out: &mut String,
    depth: usize,
) -> Result<(), CanonicalJsonError> {
    use serde_json::Value;
    if matches!(value, Value::Array(_) | Value::Object(_)) && depth >= MAX_SERIALIZE_DEPTH {
        return Err(CanonicalJsonError::new(
            CanonicalJsonViolation::Json,
            "nesting deeper than 64",
        ));
    }
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
        Value::Number(value) => {
            let number = value.as_f64().ok_or_else(|| {
                CanonicalJsonError::new(CanonicalJsonViolation::Number, "number is not finite")
            })?;
            out.push_str(&canonical_number(number));
        }
        Value::String(value) => push_escaped_canonical_string(out, value),
        Value::Array(values) => {
            out.push('[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                serialize_value(value, out, depth + 1)?;
            }
            out.push(']');
        }
        Value::Object(values) => {
            out.push('{');
            // PERF-DIVERGENCE(senpi canonical-json): upstream sorts keys and then looks each value up again by key; this port sorts (key, value) pairs so no second map lookup happens. Output is identical.
            let mut entries: Vec<_> = values.iter().collect();
            entries.sort_by(|left, right| compare_utf16(left.0, right.0));
            for (index, (key, child)) in entries.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                push_escaped_canonical_string(out, key);
                out.push(':');
                serialize_value(child, out, depth + 1)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

pub fn canonicalize_json(value: &CanonicalJsonValue) -> Result<Vec<u8>, CanonicalJsonError> {
    let mut out = String::new();
    serialize_value(value, &mut out, 0)?;
    Ok(out.into_bytes())
}
