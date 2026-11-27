//! Port of senpi `packages/coding-agent/src/core/linked-worktree-checkout.ts`.

use std::path::{Path, PathBuf};

use crate::paths::{PathInputOptions, resolve_path};

fn read_first_line(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let line = text.lines().next()?.trim();
    (!line.is_empty()).then(|| line.to_owned())
}

fn real_path_of(path: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(path).ok()
}

enum EntryKind {
    Directory,
    File,
}

fn entry_kind(path: &Path) -> Option<EntryKind> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.is_dir() {
        Some(EntryKind::Directory)
    } else if meta.is_file() {
        Some(EntryKind::File)
    } else {
        None
    }
}

/// The folder of the repository's main checkout that matches `cwd` inside a linked git worktree,
/// or `None` when `cwd` is not inside one (senpi#4388).
pub fn linked_worktree_main_checkout_dir_sync(cwd: &str) -> Option<String> {
    let resolved = PathBuf::from(resolve_path(cwd, cwd, &PathInputOptions::default()));
    let dir = real_path_of(&resolved).unwrap_or(resolved);
    let mut checkout_root = dir.clone();
    let marker_kind = loop {
        if let Some(kind) = entry_kind(&checkout_root.join(".git")) {
            break kind;
        }
        let parent = checkout_root.parent()?;
        if parent == checkout_root {
            return None;
        }
        checkout_root = parent.to_path_buf();
    };
    if !matches!(marker_kind, EntryKind::File) {
        return None;
    }
    let marker = checkout_root.join(".git");
    let first_line = read_first_line(&marker)?;
    let admin_target = first_line.strip_prefix("gitdir:")?.trim();
    if admin_target.is_empty() {
        return None;
    }
    let admin_dir = real_path_of(&checkout_root.join(admin_target))?;
    let common_dir_pointer = read_first_line(&admin_dir.join("commondir"))?;
    let back_pointer = read_first_line(&admin_dir.join("gitdir"))?;
    let marker_real = real_path_of(&marker)?;
    if real_path_of(&admin_dir.join(back_pointer))? != marker_real {
        return None;
    }
    let common_dir = real_path_of(&admin_dir.join(common_dir_pointer))?;
    if common_dir.file_name().and_then(|n| n.to_str()) != Some(".git") {
        return None;
    }
    if admin_dir.parent()? != common_dir.join("worktrees") {
        return None;
    }
    let main_checkout = common_dir.parent()?;
    let main_marker = main_checkout.join(".git");
    if !matches!(entry_kind(&main_marker), Some(EntryKind::Directory)) {
        return None;
    }
    if real_path_of(&main_marker)? != common_dir {
        return None;
    }
    let rel = dir.strip_prefix(&checkout_root).ok()?;
    Some(
        if rel.as_os_str().is_empty() {
            main_checkout.to_path_buf()
        } else {
            main_checkout.join(rel)
        }
        .to_string_lossy()
        .into_owned(),
    )
}

pub async fn linked_worktree_main_checkout_dir(cwd: &str) -> Option<String> {
    let cwd = cwd.to_owned();
    tokio::task::spawn_blocking(move || linked_worktree_main_checkout_dir_sync(&cwd))
        .await
        .ok()
        .flatten()
}
