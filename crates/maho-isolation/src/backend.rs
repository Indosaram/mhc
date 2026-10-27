//! Materialising and removing an isolated directory. Port of the MIT oh-my-pi `pi-iso` rcopy
//! backend (`rcopy.rs`): a detached `git worktree` seeded with the source's staged, unstaged and
//! untracked state when the source is a git work tree, a recursive copy otherwise. The
//! copy-on-write backends are not ported (spec §2).
use crate::settings::BackendChoice;
use crate::tree;
use maho_git::{ApplyOptions, Git};
use std::path::Path;

/// The backend actually used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendKind {
    /// `git worktree add --detach` + seeded dirty state.
    Worktree,
    /// Recursive copy (of the whole repository, `.git` included, inside git).
    Copy,
}

impl BackendKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Worktree => "worktree",
            Self::Copy => "copy",
        }
    }
}

/// Picks the backend for `choice`; the second value explains a fallback.
///
/// # Errors
/// `copy` was asked for a linked git worktree (its `.git` is a pointer file a copy cannot carry).
pub fn resolve(
    choice: &BackendChoice,
    root: &Path,
    is_git: bool,
) -> Result<(BackendKind, Option<String>), String> {
    match choice {
        BackendChoice::Auto => Ok((
            if is_git {
                BackendKind::Worktree
            } else {
                BackendKind::Copy
            },
            None,
        )),
        BackendChoice::Unsupported(name) => Ok((
            if is_git {
                BackendKind::Worktree
            } else {
                BackendKind::Copy
            },
            Some(format!("backend `{name}` is not available in this build")),
        )),
        BackendChoice::Copy => {
            if is_git && !root.join(".git").is_dir() {
                return Err(format!(
                    "the copy backend cannot isolate {}: it is a linked git worktree (use backend `auto`)",
                    root.display()
                ));
            }
            Ok((BackendKind::Copy, None))
        }
    }
}

/// Creates `merged` from `root` with `kind`.
///
/// # Errors
/// Any git or filesystem step failed (the caller removes the partial directory).
pub fn start(kind: BackendKind, root: &Path, merged: &Path) -> Result<(), String> {
    if let Some(parent) = merged.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    tree::remove_existing(merged).map_err(|e| format!("clear {}: {e}", merged.display()))?;
    match kind {
        BackendKind::Worktree => {
            let source = Git::new(root);
            if source.head().is_none() {
                return Err(
                    "the repository has no commit yet; a worktree needs one (use backend `copy`)"
                        .into(),
                );
            }
            let target = merged.to_string_lossy().into_owned();
            source
                .run(&["worktree", "add", "--detach", &target, "HEAD"])
                .map_err(|e| e.to_string())?;
            seed_dirty_state(root, merged)
        }
        BackendKind::Copy => {
            std::fs::create_dir_all(merged)
                .map_err(|e| format!("create {}: {e}", merged.display()))?;
            tree::copy_dir_contents(root, merged, &[])
                .map_err(|e| format!("copy {}: {e}", root.display()))
        }
    }
}

/// Makes the fresh worktree at `merged` mirror `root`'s live state: staged changes into index and
/// tree, unstaged changes into the tree, untracked (non-ignored) paths copied.
fn seed_dirty_state(root: &Path, merged: &Path) -> Result<(), String> {
    let source = Git::new(root);
    let target = Git::new(merged);
    let staged = source
        .run(&["diff", "--binary", "--no-color", "--cached"])
        .map_err(|e| e.to_string())?;
    if !staged.is_empty() {
        target
            .apply(
                &staged,
                ApplyOptions {
                    index: true,
                    ..ApplyOptions::default()
                },
            )
            .map_err(|e| e.to_string())?;
    }
    let unstaged = source
        .run(&["diff", "--binary", "--no-color"])
        .map_err(|e| e.to_string())?;
    if !unstaged.is_empty() {
        target
            .apply(&unstaged, ApplyOptions::default())
            .map_err(|e| e.to_string())?;
    }
    for rel in source.untracked().map_err(|e| e.to_string())? {
        tree::copy_path(&root.join(&rel), &merged.join(&rel))
            .map_err(|e| format!("copy {rel}: {e}"))?;
    }
    Ok(())
}

/// Removes `merged` (unregistering a worktree first).
pub fn stop(kind: BackendKind, root: &Path, merged: &Path) {
    if kind == BackendKind::Worktree {
        let target = merged.to_string_lossy().into_owned();
        let _ = Git::new(root).run(&["worktree", "remove", "--force", &target]);
    }
    let _ = tree::remove_existing(merged);
    if kind == BackendKind::Worktree {
        let _ = Git::new(root).run(&["worktree", "prune"]);
    }
}
