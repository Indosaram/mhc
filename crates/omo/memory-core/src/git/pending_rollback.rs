use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::errors::GitError;
use super::repo::GitMemoryRepo;

const PENDING_ROLLBACK: &str = "omo-pending-rollback.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingRollback {
    pub head: String,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingRollbackEditsKeptError {
    pub paths: Vec<String>,
}

impl fmt::Display for PendingRollbackEditsKeptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "An earlier memory write that failed behind a git lock was unstaged, but {} changed after that failure, so the current file contents were kept as they are. Commit or discard those edits, then retry the memory write.",
            self.paths.join(", ")
        )
    }
}

impl std::error::Error for PendingRollbackEditsKeptError {}

fn marker_path(dir: &Path) -> PathBuf {
    dir.join(".git").join(PENDING_ROLLBACK)
}

pub fn record_pending_rollback(dir: &Path, rollback: &PendingRollback) -> Result<(), GitError> {
    let serialized = serde_json::to_string(rollback)
        .map_err(|err| GitError::Other(err.to_string()))?;
    crate::fs::write(&marker_path(dir), serialized.as_bytes()).map_err(GitError::Io)
}

fn parse_pending(raw: &str) -> Option<PendingRollback> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let obj = value.as_object()?;
    let head = obj.get("head")?.as_str()?.to_string();
    let paths_arr = obj.get("paths")?.as_array()?;
    let mut paths = Vec::with_capacity(paths_arr.len());
    for entry in paths_arr {
        paths.push(entry.as_str()?.to_string());
    }
    Some(PendingRollback { head, paths })
}

fn remove_marker_quietly(dir: &Path) {
    let _ = crate::fs::remove_file(&marker_path(dir));
}

pub fn settle_pending_rollback(repo: &GitMemoryRepo) -> Result<(), GitError> {
    let Ok(raw) = crate::fs::read_to_string(&marker_path(&repo.dir)) else {
        return Ok(());
    };
    if crate::fs::exists(&repo.dir.join(".git").join("index.lock")) {
        return Ok(());
    }
    let Some(pending) = parse_pending(&raw) else {
        remove_marker_quietly(&repo.dir);
        return Ok(());
    };
    if pending.paths.is_empty() || repo.head()?.as_deref() != Some(pending.head.as_str()) {
        remove_marker_quietly(&repo.dir);
        return Ok(());
    }
    let at_head: BTreeSet<String> = repo.ls_tree(Some("HEAD"), None)?.into_iter().collect();
    let tracked: Vec<String> = pending
        .paths
        .iter()
        .filter(|path| at_head.contains(*path))
        .cloned()
        .collect();
    let untracked: Vec<String> = pending
        .paths
        .iter()
        .filter(|path| !at_head.contains(*path))
        .cloned()
        .collect();
    repo.unstage(&tracked, &untracked)?;
    remove_marker_quietly(&repo.dir);
    if !repo.status(&pending.paths)?.trim().is_empty() {
        return Err(GitError::PendingRollbackEditsKept(
            PendingRollbackEditsKeptError {
                paths: pending.paths,
            },
        ));
    }
    Ok(())
}
