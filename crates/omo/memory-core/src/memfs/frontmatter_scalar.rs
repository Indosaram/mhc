//! The single-line scalar subset of YAML that memory frontmatter uses (pin `frontmatter-scalar.ts`).

/// Splits `content` into its frontmatter header and body (pin `FRONTMATTER_RE`).
pub fn split_frontmatter(content: &str) -> Option<(String, String)> {
    let rest = content
        .strip_prefix("---\r\n")
        .or_else(|| content.strip_prefix("---\n"))?;
    let mut offset = 0usize;
    for line in rest.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed == "---" {
            let header = rest[..offset]
                .strip_suffix("\r\n")
                .or_else(|| rest[..offset].strip_suffix('\n'))
                .unwrap_or(&rest[..offset]);
            let after = offset + line.len();
            return Some((header.to_string(), rest[after..].to_string()));
        }
        offset += line.len();
    }
    None
}

/// True when the raw scalar source opens with a quote character.
pub fn is_quoted_scalar_source(raw: &str) -> bool {
    raw.len() >= 2 && (raw.starts_with('"') || raw.starts_with('\''))
}

/// Decode a quoted scalar written in the JSON / single-quote subset.
pub fn decode_quoted_scalar(raw: &str) -> Option<String> {
    if is_double_quoted_json(raw) {
        return serde_json::from_str::<serde_json::Value>(raw)
            .ok()
            .and_then(|value| value.as_str().map(str::to_string));
    }
    if is_single_quoted(raw) {
        let inner = &raw[1..raw.len() - 1];
        return Some(inner.replace("''", "'"));
    }
    None
}

/// Decode a JSON array of strings (the `aliases` shape).
pub fn decode_string_array(raw: &str) -> Option<Vec<String>> {
    if !raw.starts_with('[') {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let items = value.as_array()?;
    items
        .iter()
        .map(|item| item.as_str().map(str::to_string))
        .collect()
}

/// True when a plain scalar round-trips through strict YAML verbatim.
pub fn is_plain_safe_scalar(value: &str) -> bool {
    if value.is_empty() || value != value.trim() {
        return false;
    }
    if value.chars().any(is_control_or_tab) || has_unsafe_lead(value) {
        return false;
    }
    if value.contains(": ") || value.ends_with(':') || value.contains(" #") {
        return false;
    }
    !is_core_schema_special(value) && !is_core_schema_number(value)
}

/// Render a string as a plain scalar when safe, else as a JSON double-quoted scalar.
pub fn render_string_scalar(value: &str) -> String {
    if is_plain_safe_scalar(value) {
        value.to_string()
    } else {
        json_string(value)
    }
}

/// The value the memory reader sees for `key: raw`: a quoted scalar decoded, a plain one verbatim.
pub fn decode_scalar_source(raw: &str) -> String {
    decode_quoted_scalar(raw).unwrap_or_else(|| raw.to_string())
}

/// Raw scalar source that strict YAML reads exactly as the memory reader does.
pub fn is_canonical_scalar_source(raw: &str) -> bool {
    if is_quoted_scalar_source(raw) {
        return decode_quoted_scalar(raw).is_some();
    }
    if raw.starts_with('[') {
        return decode_string_array(raw).is_some();
    }
    is_plain_safe_scalar(raw) || is_core_schema_special(raw) || is_core_schema_number(raw)
}

/// Render a raw scalar source verbatim when canonical, else as a JSON double-quoted scalar.
pub fn render_raw_scalar(raw: &str) -> String {
    if is_canonical_scalar_source(raw) {
        raw.to_string()
    } else {
        json_string(raw)
    }
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| format!("\"{value}\""))
}

fn is_control_or_tab(ch: char) -> bool {
    ch <= '\u{1f}' || ch == '\u{7f}'
}

fn has_unsafe_lead(value: &str) -> bool {
    matches!(
        value.chars().next(),
        Some('-' | '?' | ':' | ',' | '[' | ']' | '{' | '}' | '#' | '&' | '*' | '!' | '|' | '>'
            | '\'' | '"' | '%' | '@' | '`')
    )
}

fn is_core_schema_special(value: &str) -> bool {
    matches!(
        value,
        "true" | "True" | "TRUE" | "false" | "False" | "FALSE" | "null" | "Null" | "NULL" | "~"
    )
}

fn is_core_schema_number(value: &str) -> bool {
    let unsigned = value
        .strip_prefix(['-', '+'])
        .unwrap_or(value);
    if let Some(octal) = unsigned.strip_prefix("0o") {
        return !octal.is_empty() && octal.bytes().all(|byte| (b'0'..=b'7').contains(&byte));
    }
    if let Some(hex) = unsigned.strip_prefix("0x") {
        return !hex.is_empty() && hex.bytes().all(|byte| byte.is_ascii_hexdigit());
    }
    if matches!(unsigned, ".inf" | ".Inf" | ".INF" | ".nan" | ".NaN" | ".NAN") {
        return true;
    }
    is_decimal_number(unsigned)
}

fn is_decimal_number(value: &str) -> bool {
    let mantissa = match value.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => {
            if exponent.is_empty() {
                return false;
            }
            let digits = exponent.strip_prefix(['-', '+']).unwrap_or(exponent);
            if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
                return false;
            }
            mantissa
        }
        None => value,
    };
    if let Some(fraction) = mantissa.strip_prefix('.') {
        return !fraction.is_empty() && fraction.bytes().all(|byte| byte.is_ascii_digit());
    }
    match mantissa.split_once('.') {
        Some((whole, fraction)) => {
            !whole.is_empty()
                && whole.bytes().all(|byte| byte.is_ascii_digit())
                && fraction.bytes().all(|byte| byte.is_ascii_digit())
        }
        None => !mantissa.is_empty() && mantissa.bytes().all(|byte| byte.is_ascii_digit()),
    }
}

fn is_double_quoted_json(raw: &str) -> bool {
    if !(raw.len() >= 2 && raw.starts_with('"') && raw.ends_with('"')) {
        return false;
    }
    let inner = &raw[1..raw.len() - 1];
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        if ch == '"' {
            return false;
        }
        if ch == '\\' {
            match chars.next() {
                Some('"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't') => {}
                Some('u') => {
                    for _ in 0..4 {
                        match chars.next() {
                            Some(digit) if digit.is_ascii_hexdigit() => {}
                            _ => return false,
                        }
                    }
                }
                _ => return false,
            }
        } else if is_control_or_tab(ch) {
            return false;
        }
    }
    true
}

fn is_single_quoted(raw: &str) -> bool {
    if !(raw.len() >= 2 && raw.starts_with('\'') && raw.ends_with('\'')) {
        return false;
    }
    let inner = &raw[1..raw.len() - 1];
    let mut chars = inner.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\'' && chars.peek() != Some(&'\'') {
            return false;
        }
        if ch == '\'' {
            chars.next();
        }
    }
    true
}

#[cfg(test)]
#[path = "frontmatter_scalar_tests.rs"]
mod tests;
