//! Validation of reflection branch commits, confinement, and clean tree invariants.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::git::exec::GitExec;

use super::worktree::ReflectionWorktree;

/// Result of evaluating a completed reflection worktree prior to merging.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CompletionValidation {
    Valid {
        #[serde(rename = "tipSha")]
        tip_sha: String,
        #[serde(rename = "changedPaths")]
        changed_paths: Vec<String>,
    },
    NoChanges {
        #[serde(rename = "tipSha")]
        tip_sha: String,
        #[serde(rename = "changedPaths")]
        changed_paths: Vec<String>,
    },
    DirtyUncommitted {
        detail: String,
    },
    Failed {
        detail: String,
    },
}

/// Validates worktree invariants, git administrative snapshots, and branch commits.
pub fn validate_completion(
    worktree: &ReflectionWorktree,
    recorded_base: &str,
    exec: &dyn GitExec,
) -> CompletionValidation {
    let git_file = match std::fs::read_to_string(&worktree.git_file_path) {
        Ok(content) => content,
        Err(err) => {
            return CompletionValidation::Failed {
                detail: format!("Failed to read git administrative file: {err}"),
            };
        }
    };

    let common_config = std::fs::read_to_string(&worktree.common_config_path).ok();
    if git_file != worktree.git_file_snapshot || common_config != worktree.common_config_snapshot {
        return CompletionValidation::Failed {
            detail: "Git administration files were modified".to_string(),
        };
    }

    let status = match exec.run_in(&worktree.dir, &["status", "--porcelain"]) {
        Ok(res) => res,
        Err(err) => {
            return CompletionValidation::Failed {
                detail: format!("git status failed: {err}"),
            };
        }
    };
    if !status.stdout.trim().is_empty() {
        return CompletionValidation::DirtyUncommitted {
            detail: status.stdout,
        };
    }

    let tip = match exec.run_in(&worktree.dir, &["rev-parse", "--verify", "HEAD"]) {
        Ok(res) => res,
        Err(err) => {
            return CompletionValidation::Failed {
                detail: format!("git rev-parse failed: {err}"),
            };
        }
    };
    let tip_sha = tip.stdout.trim().to_string();
    if tip_sha.is_empty() {
        return CompletionValidation::Failed {
            detail: "Reflection branch has no HEAD commit".to_string(),
        };
    }

    let based = match exec.run_in(
        &worktree.dir,
        &["merge-base", "--is-ancestor", recorded_base, &tip_sha],
    ) {
        Ok(res) => res,
        Err(err) => {
            return CompletionValidation::Failed {
                detail: format!("git merge-base failed: {err}"),
            };
        }
    };
    if based.code != 0 {
        return CompletionValidation::Failed {
            detail: "Reflection branch is not based on the recorded launch SHA".to_string(),
        };
    }

    if tip_sha == recorded_base {
        return CompletionValidation::NoChanges {
            tip_sha,
            changed_paths: Vec::new(),
        };
    }

    let range = format!("{recorded_base}..{tip_sha}");
    let changed = match exec.run_in(&worktree.dir, &["diff", "--name-only", "-z", &range, "--"]) {
        Ok(res) => res,
        Err(err) => {
            return CompletionValidation::Failed {
                detail: format!("git diff failed: {err}"),
            };
        }
    };

    let changed_paths: Vec<String> = changed
        .stdout
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();

    if changed_paths.is_empty() {
        return CompletionValidation::Failed {
            detail: "Reflection commits contain no changed paths".to_string(),
        };
    }

    for path in &changed_paths {
        if !is_confined_repo_path(&worktree.parent.dir, path)
            || path.split('/').any(|seg| seg == ".git")
        {
            return CompletionValidation::Failed {
                detail: format!("Changed path escapes the memory repository: {path}"),
            };
        }
    }

    CompletionValidation::Valid {
        tip_sha,
        changed_paths,
    }
}

fn is_confined_repo_path(repo_dir: &Path, relative_path: &str) -> bool {
    let path = Path::new(relative_path);
    if path.is_absolute() {
        return false;
    }

    for component in path.components() {
        match component {
            std::path::Component::ParentDir => return false,
            std::path::Component::Prefix(_) | std::path::Component::RootDir => return false,
            _ => {}
        }
    }

    let resolved = repo_dir.join(path);
    resolved.starts_with(repo_dir)
}

#[cfg(test)]
#[path = "completion_validation_tests.rs"]
mod tests;
