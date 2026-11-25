//! Port of senpi `packages/coding-agent/src/modes/rpc/json-pieces.ts`.
//!
//! A JSON value as the pieces of its `JSON.stringify` text, in order and without recursion, so
//! concatenated the pieces equal `serde_json::to_string(value)` without building the full string
//! when computing size or chunking.

use serde_json::Value;

pub struct JsonSize {
    pub bytes: usize,
    pub units: usize,
}

enum Frame<'a> {
    Array {
        items: &'a [Value],
        index: usize,
    },
    Object {
        entries: Vec<(&'a String, &'a Value)>,
        index: usize,
    },
}

/// Emits JSON pieces in order and invokes `sink` for each piece.
pub fn for_each_json_piece(value: &Value, mut sink: impl FnMut(&str)) {
    let mut stack: Vec<Frame<'_>> = Vec::new();
    let mut pending: Option<&Value> = Some(value);

    loop {
        if let Some(current) = pending.take() {
            match current {
                Value::Array(items) => {
                    sink("[");
                    stack.push(Frame::Array { items, index: 0 });
                }
                Value::Object(map) => {
                    sink("{");
                    let entries: Vec<_> = map.iter().collect();
                    stack.push(Frame::Object { entries, index: 0 });
                }
                _ => {
                    if let Ok(encoded) = serde_json::to_string(current) {
                        sink(&encoded);
                    } else {
                        sink("null");
                    }
                }
            }
        }
        let Some(top) = stack.last_mut() else {
            return;
        };
        match top {
            Frame::Array { items, index } => {
                if *index == items.len() {
                    stack.pop();
                    sink("]");
                    continue;
                }
                if *index > 0 {
                    sink(",");
                }
                let next = &items[*index];
                *index += 1;
                pending = Some(next);
            }
            Frame::Object { entries, index } => {
                if *index == entries.len() {
                    stack.pop();
                    sink("}");
                    continue;
                }
                if *index > 0 {
                    sink(",");
                }
                let (key, child) = entries[*index];
                *index += 1;
                if let Ok(encoded_key) = serde_json::to_string(key) {
                    sink(&encoded_key);
                } else {
                    sink("\"\"");
                }
                sink(":");
                pending = Some(child);
            }
        }
    }
}

/// Computes UTF-8 bytes and UTF-16 code units without allocating the whole serialized JSON string.
pub fn json_size(value: &Value) -> JsonSize {
    let mut bytes = 0usize;
    let mut units = 0usize;
    for_each_json_piece(value, |piece| {
        bytes += piece.len();
        units += piece.encode_utf16().count();
    });
    JsonSize { bytes, units }
}

/// Regroups serialized JSON pieces into chunks of `size` UTF-16 code units (last chunk may be shorter).
pub fn json_chunks(value: &Value, size: usize) -> Vec<String> {
    if size == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut pending: Vec<u16> = Vec::new();
    for_each_json_piece(value, |piece| {
        pending.extend(piece.encode_utf16());
        while pending.len() >= size {
            let chunk = String::from_utf16_lossy(&pending[..size]);
            out.push(chunk);
            pending.drain(..size);
        }
    });
    if !pending.is_empty() {
        out.push(String::from_utf16_lossy(&pending));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn chunks_concatenate_to_exact_json() {
        let value = json!({"a": [1, "hello 🌍", true, null], "b": {"nested": "ok"}});
        let expected = serde_json::to_string(&value).expect("serialize");
        let size = json_size(&value);
        assert_eq!(size.bytes, expected.len());
        assert_eq!(size.units, expected.encode_utf16().count());
        let joined: String = json_chunks(&value, 5).into_iter().collect();
        assert_eq!(joined, expected);
    }
}
