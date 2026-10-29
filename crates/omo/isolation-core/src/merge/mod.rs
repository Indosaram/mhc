pub mod branch_mode;
pub mod patch_mode;
pub mod shared;

pub use branch_mode::{
    commit_to_branch, commit_to_branch_locked, merge_task_branch, merge_task_branch_locked,
    BranchOptions, CommitMessage, TaskBranch,
};
pub use patch_mode::{
    apply_delta_patch, apply_delta_patch_locked, apply_nested_patches, NestedOutcome,
};
pub use shared::*;

use std::path::PathBuf;

use crate::backend::{IsolationError, Result};
use crate::git::baseline::WorktreeBaseline;
use crate::git::delta::{capture_delta_patch, DeltaPatchResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeMode {
    Patch,
    Branch,
}

pub struct IsolationMergeOptions {
    pub id: String,
    pub artifacts_dir: PathBuf,
    pub lock_hook: Option<LockHook>,
    pub commit_message: Option<CommitMessage>,
    pub mode: MergeMode,
    pub apply: bool,
    pub repo_root: PathBuf,
    pub isolation_dir: PathBuf,
    pub baseline: WorktreeBaseline,
    pub delta: Option<DeltaPatchResult>,
}

/// Never tears down isolation: callers retain it if artifact writing or replay fails.
pub fn merge_isolated_changes(options: IsolationMergeOptions) -> Result<IsolationMergeResult> {
    let delta = match &options.delta {
        Some(delta) => delta.clone(),
        None => capture_delta_patch(&options.isolation_dir, &options.baseline)?,
    };
    let artifacts = write_artifacts(
        &delta,
        &ArtifactOptions {
            id: options.id.clone(),
            artifacts_dir: options.artifacts_dir.clone(),
            lock_hook: options.lock_hook.clone(),
        },
    )?;
    if !options.apply {
        return summarize(artifacts.base_result(MergeState::new(MergeKind::Retained, false)));
    }
    with_repo_lock(
        &options.repo_root,
        &|| {
            if options.mode == MergeMode::Patch {
                return apply_delta_patch_locked(&options.repo_root, &delta, &artifacts);
            }
            let branch = match commit_to_branch_locked(
                &options.isolation_dir,
                &options.repo_root,
                &options.id,
                &options.baseline,
                &BranchOptions {
                    commit_message: options.commit_message.clone(),
                    lock_hook: None,
                },
                Some(&delta),
            ) {
                Ok(branch) => branch,
                Err(IsolationError::CommitReplay {
                    branch_name,
                    message,
                    ..
                }) => {
                    let mut state = MergeState::new(MergeKind::BranchMergeFailed, false);
                    state.branch_name = Some(branch_name);
                    state.conflict = Some(message);
                    return summarize(artifacts.base_result(state));
                }
                Err(error) => return Err(error),
            };
            let state = merge_task_branch_locked(&options.repo_root, &branch)?;
            if state.kind == MergeKind::BranchMergeFailed {
                return summarize(artifacts.base_result(state));
            }
            let nested =
                apply_nested_patches(&options.repo_root, &delta, &artifacts.nested_patch_paths)?;
            let mut final_state = state;
            if !delta.nested_patches.is_empty() && branch.branch_name.is_none() {
                final_state.kind = MergeKind::Applied;
                final_state.changes_applied = true;
            }
            if nested.partial.is_some() {
                final_state.partial = nested.partial;
            }
            if nested.nested_failed.is_some() {
                final_state.nested_failed = nested.nested_failed;
            }
            if nested.warning.is_some() {
                final_state.warning = nested.warning;
            }
            summarize(artifacts.base_result(final_state))
        },
        options.lock_hook.as_ref(),
    )
}
