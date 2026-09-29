//! Path validation and YAML frontmatter parsing for markdown MemFS files.

use std::fs;
use std::path::{Path, PathBuf};

use crate::support::paths::normalize;

/// Frontmatter metadata parsed from a memory markdown file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryFrontmatter {
    pub description: String,
    pub read_only: Option<String>,
    pub kind: Option<String>,
    pub aliases: Option<Vec<String>>,
}

/// Parsed memory file containing structured frontmatter and markdown body text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedMemoryFile {
    pub frontmatter: MemoryFrontmatter,
    pub body: String,
}

/// Error returned when frontmatter is missing, malformed, or missing required fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontmatterError(pub String);

impl std::fmt::Display for FrontmatterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for FrontmatterError {}

/// Error returned when a memory file path fails validation or escapes root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryPathError(pub String);

impl std::fmt::Display for MemoryPathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for MemoryPathError {}

/// Parses YAML frontmatter and body from markdown content.
pub fn parse_memory_file(content: &str) -> Result<ParsedMemoryFile, FrontmatterError> {
    let normalized = content.replace("\r\n", "\n");
    if !normalized.starts_with("---\n") {
        return Err(FrontmatterError(
            "frontmatter: target file is missing required frontmatter".to_string(),
        ));
    }

    let rest = &normalized[4..];
    let Some(end_idx) = rest.find("\n---\n").or_else(|| {
        if rest.ends_with("\n---") {
            Some(rest.len() - 4)
        } else {
            None
        }
    }) else {
        return Err(FrontmatterError(
            "frontmatter: target file is missing required frontmatter".to_string(),
        ));
    };

    let frontmatter_text = &rest[..end_idx];
    let body_start = if rest[end_idx..].starts_with("\n---\n") {
        end_idx + 5
    } else {
        end_idx + 4
    };
    let body = rest[body_start..].to_string();

    let mut description: Option<String> = None;
    let mut read_only: Option<String> = None;
    let mut kind: Option<String> = None;
    let mut aliases: Option<Vec<String>> = None;

    for line in frontmatter_text.lines() {
        let Some((key, val)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let val = val.trim();

        if key == "description" {
            description = Some(val.to_string());
        } else if key == "read_only" {
            read_only = Some(val.to_string());
        } else if key == "kind" {
            kind = Some(val.to_string());
        } else if key == "aliases" {
            if !val.starts_with('[') {
                return Err(FrontmatterError(format!(
                    "frontmatter: 'aliases' must be a JSON array of non-empty strings (line: {line})"
                )));
            }
            let parsed: Vec<String> = serde_json::from_str(val).map_err(|_| {
                FrontmatterError(format!(
                    "frontmatter: 'aliases' is not valid JSON (line: {line})"
                ))
            })?;
            if parsed.iter().any(|s| s.trim().is_empty()) {
                return Err(FrontmatterError(format!(
                    "frontmatter: 'aliases' array must contain only non-empty strings (line: {line})"
                )));
            }
            aliases = Some(parsed);
        }
    }

    let desc = description
        .filter(|d| !d.trim().is_empty())
        .ok_or_else(|| {
            FrontmatterError(
                "frontmatter: target file frontmatter is missing 'description'".to_string(),
            )
        })?;

    Ok(ParsedMemoryFile {
        frontmatter: MemoryFrontmatter {
            description: desc,
            read_only,
            kind,
            aliases,
        },
        body,
    })
}

/// Renders a memory file from frontmatter and body with sanitized single-line description.
pub fn render_memory_file(
    frontmatter: &MemoryFrontmatter,
    body: &str,
) -> Result<String, FrontmatterError> {
    let desc = frontmatter.description.trim();
    if desc.is_empty() {
        return Err(FrontmatterError(
            "frontmatter: 'description' must not be empty".to_string(),
        ));
    }

    let sanitized_desc = desc.replace(['\r', '\n'], " ");
    let mut lines = vec![
        "---".to_string(),
        format!("description: {}", sanitized_desc.trim()),
    ];

    if let Some(ro) = &frontmatter.read_only {
        lines.push(format!("read_only: {ro}"));
    }
    if let Some(k) = &frontmatter.kind {
        lines.push(format!("kind: {}", k.replace(['\r', '\n'], " ").trim()));
    }
    if let Some(a) = &frontmatter.aliases {
        let json = serde_json::to_string(a).unwrap_or_else(|_| "[]".to_string());
        lines.push(format!("aliases: {json}"));
    }
    lines.push("---".to_string());

    let header = lines.join("\n");
    if body.is_empty() {
        Ok(format!("{header}\n"))
    } else {
        Ok(format!("{header}\n{body}"))
    }
}

/// Options controlling memory path confinement checks.
#[derive(Debug, Clone, Default)]
pub struct ValidateMemoryPathOptions<'a> {
    pub tool_path: bool,
    pub field_name: &'a str,
}

/// Validates that a path is confined within the memory repository root.
pub fn validate_memory_path(
    memory_root: &Path,
    input_path: &str,
    options: ValidateMemoryPathOptions<'_>,
) -> Result<PathBuf, MemoryPathError> {
    let root = memory_root.to_path_buf();
    let field_name = if options.field_name.is_empty() {
        "path"
    } else {
        options.field_name
    };
    let raw = input_path.trim();

    if raw.is_empty() {
        return Err(invalid(&format!(
            "'{field_name}' must be a non-empty string"
        )));
    }
    if raw.contains('\0') {
        return Err(invalid(&format!(
            "'{field_name}' contains invalid null bytes"
        )));
    }
    if raw.starts_with("~/") || raw.starts_with("$HOME/") {
        return Err(invalid(&format!(
            "'{field_name}' must be a memory-relative path, not a home-relative filesystem path"
        )));
    }

    let windows_absolute = raw.len() >= 3
        && raw.as_bytes()[0].is_ascii_alphabetic()
        && raw.as_bytes()[1] == b':'
        && (raw.as_bytes()[2] == b'\\' || raw.as_bytes()[2] == b'/');

    let relative_input = if Path::new(raw).is_absolute() || windows_absolute {
        let abs = PathBuf::from(raw);
        let rel = match abs.strip_prefix(&root) {
            Ok(r) => r.to_string_lossy().into_owned(),
            Err(_) => return Err(invalid(&prefix_error(&root))),
        };
        if rel.is_empty() {
            return Err(invalid(&prefix_error(&root)));
        }
        rel
    } else {
        raw.to_string()
    };

    let normalized = normalize_relative_path(&relative_input, field_name, options.tool_path)?;
    let target = normalize(&root.join(&normalized));

    if !target.starts_with(&root) || target == root {
        return Err(invalid("resolved path escapes memory directory"));
    }

    assert_real_parent_confined(&root, &target)?;
    Ok(target)
}

/// Validates a repository path allowing non-markdown and directory targets.
pub fn validate_repository_path(
    memory_root: &Path,
    input_path: &str,
    field_name: &str,
) -> Result<PathBuf, MemoryPathError> {
    validate_memory_path(
        memory_root,
        input_path,
        ValidateMemoryPathOptions {
            tool_path: false,
            field_name,
        },
    )
}

fn normalize_relative_path(
    input: &str,
    field_name: &str,
    tool_path: bool,
) -> Result<String, MemoryPathError> {
    let mut normalized = input.trim().replace('\\', "/");
    if normalized.starts_with('/') {
        return Err(invalid(&format!(
            "'{field_name}' must be a relative path like system/contacts.md"
        )));
    }

    if let Some(stripped) = normalized.strip_prefix("memory/") {
        normalized = stripped.to_string();
    }
    if normalized.is_empty() {
        return Err(invalid(&format!(
            "'{field_name}' resolves to an empty memory label"
        )));
    }

    let mut path = normalized;
    if tool_path && let Some(idx) = path.rfind('.') {
        let ext = &path[idx..];
        if ext != ".md" {
            return Err(invalid(&format!(
                "'{field_name}' must target a lowercase .md markdown file"
            )));
        }
        path.truncate(idx);
    }

    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return Err(invalid(&format!(
            "'{field_name}' resolves to an empty memory label"
        )));
    }

    for seg in &segments {
        if *seg == "." || *seg == ".." {
            return Err(invalid(&format!(
                "'{field_name}' contains invalid path traversal segment"
            )));
        }
        if seg.contains('\0') {
            return Err(invalid(&format!(
                "'{field_name}' contains invalid null bytes"
            )));
        }
        if seg.eq_ignore_ascii_case(".git") {
            return Err(invalid(&format!(
                "'{field_name}' cannot target .git administration paths"
            )));
        }
    }

    let joined = segments.join("/");
    if tool_path {
        Ok(format!("{joined}.md"))
    } else {
        Ok(joined)
    }
}

fn assert_real_parent_confined(root: &Path, target: &Path) -> Result<(), MemoryPathError> {
    let root_real = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());

    let mut candidate = target.to_path_buf();
    while candidate.starts_with(root) {
        if candidate.exists() {
            let real = fs::canonicalize(&candidate).unwrap_or_else(|_| candidate.clone());
            if !real.starts_with(&root_real) {
                return Err(invalid("path escapes memory root through a symlink"));
            }
            return Ok(());
        }
        let Some(parent) = candidate.parent() else {
            break;
        };
        if parent == candidate {
            break;
        }
        candidate = parent.to_path_buf();
    }
    Ok(())
}

fn invalid(msg: &str) -> MemoryPathError {
    MemoryPathError(format!("memory path: {msg}"))
}

fn prefix_error(root: &Path) -> String {
    format!(
        "The memory tool can only be used to modify files in {{{}}} or provided as a relative path",
        root.display()
    )
}
