//! Projected-changes ledger between two committed memory revisions.

use std::collections::BTreeSet;

use crate::git::{GitError, GitLogOptions, GitMemoryRepo};

/// Trailer naming the session a commit belongs to (pin `compile/changes.ts`).
pub const MEMORY_SESSION_TRAILER: &str = "Omo-Session";

/// Paths whose change would alter a compiled block, grouped by what happened to them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectedChanges {
    pub added: Vec<String>,
    pub updated: Vec<String>,
    pub removed: Vec<String>,
}

/// Options for a projected-changes comparison.
#[derive(Debug, Clone, Default)]
pub struct ProjectedChangesOptions {
    pub exclude_session_id: Option<String>,
}

/// True when a comparison produced no projected change at all.
pub fn is_empty_projected_changes(changes: &ProjectedChanges) -> bool {
    changes.added.is_empty() && changes.updated.is_empty() && changes.removed.is_empty()
}

/// Compares the compiled projection between `base` and `head`.
///
/// A `None` base means the repository had no commit when the caller last looked, so every commit up
/// to `head` counts.
pub fn projected_changes_between(
    repo: &GitMemoryRepo,
    base: Option<&str>,
    head: Option<&str>,
    options: &ProjectedChangesOptions,
) -> Result<ProjectedChanges, GitError> {
    let Some(head) = head else {
        return Ok(ProjectedChanges::default());
    };
    if base == Some(head) {
        return Ok(ProjectedChanges::default());
    }

    let range = match base {
        None => head.to_string(),
        Some(base) => format!("{base}..{head}"),
    };
    let commits = repo.log(Some(&GitLogOptions {
        range: Some(range),
        include_paths: true,
        ..Default::default()
    }))?;

    let mut touched: BTreeSet<String> = BTreeSet::new();
    for commit in &commits {
        if let Some(exclude) = &options.exclude_session_id
            && commit.trailers.get(MEMORY_SESSION_TRAILER) == Some(exclude)
        {
            continue;
        }
        for path in commit.paths.iter().flatten() {
            if is_projected_path(path) {
                touched.insert(path.clone());
            }
        }
    }
    if touched.is_empty() {
        return Ok(ProjectedChanges::default());
    }

    let before = match base {
        None => Vec::new(),
        Some(base) => repo.ls_tree(Some(base), None)?,
    };
    let after = repo.ls_tree(Some(head), None)?;
    let before_set: BTreeSet<&String> = before.iter().collect();
    let after_set: BTreeSet<&String> = after.iter().collect();

    let mut changes = ProjectedChanges::default();
    for path in &touched {
        let existed_before = before_set.contains(path);
        let exists_after = after_set.contains(path);
        if !existed_before && exists_after {
            changes.added.push(path.clone());
        } else if existed_before && !exists_after {
            changes.removed.push(path.clone());
        } else if existed_before && exists_after && is_system_body_path(path) {
            changes.updated.push(path.clone());
        }
    }
    Ok(changes)
}

/// Whether git still resolves `revision` to a commit; a history rewrite can take a pinned one away.
pub fn revision_exists(repo: &GitMemoryRepo, revision: &str) -> Result<bool, GitError> {
    let options = GitLogOptions {
        range: Some(revision.to_string()),
        limit: Some(1),
        ..Default::default()
    };
    match repo.log(Some(&options)) {
        Ok(_) => Ok(true),
        Err(GitError::Command { .. }) => Ok(false),
        Err(error) => Err(error),
    }
}

fn is_projected_path(path: &str) -> bool {
    if path.starts_with("skills/") {
        return false;
    }
    !path.starts_with("system/") || is_system_body_path(path)
}

fn is_system_body_path(path: &str) -> bool {
    path.starts_with("system/") && path.ends_with(".md")
}

#[cfg(test)]
#[path = "changes_tests.rs"]
mod tests;
