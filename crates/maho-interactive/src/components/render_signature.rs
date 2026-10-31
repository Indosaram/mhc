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
const SIGNATURE_OMITTED_VALUE_DEPTH: usize = 1;
const SIGNATURE_HASH_OFFSET_BASIS: u32 = 0x811c_9dc5;
const SIGNATURE_HASH_PRIME: u32 = 0x0100_0193;

pub fn create_bounded_render_signature(value: &Value) -> String {
    serialize_bounded_render_summary(&summarize_value(value, 0))
}

/// The bounded summary of `value` found at nesting `depth`, as `create_bounded_render_signature`
/// computes it for each nested value.
pub fn summarize_bounded_render_value(value: &Value, depth: usize) -> Value {
    summarize_value(value, depth)
}

/// Serialize a summary produced by [`summarize_bounded_render_value`] into a signature.
pub fn serialize_bounded_render_summary(summary: &Value) -> String {
    serde_json::to_string(summary).unwrap_or_else(|_| String::from("null"))
}

fn summarize_string(text: &str) -> Value {
    // PERF-DIVERGENCE(senpi render-signature.ts): upstream measures UTF-16 length directly; this port skips the UTF-16 Vec for strings of <= 160 bytes (which have <= 160 units). Output is identical.
    if text.len() <= SIGNATURE_STRING_MAX_LENGTH {
        return Value::String(text.to_owned());
    }
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

fn update_signature_hash(mut hash: u32, source: &str) -> u32 {
    for unit in source.encode_utf16() {
        hash ^= u32::from(unit);
        hash = hash.wrapping_mul(SIGNATURE_HASH_PRIME);
    }
    hash
}

fn format_signature_hash(hash: u32) -> String {
    to_base36(hash)
}

fn hash_signature_string(source: &str) -> String {
    format_signature_hash(update_signature_hash(SIGNATURE_HASH_OFFSET_BASIS, source))
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

/// One omitted entry, summarized at the fixed omitted depth and folded into `hash` without
/// materializing the rest of the tail (#4415). JSON values carry no `toJSON`, so upstream's
/// `serializerKey` has no Rust counterpart.
fn hash_signature_entry(hash: u32, key: &str, value: &Value) -> u32 {
    let summarized = summarize_value(value, SIGNATURE_OMITTED_VALUE_DEPTH);
    let entry = Value::Array(vec![Value::String(key.to_owned()), summarized]);
    update_signature_hash(hash, &serialize_bounded_render_summary(&entry))
}

fn hash_signature_array_tail(tail: &[Value]) -> String {
    let mut hash = SIGNATURE_HASH_OFFSET_BASIS;
    for (offset, item) in tail.iter().enumerate() {
        let key = (SIGNATURE_ARRAY_ITEM_LIMIT + offset).to_string();
        hash = hash_signature_entry(hash, &key, item);
    }
    format_signature_hash(hash)
}

fn hash_signature_entries(entries: &[(&String, &Value)]) -> String {
    let mut hash = SIGNATURE_HASH_OFFSET_BASIS;
    for (key, value) in entries {
        hash = hash_signature_entry(hash, key, value);
    }
    format_signature_hash(hash)
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
                let tail = &items[SIGNATURE_ARRAY_ITEM_LIMIT..];
                let tail_hash = hash_signature_array_tail(tail);
                let mut out = summarized;
                out.push(Value::String(format!(
                    "[+{} items hash={}]",
                    tail.len(),
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
                let omitted = &entries[SIGNATURE_OBJECT_KEY_LIMIT..];
                summarized.insert(
                    String::from("__truncatedKeys"),
                    Value::String(format!(
                        "[+{} keys hash={}]",
                        omitted.len(),
                        hash_signature_entries(omitted)
                    )),
                );
            }
            Value::Object(summarized)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn depth_boundary_value(marker: &str) -> Value {
        let mut value = json!({ "marker": marker });
        for _ in 0..6 {
            value = json!({ "nested": value });
        }
        value
    }

    fn keyed(count: usize, width: usize) -> Map<String, Value> {
        (0..count)
            .map(|index| (format!("key-{index:0width$}"), json!(index)))
            .collect()
    }

    // Hashes pinned from senpi v2026.10.27-2 `createBoundedRenderSignature`.
    #[test]
    fn omitted_tails_hash_entry_by_entry_like_senpi() {
        let object = Value::Object(keyed(90, 3));
        assert!(create_bounded_render_signature(&object).contains("[+10 keys hash=h6jhph]"));
        let mut items: Vec<Value> = (0..40).map(|i| json!(format!("prefix-{i}"))).collect();
        items.push(depth_boundary_value("depth-old"));
        items.push(json!({ "a": [1, 2, { "b": "c" }] }));
        assert!(
            create_bounded_render_signature(&Value::Array(items))
                .contains("[+2 items hash=jn01me]")
        );
        let mut deep = keyed(80, 3);
        deep.insert("zz-tail".into(), depth_boundary_value("depth-old"));
        assert!(
            create_bounded_render_signature(&Value::Object(deep))
                .contains("[+1 keys hash=1kc3j3v]")
        );
    }

    #[test]
    fn a_change_in_a_wide_array_tail_is_detected() {
        let mut values: Vec<Value> = (0..4_000).map(|i| json!(i)).collect();
        let before = create_bounded_render_signature(&Value::Array(values.clone()));
        values[3_999] = json!(-1);
        assert_ne!(
            create_bounded_render_signature(&Value::Array(values)),
            before
        );

        let prefix: Vec<Value> = (0..40).map(|i| json!(format!("prefix-{i}"))).collect();
        let mut previous = prefix.clone();
        previous.push(json!("tail-old"));
        let mut next = prefix;
        next.push(json!("tail-new"));
        assert_ne!(
            create_bounded_render_signature(&Value::Array(next)),
            create_bounded_render_signature(&Value::Array(previous))
        );
    }

    #[test]
    fn a_change_after_the_object_key_limit_is_detected() {
        let previous = keyed(81, 3);
        let mut next = previous.clone();
        next.insert("key-080".into(), json!("changed"));
        assert_ne!(
            create_bounded_render_signature(&Value::Object(next)),
            create_bounded_render_signature(&Value::Object(previous))
        );

        let mut wide = keyed(4_000, 6);
        let before = create_bounded_render_signature(&Value::Object(wide.clone()));
        wide.insert("key-003999".into(), json!(-1));
        assert_ne!(
            create_bounded_render_signature(&Value::Object(wide)),
            before
        );
    }

    #[test]
    fn deeply_nested_omitted_values_keep_the_prior_depth_budget() {
        let prefix: Vec<Value> = (0..40).map(|i| json!(format!("prefix-{i}"))).collect();
        let with = |marker: &str| {
            let mut items = prefix.clone();
            items.push(depth_boundary_value(marker));
            json!({ "content": items })
        };
        assert_ne!(
            create_bounded_render_signature(&with("depth-new")),
            create_bounded_render_signature(&with("depth-old"))
        );

        let object_with = |marker: &str| {
            let mut details = keyed(80, 3);
            details.insert("zz-tail".into(), depth_boundary_value(marker));
            json!({ "details": details })
        };
        assert_ne!(
            create_bounded_render_signature(&object_with("depth-new")),
            create_bounded_render_signature(&object_with("depth-old"))
        );
    }
}
