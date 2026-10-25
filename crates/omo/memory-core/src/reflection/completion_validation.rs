//! Validation of reflection branch commits, confinement, and clean tree invariants.

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::agreements::AGREEMENTS_PATH;
use crate::git::exec::{GitExec, GitExecResult};
use crate::memfs::{describe_frontmatter_violation, is_memory_content_path, split_frontmatter};
use crate::sync::redact::redact_secret_like_material;

use super::completion_repair::{
    DroppedEntry, commit_count, read_dropped_trailers, squash_without_paths,
};
use super::completion_secret_scan::find_secret_like_failures;
use super::worktree::ReflectionWorktree;

/// Rounds of repair a single run may take before the gate gives up on it.
const MAX_REPAIR_ROUNDS: usize = 3;

/// Result of evaluating a completed reflection worktree prior to merging.
///
/// `dropped` lists entries the gate removed so the valid ones could land; empty when nothing was
/// dropped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CompletionValidation {
    Valid {
        #[serde(rename = "tipSha")]
        tip_sha: String,
        #[serde(rename = "changedPaths")]
        changed_paths: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        dropped: Vec<DroppedEntry>,
    },
    NoChanges {
        #[serde(rename = "tipSha")]
        tip_sha: String,
        #[serde(rename = "changedPaths")]
        changed_paths: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        dropped: Vec<DroppedEntry>,
    },
    DirtyUncommitted {
        detail: String,
    },
    Failed {
        detail: String,
    },
}

impl CompletionValidation {
    /// The entries the gate removed, for callers that only want the receipt.
    pub fn dropped(&self) -> &[DroppedEntry] {
        match self {
            Self::Valid { dropped, .. } | Self::NoChanges { dropped, .. } => dropped,
            Self::DirtyUncommitted { .. } | Self::Failed { .. } => &[],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DreamTokenBudgetValidation {
    Valid {
        total_tokens: usize,
        target_tokens: usize,
    },
    BudgetNotMet {
        total_tokens: usize,
        target_tokens: usize,
        detail: String,
    },
}

pub fn validate_dream_token_budget(
    origin: super::machine::DreamOrigin,
    total_tokens: usize,
    target_tokens: usize,
) -> DreamTokenBudgetValidation {
    if origin != super::machine::DreamOrigin::Pressure || total_tokens < target_tokens {
        DreamTokenBudgetValidation::Valid {
            total_tokens,
            target_tokens,
        }
    } else {
        DreamTokenBudgetValidation::BudgetNotMet {
            total_tokens,
            target_tokens,
            detail: format!(
                "Committed system/ estimate is {total_tokens} tokens; pressure dream target is below {target_tokens} tokens"
            ),
        }
    }
}

/// Validates worktree invariants, git administrative snapshots, and branch commits.
pub fn validate_completion(
    worktree: &ReflectionWorktree,
    recorded_base: &str,
    exec: &dyn GitExec,
) -> CompletionValidation {
    match validate_completion_inner(worktree, recorded_base, exec) {
        Ok(validation) => validation,
        Err(detail) => CompletionValidation::Failed { detail },
    }
}

fn validate_completion_inner(
    worktree: &ReflectionWorktree,
    recorded_base: &str,
    exec: &dyn GitExec,
) -> Result<CompletionValidation, String> {
    let git_file = std::fs::read_to_string(&worktree.git_file_path)
        .map_err(|err| format!("Failed to read git administrative file: {err}"))?;

    let common_config = std::fs::read_to_string(&worktree.common_config_path).ok();
    if git_file != worktree.git_file_snapshot || common_config != worktree.common_config_snapshot {
        return Ok(CompletionValidation::Failed {
            detail: "Git administration files were modified".to_string(),
        });
    }

    let status = git_must(exec, &worktree.dir, &["status", "--porcelain"])?;
    if !status.stdout.trim().is_empty() {
        return Ok(CompletionValidation::DirtyUncommitted {
            detail: status.stdout,
        });
    }

    let tip = git_must(exec, &worktree.dir, &["rev-parse", "--verify", "HEAD"])?;
    let tip_sha = tip.stdout.trim().to_string();
    if tip_sha.is_empty() {
        return Ok(CompletionValidation::Failed {
            detail: "Reflection branch has no HEAD commit".to_string(),
        });
    }

    let based = exec
        .run_in(
            &worktree.dir,
            &["merge-base", "--is-ancestor", recorded_base, &tip_sha],
        )
        .map_err(|err| err.to_string())?;
    if based.code != 0 {
        return Ok(CompletionValidation::Failed {
            detail: "Reflection branch is not based on the recorded launch SHA".to_string(),
        });
    }

    if tip_sha == recorded_base {
        return Ok(CompletionValidation::NoChanges {
            tip_sha,
            changed_paths: Vec::new(),
            dropped: Vec::new(),
        });
    }

    screen_entries(exec, worktree, recorded_base, &tip_sha)
}

/// One entry that fails validation must not discard the run's valid entries.
///
/// This REWRITES the run's branch when an entry is dropped (`reset --soft` to the base, then one
/// commit without the dropped paths), also when a caller only probes eligibility, and also on a
/// later failure: a run that then fails is discarded, so the rewrite is never merged. The squashed
/// commit carries one `Omo-Completion-Dropped` trailer per dropped entry, so validating the same tip
/// again reports them. Each round collects every per-entry failure (secret-like content or name,
/// invalid frontmatter); when there are any, the branch is rewritten without those entries and
/// screened again. Structural refusals (a path escaping the repository, no changed paths) still fail
/// the whole run.
fn screen_entries(
    exec: &dyn GitExec,
    worktree: &ReflectionWorktree,
    recorded_base: &str,
    start_tip: &str,
) -> Result<CompletionValidation, String> {
    let dir = &worktree.dir;
    let mut dropped: Vec<DroppedEntry> = Vec::new();
    let mut tip_sha = start_tip.to_string();
    let mut round = 0usize;
    loop {
        if tip_sha == recorded_base {
            return Ok(CompletionValidation::NoChanges {
                tip_sha,
                changed_paths: Vec::new(),
                dropped,
            });
        }
        let changed_paths = list_reflection_changed_paths(exec, dir, recorded_base, &tip_sha)?;
        for path in &changed_paths {
            if !is_confined_repo_path(&worktree.parent.dir, path)
                || path.split('/').any(|segment| segment == ".git")
            {
                return Ok(CompletionValidation::Failed {
                    detail: format!("Changed path escapes the memory repository: {path}"),
                });
            }
        }
        // Rename detection is off here on purpose: `git mv system/agreements.md elsewhere` must read
        // as a deletion.
        let range = format!("{recorded_base}..{tip_sha}");
        let ledger = git_must(
            exec,
            dir,
            &[
                "diff",
                "--no-renames",
                "--name-only",
                "-z",
                range.as_str(),
                "--",
                AGREEMENTS_PATH,
            ],
        )?;
        if ledger.stdout.split('\0').any(|path| !path.is_empty()) {
            return Ok(CompletionValidation::Failed {
                detail: format!(
                    "Reflection may not modify {AGREEMENTS_PATH}; agreements change only through the facts lane and /agreements"
                ),
            });
        }
        let secrets = find_secret_like_failures(exec, dir, recorded_base, &tip_sha)?;
        let frontmatter =
            find_frontmatter_failures(exec, dir, recorded_base, &tip_sha, &changed_paths)?;
        if secrets.is_empty() && frontmatter.is_empty() && changed_paths.is_empty() {
            return Ok(CompletionValidation::Failed {
                detail: "Reflection commits contain no changed paths".to_string(),
            });
        }
        if secrets.is_empty() && frontmatter.is_empty() {
            if dropped.is_empty() {
                dropped = read_dropped_trailers(exec, dir, recorded_base, &tip_sha)?;
            }
            return Ok(CompletionValidation::Valid {
                tip_sha,
                changed_paths,
                dropped,
            });
        }
        if round >= MAX_REPAIR_ROUNDS {
            let detail = secrets
                .first()
                .map(|failure| failure.detail.clone())
                .or_else(|| frontmatter.first().map(|(_, reason)| reason.clone()))
                .unwrap_or_else(|| "completion validation failed".to_string());
            return Ok(CompletionValidation::Failed { detail });
        }
        // A secret in an earlier commit may be gone from the tip, so a multi-commit branch is first
        // squashed with only the tip-level frontmatter failures removed; the squashed commit is then
        // scanned and whatever still fails is a real entry to drop.
        let multi_commit = commit_count(exec, dir, recorded_base, &tip_sha)? > 1;
        let mut now = frontmatter;
        if !multi_commit {
            for failure in &secrets {
                let position = now.iter().position(|(path, _)| path == &failure.path);
                match position {
                    Some(index) => now[index].1 = failure.detail.clone(),
                    None => now.push((failure.path.clone(), failure.detail.clone())),
                }
            }
        }
        for (path, reason) in &now {
            if !dropped.iter().any(|entry| &entry.path == path) {
                dropped.push(DroppedEntry {
                    path: path.clone(),
                    reason: reason.clone(),
                });
            }
        }
        let now_paths: BTreeSet<String> = now.into_iter().map(|(path, _)| path).collect();
        tip_sha = squash_without_paths(
            exec,
            dir,
            recorded_base,
            &tip_sha,
            &now_paths,
            &dropped,
        )?;
        round += 1;
    }
}

/// Every path the run's commits changed between `base_sha` and `tip_sha`.
pub fn list_reflection_changed_paths(
    exec: &dyn GitExec,
    dir: &Path,
    base_sha: &str,
    tip_sha: &str,
) -> Result<Vec<String>, String> {
    let range = format!("{base_sha}..{tip_sha}");
    let changed = git_must(exec, dir, &["diff", "--name-only", "-z", range.as_str(), "--"])?;
    Ok(changed
        .stdout
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(String::from)
        .collect())
}

/// A reflection may not introduce or alter frontmatter that a strict YAML consumer rejects. Legacy
/// files whose frontmatter block is byte-identical to the base are the normalizer's job, not the
/// reflection's, so a body-only edit of one still passes.
fn find_frontmatter_failures(
    exec: &dyn GitExec,
    dir: &Path,
    base_sha: &str,
    tip_sha: &str,
    changed_paths: &[String],
) -> Result<Vec<(String, String)>, String> {
    let mut failures = Vec::new();
    for path in changed_paths {
        if !is_memory_content_path(path) {
            continue;
        }
        let Some(tip_content) = show_optional(exec, dir, tip_sha, path)? else {
            continue;
        };
        let Some(violation) = describe_frontmatter_violation(&tip_content) else {
            continue;
        };
        if let Some(base_content) = show_optional(exec, dir, base_sha, path)?
            && frontmatter_block(&base_content) == frontmatter_block(&tip_content)
        {
            continue;
        }
        failures.push((
            path.clone(),
            format!("invalid frontmatter: {}", redact_secret_like_material(&violation)),
        ));
    }
    Ok(failures)
}

/// The frontmatter header text of `content`, CRLF-normalized, or `None` when it has none.
pub fn frontmatter_block(content: &str) -> Option<String> {
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    split_frontmatter(&normalized).map(|(header, _)| header)
}

/// Reads one blob, or `None` when the path is absent at that revision. `git show rev:path` would
/// fall back to reading a missing glob-named path (`k*.md`, `k[x].md`) as a pathspec and exit 0 with
/// empty output; `cat-file blob` takes only an object name, so a missing path is always a failure,
/// never a pattern.
fn show_optional(
    exec: &dyn GitExec,
    dir: &Path,
    revision: &str,
    path: &str,
) -> Result<Option<String>, String> {
    let spec = format!("{revision}:{path}");
    match exec.run_in(dir, &["cat-file", "blob", spec.as_str()]) {
        Ok(result) if result.code == 0 => Ok(Some(result.stdout)),
        Ok(_) => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

fn git_must(exec: &dyn GitExec, dir: &Path, args: &[&str]) -> Result<GitExecResult, String> {
    let result = exec.run_in(dir, args).map_err(|error| error.to_string())?;
    if result.code != 0 {
        let stderr = result.stderr.trim();
        return Err(if stderr.is_empty() {
            format!("git {} failed", args.join(" "))
        } else {
            stderr.to_string()
        });
    }
    Ok(result)
}

fn is_confined_repo_path(repo_dir: &Path, relative_path: &str) -> bool {
    if relative_path.is_empty() || relative_path.contains('\0') {
        return false;
    }
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
