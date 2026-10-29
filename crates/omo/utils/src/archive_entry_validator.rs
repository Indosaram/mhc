//! Rejects archive entries (and link targets) that would escape the extraction directory.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::contains_path::lexical_normalize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveEntryType {
    File,
    Directory,
    Symlink,
    Hardlink,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveEntry {
    pub path: String,
    pub entry_type: ArchiveEntryType,
    pub link_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsafeArchiveEntryError {
    pub message: String,
}

impl fmt::Display for UnsafeArchiveEntryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for UnsafeArchiveEntryError {}

fn unsafe_entry(detail: String) -> UnsafeArchiveEntryError {
    UnsafeArchiveEntryError {
        message: format!("Unsafe archive entry: {detail}"),
    }
}

fn normalize(path: &str) -> String {
    path.replace('\\', "/")
}

fn has_traversal(path: &str) -> bool {
    normalize(path).split('/').any(|segment| segment == "..")
}

fn is_archive_absolute(path: &str) -> bool {
    let normalized = normalize(path);
    let bytes = normalized.as_bytes();
    normalized.starts_with('/')
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && bytes[2] == b'/')
        || Path::new(&normalized).is_absolute()
}

fn escapes(root: &Path, candidate: &Path) -> bool {
    !candidate.starts_with(root)
}

fn resolve_contained(
    root: &Path,
    path: &str,
    label: &str,
) -> Result<PathBuf, UnsafeArchiveEntryError> {
    if is_archive_absolute(path) {
        return Err(unsafe_entry(format!(
            "{label} uses an absolute path ({path})"
        )));
    }
    if has_traversal(path) {
        return Err(unsafe_entry(format!(
            "{label} contains path traversal ({path})"
        )));
    }
    let resolved = lexical_normalize(&root.join(normalize(path)));
    if escapes(root, &resolved) {
        return Err(unsafe_entry(format!(
            "{label} contains path traversal ({path})"
        )));
    }
    Ok(resolved)
}

pub fn validate_archive_entries(
    entries: &[ArchiveEntry],
    dest_dir: impl AsRef<Path>,
) -> Result<(), UnsafeArchiveEntryError> {
    let dest = dest_dir.as_ref();
    let root = lexical_normalize(&if dest.is_absolute() {
        dest.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(dest)
    });
    for entry in entries {
        let resolved_entry = resolve_contained(&root, &entry.path, "path")?;
        let (kind, label) = match entry.entry_type {
            ArchiveEntryType::File | ArchiveEntryType::Directory => continue,
            ArchiveEntryType::Symlink => ("symlink", "symlink target"),
            ArchiveEntryType::Hardlink => ("hard link", "hard link target"),
        };
        let Some(link) = entry.link_path.as_deref().filter(|link| !link.is_empty()) else {
            return Err(unsafe_entry(format!(
                "{kind} target missing for {}",
                entry.path
            )));
        };
        if is_archive_absolute(link) {
            return Err(unsafe_entry(format!(
                "{label} uses an absolute path ({link})"
            )));
        }
        if has_traversal(link) {
            return Err(unsafe_entry(format!(
                "{label} contains path traversal ({link})"
            )));
        }
        let parent = resolved_entry.parent().unwrap_or(&root);
        let resolved_link = lexical_normalize(&parent.join(normalize(link)));
        if escapes(&root, &resolved_link) {
            return Err(unsafe_entry(format!(
                "{label} escapes extraction directory ({link})"
            )));
        }
    }
    Ok(())
}
