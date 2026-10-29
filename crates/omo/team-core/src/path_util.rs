//! Lexical `path.resolve` / `path.relative` equivalents.

use std::path::{Component, Path, PathBuf};

/// `path.resolve(base, ...segments)`: absolute, lexically normalized.
pub(crate) fn resolve(base: &Path, segments: &[&str]) -> PathBuf {
    let mut joined = if base.is_absolute() {
        base.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(base)
    };
    for segment in segments {
        joined.push(segment);
    }
    normalize(&joined)
}

pub(crate) fn normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}
