//! Port of `components/render-signature.ts`.
//!
//! Strings are measured and sampled in UTF-16 code units, as JavaScript does, so a signature here
//! equals the one pinned senpi computes for the same JSON value.
use serde_json::{Map, Value};

const SIGNATURE_STRING_MAX_LENGTH: usize = 160;
const SIGNATURE_STRING_SAMPLE_EDGE_LENGTH: usize = 64;
const SIGNATURE_STRING_SAMPLE_WINDOW_LENGTH: usize = 64;
const SIGNATURE_ARRAY_ITEM_LIMIT: usize = 40;
const SIGNATURE_OBJECT_KEY_LIMIT: usize = 80;
const SIGNATURE_DEPTH_LIMIT: usize = 8;

pub fn create_bounded_render_signature(value: &Value) -> String {
    serde_json::to_string(&summarize_value(value, 0)).unwrap_or_else(|_| String::from("null"))
}

fn summarize_string(text: &str) -> Value {
    let units: Vec<u16> = text.encode_utf16().collect();
    if units.len() <= SIGNATURE_STRING_MAX_LENGTH {
        return Value::String(text.to_owned());
    }
    Value::String(format!(
        "[string length={} hash={}]",
        units.len(),
        hash_signature_string(&sample_signature_string(&units))
    ))
}

fn sample_signature_string(units: &[u16]) -> String {
    let slice = |start: usize, length: usize| -> String {
        let end = start.saturating_add(length).min(units.len());
        String::from_utf16_lossy(units.get(start..end).unwrap_or_default())
    };
    let len = units.len();
    let edge = SIGNATURE_STRING_SAMPLE_EDGE_LENGTH;
    let window = SIGNATURE_STRING_SAMPLE_WINDOW_LENGTH;
    let head = slice(0, edge);
    let quarter = slice(len.saturating_sub(window) / 4, window);
    let middle = slice(len.saturating_sub(window) / 2, window);
    let three_quarter = slice((len.saturating_sub(window) * 3) / 4, window);
    let tail = slice(len.saturating_sub(edge), edge);
    [head, quarter, middle, three_quarter, tail].join("\u{0}")
}

fn hash_signature_string(source: &str) -> String {
    let mut hash: u32 = 0x811c_9dc5;
    for unit in source.encode_utf16() {
        hash ^= u32::from(unit);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    to_base36(hash)
}

fn to_base36(mut value: u32) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if value == 0 {
        return String::from("0");
    }
    let mut out = Vec::new();
    while value > 0 {
        out.push(DIGITS[(value % 36) as usize]);
        value /= 36;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_else(|_| String::from("0"))
}

fn hash_signature_value(value: &Value) -> String {
    hash_signature_string(&create_bounded_render_signature(value))
}

fn summarize_value(value: &Value, depth: usize) -> Value {
    match value {
        Value::String(text) => summarize_string(text),
        Value::Number(_) | Value::Bool(_) | Value::Null => value.clone(),
        Value::Array(items) => {
            if depth >= SIGNATURE_DEPTH_LIMIT {
                return Value::String(format!("[array depth-limit length={}]", items.len()));
            }
            let summarized: Vec<Value> = items
                .iter()
                .take(SIGNATURE_ARRAY_ITEM_LIMIT)
                .map(|item| summarize_value(item, depth + 1))
                .collect();
            if items.len() > SIGNATURE_ARRAY_ITEM_LIMIT {
                let tail_hash = hash_signature_value(&Value::Array(items[SIGNATURE_ARRAY_ITEM_LIMIT..].to_vec()));
                let mut out = summarized;
                out.push(Value::String(format!(
                    "[+{} items hash={}]",
                    items.len() - SIGNATURE_ARRAY_ITEM_LIMIT,
                    tail_hash
                )));
                return Value::Array(out);
            }
            Value::Array(summarized)
        }
        Value::Object(map) => {
            if depth >= SIGNATURE_DEPTH_LIMIT {
                return Value::String(String::from("[object depth-limit]"));
            }
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by_key(|entry| entry.0);
            let mut summarized = Map::new();
            for (key, item) in entries.iter().take(SIGNATURE_OBJECT_KEY_LIMIT) {
                summarized.insert((*key).clone(), summarize_value(item, depth + 1));
            }
            if entries.len() > SIGNATURE_OBJECT_KEY_LIMIT {
                let mut omitted = Map::new();
                for (key, item) in entries.iter().skip(SIGNATURE_OBJECT_KEY_LIMIT) {
                    omitted.insert((*key).clone(), (*item).clone());
                }
                summarized.insert(
                    String::from("__truncatedKeys"),
                    Value::String(format!(
                        "[+{} keys hash={}]",
                        entries.len() - SIGNATURE_OBJECT_KEY_LIMIT,
                        hash_signature_value(&Value::Object(omitted))
                    )),
                );
            }
            Value::Object(summarized)
        }
    }
}
