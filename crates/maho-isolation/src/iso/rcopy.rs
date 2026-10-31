//! Cross-platform fallback isolation: git worktree, or plain recursive copy. Port of `pi-iso`
//! `src/rcopy.rs` as a dispatch entry: the worktree/copy implementation itself lives in
//! [`crate::backend`] (seeded `git worktree add --detach`) and [`crate::tree`] (recursive copy
//! keeping modes and mtimes), so this backend only routes `lower` to the right one.

use std::path::Path;

use crate::iso::{BackendKind, IsoError, IsoResult, IsolationBackend, ProbeResult};

pub struct RcopyBackend;

impl IsolationBackend for RcopyBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Rcopy
    }

    fn probe(&self) -> ProbeResult {
        // Pure-stdlib fallback path is always available; the git branch reports a clear error
        // when `lower` is a git tree but git cannot run.
        ProbeResult::available()
    }

    fn start(&self, lower: &Path, merged: &Path) -> IsoResult<()> {
        let kind = if is_git_worktree(lower) {
            crate::backend::BackendKind::Worktree
        } else {
            crate::backend::BackendKind::Copy
        };
        crate::backend::start(kind, lower, merged).map_err(IsoError::other)
    }

    fn stop(&self, merged: &Path) -> IsoResult<()> {
        // Best effort: a registered worktree is removed through git so the parent's worktree list
        // stays consistent; otherwise (and afterwards) the tree is removed.
        if is_git_worktree(merged) {
            let _ = maho_git::Git::new(merged).run(&["worktree", "remove", "--force", "."]);
        }
        crate::tree::remove_existing(merged)
            .map_err(|err| IsoError::other(format!("unable to remove {}: {err}", merged.display())))
    }
}

fn is_git_worktree(path: &Path) -> bool {
    // A regular working tree has `.git` as a dir; a linked worktree has it as a `gitdir: ...`
    // text file. Either way, presence of `.git` is the signal git itself uses.
    std::fs::symlink_metadata(path.join(".git")).is_ok()
}
