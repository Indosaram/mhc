//! JSON with comments and trailing commas, as OpenCode's `opencode.jsonc` writes it (upstream
//! `omo-native/bin/lib/jsonc.js`).
//!
//! Strict JSON is tried first, so a plain `opencode.json` never pays for the scan. The scan itself
//! is string-aware: a `//` inside a quoted value is data, not a comment, and so is a `,}` or `,]`.

use serde_json::Value;

fn strip(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<char> = Vec::with_capacity(chars.len());
    // Index in `out` of a comma that nothing but whitespace or comments has followed yet; a closing
    // bracket reached while it is set makes it a trailing comma, dropped without touching string data.
    let mut pending_comma: Option<usize> = None;
    let mut index = 0;
    while index < chars.len() {
        let character = chars[index];
        if character == '"' {
            let start = index;
            index += 1;
            while index < chars.len() && chars[index] != '"' {
                index += if chars[index] == '\\' { 2 } else { 1 };
            }
            let end = (index + 1).min(chars.len());
            out.extend_from_slice(&chars[start..end]);
            index += 1;
            pending_comma = None;
            continue;
        }
        if character == '/' && chars.get(index + 1) == Some(&'/') {
            while index < chars.len() && chars[index] != '\n' {
                index += 1;
            }
            continue;
        }
        if character == '/' && chars.get(index + 1) == Some(&'*') {
            let mut cursor = index + 2;
            loop {
                if cursor + 1 >= chars.len() {
                    index = chars.len();
                    break;
                }
                if chars[cursor] == '*' && chars[cursor + 1] == '/' {
                    index = cursor + 2;
                    break;
                }
                cursor += 1;
            }
            continue;
        }
        if (character == '}' || character == ']')
            && let Some(comma) = pending_comma
        {
            out.remove(comma);
        }
        if character == ',' {
            pending_comma = Some(out.len());
        } else if !character.is_whitespace() {
            pending_comma = None;
        }
        out.push(character);
        index += 1;
    }
    out.into_iter().collect()
}

/// Parses JSON, falling back to JSON with comments and trailing commas.
///
/// # Errors
/// Neither form parses.
pub fn parse_jsonc(text: &str) -> Result<Value, String> {
    // Editors on Windows save a UTF-8 byte order mark that JSON.parse rejects as a token.
    let source = text.strip_prefix('\u{feff}').unwrap_or(text);
    serde_json::from_str(source)
        .or_else(|_| serde_json::from_str(&strip(source)))
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn comments_and_trailing_commas_are_dropped_but_string_data_is_kept() {
        let text = "\u{feff}{\n  // note\n  \"url\": \"https://x.test/a//b,}\", /* c */\n  \"list\": [1, 2,],\n}\n";
        assert_eq!(
            parse_jsonc(text).expect("parses"),
            json!({"url": "https://x.test/a//b,}", "list": [1, 2]})
        );
        assert!(parse_jsonc("{ nope").is_err());
    }
}
