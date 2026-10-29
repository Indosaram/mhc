//! Ownership validation and path-state boundary checking for facts recovery.

use std::collections::{BTreeMap, HashSet};

use serde::Serialize;

use crate::git::path_state::GitPathState;
use crate::git::{GitError, GitMemoryRepo};

use super::mutation_plan::FactsApplyRecovery;

/// Map from path to captured git path state representing verified ownership.
pub type FactsOwnedState = BTreeMap<String, GitPathState>;

/// Capture and verify repository state matching a facts recovery plan.
pub fn capture_owned_facts_state(
    repo: &GitMemoryRepo,
    recovery: &FactsApplyRecovery,
) -> Result<Option<FactsOwnedState>, GitError> {
    let head = repo.head()?;
    if head.as_deref() != Some(&recovery.head_before_apply) {
        return Ok(None);
    }
    let allowed: HashSet<&str> = recovery
        .paths
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();

    let dirty = repo.status(&[] as &[&str])?;
    for line in dirty.lines().filter(|l| !l.trim().is_empty()) {
        let Some(path) = parse_porcelain_path(line) else {
            return Ok(None);
        };
        if !allowed.contains(path) {
            return Ok(None);
        }
    }

    let paths: Vec<String> = allowed.into_iter().map(|p| p.to_string()).collect();
    let current = repo.path_state.capture_all(&paths)?;

    for entry in &recovery.paths {
        let Some(state) = current.get(&entry.path) else {
            return Ok(None);
        };
        let index_matches = same_identity(&state.index, &entry.pre.index)
            || same_identity(&state.index, &Some(entry.post.index.clone()));
        let worktree_matches = same_identity(&state.worktree, &entry.pre.worktree)
            || same_identity(&state.worktree, &entry.post.worktree);
        if !index_matches || !worktree_matches {
            return Ok(None);
        }
    }

    Ok(Some(current))
}

/// Check whether two owned state snapshots contain identical path states.
pub fn same_facts_owned_state(left: &FactsOwnedState, right: &FactsOwnedState) -> bool {
    if left.len() != right.len() {
        return false;
    }
    for (path, state) in left {
        let Some(right_state) = right.get(path) else {
            return false;
        };
        if !same_identity(state, right_state) {
            return false;
        }
    }
    true
}

/// Check JSON structural equality between two serializable values.
pub fn same_identity<T: Serialize>(left: &T, right: &T) -> bool {
    let Ok(l) = serde_json::to_string(left) else {
        return false;
    };
    let Ok(r) = serde_json::to_string(right) else {
        return false;
    };
    l == r
}

/// Extract path from a git porcelain status line.
pub fn parse_porcelain_path(line: &str) -> Option<&str> {
    if line.len() < 4 {
        return None;
    }
    let status = &line[..2];
    if status == " D" || status == "D " || status == "DD" {
        return None;
    }
    let raw_path = &line[3..];
    let path = if let Some(idx) = raw_path.rfind(" -> ") {
        &raw_path[idx + 4..]
    } else {
        raw_path
    };
    let trimmed = path.trim_matches('"');
    Some(trimmed)
}

#[cfg(test)]
#[path = "recovery_ownership_tests.rs"]
mod tests;
