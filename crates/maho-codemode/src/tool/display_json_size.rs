//! Port of `tool/display-json-size.ts` (senpi 903b8336): display JSON size accounting shared by
//! storage bounding and legacy rendering (#3261). Lengths are JS string lengths (UTF-16 units).

use serde_json::Value;

/// Model-facing display text budget; the stored jsonOutputs entry follows the same order of budget.
pub const MAX_DISPLAY_TEXT_BYTES: usize = 8_000;
/// Total serialized-size budget for one stored entry, measured in UTF-16 units like JS string length.
pub const MAX_DISPLAY_JSON_BYTES: usize = MAX_DISPLAY_TEXT_BYTES;

/// Byte length of a `{type:"Buffer",data:[...]}` serialization, or `None` when the shape is not one.
/// Only a bounded prefix of the data array is validated, so a multi-MB legacy dump never gets walked.
pub fn serialized_buffer_byte_length(value: &Value) -> Option<usize> {
    let record = value.as_object()?;
    if record.get("type").and_then(Value::as_str) != Some("Buffer") {
        return None;
    }
    let data = record.get("data")?.as_array()?;
    let valid = data.iter().take(1_024).all(|byte| {
        byte.as_f64()
            .is_some_and(|byte| byte.fract() == 0.0 && (0.0..=255.0).contains(&byte))
    });
    valid.then_some(data.len())
}

/// JS `Number(x.toFixed(1))` printed with `String()`: one decimal, trailing `.0` dropped.
fn one_decimal(value: f64) -> String {
    let rounded = format!("{value:.1}");
    rounded.strip_suffix(".0").unwrap_or(&rounded).to_owned()
}

pub fn format_byte_length(bytes: usize) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    if bytes < 1024 * 1024 {
        return format!("{} KB", one_decimal(bytes as f64 / 1024.0));
    }
    format!("{} MB", one_decimal(bytes as f64 / (1024.0 * 1024.0)))
}

pub fn buffer_placeholder(bytes: usize) -> String {
    format!("<Buffer {}>", format_byte_length(bytes))
}

/// Cost of one char in [`serialized_string_length`]: an escaped char costs
/// `JSON.stringify(character).length`, which includes its two quotes (pinned behaviour); any other
/// char costs its UTF-16 length.
fn escaped_char_length(character: char) -> usize {
    match character {
        '"' | '\\' | '\u{8}' | '\u{c}' | '\n' | '\r' | '\t' => 4,
        c if (c as u32) < 0x20 => 8,
        c => c.len_utf16(),
    }
}

/// Length of `value`'s JSON string body after escaping, bounded at 6x plus quotes, as upstream
/// counts it (escaped chars include their quotes).
pub fn serialized_string_length(value: &str) -> usize {
    2 + value.chars().map(escaped_char_length).sum::<usize>()
}

/// `JSON.stringify(value).length`: the real escaped length plus quotes.
pub fn json_string_length(value: &str) -> usize {
    2 + value
        .chars()
        .map(|character| match character {
            '"' | '\\' | '\u{8}' | '\u{c}' | '\n' | '\r' | '\t' => 2,
            c if (c as u32) < 0x20 => 6,
            c => c.len_utf16(),
        })
        .sum::<usize>()
}

/// JS `string.length`.
pub fn js_length(value: &str) -> usize {
    value.encode_utf16().count()
}

/// Iterative early-exit size estimate for stored entries; never enumerates more of the value than
/// the budget plus a key.
pub fn display_json_size_exceeds(value: &Value, budget: usize) -> bool {
    let mut size = 0usize;
    let mut pending = vec![value];
    while let Some(node) = pending.pop() {
        match node {
            Value::Null => {}
            Value::Bool(_) | Value::Number(_) => size += 8,
            Value::String(text) => size += js_length(text),
            Value::Array(items) => {
                size += 2;
                for child in items {
                    size += 1;
                    if size > budget {
                        return true;
                    }
                    pending.push(child);
                }
            }
            Value::Object(entries) => {
                size += 2;
                for (key, child) in entries {
                    size += js_length(key) + 1;
                    if size > budget {
                        return true;
                    }
                    pending.push(child);
                }
            }
        }
        if size > budget {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sizes_match_the_pinned_helpers() {
        assert_eq!(
            [0, 1023, 1024, 1536, 1_048_576, 5 * 1_048_576 + 1].map(format_byte_length),
            ["0 B", "1023 B", "1 KB", "1.5 KB", "1 MB", "5 MB"]
        );
        assert_eq!(
            ["a\"b", "\u{1}", "😀", "é"].map(serialized_string_length),
            [8, 10, 4, 3]
        );
        assert_eq!(
            serialized_buffer_byte_length(&json!({"type":"Buffer","data":[1,2,255]})),
            Some(3)
        );
        assert_eq!(
            serialized_buffer_byte_length(&json!({"type":"Buffer","data":[256]})),
            None
        );
        assert!(!display_json_size_exceeds(&json!({"a":1,"b":"x"}), 8000));
        assert!(display_json_size_exceeds(
            &json!({"a":"x".repeat(8001)}),
            8000
        ));
    }
}
