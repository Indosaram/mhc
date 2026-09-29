//! Git worktree lifecycle management for isolated reflection runs.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::git::GitMemoryRepo;
use crate::git::exec::GitExec;

use super::completion_validation::validate_completion;
use super::machine::ReflectionOutcome;
use super::worktree_integration::{
    IntegrateValidatedReflectionInput, ReflectionIntegrationMode, ReflectionIntegrationResult,
    cleanup_reflection_worktree, integrate_validated_reflection,
};

/// Identity details published prior to worktree disk creation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflectionWorktreeIdentity {
    pub dir: PathBuf,
    pub branch: String,
}

/// Active reflection worktree descriptor holding checkout paths and administration snapshots.
#[derive(Clone)]
pub struct ReflectionWorktree {
    pub parent: GitMemoryRepo,
    pub dir: PathBuf,
    pub branch: String,
    pub base_commit_sha: String,
    pub git_file_path: PathBuf,
    pub git_file_snapshot: String,
    pub common_config_path: PathBuf,
    pub common_config_snapshot: Option<String>,
    pub exec: Arc<dyn GitExec>,
}

/// Confirmation of cleaned filesystem and git branch resources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflectionCleanupReceipt {
    pub worktree_removed: bool,
    pub branch_removed: bool,
}

/// Final outcome and resource cleanup receipt from worktree finalization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflectionFinalizeResult {
    pub status: ReflectionOutcome,
    pub detail: Option<String>,
    pub cleanup: ReflectionCleanupReceipt,
}

/// Finalization mode controlling auto merge with trailers or explicit integration.
pub enum ReflectionFinalizeMode {
    Auto {
        summary: String,
        run_id: Option<String>,
        allowed_paths: Option<Vec<String>>,
    },
    Explicit,
}

/// Errors raised during reflection worktree lifecycle operations.
#[derive(Debug)]
pub enum WorktreeError {
    InvalidPath(String),
    NoHead(String),
    InvalidRunId(String),
    Io(std::io::Error),
    Git(String),
}

impl fmt::Display for WorktreeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath(msg) => write!(formatter, "invalid worktree path: {msg}"),
            Self::NoHead(msg) => write!(formatter, "no parent head: {msg}"),
            Self::InvalidRunId(msg) => write!(formatter, "invalid run id: {msg}"),
            Self::Io(err) => write!(formatter, "io error: {err}"),
            Self::Git(msg) => write!(formatter, "git error: {msg}"),
        }
    }
}

impl std::error::Error for WorktreeError {}

impl From<std::io::Error> for WorktreeError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

/// Creates a new isolated git worktree branch for a reflection run.
pub fn create_reflection_worktree(
    repo: &GitMemoryRepo,
    run_id: &str,
    worktrees_dir: &Path,
    exec: &dyn GitExec,
    before_create: Option<&dyn Fn(&ReflectionWorktreeIdentity)>,
) -> Result<ReflectionWorktree, WorktreeError> {
    if !worktrees_dir.is_absolute() {
        return Err(WorktreeError::InvalidPath(
            "worktrees_dir must be absolute".to_string(),
        ));
    }

    let base_commit_sha = repo
        .head()
        .map_err(|e| WorktreeError::Git(e.to_string()))?
        .ok_or_else(|| {
            WorktreeError::NoHead(
                "Cannot create a reflection worktree without a parent HEAD".to_string(),
            )
        })?;

    let sanitized_id = sanitize_run_id(run_id)?;
    let suffix = format!("{}-{}", crate::support::time::now_millis(), sanitized_id);
    let branch = format!("memory/reflection-{suffix}");
    let dir = worktrees_dir.join(&suffix);

    let identity = ReflectionWorktreeIdentity {
        dir: dir.clone(),
        branch: branch.clone(),
    };
    if let Some(callback) = before_create {
        callback(&identity);
    }

    std::fs::create_dir_all(worktrees_dir)?;

    if let Err(err) = repo.worktree_add(&dir, &branch, Some(&base_commit_sha)) {
        let _ = discard_reflection_worktree(repo, &dir, &branch, exec);
        return Err(WorktreeError::Git(err.to_string()));
    }

    let git_file_path = dir.join(".git");
    let git_file_snapshot = match std::fs::read_to_string(&git_file_path) {
        Ok(s) => s,
        Err(err) => {
            let _ = discard_reflection_worktree(repo, &dir, &branch, exec);
            return Err(WorktreeError::Io(err));
        }
    };

    let common_dir_res = match exec.run_in(&repo.dir, &["rev-parse", "--git-common-dir"]) {
        Ok(res) => res,
        Err(err) => {
            let _ = discard_reflection_worktree(repo, &dir, &branch, exec);
            return Err(WorktreeError::Git(err.to_string()));
        }
    };
    let raw_common_dir = common_dir_res.stdout.trim();
    let common_dir = if Path::new(raw_common_dir).is_absolute() {
        PathBuf::from(raw_common_dir)
    } else {
        repo.dir.join(raw_common_dir)
    };

    let common_config_path = common_dir.join("config");
    let common_config_snapshot = std::fs::read_to_string(&common_config_path).ok();

    Ok(ReflectionWorktree {
        parent: repo.clone(),
        dir,
        branch,
        base_commit_sha,
        git_file_path,
        git_file_snapshot,
        common_config_path,
        common_config_snapshot,
        exec: repo.exec(),
    })
}

/// Idempotently removes a reflection worktree checkout and its git branch.
pub fn discard_reflection_worktree(
    repo: &GitMemoryRepo,
    dir: &Path,
    branch: &str,
    exec: &dyn GitExec,
) -> ReflectionCleanupReceipt {
    remove_worktree_and_branch(repo, dir, branch, exec)
}

/// Validates, merges, and cleans up an active reflection worktree under writer lock protection.
pub fn finalize_reflection_worktree<L>(
    worktree: &ReflectionWorktree,
    mode: ReflectionFinalizeMode,
    with_writer_lock: L,
) -> ReflectionFinalizeResult
where
    L: FnOnce(
        &mut dyn FnMut() -> Result<ReflectionIntegrationResult, String>,
    ) -> Result<ReflectionIntegrationResult, String>,
{
    let mut status = ReflectionOutcome::Failed;
    let mut detail = None;

    let validation =
        validate_completion(worktree, &worktree.base_commit_sha, worktree.exec.as_ref());
    match validation {
        super::completion_validation::CompletionValidation::DirtyUncommitted { detail: d } => {
            status = ReflectionOutcome::DirtyUncommitted;
            detail = Some(d);
        }
        super::completion_validation::CompletionValidation::Failed { detail: d } => {
            status = ReflectionOutcome::Failed;
            detail = Some(d);
        }
        super::completion_validation::CompletionValidation::NoChanges { .. } => {
            status = ReflectionOutcome::NoChanges;
        }
        super::completion_validation::CompletionValidation::Valid {
            tip_sha,
            changed_paths,
        } => {
            let mut outside_allowed = false;
            if let ReflectionFinalizeMode::Auto {
                ref allowed_paths, ..
            } = mode
                && let Some(allowed) = allowed_paths
                && changed_paths.iter().any(|p| !allowed.contains(p))
            {
                outside_allowed = true;
                status = ReflectionOutcome::Failed;
                detail = Some(format!(
                    "Dream document maintenance changed paths outside its target: {}",
                    changed_paths.join(", ")
                ));
            }

            if !outside_allowed {
                let input = match mode {
                    ReflectionFinalizeMode::Auto {
                        summary, run_id, ..
                    } => IntegrateValidatedReflectionInput {
                        mode: ReflectionIntegrationMode::Auto,
                        run_id: run_id.unwrap_or_else(|| worktree.branch.clone()),
                        summary,
                        validated: super::worktree_integration::ValidatedReflectionTip {
                            tip_sha,
                            changed_paths,
                        },
                    },
                    ReflectionFinalizeMode::Explicit => IntegrateValidatedReflectionInput {
                        mode: ReflectionIntegrationMode::Integration,
                        run_id: worktree.branch.clone(),
                        summary: "external integration".to_string(),
                        validated: super::worktree_integration::ValidatedReflectionTip {
                            tip_sha,
                            changed_paths,
                        },
                    },
                };

                let integrated = integrate_validated_reflection(worktree, input, with_writer_lock);
                status = integrated.outcome;
                detail = integrated.detail;
            }
        }
    }

    let cleanup = cleanup_reflection_worktree(worktree);
    if !cleanup.worktree_removed || !cleanup.branch_removed {
        status = ReflectionOutcome::Failed;
        detail = Some(match detail {
            Some(d) => format!("{d}; Reflection cleanup did not fully complete"),
            None => "Reflection cleanup did not fully complete".to_string(),
        });
    }

    ReflectionFinalizeResult {
        status,
        detail,
        cleanup,
    }
}

fn remove_worktree_and_branch(
    repo: &GitMemoryRepo,
    dir: &Path,
    branch: &str,
    exec: &dyn GitExec,
) -> ReflectionCleanupReceipt {
    let _ = repo.worktree_remove(dir, true);
    let _ = std::fs::remove_dir_all(dir);
    let _ = exec.run_in(&repo.dir, &["worktree", "prune"]);
    let _ = exec.run_in(&repo.dir, &["branch", "-D", branch]);

    let listed = exec
        .run_in(&repo.dir, &["worktree", "list", "--porcelain"])
        .map(|r| r.stdout)
        .unwrap_or_default();
    let canonical = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let listed_has_dir = listed.lines().any(|line| {
        line.starts_with("worktree ") && line.contains(&canonical.to_string_lossy().to_string())
    });

    let branch_ref = exec.run_in(
        &repo.dir,
        &["show-ref", "--verify", &format!("refs/heads/{branch}")],
    );
    let branch_removed = branch_ref.map(|r| r.code != 0).unwrap_or(true);

    ReflectionCleanupReceipt {
        worktree_removed: !dir.exists() && !listed_has_dir,
        branch_removed,
    }
}

fn sanitize_run_id(run_id: &str) -> Result<String, WorktreeError> {
    let trimmed = run_id.trim();
    let file_name = Path::new(trimmed)
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();

    let mut slug = String::with_capacity(file_name.len());
    let mut last_was_dash = false;
    for ch in file_name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '-' {
            slug.push(ch);
            last_was_dash = false;
        } else if !last_was_dash {
            slug.push('-');
            last_was_dash = true;
        }
    }

    let trimmed_slug = slug.trim_matches(['-', '.']);
    if trimmed_slug.is_empty() || trimmed_slug == "." || trimmed_slug == ".." {
        return Err(WorktreeError::InvalidRunId(
            "runId must contain a safe identifier".to_string(),
        ));
    }

    Ok(trimmed_slug.chars().take(80).collect())
}

#[cfg(test)]
#[path = "worktree_tests.rs"]
mod tests;
