use crate::git::{
    GitMemoryRepo, PendingRollback, StrandedGitLockError, record_pending_rollback,
    restore_worktree_from_head,
};

use super::tool_errors::MemoryToolError;

fn index_lock_held(repo: &GitMemoryRepo) -> bool {
    crate::fs::exists(&repo.dir.join(".git").join("index.lock"))
}

fn has_staged_change(repo: &GitMemoryRepo, paths: &[String]) -> Result<bool, crate::git::GitError> {
    let status = repo.status(paths)?;
    Ok(status
        .lines()
        .map(|line| line.trim_end_matches('\r'))
        .filter(|line| line.len() > 3)
        .any(|line| {
            let first = line.as_bytes()[0];
            first != b' ' && first != b'?'
        }))
}

pub fn undo_behind_kept_lock(
    repo: &GitMemoryRepo,
    paths: &[String],
    kept: &StrandedGitLockError,
    prefix: &str,
) -> MemoryToolError {
    let mut still_staged = false;
    let restore_res = (|| -> Result<(), String> {
        if !has_staged_change(repo, paths).map_err(|e| e.to_string())? {
            restore_worktree_from_head(repo, paths).map_err(|e| e.to_string())?;
        } else if !index_lock_held(repo) {
            repo.restore_paths(paths).map_err(|e| e.to_string())?;
        } else {
            restore_worktree_from_head(repo, paths).map_err(|e| e.to_string())?;
            let head = repo
                .head()
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "the memory repository has no HEAD commit".to_string())?;
            record_pending_rollback(
                &repo.dir,
                &PendingRollback {
                    head,
                    paths: paths.to_vec(),
                },
            )
            .map_err(|e| e.to_string())?;
            still_staged = true;
        }
        Ok(())
    })();

    if let Err(reason) = restore_res {
        return MemoryToolError::new(format!(
            "{prefix}: {}\nThe change this write made could not be undone ({reason}); before retrying, restore or remove: {}",
            kept.message,
            paths.join(", ")
        ));
    }
    if !still_staged {
        return MemoryToolError::new(format!("{prefix}: {}", kept.message));
    }
    MemoryToolError::new(format!(
        "{prefix}: {}\nThis write is still staged because the index itself is locked; it is unstaged automatically before the next memory write once that lock is removed.",
        kept.message
    ))
}
