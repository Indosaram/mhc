//! Markdown frontmatter parsing and rendering.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Error returned when frontmatter parsing or rendering fails.
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

/// Parsed metadata extracted from a memory file's YAML frontmatter.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryFrontmatter {
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_only: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aliases: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<BTreeMap<String, String>>,
}

impl MemoryFrontmatter {
    pub fn new(description: impl Into<String>) -> Self {
        Self {
            description: description.into(),
            read_only: None,
            kind: None,
            aliases: None,
            extra: None,
        }
    }
}

/// A parsed memory file comprising structured frontmatter and a markdown body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedMemoryFile {
    pub frontmatter: MemoryFrontmatter,
    pub body: String,
}

const CONTRACT_KEYS: &[&str] = &["description", "read_only", "kind", "aliases", "limit"];

fn is_contract_key(key: &str) -> bool {
    CONTRACT_KEYS.contains(&key)
}

fn is_extra_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Parse a memory markdown file, extracting frontmatter and body.
pub fn parse_memory_file(content: &str) -> Result<ParsedMemoryFile, FrontmatterError> {
    let (frontmatter_text, body) = match super::frontmatter_scalar::split_frontmatter(content) {
        Some(res) => res,
        None => {
            return Err(FrontmatterError {
                message: "frontmatter: target file is missing required frontmatter".to_string(),
            });
        }
    };

    let mut description: Option<String> = None;
    let mut read_only: Option<String> = None;
    let mut kind: Option<String> = None;
    let mut aliases: Option<Vec<String>> = None;
    let mut extra = BTreeMap::new();

    for entry in super::frontmatter_entries::frontmatter_entries(&frontmatter_text) {
        let key = entry.key.as_str();
        let value = entry.value.as_str();
        let line = entry.line.as_str();

        if key == "description" {
            description = Some(super::frontmatter_scalar::decode_scalar_source(value));
        } else if key == "read_only" {
            read_only = Some(value.to_string());
        } else if key == "kind" {
            kind = Some(super::frontmatter_scalar::decode_scalar_source(value));
        } else if key == "aliases" {
            aliases = Some(parse_aliases(value, line)?);
        } else if key != "limit" && is_extra_key(key) {
            extra.insert(key.to_string(), value.to_string());
        }
    }

    let description = match description {
        Some(desc) if !desc.trim().is_empty() => desc,
        _ => {
            return Err(FrontmatterError {
                message: "frontmatter: target file frontmatter is missing 'description'".to_string(),
            });
        }
    };

    let extra = if extra.is_empty() { None } else { Some(extra) };

    Ok(ParsedMemoryFile {
        frontmatter: MemoryFrontmatter {
            description,
            read_only,
            kind,
            aliases,
            extra,
        },
        body,
    })
}

/// Render a memory markdown file from frontmatter and body.
pub fn render_memory_file(
    frontmatter: &MemoryFrontmatter,
    body: &str,
) -> Result<String, FrontmatterError> {
    let description = sanitize_frontmatter_value(&frontmatter.description);
    if description.is_empty() {
        return Err(FrontmatterError {
            message: "frontmatter: 'description' must not be empty".to_string(),
        });
    }

    let mut lines = Vec::new();
    lines.push(format!(
        "description: {}",
        super::frontmatter_scalar::render_string_scalar(&description)
    ));

    if let Some(read_only) = &frontmatter.read_only {
        lines.push(format!("read_only: {read_only}"));
    }

    if let Some(kind) = &frontmatter.kind {
        lines.push(format!(
            "kind: {}",
            super::frontmatter_scalar::render_string_scalar(&sanitize_frontmatter_value(kind))
        ));
    }

    if let Some(aliases) = &frontmatter.aliases {
        let json_aliases = serde_json::to_string(aliases).map_err(|err| FrontmatterError {
            message: format!("frontmatter: failed to serialize aliases: {err}"),
        })?;
        lines.push(format!("aliases: {json_aliases}"));
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
                super::frontmatter_scalar::render_raw_scalar(&sanitize_frontmatter_value(raw))
            ));
        }
    }

    let header_text = lines.join("\n");
    let header = format!("---\n{header_text}\n---");
    let rendered = if body.is_empty() {
        format!("{header}\n")
    } else {
        format!("{header}\n{body}")
    };

    assert_strict_round_trip(&header_text, &rendered, frontmatter, &description)?;

    Ok(rendered)
}

fn assert_strict_round_trip(
    header_text: &str,
    rendered: &str,
    frontmatter: &MemoryFrontmatter,
    sanitized_description: &str,
) -> Result<(), FrontmatterError> {
    if let Some(violation) =
        super::frontmatter_validation::describe_header_grammar_violation(header_text)
    {
        return Err(FrontmatterError {
            message: format!("frontmatter: rendered header is not strict YAML: {violation}"),
        });
    }

    let reparsed = parse_memory_file(rendered)?.frontmatter;
    let mut mismatches = Vec::new();
    if reparsed.description != sanitized_description {
        mismatches.push("description");
    }
    if reparsed.read_only != frontmatter.read_only {
        mismatches.push("read_only");
    }
    if let Some(kind) = &frontmatter.kind
        && reparsed.kind.as_deref() != Some(&sanitize_frontmatter_value(kind)) {
            mismatches.push("kind");
        }
    if serde_json::to_string(&reparsed.aliases).unwrap_or_default()
        != serde_json::to_string(&frontmatter.aliases).unwrap_or_default()
    {
        mismatches.push("aliases");
    }
    if let Some(extra) = &frontmatter.extra {
        for key in extra.keys() {
            if reparsed.extra.as_ref().and_then(|e| e.get(key)).is_none() {
                mismatches.push(key.as_str());
            }
        }
    }
    if !mismatches.is_empty() {
        return Err(FrontmatterError {
            message: format!(
                "frontmatter: rendered header does not round-trip ({})",
                mismatches.join(", ")
            ),
        });
    }

    Ok(())
}

/// Sanitize a frontmatter value to a single line by collapsing any newline sequence to a space.
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
    out.trim().to_string()
}

fn parse_aliases(value: &str, raw_line: &str) -> Result<Vec<String>, FrontmatterError> {
    let trimmed = value.trim();
    if !trimmed.starts_with('[') {
        return Err(FrontmatterError {
            message: format!(
                "frontmatter: 'aliases' must be a JSON array of non-empty strings (line: {raw_line})"
            ),
        });
    }

    let parsed: serde_json::Value =
        serde_json::from_str(trimmed).map_err(|_| FrontmatterError {
            message: format!("frontmatter: 'aliases' is not valid JSON (line: {raw_line})"),
        })?;

    let array = match parsed {
        serde_json::Value::Array(items) => items,
        other => {
            let type_name = match other {
                serde_json::Value::Object(_) => "object",
                serde_json::Value::Number(_) => "number",
                serde_json::Value::Bool(_) => "boolean",
                serde_json::Value::Null => "null",
                serde_json::Value::String(_) => "string",
                serde_json::Value::Array(_) => "array",
            };
            return Err(FrontmatterError {
                message: format!(
                    "frontmatter: 'aliases' must be a JSON array, not {type_name} (line: {raw_line})"
                ),
            });
        }
    };

    let mut result = Vec::with_capacity(array.len());
    for item in array {
        match item {
            serde_json::Value::String(s) if !s.trim().is_empty() => {
                result.push(s);
            }
            _ => {
                return Err(FrontmatterError {
                    message: format!(
                        "frontmatter: 'aliases' array must contain only non-empty strings (line: {raw_line})"
                    ),
                });
            }
        }
    }

    Ok(result)
}

#[cfg(test)]
#[path = "frontmatter_tests.rs"]
mod tests;
