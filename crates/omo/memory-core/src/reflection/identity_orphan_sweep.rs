//! Orphan reflection worktree and branch sweep for one identity from outside a session (#12517).

use std::collections::BTreeSet;
use std::path::Path;

use crate::git::GitMemoryRepo;

use super::orphan_sweep::{
    REFLECTION_ORPHAN_GRACE_MS, ReflectionOrphanSelection, ReflectionOrphanSweepOptions,
    list_reflection_leftovers, select_reflection_orphans, sweep_reflection_orphans,
};

const TERMINAL_SENTINELS: &[&str] = &["final.json", "abandoned.json", "quarantined.json"];

fn reserved_run_id(path: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let val: serde_json::Value = serde_json::from_str(&raw).ok()?;
    val.get("runId")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

/// Collects run ids that still own a worktree or branch: reserved runs plus run dirs with a
/// ledger or prelaunch file and no terminal sentinel.
pub fn collect_reflection_live_run_ids(reflection_dir: &Path) -> BTreeSet<String> {
    let mut live = BTreeSet::new();
    for name in ["active.lock", "pending.json"] {
        if let Some(run_id) = reserved_run_id(&reflection_dir.join(name)) {
            live.insert(run_id);
        }
    }
    let runs_dir = reflection_dir.join("runs");
    if let Ok(entries) = std::fs::read_dir(&runs_dir) {
        for entry in entries.filter_map(Result::ok) {
            let Ok(run_id) = entry.file_name().into_string() else {
                continue;
            };
            if run_id.starts_with('.') {
                continue;
            }
            let run_dir = runs_dir.join(&run_id);
            if TERMINAL_SENTINELS
                .iter()
                .any(|sentinel| run_dir.join(sentinel).exists())
            {
                continue;
            }
            if run_dir.join("ledger.json").exists() || run_dir.join("prelaunch.json").exists() {
                live.insert(run_id);
            }
        }
    }
    live
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptOrphanTarget {
    pub target: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedOrphanTarget {
    pub target: String,
    pub detail: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IdentityOrphanSweepReport {
    pub skipped: Option<&'static str>,
    pub removed: Vec<String>,
    pub kept: Vec<KeptOrphanTarget>,
    pub failed: Vec<FailedOrphanTarget>,
}

/// Orphan worktree and branch sweep for one identity from outside a session (`doctor --fix`, DESIGN.r2 6b).
pub fn sweep_identity_orphan_worktrees(
    identity_root: &Path,
    now_ms: i64,
    dry_run: bool,
) -> Result<IdentityOrphanSweepReport, String> {
    let repo_dir = identity_root.join("repo");
    if !repo_dir.join(".git").exists() {
        return Ok(IdentityOrphanSweepReport {
            skipped: Some("no-repo"),
            removed: Vec::new(),
            kept: Vec::new(),
            failed: Vec::new(),
        });
    }
    let runtime_dir = identity_root.join("runtime");
    let worktrees_dir = runtime_dir.join("worktrees");
    let agent_id = identity_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("default");
    let repo = GitMemoryRepo::open(&repo_dir, agent_id).map_err(|e| e.to_string())?;
    let exec = repo.exec();
    let live_run_ids = collect_reflection_live_run_ids(&runtime_dir.join("reflection"));
    let selection = ReflectionOrphanSelection {
        live_run_ids: live_run_ids.clone(),
        now_ms: now_ms as f64,
        grace_ms: Some(REFLECTION_ORPHAN_GRACE_MS),
    };
    let leftovers = list_reflection_leftovers(&repo, &worktrees_dir, exec.as_ref())?;
    let orphans = select_reflection_orphans(&leftovers, &selection);

    let mut orphan_targets = BTreeSet::new();
    for wt in &orphans.registered_worktrees {
        orphan_targets.insert(wt.dir.to_string_lossy().into_owned());
    }
    for dir in &orphans.stray_dirs {
        orphan_targets.insert(dir.to_string_lossy().into_owned());
    }
    for branch in &orphans.branches {
        orphan_targets.insert(branch.clone());
    }

    let kept_reason = |target: &str| -> String {
        let base = Path::new(target)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(target);
        if live_run_ids.iter().any(|run_id| base.ends_with(run_id)) {
            "live".to_owned()
        } else {
            "grace".to_owned()
        }
    };

    let all_targets = leftovers
        .registered_worktrees
        .iter()
        .map(|wt| wt.dir.to_string_lossy().into_owned())
        .chain(
            leftovers
                .stray_dirs
                .iter()
                .map(|dir| dir.to_string_lossy().into_owned()),
        )
        .chain(leftovers.branches.iter().cloned());

    let kept: Vec<KeptOrphanTarget> = all_targets
        .filter(|target| !orphan_targets.contains(target))
        .map(|target| KeptOrphanTarget {
            reason: kept_reason(&target),
            target,
        })
        .collect();

    if dry_run {
        return Ok(IdentityOrphanSweepReport {
            skipped: None,
            removed: orphan_targets.into_iter().collect(),
            kept,
            failed: Vec::new(),
        });
    }

    let receipts = sweep_reflection_orphans(
        &repo,
        &worktrees_dir,
        &ReflectionOrphanSweepOptions {
            live_run_ids,
            now_ms: now_ms as f64,
            grace_ms: Some(REFLECTION_ORPHAN_GRACE_MS),
            exec: Some(exec.as_ref()),
        },
    )?;

    let mut removed = Vec::new();
    let mut failed = Vec::new();
    for receipt in receipts {
        if receipt.removed {
            removed.push(receipt.target);
        } else {
            failed.push(FailedOrphanTarget {
                target: receipt.target,
                detail: receipt.detail.unwrap_or_else(|| "not removed".to_owned()),
            });
        }
    }

    Ok(IdentityOrphanSweepReport {
        skipped: None,
        removed,
        kept,
        failed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_when_repo_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        let report = sweep_identity_orphan_worktrees(dir.path(), 1_000_000, false).unwrap();
        assert_eq!(report.skipped, Some("no-repo"));
    }
}
