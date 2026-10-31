//! Port of `tool/display-json-bounds.ts` (senpi 903b8336): bounds one display value for
//! persistence in `details.jsonOutputs`. The value is trimmed inside, never replaced: every shown
//! key keeps at least a stub, serialized Buffers become `<Buffer N>`, long strings, arrays and
//! objects carry a visible `… elided` / `N more items` / `N more keys` marker, keys are trimmed like
//! strings, and the real serialized length is checked last (#3261). Lengths are JS string lengths.

use super::display_json_size::{
    MAX_DISPLAY_JSON_BYTES, buffer_placeholder, js_length, json_string_length,
    serialized_buffer_byte_length, serialized_string_length,
};
use super::json_tree::{
    JSON_TREE_MAX_DEPTH_EXPANDED, JSON_TREE_MAX_LINES_EXPANDED, JSON_TREE_SCALAR_LEN_EXPANDED,
};
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};

const STUB: &str = "…";
const STUB_COST: usize = 3; // JSON.stringify("…")
const MAX_KEY_CHARS: usize = 64;

/// `JSON.stringify(n).length` for a finite JSON number (JS prints exponents as `e+21`).
fn number_length(number: &serde_json::Number) -> usize {
    let text = number.to_string();
    match text.find('e') {
        Some(index) if !text[index + 1..].starts_with('-') => text.len() + 1,
        _ => text.len(),
    }
}

/// `JSON.stringify(value).length`, with `undefined` (an absent value) as 0.
fn serialized_length(value: &Value) -> usize {
    match value {
        Value::Null => 4,
        Value::Bool(true) => 4,
        Value::Bool(false) => 5,
        Value::Number(number) => number_length(number),
        Value::String(text) => json_string_length(text),
        Value::Array(items) => {
            2 + items.len().saturating_sub(1) + items.iter().map(serialized_length).sum::<usize>()
        }
        Value::Object(entries) => {
            2 + entries.len().saturating_sub(1)
                + entries
                    .iter()
                    .map(|(key, value)| json_string_length(key) + 1 + serialized_length(value))
                    .sum::<usize>()
        }
    }
}

fn elision_marker(elided: usize) -> String {
    format!("[…{elided}ch elided…]")
}

/// The first `units` UTF-16 units of `value`, moved back so a surrogate pair is never split.
fn utf16_prefix(value: &str, units: usize) -> String {
    let mut kept = String::new();
    let mut used = 0;
    for character in value.chars() {
        let width = character.len_utf16();
        if used + width > units {
            break;
        }
        kept.push(character);
        used += width;
    }
    kept
}

/// The longest prefix of value whose serialized length with its elision marker fits limit.
fn trimmed_string(value: &str, max_chars: usize, limit: usize) -> Option<String> {
    let length = js_length(value);
    let mut kept = js_length(&utf16_prefix(value, max_chars.min(length)));
    loop {
        let prefix = utf16_prefix(value, kept);
        let candidate = format!("{prefix}{}", elision_marker(length - js_length(&prefix)));
        if serialized_string_length(&candidate) <= limit {
            return Some(candidate);
        }
        if kept == 0 {
            return None;
        }
        kept = js_length(&utf16_prefix(value, kept / 2));
    }
}

fn bound_string(value: &str, limit: usize) -> String {
    if js_length(value) <= JSON_TREE_SCALAR_LEN_EXPANDED && serialized_string_length(value) <= limit
    {
        return value.to_owned();
    }
    trimmed_string(value, JSON_TREE_SCALAR_LEN_EXPANDED, limit).unwrap_or_else(|| STUB.into())
}

/// Keys are display data too: long or escape-heavy keys keep a prefix and stay unique.
fn fitted_key(key: &str, max_chars: usize) -> String {
    if js_length(key) <= max_chars && serialized_string_length(key) <= max_chars + 2 {
        return key.to_owned();
    }
    trimmed_string(key, max_chars, max_chars + 2).unwrap_or_else(|| STUB.into())
}

fn bound_key(key: &str, taken: &mut HashSet<String>) -> String {
    let mut unique = fitted_key(key, MAX_KEY_CHARS);
    let mut suffix = 2usize;
    while taken.contains(&unique) {
        let digits = suffix.to_string().len();
        unique = format!("{}#{suffix}", fitted_key(key, MAX_KEY_CHARS - digits - 1));
        suffix += 1;
    }
    taken.insert(unique.clone());
    unique
}

/// Serialized length of what bound_node would keep given unlimited room, capped at cap + 1.
fn bounded_need(value: &Value, depth: usize, cap: i64) -> i64 {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => serialized_length(value) as i64,
        Value::String(text) => {
            let length = js_length(text) as i64;
            if length > cap || length > JSON_TREE_SCALAR_LEN_EXPANDED as i64 {
                return cap + 1;
            }
            (cap + 1).min(serialized_string_length(text) as i64)
        }
        _ => {
            if let Some(bytes) = serialized_buffer_byte_length(value) {
                return json_string_length(&buffer_placeholder(bytes)) as i64;
            }
            if depth >= JSON_TREE_MAX_DEPTH_EXPANDED {
                return STUB_COST as i64;
            }
            let mut total = 2i64;
            match value {
                Value::Array(items) => {
                    for item in items {
                        total += 1 + bounded_need(item, depth + 1, cap - total);
                        if total > cap {
                            return cap + 1;
                        }
                    }
                }
                Value::Object(entries) => {
                    for (key, item) in entries {
                        total += 2
                            + serialized_string_length(key) as i64
                            + bounded_need(item, depth + 1, cap - total);
                        if total > cap {
                            return cap + 1;
                        }
                    }
                }
                _ => unreachable!("scalars handled above"),
            }
            total
        }
    }
}

fn bound_array(items: &[Value], depth: usize, limit: i64) -> Value {
    let mut kept = vec![];
    let mut used = 2i64;
    let full_marker_cost =
        json_string_length(&format!("… ({} more items)", items.len())) as i64 + 1;
    for (index, item) in items.iter().enumerate() {
        let marker = format!("… ({} more items)", items.len() - index);
        let comma = i64::from(index > 0);
        let marker_cost = json_string_length(&marker) as i64 + comma;
        let is_last = index == items.len() - 1;
        let room = limit - used - comma - if is_last { 0 } else { full_marker_cost };
        if index >= JSON_TREE_MAX_LINES_EXPANDED || room < STUB_COST as i64 + 8 {
            if used + marker_cost > limit {
                return if kept.is_empty() {
                    Value::String(STUB.into())
                } else {
                    Value::Array(kept)
                };
            }
            kept.push(Value::String(marker));
            return Value::Array(kept);
        }
        let child = bound_node(item, depth + 1, room);
        used += comma + serialized_length(&child) as i64;
        kept.push(child);
    }
    Value::Array(kept)
}

fn bound_record(entries: &Map<String, Value>, depth: usize, limit: i64) -> Value {
    let mut taken = HashSet::new();
    let keys: Vec<&String> = entries.keys().collect();
    let mut shown: Vec<(&String, String)> = keys
        .iter()
        .take(JSON_TREE_MAX_LINES_EXPANDED)
        .map(|key| (*key, bound_key(key, &mut taken)))
        .collect();
    let overflow_key = bound_key(STUB, &mut taken);
    let stub_cost = |stored: &str| serialized_string_length(stored) as i64 + 2 + STUB_COST as i64;
    let overflow_cost = |count: usize| {
        if count == 0 {
            0
        } else {
            serialized_string_length(&overflow_key) as i64
                + 2
                + json_string_length(&format!("{count} more keys")) as i64
        }
    };
    let mut base = 2
        + shown
            .iter()
            .map(|(_, stored)| stub_cost(stored))
            .sum::<i64>()
        + overflow_cost(keys.len() - shown.len());
    while base > limit && !shown.is_empty() {
        if let Some((_, stored)) = shown.pop() {
            base -= stub_cost(&stored);
        }
        base +=
            overflow_cost(keys.len() - shown.len()) - overflow_cost(keys.len() - shown.len() - 1);
    }
    if base > limit {
        return Value::String(STUB.into());
    }
    // Max-min fair share of the remaining room: small values are kept whole, large ones share the rest.
    let mut pool = limit - base;
    let needs: Vec<i64> = shown
        .iter()
        .map(|(key, _)| {
            (bounded_need(&entries[key.as_str()], depth + 1, pool + STUB_COST as i64)
                - STUB_COST as i64)
                .max(0)
        })
        .collect();
    let mut order: Vec<usize> = (0..shown.len()).collect();
    order.sort_by_key(|index| needs[*index]);
    let mut results = HashMap::new();
    for (position, index) in order.iter().enumerate() {
        let (key, _) = &shown[*index];
        let share = pool.div_euclid((order.len() - position) as i64);
        let grant = needs[*index].min(share);
        let child = bound_node(&entries[key.as_str()], depth + 1, STUB_COST as i64 + grant);
        pool -= (serialized_length(&child) as i64 - STUB_COST as i64).max(0);
        results.insert(*index, child);
    }
    let mut kept = Map::new();
    for (index, (_, stored)) in shown.iter().enumerate() {
        kept.insert(
            stored.clone(),
            results.remove(&index).unwrap_or(Value::Null),
        );
    }
    if keys.len() > shown.len() {
        kept.insert(
            overflow_key.clone(),
            Value::String(format!("{} more keys", keys.len() - shown.len())),
        );
    }
    Value::Object(kept)
}

/// Serialized length of value when bounding would keep it unchanged, or `None` otherwise; stops
/// counting past cap so large values are never walked in full.
fn unchanged_length(value: &Value, depth: usize, cap: i64) -> Option<i64> {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => Some(serialized_length(value) as i64),
        Value::String(text) => {
            let length = js_length(text);
            (length <= JSON_TREE_SCALAR_LEN_EXPANDED && length as i64 <= cap)
                .then(|| serialized_string_length(text) as i64)
        }
        _ if depth >= JSON_TREE_MAX_DEPTH_EXPANDED
            || serialized_buffer_byte_length(value).is_some() =>
        {
            None
        }
        Value::Array(items) => {
            if items.len() > JSON_TREE_MAX_LINES_EXPANDED {
                return None;
            }
            let mut total = 2i64;
            for item in items {
                total += 1 + unchanged_length(item, depth + 1, cap - total)?;
                if total > cap {
                    return None;
                }
            }
            Some(total)
        }
        Value::Object(entries) => {
            let mut total = 2i64;
            for (count, (key, item)) in entries.iter().enumerate() {
                if count + 1 > JSON_TREE_MAX_LINES_EXPANDED || js_length(key) > MAX_KEY_CHARS {
                    return None;
                }
                let key_length = serialized_string_length(key);
                if key_length > MAX_KEY_CHARS + 2 {
                    return None;
                }
                total += 2 + key_length as i64 + unchanged_length(item, depth + 1, cap - total)?;
                if total > cap {
                    return None;
                }
            }
            Some(total)
        }
    }
}

/// The value bounded so its serialized length is at most max(limit, 3).
fn bound_node(value: &Value, depth: usize, limit: i64) -> Value {
    if unchanged_length(value, depth, limit).is_some_and(|length| length <= limit) {
        return value.clone();
    }
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => {
            if serialized_length(value) as i64 <= limit {
                value.clone()
            } else {
                Value::String(STUB.into())
            }
        }
        Value::String(text) => Value::String(bound_string(text, limit.max(0) as usize)),
        _ => {
            if let Some(bytes) = serialized_buffer_byte_length(value) {
                let placeholder = buffer_placeholder(bytes);
                return Value::String(if json_string_length(&placeholder) as i64 <= limit {
                    placeholder
                } else {
                    STUB.into()
                });
            }
            if depth >= JSON_TREE_MAX_DEPTH_EXPANDED || limit < STUB_COST as i64 + 2 {
                return Value::String(STUB.into());
            }
            match value {
                Value::Array(items) => bound_array(items, depth, limit),
                Value::Object(entries) => bound_record(entries, depth, limit),
                _ => unreachable!("scalars handled above"),
            }
        }
    }
}

/// Every top-level key with a stub value, or a bare stub when even that cannot fit.
fn skeleton(value: &Value) -> Value {
    let Some(entries) = value.as_object() else {
        return Value::String(STUB.into());
    };
    let stubs: Map<String, Value> = entries
        .keys()
        .map(|key| (key.clone(), Value::String(STUB.into())))
        .collect();
    let stubbed = bound_record(&stubs, 0, MAX_DISPLAY_JSON_BYTES as i64);
    if serialized_length(&stubbed) <= MAX_DISPLAY_JSON_BYTES {
        stubbed
    } else {
        Value::String(STUB.into())
    }
}

/// Bounds one display value for persistence in `details.jsonOutputs`.
pub fn sanitize_display_json(value: &Value) -> Value {
    let sanitized = bound_node(value, 0, MAX_DISPLAY_JSON_BYTES as i64);
    if serialized_length(&sanitized) <= MAX_DISPLAY_JSON_BYTES {
        sanitized
    } else {
        skeleton(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Inputs and outputs dumped from the pinned `sanitizeDisplayJson` (senpi 903b8336).
    #[test]
    fn pinned_display_json_bounds_cases() {
        let cases: Map<String, Value> =
            serde_json::from_str(include_str!("display-json-bounds.golden.json"))
                .expect("golden fixture");
        let mut checked = 0;
        for (name, case) in &cases {
            if name.starts_with('@') {
                continue;
            }
            let actual = sanitize_display_json(&case["input"]);
            assert_eq!(actual, case["output"], "{name}");
            assert!(
                serialized_length(&actual) <= MAX_DISPLAY_JSON_BYTES,
                "{name}"
            );
            checked += 1;
        }
        assert_eq!(checked, 15);
    }
}
