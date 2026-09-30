//! Port of senpi packages/ai/src/tool-call-middleware/protocols/antml/repair.ts.

use serde_json::{Map, Value};

fn is_escaped_backslash_run(text: &[char], backslash_index: usize) -> bool {
    let mut preceding_backslashes = 0usize;
    let mut index = backslash_index as isize - 1;
    while index >= 0 && text[index as usize] == '\\' {
        preceding_backslashes += 1;
        index -= 1;
    }
    preceding_backslashes % 2 == 1
}

pub fn repair_unicode_escapes(json: &str) -> String {
    let chars: Vec<char> = json.chars().collect();
    let mut result = String::with_capacity(json.len());
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] == '\\' && chars.get(i + 1) == Some(&'u') {
            let has_four_hex = chars.get(i + 2..i + 6).is_some_and(|hex| hex.iter().all(|c| c.is_ascii_hexdigit()));
            if !has_four_hex {
                if is_escaped_backslash_run(&chars, i) {
                    result.push('\\');
                    result.push('u');
                } else {
                    result.push_str("\\\\u");
                }
                i += 2;
                continue;
            }
        }
        result.push(chars[i]);
        i += 1;
    }
    result
}

fn is_high_surrogate(code: u32) -> bool {
    (0xD800..=0xDBFF).contains(&code)
}

fn is_low_surrogate(code: u32) -> bool {
    (0xDC00..=0xDFFF).contains(&code)
}

pub fn repair_lone_surrogates(value: &str) -> String {
    let units: Vec<u16> = value.encode_utf16().collect();
    let mut result: Vec<u16> = Vec::with_capacity(units.len());
    let mut i = 0usize;
    while i < units.len() {
        let unit = units[i] as u32;
        if is_high_surrogate(unit) {
            let next_is_low = units.get(i + 1).is_some_and(|&next| is_low_surrogate(next as u32));
            if next_is_low {
                result.push(units[i]);
                result.push(units[i + 1]);
                i += 2;
                continue;
            }
            result.push(0xFFFD);
            i += 1;
            continue;
        }
        if is_low_surrogate(unit) {
            let prev_is_high = i > 0 && is_high_surrogate(units[i - 1] as u32);
            if !prev_is_high {
                result.push(0xFFFD);
                i += 1;
                continue;
            }
        }
        result.push(units[i]);
        i += 1;
    }
    String::from_utf16_lossy(&result)
}

pub fn repair_strings_deep(value: Value) -> Value {
    match value {
        Value::String(text) => Value::String(repair_lone_surrogates(&text)),
        Value::Array(items) => Value::Array(items.into_iter().map(repair_strings_deep).collect()),
        Value::Object(map) => {
            let mut repaired = Map::new();
            for (key, entry) in map {
                repaired.insert(repair_lone_surrogates(&key), repair_strings_deep(entry));
            }
            Value::Object(repaired)
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    #[test]
    fn repairs_broken_unicode_escapes_but_keeps_valid_ones() {
        assert_eq!(repair_unicode_escapes(r"\u12"), r"\\u12");
        assert_eq!(repair_unicode_escapes(r"\u1234"), r"\u1234");
        assert_eq!(repair_unicode_escapes(r"\\u12"), r"\\u12");
    }

    #[test]
    fn replaces_unpaired_surrogates_with_replacement_character() {
        let lone_high = String::from_utf16_lossy(&[0xD800]);
        assert_eq!(repair_lone_surrogates(&lone_high), "\u{FFFD}");
        let paired = String::from_utf16(&[0xD83D, 0xDE00]).unwrap();
        assert_eq!(repair_lone_surrogates(&paired), paired);
    }

    #[test]
    fn repairs_strings_deep_in_nested_values_including_keys() {
        let lone = String::from_utf16_lossy(&[0xD800]);
        let value = json!({"a": [lone.clone(), {"b": lone.clone()}]});
        let repaired = repair_strings_deep(value);
        assert_eq!(repaired, json!({"a": ["\u{FFFD}", {"b": "\u{FFFD}"}]}));
    }
    #[test]
    fn escapes_a_broken_unicode_sequence_so_json_parse_succeeds() {
        let broken_json = r#"{"text":"bad\uZZZZescape"}"#;
        assert!(serde_json::from_str::<Value>(broken_json).is_err());
        let repaired = repair_unicode_escapes(broken_json);
        assert_eq!(serde_json::from_str::<Value>(&repaired).expect("repaired"), json!({"text": r"bad\uZZZZescape"}));
    }

    #[test]
    fn escapes_a_truncated_unicode_sequence_with_fewer_than_four_hex_digits() {
        let broken_json = r#"{"text":"cut\u12"}"#;
        let repaired = repair_unicode_escapes(broken_json);
        assert_eq!(serde_json::from_str::<Value>(&repaired).expect("repaired"), json!({"text": r"cut\u12"}));
    }

    #[test]
    fn leaves_valid_unicode_escapes_untouched() {
        let valid_json = r#"{"text":"ok\u0041"}"#;
        let repaired = repair_unicode_escapes(valid_json);
        assert_eq!(repaired, valid_json);
        assert_eq!(serde_json::from_str::<Value>(&repaired).expect("repaired"), json!({"text": "okA"}));
    }

    #[test]
    fn leaves_an_escaped_backslash_before_u_untouched() {
        let valid_json = r#"{"path":"C:\\users"}"#;
        let repaired = repair_unicode_escapes(valid_json);
        assert_eq!(repaired, valid_json);
        assert_eq!(serde_json::from_str::<Value>(&repaired).expect("repaired"), json!({"path": r"C:\users"}));
    }

    #[test]
    fn replaces_lone_high_and_low_surrogates_with_the_replacement_character() {
        let lone_high = format!("bad{}end", String::from_utf16_lossy(&[0xD800]));
        let lone_low = format!("bad{}end", String::from_utf16_lossy(&[0xDC00]));
        assert_eq!(repair_lone_surrogates(&lone_high), "bad\u{FFFD}end");
        assert_eq!(repair_lone_surrogates(&lone_low), "bad\u{FFFD}end");
    }

    #[test]
    fn keeps_valid_surrogate_pairs_intact() {
        let emoji = "ok\u{1F600}done";
        assert_eq!(repair_lone_surrogates(emoji), emoji);
    }

    #[test]
    fn replaces_a_reversed_surrogate_pair_entirely() {
        let reversed = format!("x{}{}y", String::from_utf16_lossy(&[0xDC00]), String::from_utf16_lossy(&[0xD800]));
        assert_eq!(repair_lone_surrogates(&reversed), "x\u{FFFD}\u{FFFD}y");
    }

}
