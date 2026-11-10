//! Normalizing legacy memory frontmatter into the strict single-line YAML form.
//!
//! Behaviour spec: `docs/cleanroom/specs/memory-normalize.md`. The pass is idempotent and must never
//! lose a memory, so every rewrite is verified by re-parsing and comparing before it touches disk; a
//! file that cannot be reproduced exactly is skipped instead of approximated.
use std::collections::BTreeMap;

use crate::frontmatter::{
    decode_scalar_source, describe_frontmatter_grammar_violation, render_raw_scalar,
    render_string_scalar, split_frontmatter,
};

pub const FRONTMATTER_NORMALIZATION_MARKER: &str = "mhc-memory-frontmatter-normalized";
pub const FRONTMATTER_NORMALIZATION_SUBJECT: &str =
    "chore(memory): normalize frontmatter to strict YAML";

const CONTRACT_KEYS: &[&str] = &["description", "read_only", "kind", "aliases", "limit"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryFrontmatter {
    pub description: String,
    pub read_only: Option<String>,
    pub kind: Option<String>,
    pub aliases: Option<Vec<String>>,
    pub extra: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedMemoryFile {
    pub frontmatter: MemoryFrontmatter,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontmatterError {
    pub message: String,
}

impl std::fmt::Display for FrontmatterError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for FrontmatterError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalizeOutcome {
    Unchanged,
    Rewritten,
    Skip(String),
}

/// A frontmatter value is one line by contract; a value carrying a newline would either split into a
/// continuation line or be silently truncated. Flattening keeps the text and drops only the break,
/// collapsing a run of breaks to one space so a wrapped paragraph does not become a wall of blanks.
pub fn sanitize_frontmatter_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut in_newline = false;
    for ch in value.chars() {
        if ch == '\r' || ch == '\n' {
            if !in_newline {
                out.push(' ');
                in_newline = true;
            }
        } else {
            out.push(ch);
            in_newline = false;
        }
    }
    out.trim().to_owned()
}

pub fn is_extra_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

pub fn is_contract_key(key: &str) -> bool {
    CONTRACT_KEYS.contains(&key)
}

pub fn parse_memory_file(content: &str) -> Result<ParsedMemoryFile, FrontmatterError> {
    let (header, body) = split_frontmatter(content).ok_or_else(|| FrontmatterError {
        message: "frontmatter: target file is missing required frontmatter".to_owned(),
    })?;

    let mut description: Option<String> = None;
    let mut read_only: Option<String> = None;
    let mut kind: Option<String> = None;
    let mut aliases: Option<Vec<String>> = None;
    let mut extra = BTreeMap::new();

    for line in header.split('\n') {
        if line.trim().is_empty() {
            continue;
        }
        let Some((key, raw)) = crate::frontmatter::parse_header_line(line) else {
            return Err(FrontmatterError {
                message: format!("frontmatter: not a 'key: value' line: {line}"),
            });
        };
        match key.as_str() {
            "description" => description = Some(decode_scalar_source(&raw)),
            "read_only" => read_only = Some(decode_scalar_source(&raw)),
            "kind" => kind = Some(decode_scalar_source(&raw)),
            "aliases" => {
                aliases = Some(crate::frontmatter::decode_string_array(&raw).ok_or_else(|| {
                    FrontmatterError {
                        message: format!(
                            "frontmatter: 'aliases' must be a JSON array of non-empty strings (line: {line})"
                        ),
                    }
                })?);
            }
            _ => {
                extra.insert(key, raw);
            }
        }
    }

    let description = description.ok_or_else(|| FrontmatterError {
        message: "frontmatter: 'description' must not be empty".to_owned(),
    })?;
    if description.trim().is_empty() {
        return Err(FrontmatterError {
            message: "frontmatter: 'description' must not be empty".to_owned(),
        });
    }

    Ok(ParsedMemoryFile {
        frontmatter: MemoryFrontmatter {
            description,
            read_only,
            kind,
            aliases,
            extra: (!extra.is_empty()).then_some(extra),
        },
        body,
    })
}

/// Render in the canonical key order: `description`, `read_only`, `kind`, `aliases`, then extras sorted
/// by key. A fixed order is what makes a rewrite a no-op for an already-normal file.
pub fn render_memory_file(
    frontmatter: &MemoryFrontmatter,
    body: &str,
) -> Result<String, FrontmatterError> {
    let description = sanitize_frontmatter_value(&frontmatter.description);
    if description.is_empty() {
        return Err(FrontmatterError {
            message: "frontmatter: 'description' must not be empty".to_owned(),
        });
    }

    let mut lines = vec![format!(
        "description: {}",
        render_string_scalar(&description)
    )];
    if let Some(read_only) = &frontmatter.read_only {
        lines.push(format!("read_only: {read_only}"));
    }
    if let Some(kind) = &frontmatter.kind {
        lines.push(format!(
            "kind: {}",
            render_string_scalar(&sanitize_frontmatter_value(kind))
        ));
    }
    if let Some(aliases) = &frontmatter.aliases {
        let json = serde_json::to_string(aliases).map_err(|err| FrontmatterError {
            message: format!("frontmatter: failed to serialize aliases: {err}"),
        })?;
        lines.push(format!("aliases: {json}"));
    }
    if let Some(extra) = &frontmatter.extra {
        for (key, raw) in extra {
            if !is_extra_key(key) || is_contract_key(key) {
                return Err(FrontmatterError {
                    message: format!("frontmatter: '{key}' is not a valid extra frontmatter key"),
                });
            }
            lines.push(format!(
                "{key}: {}",
                render_raw_scalar(&sanitize_frontmatter_value(raw))
            ));
        }
    }

    let header = format!("---\n{}\n---", lines.join("\n"));
    Ok(if body.is_empty() {
        format!("{header}\n")
    } else {
        format!("{header}\n{body}")
    })
}

pub fn same_frontmatter(left: &MemoryFrontmatter, right: &MemoryFrontmatter) -> bool {
    if left.description != right.description
        || left.read_only != right.read_only
        || left.kind != right.kind
        || left.aliases != right.aliases
    {
        return false;
    }
    let left_extra = left.extra.as_ref();
    let right_extra = right.extra.as_ref();
    if left_extra.map_or(0, BTreeMap::len) != right_extra.map_or(0, BTreeMap::len) {
        return false;
    }
    match (left_extra, right_extra) {
        (Some(left_extra), Some(right_extra)) => left_extra.iter().all(|(key, left_value)| {
            right_extra.get(key).is_some_and(|right_value| {
                decode_scalar_source(left_value) == decode_scalar_source(right_value)
            })
        }),
        _ => true,
    }
}

pub fn same_memory_file(left: &ParsedMemoryFile, right: &ParsedMemoryFile) -> bool {
    left.body == right.body && same_frontmatter(&left.frontmatter, &right.frontmatter)
}

/// Decide what to do with one file's contents.
///
/// The order matters: a file already strict is untouched without being parsed, and a `read_only` file is
/// skipped before any rewrite is attempted because the contract says the owner forbade changes. A
/// rewrite that does not re-parse to exactly the same memory is refused rather than written, so a
/// normalization bug cannot corrupt a store.
pub fn normalize_contents(content: &str) -> NormalizeOutcome {
    if describe_frontmatter_grammar_violation(content).is_none() {
        return NormalizeOutcome::Unchanged;
    }
    let Ok(parsed) = parse_memory_file(content) else {
        return NormalizeOutcome::Skip("unparseable".to_owned());
    };
    if parsed.frontmatter.read_only.as_deref() == Some("true") {
        return NormalizeOutcome::Skip("read_only".to_owned());
    }
    let Ok(rendered) = render_memory_file(&parsed.frontmatter, &parsed.body) else {
        return NormalizeOutcome::Skip("unparseable".to_owned());
    };
    if describe_frontmatter_grammar_violation(&rendered).is_some() {
        return NormalizeOutcome::Skip("not_reproducible".to_owned());
    }
    let Ok(reparsed) = parse_memory_file(&rendered) else {
        return NormalizeOutcome::Skip("not_reproducible".to_owned());
    };
    if !same_memory_file(&reparsed, &parsed) {
        return NormalizeOutcome::Skip("not_reproducible".to_owned());
    }
    NormalizeOutcome::Rewritten
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_strict_file_is_unchanged() {
        let doc = "---\ndescription: a memory\nkind: note\n---\nbody\n";
        assert_eq!(normalize_contents(doc), NormalizeOutcome::Unchanged);
    }

    #[test]
    fn an_already_quoted_value_is_left_alone() {
        let doc = "---\ndescription: \"a: b\"\n---\nbody\n";
        assert_eq!(normalize_contents(doc), NormalizeOutcome::Unchanged);
    }

    #[test]
    fn an_unquoted_unsafe_value_is_rewritten_to_a_quoted_one() {
        let doc = "---\ndescription: a: b\n---\nbody\n";
        assert_eq!(normalize_contents(doc), NormalizeOutcome::Rewritten);
    }

    #[test]
    fn an_unquoted_reinterpretable_value_is_quoted_on_rewrite() {
        let doc = "---\ndescription: true\n---\nbody\n";
        assert_eq!(normalize_contents(doc), NormalizeOutcome::Rewritten);
        let NormalizeOutcome::Rewritten = normalize_contents(doc) else {
            panic!("expected rewrite");
        };
        let parsed = parse_memory_file(doc).expect("parses");
        let rendered = render_memory_file(&parsed.frontmatter, &parsed.body).expect("renders");
        assert!(rendered.contains("description: \"true\""));
    }

    #[test]
    fn a_file_without_frontmatter_is_skipped_as_unparseable() {
        assert_eq!(
            normalize_contents("just a body\n"),
            NormalizeOutcome::Skip("unparseable".to_owned())
        );
    }

    #[test]
    fn a_read_only_file_is_skipped_even_when_it_needs_normalizing() {
        let doc = "---\ndescription: true\nread_only: \"true\"\n---\nbody\n";
        assert_eq!(
            normalize_contents(doc),
            NormalizeOutcome::Skip("read_only".to_owned())
        );
    }

    #[test]
    fn a_render_is_a_fixed_point() {
        let doc = "---\ndescription: true\nkind: \"a: b\"\naliases: [\"x\"]\nzeta: 1\nalpha: 2\n---\nbody\n";
        let parsed = parse_memory_file(doc).expect("parses");
        let once = render_memory_file(&parsed.frontmatter, &parsed.body).expect("renders");
        let reparsed = parse_memory_file(&once).expect("reparses");
        let twice = render_memory_file(&reparsed.frontmatter, &reparsed.body).expect("renders");
        assert_eq!(once, twice);
        assert_eq!(normalize_contents(&once), NormalizeOutcome::Unchanged);
    }

    #[test]
    fn extras_render_sorted_by_key() {
        let doc = "---\ndescription: a\nzeta: 1\nalpha: 2\n---\n";
        let parsed = parse_memory_file(doc).expect("parses");
        let rendered = render_memory_file(&parsed.frontmatter, &parsed.body).expect("renders");
        assert!(rendered.find("alpha:").expect("alpha") < rendered.find("zeta:").expect("zeta"));
    }

    #[test]
    fn a_contract_key_is_refused_as_an_extra() {
        let frontmatter = MemoryFrontmatter {
            description: "a".to_owned(),
            read_only: None,
            kind: None,
            aliases: None,
            extra: Some(BTreeMap::from([("kind".to_owned(), "note".to_owned())])),
        };
        assert!(render_memory_file(&frontmatter, "").is_err());
    }

    #[test]
    fn a_newline_in_a_value_flattens_to_one_space() {
        assert_eq!(sanitize_frontmatter_value("one\ntwo"), "one two");
        assert_eq!(sanitize_frontmatter_value("one\r\n\n\ntwo"), "one two");
        assert_eq!(sanitize_frontmatter_value("  padded  "), "padded");
    }

    #[test]
    fn a_multiline_description_round_trips_as_a_single_line() {
        let doc = "---\ndescription: \"one\\ntwo\"\n---\nbody\n";
        let parsed = parse_memory_file(doc).expect("parses");
        let rendered = render_memory_file(&parsed.frontmatter, &parsed.body).expect("renders");
        assert!(!rendered.contains("\\n"));
        let reparsed = parse_memory_file(&rendered).expect("reparses");
        assert_eq!(reparsed.frontmatter.description, "one two");
    }

    #[test]
    fn a_body_is_preserved_verbatim() {
        let doc = "---\ndescription: true\n---\nline one\n\nline three\n";
        let parsed = parse_memory_file(doc).expect("parses");
        let rendered = render_memory_file(&parsed.frontmatter, &parsed.body).expect("renders");
        assert!(rendered.ends_with("line one\n\nline three\n"));
    }

    #[test]
    fn an_empty_body_renders_a_single_trailing_newline() {
        let parsed = parse_memory_file("---\ndescription: a\n---\n").expect("parses");
        let rendered = render_memory_file(&parsed.frontmatter, &parsed.body).expect("renders");
        assert_eq!(rendered, "---\ndescription: a\n---\n");
    }

    #[test]
    fn a_korean_description_survives_normalization() {
        let doc = "---\ndescription: 한국어 메모리\n---\n본문\n";
        let parsed = parse_memory_file(doc).expect("parses");
        let rendered = render_memory_file(&parsed.frontmatter, &parsed.body).expect("renders");
        let reparsed = parse_memory_file(&rendered).expect("reparses");
        assert_eq!(reparsed.frontmatter.description, "한국어 메모리");
    }

    #[test]
    fn an_extra_key_must_be_alphanumeric_dash_or_underscore() {
        assert!(is_extra_key("created_at"));
        assert!(is_extra_key("my-key"));
        assert!(!is_extra_key("a b"));
        assert!(!is_extra_key(""));
    }

    #[test]
    fn a_missing_description_is_unparseable() {
        assert_eq!(
            normalize_contents("---\nkind: note\n---\nbody\n"),
            NormalizeOutcome::Skip("unparseable".to_owned())
        );
    }
}
