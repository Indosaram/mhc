//! Git branch integration, merge conflict detection, and worktree cleanup.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::git::exec::GitExec;

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
    let receipts_path = worktree
        .parent
        .dir
        .join(".omo/runtime/reflection/tool-receipts.json");
    let raw = match std::fs::read_to_string(&receipts_path) {
        Ok(s) => s,
        Err(_) => return LegacyAutoRunReceiptProbe::Incomplete,
    };

    let parsed: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return LegacyAutoRunReceiptProbe::Incomplete,
    };

    if let Some(receipt) = parsed.get(run_id) {
        let sha = receipt
            .get("integrationSha")
            .and_then(|s| s.as_str())
            .map(String::from);
        LegacyAutoRunReceiptProbe::AlreadyRecorded {
            integration_sha: sha,
        }
    } else {
        LegacyAutoRunReceiptProbe::Incomplete
    }
}

/// Probes whether a branch tip or run ID has already landed in parent HEAD.
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
    let exec = worktree.exec.as_ref();
    let parent_dir = &worktree.parent.dir;

    let ancestor = exec.run_in(
        parent_dir,
        &["merge-base", "--is-ancestor", tip_sha, "HEAD"],
    );
    if ancestor.map(|r| r.code == 0).unwrap_or(false) {
        let head = exec
            .run_in(parent_dir, &["rev-parse", "--verify", "HEAD"])
            .map(|r| r.stdout.trim().to_string())
            .unwrap_or_default();
        return ReflectionIntegrationProbe::AlreadyMerged {
            integration_sha: head,
        };
    }

    if mode == ReflectionIntegrationMode::Auto {
        let trailer_pattern = format!("--grep=^Omo-Run: {run_id}$");
        let matches = exec.run_in(
            parent_dir,
            &["log", "-n", "1", "--format=%H", &trailer_pattern, "HEAD"],
        );
        if let Ok(m) = matches {
            let sha = m.stdout.trim().to_string();
            if !sha.is_empty() {
                return ReflectionIntegrationProbe::AlreadyMerged {
                    integration_sha: sha,
                };
            }
        }
    }

    ReflectionIntegrationProbe::NotIntegrated
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
