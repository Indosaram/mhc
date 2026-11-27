//! Rollback of partial/abandoned worktrees (`thread/worktree/discard-worktree.ts`).

use std::path::{Component, Path, PathBuf};

use serde_json::{Map, Value};

use super::super::contracts_results::ThreadDataError;
use super::failures::{FolderFailureReason, folder_failure};
use super::thread_worktree::GitRun;

fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

fn inside(root: &Path, path: &Path) -> bool {
    let r = normalize_lexical(root);
    let p = normalize_lexical(path);
    p != r && p.starts_with(&r)
}

/// Whether `path` lies inside the worktrees `root` lexically and by canonical path.
pub fn inside_root(root: &Path, path: &Path) -> bool {
    if !path.is_absolute() || !inside(root, path) {
        return false;
    }
    let real_root = std::fs::canonicalize(root).unwrap_or_else(|_| normalize_lexical(root));
    if !path.exists() {
        return true;
    }
    match std::fs::canonicalize(path) {
        Ok(real_path) => inside(&real_root, &real_path),
        Err(_) => false,
    }
}

pub struct DiscardWorktreeOptions<'a> {
    pub repository: &'a Path,
    pub path: &'a Path,
    pub branch: &'a str,
    pub root: &'a Path,
    pub git: &'a GitRun,
    pub created_folder: Option<&'a Path>,
    pub remove_unregistered_folder: bool,
}

/// Undoes a worktree created by this call without touching locked or unrelated worktrees.
pub async fn discard_worktree(options: DiscardWorktreeOptions<'_>) -> Option<ThreadDataError> {
    let path_str = options.path.to_string_lossy().into_owned();
    if !inside_root(options.root, options.path) {
        let mut details = Map::new();
        details.insert("path".into(), Value::String(path_str.clone()));
        details.insert("branch".into(), Value::String(options.branch.to_owned()));
        return Some(folder_failure(
            FolderFailureReason::WorktreeLeftBehind,
            format!("{path_str} is outside OmO's worktrees folder; it was not removed."),
            Some(details),
        ));
    }

    let registered = || async {
        let list = (options.git)(
            &["worktree".into(), "list".into(), "--porcelain".into()],
            options.repository,
        )
        .await;
        if list.exit != Some(0) {
            return true;
        }
        let target =
            std::fs::canonicalize(options.path).unwrap_or_else(|_| normalize_lexical(options.path));
        list.stdout
            .lines()
            .filter_map(|line| line.strip_prefix("worktree "))
            .any(|p| {
                let cand =
                    std::fs::canonicalize(p).unwrap_or_else(|_| normalize_lexical(Path::new(p)));
                cand == target
            })
    };

    if options.path.exists() {
        let _ = (options.git)(
            &[
                "worktree".into(),
                "remove".into(),
                "--force".into(),
                path_str.clone(),
            ],
            options.repository,
        )
        .await;
    }
    if options.remove_unregistered_folder && options.path.exists() && !registered().await {
        let _ = std::fs::remove_dir_all(options.path);
    }
    let _ = (options.git)(&["worktree".into(), "prune".into()], options.repository).await;

    let has_branch = || async {
        (options.git)(
            &[
                "show-ref".into(),
                "--verify".into(),
                "--quiet".into(),
                format!("refs/heads/{}", options.branch),
            ],
            options.repository,
        )
        .await
        .exit
            == Some(0)
    };
    if has_branch().await && !registered().await {
        let _ = (options.git)(
            &["branch".into(), "-D".into(), options.branch.to_owned()],
            options.repository,
        )
        .await;
    }
    if let Some(created) = options.created_folder {
        let _ = std::fs::remove_dir(created);
    }

    let left_folder = options.path.exists();
    let left_branch = has_branch().await;
    if !left_folder && !left_branch {
        return None;
    }
    let mut details = Map::new();
    details.insert("path".into(), Value::String(path_str));
    details.insert("branch".into(), Value::String(options.branch.to_owned()));
    details.insert(
        "repository".into(),
        Value::String(options.repository.to_string_lossy().into_owned()),
    );
    details.insert(
        "left".into(),
        serde_json::json!({ "folder": left_folder, "branch": left_branch }),
    );
    Some(folder_failure(
        FolderFailureReason::WorktreeLeftBehind,
        "The worktree this call created could not be fully removed.",
        Some(details),
    ))
}
