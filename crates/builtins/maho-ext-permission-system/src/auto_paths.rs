//! Port of `permission-system/auto-paths.ts` (senpi 903b8336): which filesystem targets the `auto`
//! preset may approve.

use crate::auto_credentials::is_credential_path;
use std::path::{Component, Path, PathBuf};

/// Hidden project entries `auto` may touch: repository metadata and formatter/linter configs.
const SAFE_HIDDEN_NAMES: &[&str] = &[
    ".github",
    ".gitignore",
    ".gitattributes",
    ".editorconfig",
    ".nvmrc",
    ".node-version",
    ".prettierrc",
    ".prettierignore",
    ".eslintrc",
    ".eslintignore",
    ".stylelintrc",
    ".markdownlint.json",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetKind {
    File,
    Directory,
    Missing,
    Other,
}

fn lexical_resolve(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(path)
    };
    let mut resolved = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            other => resolved.push(other.as_os_str()),
        }
    }
    resolved
}

fn physical_path(target: &Path) -> Option<PathBuf> {
    let resolved = maho_tools::path_utils::realpath_without_open_strict(target).ok()?;
    if resolved
        .components()
        .any(|component| component == Component::ParentDir)
    {
        return None;
    }
    Some(resolved)
}

fn is_hidden(segment: &std::ffi::OsStr) -> bool {
    segment.to_string_lossy().starts_with('.')
}

/// The project root `auto` works in, or `None` when the session root is `/`, the home directory,
/// any ancestor of it (`/Users`, `/home`), or inside a hidden directory (`~/.config`).
fn project_root(cwd: &Path) -> Option<PathBuf> {
    let root = physical_path(&lexical_resolve(cwd))?;
    let home = physical_path(&maho_core::config::home_dir_path())?;
    let root_contains_home = home.starts_with(&root);
    let hidden_root = root.components().any(|component| match component {
        Component::Normal(segment) => is_hidden(segment),
        _ => false,
    });
    let filesystem_root = root.parent().is_none();
    if filesystem_root || root_contains_home || hidden_root {
        None
    } else {
        Some(root)
    }
}

/// Whether something, even a dangling symlink, exists at `target` itself; git looks at the name.
pub fn exists_at_name(target: &Path) -> bool {
    match std::fs::symlink_metadata(target) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        // A name the filesystem refuses (too long, a loop, no permission) is not a plain ref either:
        // report it as existing so the caller checks it as a path, and that check asks.
        Err(_) => true,
    }
}

/// Whether `cwd` is a root `auto` approves anything in (see `project_root`).
pub fn is_project_session(cwd: &Path) -> bool {
    project_root(cwd).is_some()
}

/// What is at `target` after following every symlink, without opening anything.
pub fn target_kind(target: &Path) -> TargetKind {
    let Some(physical) = physical_path(target) else {
        return TargetKind::Other;
    };
    match std::fs::symlink_metadata(&physical) {
        Ok(stats) if stats.is_file() => TargetKind::File,
        Ok(stats) if stats.is_dir() => TargetKind::Directory,
        Ok(_) => TargetKind::Other,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => TargetKind::Missing,
        Err(_) => TargetKind::Other,
    }
}

/// Whether `auto` may touch the absolute path `target` that a tool resolved: after following every
/// symlink it is inside the project root, no component below the root is hidden (other than a short
/// list of repository and formatter files), and nothing along it is credential-shaped.
pub fn is_approvable_target(target: &Path, cwd: &Path) -> bool {
    let (Some(root), Some(physical)) = (project_root(cwd), physical_path(target)) else {
        return false;
    };
    let Ok(below) = physical.strip_prefix(&root) else {
        return false;
    };
    if below.as_os_str().is_empty() {
        return true;
    }
    let hidden = below.components().any(|component| match component {
        Component::Normal(segment) => {
            is_hidden(segment) && !SAFE_HIDDEN_NAMES.iter().any(|name| segment == *name)
        }
        _ => true,
    });
    if hidden {
        return false;
    }
    !is_credential_path(&physical.to_string_lossy())
        && !is_credential_path(&target.to_string_lossy())
}
