//! Lexical path helpers mirroring `node:path` semantics used by the package.

use std::path::{Component, Path, PathBuf};

/// Lexically resolve `path` against `base`, collapsing `.` and `..` without
/// touching the filesystem (the port of `path.resolve`).
pub fn resolve_from(base: &Path, path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    normalize(&joined)
}

/// Collapse `.`/`..` components. `..` at the root is dropped, never escaping it.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(Component::RootDir.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            Component::Normal(segment) => out.push(segment),
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// Convert an absolute or relative path to the forward-slash form git reports.
pub fn to_git_path(path: &Path) -> String {
    let mut out = String::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => {
                out.push_str(&prefix.as_os_str().to_string_lossy());
            }
            Component::RootDir => out.push('/'),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.is_empty() && !out.ends_with('/') {
                    out.push('/');
                }
                out.push_str("..");
            }
            Component::Normal(segment) => {
                if !out.is_empty() && !out.ends_with('/') {
                    out.push('/');
                }
                out.push_str(&segment.to_string_lossy());
            }
        }
    }
    out
}

/// The final path segment, or the empty string for the filesystem root.
pub fn base_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}
