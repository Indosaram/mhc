//! Strict single-line frontmatter validation (pin `frontmatter-validation.ts`).

use super::frontmatter_scalar::{
    decode_quoted_scalar, decode_string_array, is_canonical_scalar_source, is_plain_safe_scalar,
    is_quoted_scalar_source, split_frontmatter,
};

/// Mirrors the skill loader's description cap; memory descriptions also feed the prompt index.
pub const MAX_DESCRIPTION_LENGTH: usize = 1024;

/// Validate a memory file against the strict grammar plus the description contract.
pub fn describe_frontmatter_violation(content: &str) -> Option<String> {
    describe_frontmatter_with(content, describe_header_violation)
}

/// Grammar only: what the renderer and the legacy normalizer guarantee.
pub fn describe_frontmatter_grammar_violation(content: &str) -> Option<String> {
    describe_frontmatter_with(content, describe_header_grammar_violation)
}

/// The description contract alone.
pub fn describe_description_violation(description: &str) -> Option<String> {
    if description.trim().is_empty() {
        return Some("'description' must not be empty".to_string());
    }
    if let Some(scaffolding) = find_tool_call_scaffolding(description) {
        return Some(format!(
            "'description' contains tool-call scaffolding (\"{scaffolding}\"); the arguments of this call were malformed and split incorrectly - resend it with a one-line description and the body in file_text"
        ));
    }
    if description.contains('\r') || description.contains('\n') {
        return Some("'description' must be a single line".to_string());
    }
    if description.chars().count() > MAX_DESCRIPTION_LENGTH {
        return Some(format!(
            "'description' exceeds {MAX_DESCRIPTION_LENGTH} characters ({})",
            description.chars().count()
        ));
    }
    None
}

/// Grammar plus the description contract for one header block.
pub fn describe_header_violation(header_text: &str) -> Option<String> {
    if let Some(grammar) = describe_header_grammar_violation(header_text) {
        return Some(grammar);
    }
    describe_description_violation(&header_description(header_text))
}

/// The strict grammar for one header block.
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
            return Some(format!("comment lines are not allowed in frontmatter: {line}"));
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
        return Some("missing required field 'description'".to_string());
    };
    if description.trim().is_empty() {
        return Some("'description' must not be empty".to_string());
    }
    if description.contains('\r') || description.contains('\n') {
        return Some("'description' must be a single line".to_string());
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
        None => Some("missing frontmatter (must start with --- and close with ---)".to_string()),
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
            "'description' must not be empty".to_string()
        } else {
            format!("'{key}' has no value")
        });
    }
    if is_quoted_scalar_source(raw) {
        return decode_quoted_scalar(raw)
            .is_none()
            .then(|| format!("'{key}' is not a valid quoted scalar (use JSON double quotes): {raw}"));
    }
    if raw.starts_with('[') {
        return decode_string_array(raw)
            .is_none()
            .then(|| format!("'{key}' must be a JSON array of strings: {raw}"));
    }
    if raw.starts_with('>') || raw.starts_with('|') {
        return Some(format!("'{key}' must be a non-empty single line"));
    }
    let safe = if key == "description" || key == "kind" {
        is_plain_safe_scalar(raw)
    } else {
        is_canonical_scalar_source(raw)
    };
    (!safe).then(|| {
        format!("'{key}' is not a safe YAML plain scalar (quote it, or remove ': ' and ' #'): {raw}")
    })
}

/// `^([A-Za-z0-9_-]+):(?:[ \t]+(.*?))?[ \t]*$` as `(key, raw)`.
fn parse_header_line(line: &str) -> Option<(String, String)> {
    let idx = line.find(':')?;
    let key = &line[..idx];
    if key.is_empty()
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return None;
    }
    let rest = &line[idx + 1..];
    if rest.is_empty() {
        return Some((key.to_string(), String::new()));
    }
    if rest.starts_with(' ') || rest.starts_with('\t') {
        let raw = rest.trim_start_matches([' ', '\t']).trim_end_matches([' ', '\t']);
        return Some((key.to_string(), raw.to_string()));
    }
    rest.bytes()
        .all(|byte| byte == b' ' || byte == b'\t')
        .then(|| (key.to_string(), String::new()))
}

/// `<\/?(?:description|parameter)\b[^>]*>?` case-insensitively; returns the matched text.
pub fn find_tool_call_scaffolding(text: &str) -> Option<String> {
    let bytes: Vec<char> = text.chars().collect();
    for start in 0..bytes.len() {
        if bytes[start] != '<' {
            continue;
        }
        let mut index = start + 1;
        if bytes.get(index) == Some(&'/') {
            index += 1;
        }
        let rest: String = bytes[index..].iter().collect();
        let lowered = rest.to_ascii_lowercase();
        let keyword_len = if lowered.starts_with("description") {
            "description".len()
        } else if lowered.starts_with("parameter") {
            "parameter".len()
        } else {
            continue;
        };
        let after = index + keyword_len;
        if bytes
            .get(after)
            .is_some_and(|ch| ch.is_alphanumeric() || *ch == '_')
        {
            continue;
        }
        let mut end = after;
        while bytes.get(end).is_some_and(|ch| *ch != '>') {
            end += 1;
        }
        if bytes.get(end) == Some(&'>') {
            end += 1;
        }
        return Some(bytes[start..end].iter().collect());
    }
    None
}

#[cfg(test)]
#[path = "frontmatter_validation_tests.rs"]
mod tests;
