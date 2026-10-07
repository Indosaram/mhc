use std::path::{Path, PathBuf};

use crate::backend::{error_text, IsolationError, Result};
use crate::git::command::{run_git, str_args, GitOptions};
use crate::git::delta::DeltaPatchResult;
use crate::merge::shared::{
    nested_path, stash_pop, stash_push, summarize, with_repo_lock, write_artifacts, ArtifactOptions,
    IsolationMergeResult, MergeKind, MergeState, NestedFailure, WrittenArtifacts,
};

#[derive(Debug, Clone, Default)]
pub struct NestedOutcome {
    pub partial: Option<bool>,
    pub nested_failed: Option<Vec<NestedFailure>>,
    pub warning: Option<String>,
}

fn git_options(cwd: &Path) -> GitOptions {
    GitOptions::new(cwd.to_path_buf())
}

fn already_applied(cwd: &Path, patch_path: &Path) -> Result<bool> {
    let mut options = git_options(cwd);
    options.allowed_exit_codes = Some(vec![0, 1, 128]);
    let reverse = run_git(
        &str_args(&["apply", "--check", "--reverse", &patch_path.to_string_lossy()]),
        &options,
    )?;
    let forward = run_git(
        &str_args(&["apply", "--check", &patch_path.to_string_lossy()]),
        &options,
    )?;
    Ok(reverse.code == 0 && forward.code != 0)
}

pub fn apply_nested_patches(
    repo_root: &Path,
    delta: &DeltaPatchResult,
    patch_paths: &[PathBuf],
) -> Result<NestedOutcome> {
    let mut failed: Vec<NestedFailure> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    for (index, nested) in delta.nested_patches.iter().enumerate() {
        if nested.patch.trim().is_empty() {
            continue;
        }
        let Some(patch_path) = patch_paths.get(index) else {
            continue;
        };
        let cwd = nested_path(repo_root, &nested.relative_path)?;
        if already_applied(&cwd, patch_path)? {
            continue;
        }
        let stashed = stash_push(&cwd)?;
        let apply = (|| -> Result<()> {
            run_git(
                &str_args(&[
                    "apply",
                    "--index",
                    "--binary",
                    "--whitespace=nowarn",
                    &patch_path.to_string_lossy(),
                ]),
                &git_options(&cwd),
            )?;
            run_git(
                &str_args(&["commit", "-m", "chore(task): isolated nested changes"]),
                &git_options(&cwd),
            )?;
            Ok(())
        })();
        if let Some(stashed) = &stashed {
            if let Some(warning) = stash_pop(&cwd, stashed)? {
                warnings.push(format!("{}: {warning}", nested.relative_path));
            }
        }
        if let Err(error) = apply {
            if !matches!(&error, IsolationError::Git { .. }) {
                return Err(error);
            }
            failed.push(NestedFailure {
                path: nested.relative_path.clone(),
                error: error_text(&error),
            });
        }
    }
    let mut outcome = NestedOutcome::default();
    if !failed.is_empty() {
        outcome.partial = Some(true);
        outcome.nested_failed = Some(failed);
    }
    if !warnings.is_empty() {
        outcome.warning = Some(warnings.join("\n"));
        outcome.partial = Some(true);
    }
    Ok(outcome)
}

/// Caller holds the common-directory lock. Root apply is deliberately never --3way.
pub fn apply_delta_patch_locked(
    repo_root: &Path,
    delta: &DeltaPatchResult,
    artifacts: &WrittenArtifacts,
) -> Result<IsolationMergeResult> {
    if delta.root_patch.trim().is_empty()
        && !delta
            .nested_patches
            .iter()
            .any(|nested| !nested.patch.trim().is_empty())
    {
        return summarize(artifacts.base_result(MergeState::new(MergeKind::NoChanges, false)));
    }
    let mut kind = MergeKind::Applied;
    if !delta.root_patch.trim().is_empty() {
        if already_applied(repo_root, &artifacts.patch_path)? {
            kind = MergeKind::AlreadyApplied;
        } else {
            match run_git(
                &str_args(&[
                    "apply",
                    "--binary",
                    "--whitespace=nowarn",
                    &artifacts.patch_path.to_string_lossy(),
                ]),
                &git_options(repo_root),
            ) {
                Ok(_) => {}
                Err(IsolationError::Git { stderr, .. }) => {
                    let mut state = MergeState::new(MergeKind::NotApplied, false);
                    state.conflict = Some(stderr);
                    state.manual_command =
                        Some(format!("git apply --3way {}", artifacts.patch_path.display()));
                    return summarize(artifacts.base_result(state));
                }
                Err(error) => return Err(error),
            }
        }
    }
    let nested = apply_nested_patches(repo_root, delta, &artifacts.nested_patch_paths)?;
    let expected = delta
        .nested_patches
        .iter()
        .filter(|nested| !nested.patch.trim().is_empty())
        .count();
    let nested_failed_count = nested
        .nested_failed
        .as_ref()
        .map(|failed| failed.len())
        .unwrap_or(0);
    if delta.root_patch.trim().is_empty() && expected > 0 && nested_failed_count == expected {
        // Nothing landed anywhere: report the merge as not applied, with recovery.
        let conflict = nested
            .nested_failed
            .as_ref()
            .map(|failed| {
                failed
                    .iter()
                    .map(|failure| format!("{}: {}", failure.path, failure.error))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        let mut state = MergeState::new(MergeKind::NotApplied, false);
        state.conflict = Some(conflict);
        state.manual_command = Some("git apply --3way <nested patch>".to_string());
        state.partial = nested.partial;
        state.nested_failed = nested.nested_failed;
        state.warning = nested.warning;
        return summarize(artifacts.base_result(state));
    }
    let mut state = MergeState::new(kind, true);
    state.partial = nested.partial;
    state.nested_failed = nested.nested_failed;
    state.warning = nested.warning;
    summarize(artifacts.base_result(state))
}

pub fn apply_delta_patch(
    repo_root: &Path,
    delta: &DeltaPatchResult,
    options: &ArtifactOptions,
) -> Result<IsolationMergeResult> {
    let artifacts = write_artifacts(delta, options)?;
    with_repo_lock(
        repo_root,
        &|| apply_delta_patch_locked(repo_root, delta, &artifacts),
        options.lock_hook.as_ref(),
    )
}
