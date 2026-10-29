//! Refuses memory-tool writes that break the agreements ledger rules.

use std::collections::BTreeSet;

use crate::agreements::{
    AGREEMENTS_PATH, describe_agreements_tool_violation, read_agreements_at,
    read_agreements_worktree,
};
use crate::git::{GitError, GitMemoryRepo, normalize_pathspecs};

/// The reason `paths` (already written to the worktree) break the ledger rules, if they do.
/// A refused write is rolled back to HEAD so it leaves no trace behind.
pub fn refuse_agreements_violation(
    repo: &GitMemoryRepo,
    paths: &[String],
) -> Result<Option<String>, GitError> {
    if !paths.iter().any(|path| path == AGREEMENTS_PATH) {
        return Ok(None);
    }
    let head = repo.head()?;
    let before = match head {
        Some(head) => read_agreements_at(repo, &head)?,
        None => None,
    };
    let after = read_agreements_worktree(&repo.dir).map_err(GitError::Io)?;
    let violation = describe_agreements_tool_violation(before.as_deref(), after.as_deref());
    if violation.is_some() {
        restore_paths(repo, paths)?;
    }
    Ok(violation)
}

/// Rolls index and worktree back to HEAD for `paths`: tracked paths are restored on both sides,
/// paths new at HEAD are unstaged and removed.
fn restore_paths(repo: &GitMemoryRepo, paths: &[String]) -> Result<(), GitError> {
    let normalized = normalize_pathspecs(paths);
    if normalized.is_empty() {
        return Ok(());
    }
    let at_head: BTreeSet<String> = repo.ls_tree(Some("HEAD"), None)?.into_iter().collect();
    let (tracked, untracked): (Vec<String>, Vec<String>) = normalized
        .into_iter()
        .partition(|path| at_head.contains(path));
    let exec = repo.exec();
    let run = |args: &[&str]| -> Result<(), GitError> {
        let result = exec.run_in(&repo.dir, args).map_err(GitError::Io)?;
        if result.code == 0 {
            Ok(())
        } else {
            Err(GitError::Other(format!(
                "git {} failed: {}",
                args.join(" "),
                result.stderr.trim()
            )))
        }
    };
    if !tracked.is_empty() {
        let mut args = vec!["restore", "--source=HEAD", "--staged", "--worktree", "--"];
        args.extend(tracked.iter().map(String::as_str));
        run(&args)?;
    }
    for path in &untracked {
        run(&["rm", "-q", "--cached", "--ignore-unmatch", "--", path])?;
        match std::fs::remove_file(repo.dir.join(path)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(GitError::Io(error)),
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "agreements_write_guard_tests.rs"]
mod tests;
