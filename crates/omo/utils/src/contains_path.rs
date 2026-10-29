//! Canonical path containment checks that tolerate not-yet-existing paths.

use std::fs;
use std::path::{Component, Path, PathBuf};

fn absolutize(path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    lexical_normalize(&joined)
}

/// Resolve `.` and `..` lexically (the equivalent of Node's `path.resolve`).
pub(crate) fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn to_canonical_path(path: &Path) -> PathBuf {
    let resolved = absolutize(path);
    if resolved.exists() {
        return fs::canonicalize(&resolved).unwrap_or(resolved);
    }
    let mut ancestor = resolved.clone();
    while !ancestor.exists() {
        if !ancestor.pop() {
            break;
        }
    }
    let canonical_ancestor = fs::canonicalize(&ancestor).unwrap_or_else(|_| ancestor.clone());
    let rest = resolved.strip_prefix(&ancestor).unwrap_or(&resolved);
    lexical_normalize(&canonical_ancestor.join(rest))
}

pub fn contains_path(root_path: impl AsRef<Path>, candidate_path: impl AsRef<Path>) -> bool {
    let root = to_canonical_path(root_path.as_ref());
    let candidate = to_canonical_path(candidate_path.as_ref());
    candidate.starts_with(&root)
}

pub fn is_within_project(candidate_path: impl AsRef<Path>, project_root: impl AsRef<Path>) -> bool {
    contains_path(project_root, candidate_path)
}
