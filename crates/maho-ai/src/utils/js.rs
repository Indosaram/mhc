//! JavaScript string semantics the ported TS relies on.

/// `String.prototype.trim`: strips ECMAScript WhiteSpace and LineTerminator code points.
pub fn trim(text: &str) -> &str {
    text.trim_matches(is_js_whitespace)
}

pub fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{0009}' | '\u{000B}' | '\u{000C}' | '\u{0020}' | '\u{00A0}' | '\u{FEFF}' | '\u{000A}' | '\u{000D}'
            | '\u{2028}' | '\u{2029}' | '\u{1680}' | '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}'
    )
}

/// UTF-16 length, i.e. JS `string.length`.
pub fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// A JS number as JSON: integral values serialize without a fraction, as `JSON.stringify` does.
pub fn json_number(value: f64) -> serde_json::Value {
    if value.fract() == 0.0 && value.abs() < 9_007_199_254_740_992.0 {
        serde_json::Value::from(value as i64)
    } else {
        serde_json::Number::from_f64(value).map_or(serde_json::Value::Null, serde_json::Value::Number)
    }
}

/// `Number.prototype.toString()` (radix 10) for a finite or non-finite double.
pub fn number_to_string(value: f64) -> String {
    if value.is_nan() {
        return "NaN".into();
    }
    if value == 0.0 {
        return "0".into();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    let sign = if value < 0.0 { "-" } else { "" };
    // Rust's `{:e}` prints the shortest round-trip digits, the same digits JS picks.
    let exp_form = format!("{:e}", value.abs());
    let (mantissa, exponent) = exp_form.split_once('e').unwrap_or((exp_form.as_str(), "0"));
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let n = exponent.parse::<i32>().unwrap_or(0) + 1;
    let body = if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let e = n - 1;
        let e = if e >= 0 { format!("+{e}") } else { e.to_string() };
        if k == 1 { format!("{digits}e{e}") } else { format!("{}.{}e{e}", &digits[..1], &digits[1..]) }
    };
    format!("{sign}{body}")
}

/// `Number(string)`: StringToNumber, NaN when the text is not a numeric literal.
pub fn string_to_number(text: &str) -> f64 {
    let text = trim(text);
    if text.is_empty() {
        return 0.0;
    }
    match text {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }
    for (prefixes, radix) in [(["0x", "0X"], 16), (["0o", "0O"], 8), (["0b", "0B"], 2)] {
        if let Some(body) = prefixes.iter().find_map(|p| text.strip_prefix(p)) {
            if body.is_empty() || !body.chars().all(|c| c.is_digit(radix)) {
                return f64::NAN;
            }
            return body.chars().fold(0.0, |acc, c| acc * f64::from(radix) + f64::from(c.to_digit(radix).unwrap_or(0)));
        }
    }
    if is_decimal_literal(text) { text.parse::<f64>().unwrap_or(f64::NAN) } else { f64::NAN }
}

fn is_decimal_literal(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut i = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    let digits = |i: &mut usize| {
        let start = *i;
        while bytes.get(*i).is_some_and(u8::is_ascii_digit) {
            *i += 1;
        }
        *i - start
    };
    let mut mantissa = digits(&mut i);
    if bytes.get(i) == Some(&b'.') {
        i += 1;
        mantissa += digits(&mut i);
    }
    if mantissa == 0 {
        return false;
    }
    if matches!(bytes.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(bytes.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        if digits(&mut i) == 0 {
            return false;
        }
    }
    i == bytes.len()
}

/// `Object.keys` order: array-index keys ascending, then string keys in insertion order.
pub fn object_keys(map: &serde_json::Map<String, serde_json::Value>) -> Vec<&String> {
    let (mut indices, mut rest) = (Vec::new(), Vec::new());
    for key in map.keys() {
        match array_index(key) {
            Some(index) => indices.push((index, key)),
            None => rest.push(key),
        }
    }
    indices.sort_by_key(|(index, _)| *index);
    indices.into_iter().map(|(_, key)| key).chain(rest).collect()
}

fn array_index(key: &str) -> Option<u32> {
    if key == "0" {
        return Some(0);
    }
    if key.is_empty() || key.starts_with('0') || !key.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    key.parse::<u32>().ok().filter(|index| *index != u32::MAX)
}

/// `JSON.stringify(value, null, 2)`.
pub fn json_stringify_pretty(value: &serde_json::Value) -> String {
    let mut out = String::new();
    write_json(&mut out, value, 0);
    out
}

fn write_json(out: &mut String, value: &serde_json::Value, depth: usize) {
    use serde_json::Value;
    let indent = |out: &mut String, depth: usize| out.push_str(&"  ".repeat(depth));
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => match n.as_f64().filter(|f| f.is_finite()) {
            Some(f) => out.push_str(&number_to_string(f)),
            None => out.push_str("null"),
        },
        Value::String(s) => out.push_str(&serde_json::to_string(s).unwrap_or_default()),
        Value::Array(items) if items.is_empty() => out.push_str("[]"),
        Value::Array(items) => {
            out.push_str("[\n");
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(",\n");
                }
                indent(out, depth + 1);
                write_json(out, item, depth + 1);
            }
            out.push('\n');
            indent(out, depth);
            out.push(']');
        }
        Value::Object(map) if map.is_empty() => out.push_str("{}"),
        Value::Object(map) => {
            out.push_str("{\n");
            for (i, key) in object_keys(map).into_iter().enumerate() {
                if i > 0 {
                    out.push_str(",\n");
                }
                indent(out, depth + 1);
                out.push_str(&serde_json::to_string(key).unwrap_or_default());
                out.push_str(": ");
                if let Some(item) = map.get(key) {
                    write_json(out, item, depth + 1);
                }
            }
            out.push('\n');
            indent(out, depth);
            out.push('}');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_js_whitespace_only() {
        assert_eq!(trim("\u{FEFF}\u{3000} a \u{2029}"), "a");
        assert_eq!(trim("\u{200B}a"), "\u{200B}a");
        assert_eq!(utf16_len("🙈a"), 3);
    }

    #[test]
    fn formats_numbers_like_js() {
        let cases = [(1e21, "1e+21"), (1e-7, "1e-7"), (123e-20, "1.23e-18"), (0.1, "0.1"), (-0.0, "0"), (1.5e300, "1.5e+300"), (9007199254740992.0, "9007199254740992"), (123456789012345680000.0, "123456789012345680000"), (0.000001, "0.000001"), (-2.5, "-2.5"), (42.0, "42")];
        for (value, expected) in cases {
            assert_eq!(number_to_string(value), expected, "{value}");
        }
    }

    #[test]
    fn parses_numbers_like_js() {
        assert_eq!(string_to_number(" 42 "), 42.0);
        assert_eq!(string_to_number("0x1A"), 26.0);
        assert_eq!(string_to_number("-1.5e2"), -150.0);
        assert_eq!(string_to_number(".5"), 0.5);
        assert!(string_to_number("inf").is_nan());
        assert!(string_to_number("1_0").is_nan());
        assert!(string_to_number("-0x1").is_nan());
        assert_eq!(string_to_number("-Infinity"), f64::NEG_INFINITY);
    }

    #[test]
    fn stringifies_like_js() {
        let value = serde_json::json!({"b": 1, "2": 1, "a": [], "1": {}, "c": 1.0, "d": [true, null, "x"]});
        assert_eq!(json_stringify_pretty(&value), "{\n  \"1\": {},\n  \"2\": 1,\n  \"b\": 1,\n  \"a\": [],\n  \"c\": 1,\n  \"d\": [\n    true,\n    null,\n    \"x\"\n  ]\n}");
    }
}
