use std::path::Path;
use std::sync::Arc;

use crate::backend::{error_text, IsolationError, Result};
use crate::git::baseline::{RepoBaseline, WorktreeBaseline};
use crate::git::command::{git_text, run_git, str_args, GitOptions};
use crate::git::delta::{capture_delta_patch, DeltaPatchResult, NestedPatch};
use crate::git::synthetic_tree::write_synthetic_tree;
use crate::merge::shared::{
    stash_pop, stash_push, task_branch, with_repo_lock, LockHook, MergeKind, MergeState,
};
use crate::util::mkdtemp;

pub type CommitMessage = Arc<dyn Fn(&str) -> Result<Option<String>> + Send + Sync>;

#[derive(Clone, Default)]
pub struct BranchOptions {
    pub commit_message: Option<CommitMessage>,
    pub lock_hook: Option<LockHook>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskBranch {
    pub branch_name: Option<String>,
    pub base_sha: String,
    pub nested_patches: Vec<NestedPatch>,
}

fn git_options(cwd: &Path) -> GitOptions {
    GitOptions::new(cwd.to_path_buf())
}

fn output(cwd: &Path, args: &[String]) -> Result<String> {
    git_text(cwd, args)
}

fn diff(cwd: &Path, base: &str, head: &str) -> Result<String> {
    let result = run_git(
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
        &git_options(cwd),
    )?;
    Ok(String::from_utf8_lossy(&result.stdout).into_owned())
}

/// Replay only in a disposable index: even a failed three-way merge cannot touch user files.
fn filtered_tree(repo_root: &Path, base: &str, patch: &str) -> Result<String> {
    let dir = mkdtemp("isolation-replay-")?;
    let index_file = dir.join("index");
    let mut options = git_options(repo_root);
    options
        .env
        .push(("GIT_INDEX_FILE".to_string(), index_file.to_string_lossy().into_owned()));
    let result = (|| {
        run_git(&str_args(&["read-tree", base]), &options)?;
        if !patch.trim().is_empty() {
            let mut apply_options = options.clone();
            apply_options.input = Some(patch.as_bytes().to_vec());
            let applied = run_git(
                &str_args(&[
                    "apply",
                    "--cached",
                    "--binary",
                    "--whitespace=nowarn",
                    "-",
                ]),
                &apply_options,
            );
            match applied {
                Ok(_) => {}
                Err(error) if matches!(&error, IsolationError::Git { .. }) => {
                    run_git(&str_args(&["read-tree", base]), &options)?;
                    run_git(
                        &str_args(&[
                            "apply",
                            "--cached",
                            "--3way",
                            "--binary",
                            "--whitespace=nowarn",
                            "-",
                        ]),
                        &apply_options,
                    )?;
                }
                Err(error) => return Err(error),
            }
        }
        let written = run_git(&str_args(&["write-tree"]), &options)?;
        Ok(String::from_utf8_lossy(&written.stdout).trim().to_string())
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn commit_tree(
    repo_root: &Path,
    branch: &str,
    tree: &str,
    message: &str,
    author: Option<&[(String, String)]>,
) -> Result<()> {
    let parent = output(repo_root, &str_args(&["rev-parse", branch]))?;
    let mut options = git_options(repo_root);
    options.input = Some(message.as_bytes().to_vec());
    if let Some(author) = author {
        for (key, value) in author {
            options.env.push((key.clone(), value.clone()));
        }
    }
    let sha = String::from_utf8_lossy(
        &run_git(
            &str_args(&["commit-tree", tree, "-p", &parent]),
            &options,
        )?
        .stdout,
    )
    .trim()
    .to_string();
    run_git(
        &str_args(&["update-ref", &format!("refs/heads/{branch}"), &sha, &parent]),
        &git_options(repo_root),
    )?;
    Ok(())
}

fn replay_error(commit: &str, branch_name: &str, error: &IsolationError) -> IsolationError {
    let cause = error_text(error);
    IsolationError::CommitReplay {
        commit: commit.to_string(),
        branch_name: branch_name.to_string(),
        cause: cause.clone(),
        message: format!("Cannot replay isolation commit {commit} onto {branch_name}: {cause}"),
    }
}

pub fn commit_to_branch_locked(
    isolation_dir: &Path,
    repo_root: &Path,
    id: &str,
    baseline: &WorktreeBaseline,
    options: &BranchOptions,
    supplied_delta: Option<&DeltaPatchResult>,
) -> Result<TaskBranch> {
    let branch_name = task_branch(id)?;
    let base_sha = baseline.root.head_commit.clone();
    let delta = match supplied_delta {
        Some(delta) => delta.clone(),
        None => capture_delta_patch(isolation_dir, baseline)?,
    };
    if delta.root_patch.trim().is_empty() {
        return Ok(TaskBranch {
            branch_name: None,
            base_sha,
            nested_patches: delta.nested_patches,
        });
    }
    let head = output(isolation_dir, &str_args(&["rev-parse", "HEAD"]))?;
    // Reject rewritten/non-descendant child histories instead of importing unrelated commits.
    run_git(
        &str_args(&["merge-base", "--is-ancestor", &base_sha, &head]),
        &git_options(isolation_dir),
    )?;
    let dirty = [
        &baseline.root.staged,
        &baseline.root.unstaged,
        &baseline.root.untracked_patch,
    ]
    .iter()
    .any(|patch| !patch.trim().is_empty());
    let message = |patch: &str| -> Result<String> {
        if let Some(callback) = &options.commit_message && let Some(text) = callback(patch)? {
            return Ok(text);
        }
        Ok(format!("chore(task): {id} leftovers"))
    };
    // Ref creation is non-forcing, including retries with a retained branch of this id.
    run_git(
        &str_args(&["branch", &branch_name, &base_sha]),
        &git_options(repo_root),
    )?;
    if !dirty && head != base_sha {
        run_git(
            &str_args(&[
                "fetch",
                "--no-tags",
                &isolation_dir.to_string_lossy(),
                &format!("HEAD:refs/heads/{branch_name}"),
            ]),
            &git_options(repo_root),
        )?;
        let empty = WorktreeBaseline {
            root: RepoBaseline {
                repo_root: isolation_dir.to_path_buf(),
                head_commit: head.clone(),
                staged: String::new(),
                unstaged: String::new(),
                untracked_files: Vec::new(),
                untracked_patch: String::new(),
            },
            nested: Vec::new(),
        };
        let leftovers = capture_delta_patch(isolation_dir, &empty)?;
        if !leftovers.root_patch.trim().is_empty() {
            let tree = write_synthetic_tree(repo_root, &head, std::slice::from_ref(&leftovers.root_patch))?;
            commit_tree(
                repo_root,
                &branch_name,
                &tree,
                &message(&leftovers.root_patch)?,
                None,
            )?;
        }
    } else {
        // Seed WIP-side blobs in both object databases. Diffing cumulative child states
        // against this tree subtracts inherited WIP even when a child used `git add .`.
        let wip = vec![
            baseline.root.staged.clone(),
            baseline.root.unstaged.clone(),
            baseline.root.untracked_patch.clone(),
        ];
        let dirty_tree = write_synthetic_tree(isolation_dir, &base_sha, &wip)?;
        write_synthetic_tree(repo_root, &base_sha, &wip)?;
        let commits: Vec<String> = output(
            isolation_dir,
            &str_args(&["rev-list", "--reverse", &format!("{base_sha}..{head}")]),
        )?
        .split('\n')
        .filter(|commit| !commit.is_empty())
        .map(|commit| commit.to_string())
        .collect();
        let mut previous_tree = output(
            repo_root,
            &str_args(&["rev-parse", &format!("{base_sha}^{{tree}}")]),
        )?;
        for commit in &commits {
            let step = (|| -> Result<()> {
                let patch = diff(isolation_dir, &dirty_tree, &format!("{commit}^{{tree}}"))?;
                let tree = filtered_tree(repo_root, &base_sha, &patch)?;
                if tree != previous_tree {
                    let details = run_git(
                        &str_args(&["show", "-s", "--format=%an%x00%ae%x00%aI%x00%B", commit]),
                        &git_options(isolation_dir),
                    )?;
                    let details: Vec<String> = String::from_utf8_lossy(&details.stdout)
                        .split('\0')
                        .map(|part| part.to_string())
                        .collect();
                    let author = [
                        ("GIT_AUTHOR_NAME".to_string(), details[0].clone()),
                        ("GIT_AUTHOR_EMAIL".to_string(), details[1].clone()),
                        ("GIT_AUTHOR_DATE".to_string(), details[2].clone()),
                    ];
                    commit_tree(repo_root, &branch_name, &tree, &details[3], Some(&author))?;
                }
                previous_tree = tree;
                Ok(())
            })();
            if let Err(error) = step {
                if !matches!(&error, IsolationError::Git { .. }) {
                    return Err(error);
                }
                return Err(replay_error(commit, &branch_name, &error));
            }
        }
        let final_step = (|| -> Result<()> {
            let final_tree = filtered_tree(repo_root, &base_sha, &delta.root_patch)?;
            if final_tree != previous_tree {
                let message_text = message(&diff(repo_root, &previous_tree, &final_tree)?)?;
                commit_tree(repo_root, &branch_name, &final_tree, &message_text, None)?;
            }
            Ok(())
        })();
        if let Err(error) = final_step {
            if !matches!(&error, IsolationError::Git { .. }) {
                return Err(error);
            }
            return Err(replay_error(
                &format!("{head} (leftovers)"),
                &branch_name,
                &error,
            ));
        }
    }
    Ok(TaskBranch {
        branch_name: Some(branch_name),
        base_sha,
        nested_patches: delta.nested_patches,
    })
}

pub fn commit_to_branch(
    isolation_dir: &Path,
    repo_root: &Path,
    id: &str,
    baseline: &WorktreeBaseline,
    options: &BranchOptions,
) -> Result<TaskBranch> {
    with_repo_lock(
        repo_root,
        &|| commit_to_branch_locked(isolation_dir, repo_root, id, baseline, options, None),
        options.lock_hook.as_ref(),
    )
}

pub fn merge_task_branch_locked(repo_root: &Path, branch: &TaskBranch) -> Result<MergeState> {
    let Some(branch_name) = &branch.branch_name else {
        return Ok(MergeState::new(MergeKind::NoChanges, false));
    };
    let revisions = output(
        repo_root,
        &str_args(&["rev-list", &format!("{}..{}", branch.base_sha, branch_name)]),
    )?;
    if revisions.is_empty() {
        run_git(&str_args(&["branch", "-D", branch_name]), &git_options(repo_root))?;
        return Ok(MergeState::new(MergeKind::NoChanges, false));
    }
    let stashed = stash_push(repo_root)?;
    let pick = run_git(
        &str_args(&[
            "cherry-pick",
            &format!("{}..{}", branch.base_sha, branch_name),
        ]),
        &git_options(repo_root),
    );
    let state_result = match pick {
        Ok(_) => Ok(MergeState::new(MergeKind::BranchMerged, true)),
        Err(IsolationError::Git { stderr, .. }) => {
            let abort = run_git(&str_args(&["cherry-pick", "--abort"]), &git_options(repo_root));
            match abort {
                Ok(_) => {
                    let mut state = MergeState::new(MergeKind::BranchMergeFailed, false);
                    state.branch_name = Some(branch_name.clone());
                    state.conflict = Some(stderr);
                    Ok(state)
                }
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    };
    let warning = match &stashed {
        Some(stashed) => stash_pop(repo_root, stashed)?,
        None => None,
    };
    let mut state = state_result?;
    if state.changes_applied {
        run_git(&str_args(&["branch", "-D", branch_name]), &git_options(repo_root))?;
    }
    if let Some(warning) = warning {
        state.warning = Some(warning);
    }
    Ok(state)
}

pub fn merge_task_branch(
    repo_root: &Path,
    branch: &TaskBranch,
    lock_hook: Option<&LockHook>,
) -> Result<MergeState> {
    with_repo_lock(
        repo_root,
        &|| merge_task_branch_locked(repo_root, branch),
        lock_hook,
    )
}
