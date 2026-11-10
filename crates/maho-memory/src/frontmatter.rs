//! Memory frontmatter: splitting it from the body, decoding single-line scalars, and validating it.
//!
//! Behaviour spec: `docs/cleanroom/specs/memory-frontmatter.md`. Memory files are Markdown with a
//! `---` header, and that header is read by a strict single-line YAML subset rather than a full YAML
//! parser. The subset exists so a description can never smuggle in a second line, a nested mapping, or
//! a block scalar that the prompt index would render as something other than what the writer meant.
use std::fmt::Write as _;

/// The cap the skill loader uses too; memory descriptions feed the prompt index.
pub const MAX_DESCRIPTION_LENGTH: usize = 1024;

/// Split `content` into its frontmatter header and body.
///
/// The header excludes the closing `---` line and the newline before it. `None` means the content has
/// no well-formed header at all, which is different from an empty header.
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
            return Some((header.to_owned(), rest[after..].to_owned()));
        }
        offset += line.len();
    }
    None
}

pub fn is_quoted_scalar_source(raw: &str) -> bool {
    raw.len() >= 2 && (raw.starts_with('"') || raw.starts_with('\''))
}

/// Decode a quoted scalar written as either JSON double quotes or YAML single quotes.
pub fn decode_quoted_scalar(raw: &str) -> Option<String> {
    if is_double_quoted_json(raw) {
        return serde_json::from_str::<serde_json::Value>(raw)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned));
    }
    if is_single_quoted(raw) {
        let inner = &raw[1..raw.len() - 1];
        return Some(inner.replace("''", "'"));
    }
    None
}

/// Decode a JSON array of strings, the shape `aliases` uses.
pub fn decode_string_array(raw: &str) -> Option<Vec<String>> {
    if !raw.starts_with('[') {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    value
        .as_array()?
        .iter()
        .map(|item| item.as_str().map(str::to_owned))
        .collect()
}

/// Whether `value` can be written as a plain scalar and still read back verbatim.
///
/// The rejected forms are exactly the ones YAML reinterprets: leading indicators (`-`, `?`, `#`, ...),
/// `: ` and ` #` inside the text, surrounding whitespace, and the core-schema words and numbers
/// (`true`, `null`, `0x10`, `1e3`) that a YAML reader would turn into a boolean, a null or a number.
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

pub fn render_string_scalar(value: &str) -> String {
    if is_plain_safe_scalar(value) {
        value.to_owned()
    } else {
        json_string(value)
    }
}

pub fn decode_scalar_source(raw: &str) -> String {
    decode_quoted_scalar(raw).unwrap_or_else(|| raw.to_owned())
}

/// Whether a raw scalar source reads identically through strict YAML and through this decoder.
pub fn is_canonical_scalar_source(raw: &str) -> bool {
    if is_quoted_scalar_source(raw) {
        return decode_quoted_scalar(raw).is_some();
    }
    if raw.starts_with('[') {
        return decode_string_array(raw).is_some();
    }
    is_plain_safe_scalar(raw) || is_core_schema_special(raw) || is_core_schema_number(raw)
}

pub fn render_raw_scalar(raw: &str) -> String {
    if is_canonical_scalar_source(raw) {
        raw.to_owned()
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
        Some(
            '-' | '?'
                | ':'
                | ','
                | '['
                | ']'
                | '{'
                | '}'
                | '#'
                | '&'
                | '*'
                | '!'
                | '|'
                | '>'
                | '\''
                | '"'
                | '%'
                | '@'
                | '`'
        )
    )
}

fn is_core_schema_special(value: &str) -> bool {
    matches!(
        value,
        "true" | "True" | "TRUE" | "false" | "False" | "FALSE" | "null" | "Null" | "NULL" | "~"
    )
}

fn is_core_schema_number(value: &str) -> bool {
    let unsigned = value.strip_prefix(['-', '+']).unwrap_or(value);
    if let Some(octal) = unsigned.strip_prefix("0o") {
        return !octal.is_empty() && octal.bytes().all(|byte| (b'0'..=b'7').contains(&byte));
    }
    if let Some(hex) = unsigned.strip_prefix("0x") {
        return !hex.is_empty() && hex.bytes().all(|byte| byte.is_ascii_hexdigit());
    }
    if matches!(
        unsigned,
        ".inf" | ".Inf" | ".INF" | ".nan" | ".NaN" | ".NAN"
    ) {
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

/// `^([A-Za-z0-9_-]+):(?:[ \t]+(.*?))?[ \t]*$` as `(key, raw)`.
pub fn parse_header_line(line: &str) -> Option<(String, String)> {
    let index = line.find(':')?;
    let key = &line[..index];
    if key.is_empty()
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return None;
    }
    let rest = &line[index + 1..];
    if rest.is_empty() {
        return Some((key.to_owned(), String::new()));
    }
    if rest.starts_with(' ') || rest.starts_with('\t') {
        let raw = rest
            .trim_start_matches([' ', '\t'])
            .trim_end_matches([' ', '\t']);
        return Some((key.to_owned(), raw.to_owned()));
    }
    rest.bytes()
        .all(|byte| byte == b' ' || byte == b'\t')
        .then(|| (key.to_owned(), String::new()))
}

/// `<\/?(?:description|parameter)\b[^>]*>?`, case-insensitive; returns the matched text.
///
/// This catches a malformed tool call whose arguments split across the description and the body: the
/// leftover tag is not a memory, and letting it through writes `</parameter>` into a user's notes.
pub fn find_tool_call_scaffolding(text: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    for start in 0..chars.len() {
        if chars[start] != '<' {
            continue;
        }
        let mut index = start + 1;
        if chars.get(index) == Some(&'/') {
            index += 1;
        }
        let rest: String = chars[index..].iter().collect();
        let lowered = rest.to_ascii_lowercase();
        let keyword_len = if lowered.starts_with("description") {
            "description".len()
        } else if lowered.starts_with("parameter") {
            "parameter".len()
        } else {
            continue;
        };
        let after = index + keyword_len;
        if chars
            .get(after)
            .is_some_and(|ch| ch.is_alphanumeric() || *ch == '_')
        {
            continue;
        }
        let mut end = after;
        while chars.get(end).is_some_and(|ch| *ch != '>') {
            end += 1;
        }
        if chars.get(end) == Some(&'>') {
            end += 1;
        }
        return Some(chars[start..end].iter().collect());
    }
    None
}

pub fn describe_frontmatter_violation(content: &str) -> Option<String> {
    describe_frontmatter_with(content, describe_header_violation)
}

/// Grammar only: what the renderer and the legacy normalizer guarantee.
pub fn describe_frontmatter_grammar_violation(content: &str) -> Option<String> {
    describe_frontmatter_with(content, describe_header_grammar_violation)
}

pub fn describe_description_violation(description: &str) -> Option<String> {
    if description.trim().is_empty() {
        return Some("'description' must not be empty".to_owned());
    }
    if let Some(scaffolding) = find_tool_call_scaffolding(description) {
        return Some(format!(
            "'description' contains tool-call scaffolding (\"{scaffolding}\"); the arguments of this call were malformed and split incorrectly - resend it with a one-line description and the body in file_text"
        ));
    }
    if description.contains('\r') || description.contains('\n') {
        return Some("'description' must be a single line".to_owned());
    }
    if description.chars().count() > MAX_DESCRIPTION_LENGTH {
        return Some(format!(
            "'description' exceeds {MAX_DESCRIPTION_LENGTH} characters ({})",
            description.chars().count()
        ));
    }
    None
}

pub fn describe_header_violation(header_text: &str) -> Option<String> {
    if let Some(grammar) = describe_header_grammar_violation(header_text) {
        return Some(grammar);
    }
    describe_description_violation(&header_description(header_text))
}

/// The strict grammar for one header block: one `key: value` per line, no comments, no continuations,
/// no duplicate keys, and a non-empty `description`.
pub fn describe_header_grammar_violation(header_text: &str) -> Option<String> {
    let mut seen: Vec<String> = Vec::new();
    let mut description: Option<String> = None;
    for line in header_text.split('\n') {
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            return Some(format!(
                "indented continuation lines are not allowed (single-line scalars only): {}",
                line.trim()
            ));
        }
        if line.starts_with('#') {
            return Some(format!(
                "comment lines are not allowed in frontmatter: {line}"
            ));
        }
        let Some((key, raw)) = parse_header_line(line) else {
            return Some(format!("not a 'key: value' line: {line}"));
        };
        if seen.contains(&key) {
            return Some(format!("duplicate frontmatter key '{key}'"));
        }
        seen.push(key.clone());
        if let Some(violation) = describe_value_violation(&key, &raw) {
            return Some(violation);
        }
        if key == "description" {
            description = Some(decode_quoted_scalar(&raw).unwrap_or(raw));
        }
    }
    let Some(description) = description else {
        return Some("missing required field 'description'".to_owned());
    };
    if description.trim().is_empty() {
        return Some("'description' must not be empty".to_owned());
    }
    if description.contains('\r') || description.contains('\n') {
        return Some("'description' must be a single line".to_owned());
    }
    None
}

fn describe_frontmatter_with(
    content: &str,
    check: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    match split_frontmatter(&normalized) {
        Some((header, _)) => check(&header),
        None => Some("missing frontmatter (must start with --- and close with ---)".to_owned()),
    }
}

fn header_description(header_text: &str) -> String {
    for line in header_text.split('\n') {
        if let Some((key, raw)) = parse_header_line(line)
            && key == "description"
        {
            return decode_quoted_scalar(&raw).unwrap_or(raw);
        }
    }
    String::new()
}

fn describe_value_violation(key: &str, raw: &str) -> Option<String> {
    if raw.is_empty() {
        return Some(if key == "description" {
            "'description' must not be empty".to_owned()
        } else {
            format!("'{key}' has no value")
        });
    }
    if is_quoted_scalar_source(raw) {
        return decode_quoted_scalar(raw).is_none().then(|| {
            format!("'{key}' is not a valid quoted scalar (use JSON double quotes): {raw}")
        });
    }
    if raw.starts_with('[') {
        return decode_string_array(raw)
            .is_none()
            .then(|| format!("'{key}' must be a JSON array of strings: {raw}"));
    }
    if raw.starts_with('>') || raw.starts_with('|') {
        return Some(format!("'{key}' must be a non-empty single line"));
    }
    // `description` and `kind` are written by the model, so they are held to the plain-scalar rule
    // that forbids reinterpreting words. Other keys may legitimately carry `true` or a number.
    let safe = if key == "description" || key == "kind" {
        is_plain_safe_scalar(raw)
    } else {
        is_canonical_scalar_source(raw)
    };
    (!safe).then(|| {
        format!(
            "'{key}' is not a safe YAML plain scalar (quote it, or remove ': ' and ' #'): {raw}"
        )
    })
}

pub fn render_document(header: &[(&str, &str)], body: &str) -> String {
    let mut out = String::from("---\n");
    for (key, raw) in header {
        let _ = writeln!(out, "{key}: {}", render_raw_scalar(raw));
    }
    out.push_str("---\n");
    out.push_str(body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "---\ndescription: a memory\nkind: note\n---\nbody text\n";

    #[test]
    fn a_document_splits_into_header_and_body() {
        let (header, body) = split_frontmatter(DOC).expect("header");
        assert_eq!(header, "description: a memory\nkind: note");
        assert_eq!(body, "body text\n");
    }

    #[test]
    fn a_crlf_document_splits_the_same_way() {
        let (header, body) =
            split_frontmatter("---\r\ndescription: a\r\n---\r\nbody\r\n").expect("header");
        assert_eq!(header, "description: a");
        assert_eq!(body, "body\r\n");
    }

    #[test]
    fn an_empty_header_is_present_but_empty() {
        let (header, body) = split_frontmatter("---\n---\nbody").expect("header");
        assert_eq!(header, "");
        assert_eq!(body, "body");
    }

    #[test]
    fn a_document_without_a_closing_marker_has_no_header() {
        assert_eq!(split_frontmatter("---\ndescription: a\nbody"), None);
    }

    #[test]
    fn a_document_not_starting_with_a_marker_has_no_header() {
        assert_eq!(split_frontmatter("body\n---\n"), None);
    }

    #[test]
    fn a_body_containing_a_marker_line_is_not_split_twice() {
        let (_, body) = split_frontmatter("---\ndescription: a\n---\n---\nmore").expect("header");
        assert_eq!(body, "---\nmore");
    }

    #[test]
    fn a_header_line_parses_key_and_value() {
        assert_eq!(
            parse_header_line("description: hello world"),
            Some(("description".to_owned(), "hello world".to_owned()))
        );
    }

    #[test]
    fn a_header_line_without_a_value_parses_to_an_empty_value() {
        assert_eq!(
            parse_header_line("description:"),
            Some(("description".to_owned(), String::new()))
        );
    }

    #[test]
    fn a_header_line_trims_surrounding_spaces_from_the_value() {
        assert_eq!(
            parse_header_line("kind:   note   "),
            Some(("kind".to_owned(), "note".to_owned()))
        );
    }

    #[test]
    fn a_key_with_illegal_characters_is_not_a_header_line() {
        assert_eq!(parse_header_line("de scription: value"), None);
        assert_eq!(parse_header_line(": value"), None);
        assert_eq!(parse_header_line("no colon here"), None);
    }

    #[test]
    fn a_key_may_contain_underscores_and_dashes() {
        assert_eq!(
            parse_header_line("created_at-x: 1"),
            Some(("created_at-x".to_owned(), "1".to_owned()))
        );
    }

    #[test]
    fn a_json_double_quoted_scalar_decodes_its_escapes() {
        assert_eq!(
            decode_quoted_scalar(r#""two\nlines""#),
            Some("two\nlines".to_owned())
        );
        assert_eq!(decode_quoted_scalar(r#""a\"b""#), Some("a\"b".to_owned()));
    }

    #[test]
    fn a_single_quoted_scalar_unescapes_a_doubled_quote() {
        assert_eq!(
            decode_quoted_scalar("'it''s here'"),
            Some("it's here".to_owned())
        );
    }

    #[test]
    fn an_unterminated_quote_does_not_decode() {
        assert_eq!(decode_quoted_scalar("\"open"), None);
        assert_eq!(decode_quoted_scalar("'open"), None);
    }

    #[test]
    fn a_bad_json_escape_does_not_decode() {
        assert_eq!(decode_quoted_scalar(r#""bad\q""#), None);
    }

    #[test]
    fn a_raw_control_character_inside_a_double_quoted_scalar_does_not_decode() {
        assert_eq!(decode_quoted_scalar("\"a\u{1}b\""), None);
    }

    #[test]
    fn a_string_array_decodes() {
        assert_eq!(
            decode_string_array(r#"["a", "b"]"#),
            Some(vec!["a".to_owned(), "b".to_owned()])
        );
    }

    #[test]
    fn a_non_string_array_does_not_decode() {
        assert_eq!(decode_string_array("[1, 2]"), None);
        assert_eq!(decode_string_array("not an array"), None);
    }

    #[test]
    fn a_plain_word_is_safe() {
        assert!(is_plain_safe_scalar("a memory about locks"));
        assert!(is_plain_safe_scalar("한국어 설명"));
    }

    #[test]
    fn a_core_schema_word_is_not_plain_safe() {
        for word in ["true", "false", "null", "~", "Null", "TRUE"] {
            assert!(!is_plain_safe_scalar(word), "{word} must not be plain");
        }
    }

    #[test]
    fn a_number_like_value_is_not_plain_safe() {
        for number in ["1", "1.5", "-2", "0x10", "0o7", "1e3", ".5", ".inf", ".nan"] {
            assert!(!is_plain_safe_scalar(number), "{number} must not be plain");
        }
    }

    #[test]
    fn a_value_with_an_unsafe_leading_character_is_not_plain_safe() {
        for value in [
            "- dash", "? q", "# hash", "[x]", "&anchor", "*alias", "!tag",
        ] {
            assert!(!is_plain_safe_scalar(value), "{value} must not be plain");
        }
    }

    #[test]
    fn a_value_with_inner_mapping_or_comment_syntax_is_not_plain_safe() {
        assert!(!is_plain_safe_scalar("key: value"));
        assert!(!is_plain_safe_scalar("ends with colon:"));
        assert!(!is_plain_safe_scalar("has # comment"));
    }

    #[test]
    fn a_value_with_surrounding_whitespace_is_not_plain_safe() {
        assert!(!is_plain_safe_scalar(" padded"));
        assert!(!is_plain_safe_scalar("padded "));
        assert!(!is_plain_safe_scalar(""));
    }

    #[test]
    fn a_value_with_a_control_character_is_not_plain_safe() {
        assert!(!is_plain_safe_scalar("a\tb"));
        assert!(!is_plain_safe_scalar("a\u{1}b"));
    }

    #[test]
    fn rendering_a_safe_value_leaves_it_plain() {
        assert_eq!(render_string_scalar("plain words"), "plain words");
    }

    #[test]
    fn rendering_an_unsafe_value_quotes_it() {
        assert_eq!(render_string_scalar("true"), "\"true\"");
        assert_eq!(render_string_scalar("a: b"), "\"a: b\"");
    }

    #[test]
    fn a_rendered_unsafe_value_decodes_back_to_itself() {
        for value in ["true", "a: b", "# hash", "1e3", "a\nb", "한국어"] {
            let rendered = render_string_scalar(value);
            assert_eq!(decode_scalar_source(&rendered), value);
        }
    }

    #[test]
    fn a_canonical_source_is_recognised() {
        assert!(is_canonical_scalar_source("plain words"));
        assert!(is_canonical_scalar_source("\"quoted\""));
        assert!(is_canonical_scalar_source("[\"a\"]"));
        assert!(is_canonical_scalar_source("true"));
        assert!(is_canonical_scalar_source("42"));
    }

    #[test]
    fn a_non_canonical_source_is_rejected() {
        assert!(!is_canonical_scalar_source("\"unterminated"));
        assert!(!is_canonical_scalar_source("[1]"));
        assert!(!is_canonical_scalar_source("a: b"));
    }

    #[test]
    fn rendering_a_raw_scalar_keeps_a_canonical_one_verbatim() {
        assert_eq!(render_raw_scalar("true"), "true");
        assert_eq!(render_raw_scalar("plain"), "plain");
    }

    #[test]
    fn rendering_a_raw_scalar_quotes_a_non_canonical_one() {
        assert_eq!(render_raw_scalar("a: b"), "\"a: b\"");
    }

    #[test]
    fn a_well_formed_document_has_no_violation() {
        assert_eq!(describe_frontmatter_violation(DOC), None);
    }

    #[test]
    fn a_document_without_frontmatter_reports_it() {
        let violation = describe_frontmatter_violation("just a body").expect("violation");
        assert!(violation.contains("missing frontmatter"));
    }

    #[test]
    fn a_header_without_a_description_reports_it() {
        let violation =
            describe_frontmatter_violation("---\nkind: note\n---\nbody").expect("violation");
        assert!(violation.contains("missing required field 'description'"));
    }

    #[test]
    fn an_empty_description_reports_it() {
        let violation =
            describe_frontmatter_violation("---\ndescription:\n---\nbody").expect("violation");
        assert!(violation.contains("must not be empty"));
    }

    #[test]
    fn a_whitespace_only_description_reports_it() {
        let violation = describe_frontmatter_violation("---\ndescription: \"   \"\n---\nbody")
            .expect("violation");
        assert!(violation.contains("must not be empty"));
    }

    #[test]
    fn an_indented_continuation_line_is_rejected() {
        let violation = describe_frontmatter_violation("---\ndescription: a\n  continued\n---\n")
            .expect("violation");
        assert!(violation.contains("continuation lines are not allowed"));
    }

    #[test]
    fn a_comment_line_is_rejected() {
        let violation = describe_frontmatter_violation("---\ndescription: a\n# comment\n---\n")
            .expect("violation");
        assert!(violation.contains("comment lines are not allowed"));
    }

    #[test]
    fn a_duplicate_key_is_rejected() {
        let violation =
            describe_frontmatter_violation("---\ndescription: a\ndescription: b\n---\n")
                .expect("violation");
        assert!(violation.contains("duplicate frontmatter key 'description'"));
    }

    #[test]
    fn a_line_that_is_not_a_key_value_pair_is_rejected() {
        let violation = describe_frontmatter_violation("---\ndescription: a\njust text\n---\n")
            .expect("violation");
        assert!(violation.contains("not a 'key: value' line"));
    }

    #[test]
    fn a_block_scalar_is_rejected() {
        for marker in ["> folded", "| literal"] {
            let doc = format!("---\ndescription: {marker}\n---\n");
            let violation = describe_frontmatter_violation(&doc).expect("violation");
            assert!(violation.contains("must be a non-empty single line"));
        }
    }

    #[test]
    fn an_unquoted_value_that_yaml_would_reinterpret_is_rejected_for_a_description() {
        let violation =
            describe_frontmatter_violation("---\ndescription: a: b\n---\n").expect("violation");
        assert!(violation.contains("not a safe YAML plain scalar"));
    }

    #[test]
    fn a_boolean_value_is_allowed_for_an_other_key() {
        assert_eq!(
            describe_frontmatter_violation("---\ndescription: a\npinned: true\n---\n"),
            None
        );
    }

    #[test]
    fn a_boolean_description_is_rejected() {
        let violation =
            describe_frontmatter_violation("---\ndescription: true\n---\n").expect("violation");
        assert!(violation.contains("not a safe YAML plain scalar"));
    }

    #[test]
    fn a_key_with_no_value_is_rejected_for_an_other_key() {
        let violation =
            describe_frontmatter_violation("---\ndescription: a\nkind:\n---\n").expect("violation");
        assert!(violation.contains("'kind' has no value"));
    }

    #[test]
    fn a_description_over_the_cap_is_rejected() {
        let long = "a".repeat(MAX_DESCRIPTION_LENGTH + 1);
        let violation = describe_description_violation(&long).expect("violation");
        assert!(violation.contains("exceeds"));
    }

    #[test]
    fn a_description_exactly_at_the_cap_is_accepted() {
        let exact = "a".repeat(MAX_DESCRIPTION_LENGTH);
        assert_eq!(describe_description_violation(&exact), None);
    }

    #[test]
    fn the_cap_counts_characters_not_bytes() {
        let korean = "가".repeat(MAX_DESCRIPTION_LENGTH);
        assert_eq!(describe_description_violation(&korean), None);
    }

    #[test]
    fn a_multi_line_description_is_rejected() {
        assert!(describe_description_violation("one\ntwo").is_some());
        assert!(describe_description_violation("one\rtwo").is_some());
    }

    #[test]
    fn tool_call_scaffolding_in_a_description_is_reported() {
        let violation = describe_description_violation("a memory</parameter>").expect("violation");
        assert!(violation.contains("tool-call scaffolding"));
    }

    #[test]
    fn scaffolding_is_found_in_every_case_and_position() {
        for text in [
            "<parameter name=\"description\">x",
            "</parameter>",
            "<DESCRIPTION>",
            "<Parameter/>",
            "prefix </description> suffix",
        ] {
            assert!(
                find_tool_call_scaffolding(text).is_some(),
                "expected scaffolding in {text:?}"
            );
        }
    }

    #[test]
    fn a_word_merely_starting_with_the_keyword_is_not_scaffolding() {
        for text in [
            "<descriptions>",
            "<parameters_list>",
            "a description",
            "a parameter",
        ] {
            assert_eq!(find_tool_call_scaffolding(text), None, "for {text:?}");
        }
    }

    #[test]
    fn an_unclosed_scaffolding_tag_is_still_found() {
        assert_eq!(
            find_tool_call_scaffolding("x <parameter name"),
            Some("<parameter name".to_owned())
        );
    }

    #[test]
    fn the_grammar_check_ignores_the_description_contract() {
        let doc = "---\ndescription: a memory</parameter>\n---\n";
        assert_eq!(describe_frontmatter_grammar_violation(doc), None);
        assert!(describe_frontmatter_violation(doc).is_some());
    }

    #[test]
    fn a_crlf_document_is_normalized_before_validation() {
        assert_eq!(
            describe_frontmatter_violation("---\r\ndescription: a\r\n---\r\nbody\r\n"),
            None
        );
    }

    #[test]
    fn a_korean_description_validates() {
        assert_eq!(
            describe_frontmatter_violation("---\ndescription: 한국어 메모리\n---\n본문\n"),
            None
        );
    }

    #[test]
    fn a_rendered_document_parses_back_to_the_same_values() {
        let doc = render_document(
            &[("description", "a: tricky value"), ("kind", "note")],
            "body\n",
        );
        let (header, body) = split_frontmatter(&doc).expect("header");
        assert_eq!(body, "body\n");
        assert_eq!(describe_frontmatter_violation(&doc), None);
        assert!(header.contains("\"a: tricky value\""));
    }
}
