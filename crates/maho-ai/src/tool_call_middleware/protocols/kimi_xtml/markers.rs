//! Port of senpi packages/ai/src/tool-call-middleware/protocols/kimi-xtml/markers.ts.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;

pub const XTML_TOOLS_OPEN: &str = "<|open|>tools<|sep|>";
pub const XTML_TOOLS_CLOSE: &str = "<|close|>tools<|sep|>";
pub const XTML_CALL_OPEN: &str = "<|open|>call ";
pub const XTML_CALL_CLOSE: &str = "<|close|>call<|sep|>";
pub const XTML_ARGUMENT_OPEN: &str = "<|open|>argument ";
pub const XTML_ARGUMENT_CLOSE: &str = "<|close|>argument<|sep|>";
pub const XTML_SEP: &str = "<|sep|>";

pub const XTML_OPEN_PREFIX: &str = "<|open|>";
pub const XTML_CLOSE_PREFIX: &str = "<|close|>";

/// senpi builds this from `CHANNEL_NAME`/`STRUCTURAL_CHANNEL_NAME`/`SEPLESS_CHANNEL_NAME`
/// fragments; the equivalent combined pattern (anchored at the start, matching `find` semantics
/// rather than JS's stateful global `exec`) is:
/// `^(?:<\|(?:open|close)\|>(?:[a-zA-Z_][a-zA-Z0-9_]*)?<\|sep\|>|<\|(?:open|close)\|>(?:(?!(?:call|argument)\b)[a-zA-Z_][a-zA-Z0-9_]*)?(?=$|[^a-zA-Z0-9_])|<\|sep\|>)`.
static ANCHORED_CHANNEL_MARKER_PATTERN: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    fancy_regex::Regex::new(
        r"^(?:<\|(?:open|close)\|>(?:[a-zA-Z_][a-zA-Z0-9_]*)?<\|sep\|>|<\|(?:open|close)\|>(?:(?!(?:call|argument)\b)[a-zA-Z_][a-zA-Z0-9_]*)?(?=$|[^a-zA-Z0-9_])|<\|sep\|>)",
    )
    .expect("ANCHORED_CHANNEL_MARKER_PATTERN is a valid fixed regex")
});

pub fn match_xtml_channel_marker(text: &str) -> Option<String> {
    ANCHORED_CHANNEL_MARKER_PATTERN.find(text).ok().flatten().map(|m| m.as_str().to_string())
}

/// `/([\w-]+)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s<]+))/g`.
static ATTRIBUTE_PATTERN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"([\w-]+)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s<]+))"#).expect("ATTRIBUTE_PATTERN is a valid fixed regex"));

pub fn parse_xtml_attributes(header: &str) -> HashMap<String, String> {
    let mut attributes = HashMap::new();
    for captures in ATTRIBUTE_PATTERN.captures_iter(header) {
        let Some(key) = captures.get(1) else { continue };
        let value = captures.get(2).or_else(|| captures.get(3)).or_else(|| captures.get(4)).map(|m| m.as_str()).unwrap_or("");
        attributes.insert(key.as_str().to_string(), value.to_string());
    }
    attributes
}

pub fn get_partial_xtml_suffix(text: &str, tokens: &[&str]) -> String {
    for token in tokens {
        let chars: Vec<char> = token.chars().collect();
        for length in (1..chars.len()).rev() {
            let prefix: String = chars[..length].iter().collect();
            if text.ends_with(&prefix) {
                return prefix;
            }
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_xtml_channel_marker_matches_open_and_close_tokens_with_and_without_sep() {
        assert_eq!(match_xtml_channel_marker("<|open|>call <rest>"), None);
        assert_eq!(match_xtml_channel_marker("<|sep|>rest"), Some("<|sep|>".to_string()));
        assert_eq!(match_xtml_channel_marker("<|open|>argument<|sep|>value"), Some("<|open|>argument<|sep|>".to_string()));
        assert_eq!(match_xtml_channel_marker("not a marker"), None);
    }

    #[test]
    fn match_xtml_channel_marker_excludes_structural_channel_names_from_the_sepless_branch() {
        assert_eq!(match_xtml_channel_marker("<|open|>call"), None);
        assert_eq!(match_xtml_channel_marker("<|open|>argument"), None);
        assert_eq!(match_xtml_channel_marker("<|open|>thinking rest"), Some("<|open|>thinking".to_string()));
    }

    #[test]
    fn parse_xtml_attributes_reads_double_single_and_bare_values() {
        let attributes = parse_xtml_attributes(r#"tool="get_weather" index='1' bare=value"#);
        assert_eq!(attributes.get("tool"), Some(&"get_weather".to_string()));
        assert_eq!(attributes.get("index"), Some(&"1".to_string()));
        assert_eq!(attributes.get("bare"), Some(&"value".to_string()));
    }

    #[test]
    fn get_partial_xtml_suffix_returns_the_longest_matching_trailing_prefix() {
        assert_eq!(get_partial_xtml_suffix("hello <|op", &["<|open|>"]), "<|op");
        assert_eq!(get_partial_xtml_suffix("hello", &["<|open|>"]), "");
    }
}
