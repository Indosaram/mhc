//! Markdown frontmatter parsing and rendering.

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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryFrontmatter {
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_only: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aliases: Option<Vec<String>>,
}

/// A parsed memory file comprising structured frontmatter and a markdown body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedMemoryFile {
    pub frontmatter: MemoryFrontmatter,
    pub body: String,
}

/// Parse a memory markdown file, extracting frontmatter and body.
pub fn parse_memory_file(content: &str) -> Result<ParsedMemoryFile, FrontmatterError> {
    let rest = if let Some(stripped) = content.strip_prefix("---\r\n") {
        stripped
    } else if let Some(stripped) = content.strip_prefix("---\n") {
        stripped
    } else {
        return Err(FrontmatterError {
            message: "frontmatter: target file is missing required frontmatter".to_string(),
        });
    };

    let mut closing_start = None;
    let mut body_start = None;
    let mut line_start = 0;

    for line in rest.split_inclusive('\n') {
        let trimmed_line = line.trim_end_matches(['\r', '\n']);
        if trimmed_line == "---" {
            closing_start = Some(line_start);
            body_start = Some(line_start + line.len());
            break;
        }
        line_start += line.len();
    }

    let (frontmatter_text, body) = match (closing_start, body_start) {
        (Some(c_start), Some(b_start)) => {
            let fm = &rest[..c_start];
            let fm = fm
                .strip_suffix("\r\n")
                .or_else(|| fm.strip_suffix('\n'))
                .unwrap_or(fm);
            let b = &rest[b_start..];
            (fm, b.to_string())
        }
        _ => {
            return Err(FrontmatterError {
                message: "frontmatter: target file is missing required frontmatter".to_string(),
            });
        }
    };

    let mut description: Option<String> = None;
    let mut read_only: Option<String> = None;
    let mut kind: Option<String> = None;
    let mut aliases: Option<Vec<String>> = None;

    for raw_line in frontmatter_text.split('\n') {
        let line = raw_line.trim_end_matches('\r');
        let idx = match line.find(':') {
            Some(i) if i > 0 => i,
            _ => continue,
        };

        let key = line[..idx].trim();
        let value = line[idx + 1..].trim();

        if key == "description" {
            description = Some(value.to_string());
        } else if key == "read_only" {
            read_only = Some(value.to_string());
        } else if key == "kind" {
            kind = Some(value.to_string());
        } else if key == "aliases" {
            aliases = Some(parse_aliases(value, line)?);
        }
    }

    let description = match description {
        Some(desc) if !desc.trim().is_empty() => desc,
        _ => {
            return Err(FrontmatterError {
                message: "frontmatter: target file frontmatter is missing 'description'"
                    .to_string(),
            });
        }
    };

    Ok(ParsedMemoryFile {
        frontmatter: MemoryFrontmatter {
            description,
            read_only,
            kind,
            aliases,
        },
        body,
    })
}

/// Render a memory markdown file from frontmatter and body.
pub fn render_memory_file(
    frontmatter: &MemoryFrontmatter,
    body: &str,
) -> Result<String, FrontmatterError> {
    let description = frontmatter.description.trim();
    if description.is_empty() {
        return Err(FrontmatterError {
            message: "frontmatter: 'description' must not be empty".to_string(),
        });
    }

    let mut lines = Vec::new();
    lines.push("---".to_string());
    lines.push(format!(
        "description: {}",
        sanitize_frontmatter_value(description)
    ));

    if let Some(read_only) = &frontmatter.read_only {
        lines.push(format!("read_only: {read_only}"));
    }

    if let Some(kind) = &frontmatter.kind {
        lines.push(format!("kind: {}", sanitize_frontmatter_value(kind)));
    }

    if let Some(aliases) = &frontmatter.aliases {
        let json_aliases = serde_json::to_string(aliases).map_err(|err| FrontmatterError {
            message: format!("frontmatter: failed to serialize aliases: {err}"),
        })?;
        lines.push(format!("aliases: {json_aliases}"));
    }

    lines.push("---".to_string());

    let header = lines.join("\n");
    if body.is_empty() {
        Ok(format!("{header}\n"))
    } else {
        Ok(format!("{header}\n{body}"))
    }
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
