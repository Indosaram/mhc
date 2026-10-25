//! Git branch integration, merge conflict detection, and worktree cleanup.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::git::exec::GitExec;

use super::completion_repair::STABLE_LOG_OUTPUT;
use super::machine::ReflectionOutcome;
use super::worktree::{ReflectionCleanupReceipt, ReflectionWorktree, discard_reflection_worktree};

/// Integration mode: automatic background merge or explicit parent verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReflectionIntegrationMode {
    Auto,
    Integration,
}

/// Validated tip SHA and files changed by a completed reflection run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidatedReflectionTip {
    pub tip_sha: String,
    pub changed_paths: Vec<String>,
}

/// Parameters for integrating a validated reflection branch into the parent repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrateValidatedReflectionInput {
    pub mode: ReflectionIntegrationMode,
    pub run_id: String,
    pub summary: String,
    pub validated: ValidatedReflectionTip,
}

/// Result of attempting to merge a reflection branch into the parent repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReflectionIntegrationResult {
    pub outcome: ReflectionOutcome,
    pub integration_sha: Option<String>,
    pub detail: Option<String>,
}

/// Status returned when probing legacy tool receipts for a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LegacyAutoRunReceiptProbe {
    Incomplete,
    AlreadyRecorded { integration_sha: Option<String> },
}

/// Probes whether a run ID was already recorded in legacy tool receipts.
pub fn probe_legacy_auto_run_receipt(
    worktree: &ReflectionWorktree,
    run_id: &str,
) -> LegacyAutoRunReceiptProbe {
    match find_exact_run_receipt(worktree, run_id, None) {
        Some(sha) => LegacyAutoRunReceiptProbe::AlreadyRecorded {
            integration_sha: Some(sha),
        },
        None => LegacyAutoRunReceiptProbe::Incomplete,
    }
}

/// Probes whether a branch tip or run ID has already landed in parent HEAD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReflectionIntegrationProbe {
    NotIntegrated,
    AlreadyMerged { integration_sha: String },
}

/// Probes parent git history to detect if reflection commits are already integrated.
pub fn probe_reflection_integration(
    worktree: &ReflectionWorktree,
    mode: ReflectionIntegrationMode,
    run_id: &str,
    tip_sha: &str,
) -> ReflectionIntegrationProbe {
    if mode == ReflectionIntegrationMode::Auto
        && let Some(receipt) = find_exact_run_receipt(worktree, run_id, Some(tip_sha))
    {
        return ReflectionIntegrationProbe::AlreadyMerged {
            integration_sha: receipt,
        };
    }

    let exec = worktree.exec.as_ref();
    let parent_dir = &worktree.parent.dir;

    if is_ancestor(exec, parent_dir, tip_sha, "HEAD") {
        let head = exec
            .run_in(parent_dir, &["rev-parse", "--verify", "HEAD"])
            .map(|r| r.stdout.trim().to_string())
            .unwrap_or_default();
        return ReflectionIntegrationProbe::AlreadyMerged {
            integration_sha: head,
        };
    }

    ReflectionIntegrationProbe::NotIntegrated
}

fn find_exact_run_receipt(
    worktree: &ReflectionWorktree,
    run_id: &str,
    validated_tip_sha: Option<&str>,
) -> Option<String> {
    let exec = worktree.exec.as_ref();
    let parent_dir = &worktree.parent.dir;
    let rev_range = format!("{}..HEAD", worktree.base_sha());
    let commits_res = exec.run_in(parent_dir, &["rev-list", &rev_range]).ok()?;
    if commits_res.code != 0 {
        return None;
    }
    for commit in commits_res.stdout.lines().map(str::trim).filter(|c| !c.is_empty()) {
        if !has_exact_run_receipt(exec, parent_dir, commit, run_id) {
            continue;
        }
        if validated_tip_sha.is_none()
            || validated_tip_sha.is_some_and(|tip| is_ancestor(exec, parent_dir, tip, commit))
        {
            return Some(commit.to_string());
        }
    }
    None
}

fn has_exact_run_receipt(exec: &dyn GitExec, cwd: &Path, commit: &str, run_id: &str) -> bool {
    // A user's `log.showSignature` puts gpg/ssh verification lines ahead of the parsed trailers and
    // `log.follow` turns a single-path log into `--follow`, so both are pinned off for this read.
    let mut argv: Vec<&str> = STABLE_LOG_OUTPUT.to_vec();
    argv.extend([
        "show",
        "-s",
        "--format=%(trailers:key=Omo-Run,valueonly)",
        commit,
    ]);
    let trailers = exec.run_in(cwd, &argv);
    match trailers {
        Ok(res) if res.code == 0 => res.stdout.lines().any(|v| v.trim() == run_id),
        _ => false,
    }
}

fn is_ancestor(exec: &dyn GitExec, cwd: &Path, ancestor: &str, descendant: &str) -> bool {
    exec.run_in(cwd, &["merge-base", "--is-ancestor", ancestor, descendant])
        .map(|r| r.code == 0)
        .unwrap_or(false)
}

/// Integrates a validated reflection worktree branch under writer lock protection.
pub fn integrate_validated_reflection<L>(
    worktree: &ReflectionWorktree,
    input: IntegrateValidatedReflectionInput,
    with_writer_lock: L,
) -> ReflectionIntegrationResult
where
    L: FnOnce(
        &mut dyn FnMut() -> Result<ReflectionIntegrationResult, String>,
    ) -> Result<ReflectionIntegrationResult, String>,
{
    let mut operation = || -> Result<ReflectionIntegrationResult, String> {
        let probe = probe_reflection_integration(
            worktree,
            input.mode,
            &input.run_id,
            &input.validated.tip_sha,
        );
        if let ReflectionIntegrationProbe::AlreadyMerged { integration_sha } = probe {
            return Ok(ReflectionIntegrationResult {
                outcome: ReflectionOutcome::Merged,
                integration_sha: Some(integration_sha),
                detail: None,
            });
        }

        if input.mode == ReflectionIntegrationMode::Integration {
            return Ok(ReflectionIntegrationResult {
                outcome: ReflectionOutcome::Failed,
                integration_sha: None,
                detail: Some("Reflection branch tip is not reachable from parent HEAD".to_string()),
            });
        }

        let exec = worktree.exec.as_ref();
        let parent_dir = &worktree.parent.dir;

        let merge_head = optional_revision(exec, parent_dir, "MERGE_HEAD");
        if merge_head.as_deref() == Some(&input.validated.tip_sha) {
            let abort = exec.run_in(parent_dir, &["merge", "--abort"]);
            if abort.map(|r| r.code != 0).unwrap_or(true) {
                return Ok(ReflectionIntegrationResult {
                    outcome: ReflectionOutcome::Failed,
                    integration_sha: None,
                    detail: Some("Failed to abort previously interrupted merge".to_string()),
                });
            }
        } else if merge_head.is_some() {
            return Ok(ReflectionIntegrationResult {
                outcome: ReflectionOutcome::ParentDirty,
                integration_sha: None,
                detail: Some("Parent repository contains another interrupted merge".to_string()),
            });
        }

        let status = match exec.run_in(parent_dir, &["status", "--porcelain"]) {
            Ok(res) => res.stdout,
            Err(err) => {
                return Ok(ReflectionIntegrationResult {
                    outcome: ReflectionOutcome::Failed,
                    integration_sha: None,
                    detail: Some(format!("Failed to check parent repository status: {err}")),
                });
            }
        };

        if !status.trim().is_empty() {
            return Ok(ReflectionIntegrationResult {
                outcome: ReflectionOutcome::ParentDirty,
                integration_sha: None,
                detail: Some(status),
            });
        }

        let commit_msg = format!("merge(reflection): {}", input.summary);
        let trailer_msg = format!("Omo-Run: {}", input.run_id);
        let merge = exec.run_in(
            parent_dir,
            &[
                "merge",
                "--no-ff",
                &input.validated.tip_sha,
                "-m",
                &commit_msg,
                "-m",
                &trailer_msg,
            ],
        );

        match merge {
            Ok(res) if res.code == 0 => {
                let head = exec
                    .run_in(parent_dir, &["rev-parse", "--verify", "HEAD"])
                    .map(|r| r.stdout.trim().to_string())
                    .unwrap_or_default();
                Ok(ReflectionIntegrationResult {
                    outcome: ReflectionOutcome::Merged,
                    integration_sha: Some(head),
                    detail: None,
                })
            }
            Ok(res) => {
                let landed_merge_head = optional_revision(exec, parent_dir, "MERGE_HEAD");
                let unmerged = exec
                    .run_in(parent_dir, &["diff", "--name-only", "--diff-filter=U"])
                    .map(|r| r.stdout)
                    .unwrap_or_default();

                if landed_merge_head.as_deref() == Some(&input.validated.tip_sha)
                    || !unmerged.trim().is_empty()
                {
                    let _ = exec.run_in(parent_dir, &["merge", "--abort"]);
                    return Ok(ReflectionIntegrationResult {
                        outcome: ReflectionOutcome::MergeConflict,
                        integration_sha: None,
                        detail: Some(if !unmerged.trim().is_empty() {
                            unmerged
                        } else {
                            res.stderr
                        }),
                    });
                }

                Ok(ReflectionIntegrationResult {
                    outcome: ReflectionOutcome::Failed,
                    integration_sha: None,
                    detail: Some(res.stderr),
                })
            }
            Err(err) => Ok(ReflectionIntegrationResult {
                outcome: ReflectionOutcome::Failed,
                integration_sha: None,
                detail: Some(err.to_string()),
            }),
        }
    };

    with_writer_lock(&mut operation).unwrap_or_else(|err| ReflectionIntegrationResult {
        outcome: ReflectionOutcome::Failed,
        integration_sha: None,
        detail: Some(err),
    })
}

/// Discards reflection worktree directory and branch.
pub fn cleanup_reflection_worktree(worktree: &ReflectionWorktree) -> ReflectionCleanupReceipt {
    discard_reflection_worktree(
        &worktree.parent,
        &worktree.dir,
        &worktree.branch,
        worktree.exec.as_ref(),
    )
}

fn optional_revision(exec: &dyn GitExec, cwd: &Path, name: &str) -> Option<String> {
    let res = exec
        .run_in(cwd, &["rev-parse", "--verify", "-q", name])
        .ok()?;
    let trimmed = res.stdout.trim();
    if res.code == 0 && !trimmed.is_empty() {
        Some(trimmed.to_string())
    } else {
        None
    }
}

#[cfg(test)]
#[path = "worktree_integration_tests.rs"]
mod tests;
