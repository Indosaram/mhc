//! Path containment and small file helpers.

use std::path::{Component, Path, PathBuf};

/// Removes `.` and resolves `..` lexically (without touching the filesystem).
pub fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    // Above the root of a relative path: keep the `..`.
                    if !out.has_root() {
                        out.push("..");
                    }
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn absolutize(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

/// Canonicalises `path`; when it does not exist, canonicalises its deepest existing ancestor and
/// re-appends the remaining components.
pub fn canonicalize_lenient(path: &Path) -> PathBuf {
    let path = normalize_lexically(&absolutize(path));
    if let Ok(real) = path.canonicalize() {
        return real;
    }
    let mut remainder: Vec<std::ffi::OsString> = Vec::new();
    let mut current = path.clone();
    loop {
        if let Ok(real) = current.canonicalize() {
            let mut out = real;
            for part in remainder.iter().rev() {
                out.push(part);
            }
            return out;
        }
        match (
            current.file_name().map(|name| name.to_os_string()),
            current.parent(),
        ) {
            (Some(name), Some(parent)) => {
                remainder.push(name);
                current = parent.to_path_buf();
            }
            _ => return path,
        }
    }
}

/// True when `candidate` equals `root` or lies below it (component-wise, after normalisation and
/// symlink resolution).
pub fn contains_path(root: &Path, candidate: &Path) -> bool {
    let root = canonicalize_lenient(root);
    let candidate = canonicalize_lenient(candidate);
    candidate.starts_with(&root)
}

/// [`contains_path`] with the arguments swapped.
pub fn is_within(candidate: &Path, root: &Path) -> bool {
    contains_path(root, candidate)
}

/// A regular file whose name ends in `.md` (case-sensitive) and does not start with `.`.
pub fn is_markdown_file(name: &str, is_file: bool) -> bool {
    is_file && name.ends_with(".md") && !name.starts_with('.')
}

/// The canonical path if resolvable, else the input unchanged.
pub fn resolve_symlink(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Whether `path` itself is a symlink; errors count as "no".
pub fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
}
