//! RFC 8785 serialization, retaining ECMAScript binary64 number semantics.

use super::canonical_json_types::{CanonicalJsonError, CanonicalJsonValue, CanonicalJsonViolation};
use std::cmp::Ordering;

pub fn escape_canonical_string(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::from("\"");
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{0c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            c if c < '\u{20}' => {
                let mut units = [0; 2];
                let code = usize::from(c.encode_utf16(&mut units)[0]);
                out.push_str("\\u00");
                out.push(char::from(HEX[code / 16]));
                out.push(char::from(HEX[code % 16]));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
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
        Value::String(value) => out.push_str(&escape_canonical_string(value)),
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
            let mut keys: Vec<_> = values.keys().collect();
            keys.sort_by(|left, right| compare_utf16(left, right));
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&escape_canonical_string(key));
                out.push(':');
                serialize_value(&values[key], out, depth + 1)?;
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
