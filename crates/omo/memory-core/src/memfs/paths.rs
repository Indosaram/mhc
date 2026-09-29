//! Confined memory path validation.

use std::path::{Path, PathBuf};

/// Error returned when memory path validation fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryPathError {
    pub message: String,
}

impl std::fmt::Display for MemoryPathError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for MemoryPathError {}

/// Options controlling memory path validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidateMemoryPathOptions<'a> {
    /// Tool paths are markdown labels; repository paths may target directories.
    pub tool_path: bool,
    pub field_name: &'a str,
}

impl<'a> Default for ValidateMemoryPathOptions<'a> {
    fn default() -> Self {
        Self {
            tool_path: true,
            field_name: "path",
        }
    }
}

/// Validate that a path input resolves to a confined location inside `memory_root`.
pub fn validate_memory_path(
    memory_root: &Path,
    input_path: &str,
    options: ValidateMemoryPathOptions<'_>,
) -> Result<PathBuf, MemoryPathError> {
    let root = memory_root.to_path_buf();
    let field_name = options.field_name;
    let tool_path = options.tool_path;
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

    let windows_absolute = is_windows_absolute(raw);
    let mut relative_input = raw.to_string();
    if Path::new(raw).is_absolute() || windows_absolute {
        let abs_raw = Path::new(raw);
        let rel_to_memory = if let Ok(stripped) = abs_raw.strip_prefix(&root) {
            stripped.to_path_buf()
        } else {
            let canon_root = root.canonicalize().unwrap_or_else(|_| root.clone());
            let canon_raw = abs_raw
                .canonicalize()
                .unwrap_or_else(|_| abs_raw.to_path_buf());
            if let Ok(stripped) = canon_raw.strip_prefix(&canon_root) {
                stripped.to_path_buf()
            } else {
                return Err(invalid(&prefix_error(&root)));
            }
        };

        if rel_to_memory.as_os_str().is_empty() || !is_confined_relative(&rel_to_memory) {
            return Err(invalid(&prefix_error(&root)));
        }
        relative_input = rel_to_memory.to_string_lossy().to_string();
    }

    let normalized = normalize_relative_path(&relative_input, field_name, tool_path)?;
    let target = root.join(&normalized);
    let rel_to_root = if let Ok(stripped) = target.strip_prefix(&root) {
        stripped.to_path_buf()
    } else {
        return Err(invalid("resolved path escapes memory directory"));
    };

    if rel_to_root.as_os_str().is_empty() || !is_confined_relative(&rel_to_root) {
        return Err(invalid("resolved path escapes memory directory"));
    }

    assert_real_parent_confined(&root, &target)?;
    Ok(target)
}

/// Validate a path in repository mode (allows directories, does not enforce `.md` extension).
pub fn validate_repository_path(
    memory_root: &Path,
    input_path: &str,
) -> Result<PathBuf, MemoryPathError> {
    validate_memory_path(
        memory_root,
        input_path,
        ValidateMemoryPathOptions {
            tool_path: false,
            field_name: "path",
        },
    )
}

/// Validate a path in repository mode with a custom field name.
pub fn validate_repository_path_with_field(
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
    input_path: &str,
    field_name: &str,
    tool_path: bool,
) -> Result<String, MemoryPathError> {
    let normalized = input_path.trim().replace('\\', "/");
    if normalized.starts_with('/') {
        return Err(invalid(&format!(
            "'{field_name}' must be a relative path like system/contacts.md"
        )));
    }

    let path_no_mem = if let Some(stripped) = normalized.strip_prefix("memory/") {
        stripped
    } else {
        &normalized
    };

    if path_no_mem.is_empty() {
        return Err(invalid(&format!(
            "'{field_name}' resolves to an empty memory label"
        )));
    }

    let mut path = path_no_mem.to_string();
    if tool_path {
        let ext = extension_of(&path);
        if !ext.is_empty() && ext != ".md" {
            return Err(invalid(&format!(
                "'{field_name}' must target a lowercase .md markdown file"
            )));
        }
        if path.ends_with(".md") {
            path.truncate(path.len() - 3);
        }
    }

    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return Err(invalid(&format!(
            "'{field_name}' resolves to an empty memory label"
        )));
    }

    for segment in &segments {
        if *segment == "." || *segment == ".." {
            return Err(invalid(&format!(
                "'{field_name}' contains invalid path traversal segment"
            )));
        }
        if segment.contains('\0') {
            return Err(invalid(&format!(
                "'{field_name}' contains invalid null bytes"
            )));
        }
        if segment.eq_ignore_ascii_case(".git") {
            return Err(invalid(&format!(
                "'{field_name}' cannot target .git administration paths"
            )));
        }
    }

    let repo_path = segments.join("/");
    if tool_path {
        Ok(format!("{repo_path}.md"))
    } else {
        Ok(repo_path)
    }
}

fn assert_real_parent_confined(root: &Path, target: &Path) -> Result<(), MemoryPathError> {
    let root_real = match root.canonicalize() {
        Ok(path) => path,
        Err(err) => {
            return Err(invalid(&format!("memory root cannot be resolved: {err}")));
        }
    };

    let mut candidate = target.to_path_buf();
    loop {
        match std::fs::symlink_metadata(&candidate) {
            Ok(_) => {
                let candidate_real = match candidate.canonicalize() {
                    Ok(path) => path,
                    Err(err) => {
                        return Err(invalid(&format!(
                            "path contains an unresolved symlink: {err}"
                        )));
                    }
                };

                if !candidate_real.starts_with(&root_real) {
                    return Err(invalid("path escapes memory root through a symlink"));
                }
                return Ok(());
            }
            Err(err) => {
                if is_missing_path_error(&err) {
                    let parent = match candidate.parent() {
                        Some(p) => p.to_path_buf(),
                        None => {
                            return Err(invalid(
                                "path has no existing ancestor inside memory root",
                            ));
                        }
                    };
                    if parent == candidate {
                        return Err(invalid("path has no existing ancestor inside memory root"));
                    }
                    candidate = parent;
                    continue;
                }
                return Err(invalid(&format!(
                    "path ancestor cannot be inspected: {err}"
                )));
            }
        }
    }
}

fn is_confined_relative(path: &Path) -> bool {
    let path_str = path.to_string_lossy();
    !path_str.starts_with("..") && !path.is_absolute()
}

fn is_missing_path_error(err: &std::io::Error) -> bool {
    err.kind() == std::io::ErrorKind::NotFound
}

fn prefix_error(root: &Path) -> String {
    format!(
        "The memory tool can only be used to modify files in {{{}}} or provided as a relative path",
        root.display()
    )
}

fn invalid(message: &str) -> MemoryPathError {
    MemoryPathError {
        message: format!("memory path: {message}"),
    }
}

fn is_windows_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

fn extension_of(path: &str) -> String {
    if let Some(pos) = path.rfind('/') {
        let filename = &path[pos + 1..];
        if let Some(dot_pos) = filename.rfind('.') {
            filename[dot_pos..].to_string()
        } else {
            String::new()
        }
    } else if let Some(dot_pos) = path.rfind('.') {
        path[dot_pos..].to_string()
    } else {
        String::new()
    }
}

#[cfg(test)]
#[path = "paths_tests.rs"]
mod tests;
