//! Strict path containment using lexical resolution followed by native canonicalization.

use std::io;
use std::path::{Component, Path, PathBuf};

pub type Realpath<'a> = &'a dyn Fn(&Path) -> io::Result<PathBuf>;

/// Options for path containment. `realpath` is injectable for alternate filesystems/tests.
#[derive(Clone, Copy, Default)]
pub struct PathContainmentOptions<'a> {
    pub platform: Option<&'a str>,
    pub realpath: Option<Realpath<'a>>,
}

fn lexical_resolve(path: &Path) -> io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut result = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::ParentDir => {
                result.pop();
            }
            Component::CurDir => {}
            other => result.push(other.as_os_str()),
        }
    }
    Ok(result)
}

fn canonical(path: &Path, options: &PathContainmentOptions<'_>) -> io::Result<PathBuf> {
    let resolved = lexical_resolve(path)?;
    let canonical = match options.realpath {
        Some(realpath) => realpath(&resolved)?,
        None => std::fs::canonicalize(resolved)?,
    };
    lexical_resolve(&canonical)
}

/// Return the canonical child's path relative to the canonical parent, or `None` when outside.
/// Both paths must exist; resolution failures propagate to let callers deny safely.
pub fn contained_relative_path(
    child: impl AsRef<Path>,
    parent: impl AsRef<Path>,
    options: PathContainmentOptions<'_>,
) -> io::Result<Option<PathBuf>> {
    let windows = options
        .platform
        .map_or(cfg!(windows), |platform| platform == "win32");
    if windows {
        let child = canonical_windows(child.as_ref(), &options)?;
        let parent = canonical_windows(parent.as_ref(), &options)?;
        let child_parts: Vec<_> = child.split('\\').filter(|part| !part.is_empty()).collect();
        let parent_parts: Vec<_> = parent.split('\\').filter(|part| !part.is_empty()).collect();
        if child_parts.len() < parent_parts.len()
            || !child_parts
                .iter()
                .zip(&parent_parts)
                .all(|(a, b)| a.eq_ignore_ascii_case(b))
        {
            return Ok(None);
        }
        return Ok(Some(PathBuf::from(
            child_parts[parent_parts.len()..].join("\\"),
        )));
    }
    let child = canonical(child.as_ref(), &options)?;
    let parent = canonical(parent.as_ref(), &options)?;
    Ok(child.strip_prefix(parent).ok().map(Path::to_path_buf))
}

/// Whether `child` is equal to or contained beneath `parent` after strict canonicalization.
pub fn is_path_inside(
    child: impl AsRef<Path>,
    parent: impl AsRef<Path>,
    options: PathContainmentOptions<'_>,
) -> io::Result<bool> {
    Ok(contained_relative_path(child, parent, options)?.is_some())
}

fn normalize_windows(path: &str) -> String {
    let path = path.replace('/', "\\");
    let path = path
        .strip_prefix("\\\\?\\UNC\\")
        .map(|tail| format!("\\\\{tail}"))
        .unwrap_or_else(|| path.strip_prefix("\\\\?\\").unwrap_or(&path).to_owned());
    let unc = path.starts_with("\\\\");
    let mut parts: Vec<&str> = Vec::new();
    let floor = if unc { 2 } else { 1 };
    for part in path
        .split('\\')
        .filter(|part| !part.is_empty() && *part != ".")
    {
        if part == ".." {
            if parts.len() > floor {
                parts.pop();
            }
        } else {
            parts.push(part);
        }
    }
    let mut normalized = parts.join("\\");
    if unc {
        normalized.insert_str(0, "\\\\");
    } else if parts.len() == 1 && normalized.ends_with(':') {
        normalized.push('\\');
    }
    normalized
}

fn canonical_windows(path: &Path, options: &PathContainmentOptions<'_>) -> io::Result<String> {
    let raw = path.to_string_lossy().replace('/', "\\");
    let absolute = if raw.starts_with("\\\\") || raw.as_bytes().get(1) == Some(&b':') {
        raw
    } else {
        format!("{}\\{}", std::env::current_dir()?.display(), raw)
    };
    let resolved = PathBuf::from(normalize_windows(&absolute));
    let canonical = match options.realpath {
        Some(realpath) => realpath(&resolved)?,
        None => std::fs::canonicalize(resolved)?,
    };
    Ok(normalize_windows(&canonical.to_string_lossy()))
}
