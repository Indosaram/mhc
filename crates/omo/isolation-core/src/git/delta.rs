use std::path::Path;

use crate::backend::{IsolationError, Result};
use crate::git::baseline::{
    capture_repo_baseline, BaselineCaptureOptions, RepoBaseline, WorktreeBaseline,
    ISOLATION_BASELINE_MAX_CONTENT_BYTES,
};
use crate::git::command::{exists, run_git, str_args, GitOptions};
use crate::git::synthetic_tree::write_synthetic_tree;
use crate::merge::shared::nested_path;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NestedPatch {
    pub relative_path: String,
    pub patch: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeltaPatchResult {
    pub root_patch: String,
    pub nested_patches: Vec<NestedPatch>,
}

fn diff_trees(repo_root: &Path, base: &str, head: &str) -> Result<String> {
    let output = run_git(
        &str_args(&[
            "diff-tree",
            "--no-commit-id",
            "-r",
            "-p",
            "--binary",
            "--no-ext-diff",
            "--no-textconv",
            base,
            head,
        ]),
        &GitOptions::new(repo_root.to_path_buf()),
    )?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn capture_repo_delta_patch(repo_dir: &Path, baseline: &RepoBaseline) -> Result<String> {
    let current = capture_repo_baseline(
        repo_dir,
        ISOLATION_BASELINE_MAX_CONTENT_BYTES,
        &BaselineCaptureOptions::default(),
    )?;
    let mut committed_patch = String::new();
    if !current.head_commit.is_empty() && current.head_commit != baseline.head_commit {
        let base = if baseline.head_commit.is_empty() {
            write_synthetic_tree(repo_dir, "", &[])?
        } else {
            baseline.head_commit.clone()
        };
        committed_patch = diff_trees(repo_dir, &base, &current.head_commit)?;
    }
    let baseline_tree = write_synthetic_tree(
        &baseline.repo_root,
        &baseline.head_commit,
        &[
            baseline.staged.clone(),
            baseline.unstaged.clone(),
            baseline.untracked_patch.clone(),
        ],
    )?;
    let current_tree = write_synthetic_tree(
        &baseline.repo_root,
        &baseline.head_commit,
        &[
            committed_patch,
            current.staged.clone(),
            current.unstaged.clone(),
            current.untracked_patch.clone(),
        ],
    )?;
    diff_trees(&baseline.repo_root, &baseline_tree, &current_tree)
}

pub fn capture_delta_patch(
    isolation_dir: &Path,
    baseline: &WorktreeBaseline,
) -> Result<DeltaPatchResult> {
    let root_patch = capture_repo_delta_patch(isolation_dir, &baseline.root)?;
    let mut nested_patches: Vec<NestedPatch> = Vec::new();
    for nested in &baseline.nested {
        // The baseline's repository path is a mutation target (synthetic trees write
        // into its object database), so it must still canonically sit under the root.
        nested_path(&baseline.root.repo_root, &nested.relative_path)?;
        let dir = isolation_dir.join(&nested.relative_path);
        if !exists(&dir.join(".git"))? {
            // A baseline-listed repository absent from the isolation means the isolated
            // tree is incomplete; silently treating it as an empty delta would drop work.
            return Err(IsolationError::other(format!(
                "baseline nested repository missing from isolation: {}",
                nested.relative_path
            )));
        }
        let patch = capture_repo_delta_patch(&dir, &nested.baseline)?;
        if !patch.trim().is_empty() {
            nested_patches.push(NestedPatch {
                relative_path: nested.relative_path.clone(),
                patch,
            });
        }
    }
    Ok(DeltaPatchResult {
        root_patch,
        nested_patches,
    })
}
