//! Port of senpi packages/coding-agent/src/core/share/share-redaction-value.ts.

use serde_json::{Map, Value};

use super::share_entropy::redact_across_boundary;
use super::share_redaction::{
    ShareRedactionCategory, ShareRedactionContext, ShareRedactionTally, count, redact_share_text,
};
use super::share_sensitive_key::is_sensitive_key_name;

const REDACTED: &str = "<redacted>";
const SCHEMA_TYPES: [&str; 7] = [
    "string", "number", "integer", "boolean", "object", "array", "null",
];
const SCHEMA_KEYWORDS: [&str; 13] = [
    "type",
    "description",
    "title",
    "format",
    "enum",
    "items",
    "properties",
    "required",
    "minLength",
    "maxLength",
    "pattern",
    "additionalProperties",
    "nullable",
];

/// Any provider's image block: `{type:"image"}`, Anthropic `{type:"image", source}`, OpenAI `image_url`/`input_image`.
fn is_image_block(record: &Map<String, Value>) -> bool {
    if record
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|t| t.to_ascii_lowercase().contains("image"))
    {
        return true;
    }
    record
        .get("source")
        .and_then(|source| source.get("type"))
        .and_then(Value::as_str)
        == Some("base64")
}

/// A JSON-schema node (a tool parameter named `apiKey`) describes a credential; it does not hold one.
fn is_schema_node(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    let Some(kind) = object.get("type").and_then(Value::as_str) else {
        return false;
    };
    SCHEMA_TYPES.contains(&kind)
        && object
            .keys()
            .all(|key| SCHEMA_KEYWORDS.contains(&key.as_str()))
}

/// Every string under a sensitive key is a credential, whatever the nesting.
fn drop_all_strings(value: &Value, tally: &mut ShareRedactionTally) -> Value {
    match value {
        Value::String(text) => {
            if text.is_empty() {
                return value.clone();
            }
            if text != REDACTED {
                count(tally, ShareRedactionCategory::Credential);
            }
            Value::String(REDACTED.to_owned())
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| drop_all_strings(item, tally))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, child)| (key.clone(), drop_all_strings(child, tally)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Redacts every string reachable from `value` (keys stay, they are schema); every value under a
/// sensitive-named key is dropped whole, and image payloads are replaced by a text note.
pub fn redact_share_value(
    value: &Value,
    context: &ShareRedactionContext,
    tally: &mut ShareRedactionTally,
) -> Value {
    match value {
        Value::String(text) => Value::String(redact_share_text(text, context, tally)),
        Value::Array(items) => {
            let redacted = items
                .iter()
                .map(|item| redact_share_value(item, context, tally))
                .collect();
            Value::Array(join_adjacent_text(redacted, tally))
        }
        Value::Object(record) => {
            if is_image_block(record) {
                count(tally, ShareRedactionCategory::Image);
                let mut note = Map::new();
                note.insert("type".to_owned(), Value::String("text".to_owned()));
                note.insert(
                    "text".to_owned(),
                    Value::String("[image omitted from share]".to_owned()),
                );
                return Value::Object(note);
            }
            let mut out = Map::new();
            for (key, child) in record {
                let next = if is_sensitive_key_name(key) && !is_schema_node(child) {
                    drop_all_strings(child, tally)
                } else {
                    redact_share_value(child, context, tally)
                };
                out.insert(key.clone(), next);
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

fn text_of(item: &Value) -> Option<String> {
    match item {
        Value::String(text) => Some(text.clone()),
        Value::Object(map) if map.get("type").and_then(Value::as_str) == Some("text") => {
            map.get("text").and_then(Value::as_str).map(str::to_owned)
        }
        _ => None,
    }
}

fn with_text(item: &Value, text: String) -> Value {
    match item {
        Value::Object(map) => {
            let mut next = map.clone();
            next.insert("text".to_owned(), Value::String(text));
            Value::Object(next)
        }
        _ => Value::String(text),
    }
}

/// A token split across two adjacent text blocks or strings is one value; both halves go.
fn join_adjacent_text(mut items: Vec<Value>, tally: &mut ShareRedactionTally) -> Vec<Value> {
    let mut index = 0;
    while index + 1 < items.len() {
        if let (Some(left), Some(right)) = (text_of(&items[index]), text_of(&items[index + 1])) {
            let mut hits = 0usize;
            let (next_left, next_right) = redact_across_boundary(&left, &right, &mut || hits += 1);
            for _ in 0..hits {
                count(tally, ShareRedactionCategory::Credential);
            }
            if next_left != left {
                items[index] = with_text(&items[index], next_left);
                items[index + 1] = with_text(&items[index + 1], next_right);
            }
        }
        index += 1;
    }
    items
}
