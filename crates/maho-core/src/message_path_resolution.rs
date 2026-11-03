//! Port of senpi packages/coding-agent/src/core/message-path-resolution.ts.
//!
//! What a path in a message means (#3415): the one rule the TUI, RPC clients and the desktop app share,
//! so the same link opens the same file everywhere. A relative path is tried against the session's
//! working directory first, then the project root (the nearest ancestor holding `.git`, else the nearest
//! project marker; a `.git` in the home folder does not count). The first existing candidate wins, and
//! the answer is its realpath. Only a local path ever reaches the file system (`is_local_message_path`):
//! on Windows a drive-letter path, never a network share or device path, because resolving one makes
//! Windows connect to that host (#3419 review H1).

use std::path::{Path, PathBuf};

use crate::paths::strip_verbatim_prefix;

/// The project markers of the rules extension's `findProjectRoot`, in its order.
const PROJECT_MARKERS: &[&str] = &[
    ".git",
    "pnpm-workspace.yaml",
    "package.json",
    "pyproject.toml",
    "Cargo.toml",
    "go.mod",
    ".venv",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessagePathKind {
    File,
    Directory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessagePathBase {
    Absolute,
    Cwd,
    ProjectRoot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedMessagePath {
    Found {
        realpath: String,
        kind: MessagePathKind,
        base: MessagePathBase,
    },
    NotFound {
        tried: Vec<String>,
    },
}

/// Whether `path` (absolute) is a local path this process may hand to the file system.
pub fn is_local_message_path(path: &str, windows: bool) -> bool {
    if path.contains('\0') {
        return false;
    }
    if windows {
        let bytes = path.as_bytes();
        return bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'\\' | b'/');
    }
    path.starts_with('/') && !path.starts_with("//")
}

fn find_project_root(start: &Path, markers: &[&str]) -> Option<PathBuf> {
    let resolved = strip_verbatim_prefix(start.canonicalize().ok()?);
    let mut current = if resolved.metadata().ok()?.is_dir() {
        resolved
    } else {
        resolved.parent()?.to_owned()
    };
    loop {
        if markers.iter().any(|marker| current.join(marker).exists()) {
            return Some(current);
        }
        current = current.parent()?.to_owned();
    }
}

/// The project root a session's relative paths fall back to, or `None` when there is none.
pub fn message_path_project_root(cwd: &str) -> Option<String> {
    let cwd = Path::new(cwd);
    let candidate =
        find_project_root(cwd, &[".git"]).or_else(|| find_project_root(cwd, PROJECT_MARKERS))?;
    let home = PathBuf::from(crate::config::home_dir());
    let home = home.canonicalize().map(strip_verbatim_prefix).unwrap_or(home);
    (candidate != home).then(|| candidate.to_string_lossy().into_owned())
}

fn existing(candidate: &str) -> Option<(String, MessagePathKind)> {
    let realpath = strip_verbatim_prefix(std::fs::canonicalize(candidate).ok()?);
    let metadata = std::fs::metadata(&realpath).ok()?;
    let kind = if metadata.is_file() {
        MessagePathKind::File
    } else if metadata.is_dir() {
        MessagePathKind::Directory
    } else {
        return None;
    };
    Some((realpath.to_string_lossy().into_owned(), kind))
}

fn expand_home(path: &str) -> String {
    if path == "~" {
        return crate::config::home_dir();
    }
    match path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        Some(rest) => Path::new(&crate::config::home_dir())
            .join(rest)
            .to_string_lossy()
            .into_owned(),
        None => path.to_owned(),
    }
}

/// Lexical `path.resolve(base, path)` on the native platform: joins and normalizes `.` and `..`.
fn resolve(base: &str, path: &str) -> String {
    let mut out = PathBuf::new();
    for component in Path::new(base).join(path).components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out.to_string_lossy().into_owned()
}

pub fn resolve_message_path(
    path: &str,
    cwd: &str,
    project_root: Option<&str>,
) -> ResolvedMessagePath {
    let windows = cfg!(windows);
    let path = expand_home(path.trim());
    if path.is_empty() || path.contains('\0') {
        return ResolvedMessagePath::NotFound { tried: Vec::new() };
    }
    let bases: Vec<(MessagePathBase, String)> = if Path::new(&path).is_absolute() {
        vec![(MessagePathBase::Absolute, path.clone())]
    } else {
        let mut bases = vec![(MessagePathBase::Cwd, resolve(cwd, &path))];
        if let Some(root) = project_root.filter(|root| resolve(root, "") != resolve(cwd, "")) {
            bases.push((MessagePathBase::ProjectRoot, resolve(root, &path)));
        }
        bases
    };
    if !bases
        .iter()
        .all(|(_, candidate)| is_local_message_path(candidate, windows))
    {
        return ResolvedMessagePath::NotFound { tried: Vec::new() };
    }
    for (base, candidate) in &bases {
        if let Some((realpath, kind)) = existing(candidate)
            && is_local_message_path(&realpath, windows)
        {
            return ResolvedMessagePath::Found {
                realpath,
                kind,
                base: *base,
            };
        }
    }
    ResolvedMessagePath::NotFound {
        tried: bases.into_iter().map(|(_, candidate)| candidate).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_local_paths_reach_the_file_system() {
        assert!(is_local_message_path("/a/b", false));
        assert!(!is_local_message_path("//host/share", false));
        assert!(!is_local_message_path("/a\0b", false));
        assert!(is_local_message_path("C:\\x", true));
        assert!(!is_local_message_path("\\\\host\\share", true));
        assert!(!is_local_message_path("\\\\?\\C:\\x", true));
    }

    #[cfg(unix)]
    #[test]
    fn tries_cwd_then_project_root_and_reports_what_it_tried() {
        let Some(dir) = tempfile::tempdir().ok() else {
            panic!("tempdir")
        };
        let Some(root) = dir.path().canonicalize().ok() else {
            panic!("canonical")
        };
        assert!(std::fs::create_dir_all(root.join(".git")).is_ok());
        assert!(std::fs::create_dir_all(root.join("pkg")).is_ok());
        assert!(std::fs::write(root.join("README.md"), "x").is_ok());
        let cwd = root.join("pkg").to_string_lossy().into_owned();
        let project_root = message_path_project_root(&cwd);
        assert_eq!(
            project_root.as_deref(),
            Some(root.to_string_lossy().as_ref())
        );
        let found = resolve_message_path("README.md", &cwd, project_root.as_deref());
        assert_eq!(
            found,
            ResolvedMessagePath::Found {
                realpath: root.join("README.md").to_string_lossy().into_owned(),
                kind: MessagePathKind::File,
                base: MessagePathBase::ProjectRoot,
            }
        );
        let found = resolve_message_path(" ../pkg ", &cwd, None);
        assert!(matches!(
            found,
            ResolvedMessagePath::Found {
                kind: MessagePathKind::Directory,
                base: MessagePathBase::Cwd,
                ..
            }
        ));
        let missing = resolve_message_path("nope.txt", &cwd, project_root.as_deref());
        assert_eq!(
            missing,
            ResolvedMessagePath::NotFound {
                tried: vec![
                    format!("{cwd}/nope.txt"),
                    root.join("nope.txt").to_string_lossy().into_owned()
                ],
            }
        );
        assert_eq!(
            resolve_message_path("//host/x", &cwd, None),
            ResolvedMessagePath::NotFound { tried: Vec::new() }
        );
    }
}
