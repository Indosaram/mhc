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

/// Canonicalises `path` as given (made absolute, not lexically normalised first), so the
/// operating system follows each symlink before a later `..` is applied. When it does not exist,
/// canonicalises its deepest existing prefix and appends the remaining components, dropping `.`
/// and letting each `..` remove the last component built so far.
pub fn canonicalize_lenient(path: &Path) -> PathBuf {
    let path = absolutize(path);
    if let Ok(real) = path.canonicalize() {
        return real;
    }
    let components: Vec<Component<'_>> = path.components().collect();
    for split in (0..components.len()).rev() {
        let prefix: PathBuf = components[..split].iter().collect();
        if prefix.as_os_str().is_empty() {
            continue;
        }
        let Ok(mut out) = prefix.canonicalize() else {
            continue;
        };
        for component in &components[split..] {
            match component {
                Component::CurDir => {}
                Component::ParentDir => {
                    out.pop();
                }
                other => out.push(other.as_os_str()),
            }
        }
        return out;
    }
    normalize_lexically(&path)
}

/// True when `candidate` equals `root` or lies below it (component-wise, after
/// [`canonicalize_lenient`]). The verdict is about the exact string passed in: callers must check
/// the same string they later open.
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
