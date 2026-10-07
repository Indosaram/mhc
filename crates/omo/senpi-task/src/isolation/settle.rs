//! isolation/settle.ts: merge (or retain) an isolated child's clone at the end of its run.

use std::path::{Path, PathBuf};

use isolation_core::{IsolationHandle, WorktreeBaseline};

use crate::isolation::baseline_store::read_baseline;
use crate::isolation::runtime::IsolationRuntime;
use crate::state::{IsolationMergeKind, IsolationMergeResult, TaskIsolationSpec};

/// Kinds whose clone is the user's only copy of the work, so it is renamed aside, never deleted.
fn retaining_kind(kind: IsolationMergeKind) -> bool {
    matches!(
        kind,
        IsolationMergeKind::NotApplied | IsolationMergeKind::BranchMergeFailed
    )
}

/// Input of settle_isolation (TS SettleIsolationInput).
pub struct SettleIsolationInput<'a> {
    pub runtime: &'a dyn IsolationRuntime,
    pub state_dir: &'a Path,
    pub task_id: &'a str,
    pub isolation: &'a TaskIsolationSpec,
    /// Only a completed child earns a merge; every other terminal keeps artifacts and touches nothing.
    pub merge: bool,
    pub reason: Option<&'a str>,
    pub baseline: Option<&'a WorktreeBaseline>,
    pub handle: Option<&'a IsolationHandle>,
}

/// Capture the delta, merge (or retain), then dispose of the workspace.
pub fn settle_isolation(input: &SettleIsolationInput<'_>) -> IsolationMergeResult {
    let started = crate::state::system_now_ms();
    let elapsed = |now: u64| now.saturating_sub(started);

    let baseline = match input.baseline {
        Some(baseline) => baseline.clone(),
        None => match read_baseline(input.state_dir, input.task_id) {
            Ok(Some(baseline)) => baseline,
            Ok(None) => {
                return IsolationMergeResult::retained(
                    Some(
                        "isolation baseline is unavailable; the clone is left for manual inspection"
                            .to_string(),
                    ),
                    input.reason.map(str::to_string),
                    elapsed(crate::state::system_now_ms()),
                );
            }
            Err(error) => {
                return IsolationMergeResult::retained(
                    Some(error.to_string()),
                    input.reason.map(str::to_string),
                    elapsed(crate::state::system_now_ms()),
                );
            }
        },
    };

    let delta = match input.runtime.capture_delta(
        Path::new(&input.isolation.merged_dir),
        &baseline,
    ) {
        Ok(delta) => delta,
        Err(error) => {
            return IsolationMergeResult::retained(
                Some(error.to_string()),
                input.reason.map(str::to_string),
                elapsed(crate::state::system_now_ms()),
            );
        }
    };

    let options = isolation_core::IsolationMergeOptions {
        id: input.task_id.to_string(),
        artifacts_dir: input.state_dir.to_path_buf(),
        lock_hook: None,
        commit_message: None,
        mode: input.isolation.mode.into(),
        apply: input.merge && input.isolation.apply,
        repo_root: baseline.root.repo_root.clone(),
        isolation_dir: PathBuf::from(&input.isolation.merged_dir),
        baseline: baseline.clone(),
        delta: Some(delta),
    };
    let core = match input.runtime.merge(options) {
        Ok(core) => core,
        Err(error) => {
            return IsolationMergeResult::retained(
                Some(error.to_string()),
                input.reason.map(str::to_string),
                elapsed(crate::state::system_now_ms()),
            );
        }
    };

    let result = project(&core, elapsed(crate::state::system_now_ms()), input.reason);
    dispose_workspace(input, &result);
    result
}

/// Project the core merge result into the task-level IsolationMergeResult.
fn project(
    core: &isolation_core::IsolationMergeResult,
    duration_ms: u64,
    reason: Option<&str>,
) -> IsolationMergeResult {
    IsolationMergeResult {
        kind: core.state.kind,
        changes_applied: core.state.changes_applied,
        duration_ms: Some(duration_ms),
        patch_path: core.patch_path.clone(),
        error: core.state.warning.clone(),
        reason: reason.map(str::to_string),
        summary_path: Some(core.summary_path.clone()),
        files_changed: Some(core.files_changed),
        nested_patch_paths: core.nested_patch_paths.clone(),
        branch_name: core.state.branch_name.clone(),
        partial: core.state.partial,
        conflict: core.state.conflict.clone(),
        manual_command: core.state.manual_command.clone(),
    }
}

/// A clone whose changes could not land is renamed aside instead of deleted. Without a handle (a
/// crash salvaged by a later process) nothing is touched: the startup sweep owns reclamation once
/// the dead owner is proven.
fn dispose_workspace(input: &SettleIsolationInput<'_>, result: &IsolationMergeResult) {
    let Some(handle) = input.handle else {
        return;
    };
    if retaining_kind(result.kind) {
        let _ = input.runtime.retain(handle, result.kind.as_str());
        return;
    }
    input.runtime.cleanup(handle);
}
