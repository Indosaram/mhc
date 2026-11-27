//! Git worktree creation (`thread/worktree/thread-worktree.ts`).

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use super::super::contracts_results::ThreadDataError;
use super::discard_worktree::{DiscardWorktreeOptions, discard_worktree};
use super::failures::{FolderFailureReason, errno_reason, folder_failure, git_failure_reason};

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadWorktree {
    pub path: String,
    pub branch: String,
    pub base: String,
    pub repository: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup_session_left_open: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum WorktreeOutcome {
    Ok(ThreadWorktree),
    Error(ThreadDataError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitResult {
    /// `Some(code)` when git ran, `None` when spawn failed (with `spawn_failed = true`) or timed out (`timed_out = true`).
    pub exit: Option<i32>,
    pub spawn_failed: bool,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
}

pub type GitRun = Arc<dyn Fn(&[String], &Path) -> BoxFuture<GitResult> + Send + Sync>;

const GIT_TIMEOUT_MS: u64 = 60_000;

pub fn run_git() -> GitRun {
    Arc::new(|args: &[String], cwd: &Path| {
        let args = args.to_vec();
        let cwd = cwd.to_path_buf();
        Box::pin(async move {
            let mut cmd = tokio::process::Command::new("git");
            cmd.args(&args)
                .current_dir(&cwd)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true);
            let child = match cmd.spawn() {
                Ok(c) => c,
                Err(err) => {
                    return GitResult {
                        exit: None,
                        spawn_failed: err.kind() == std::io::ErrorKind::NotFound,
                        timed_out: false,
                        stdout: String::new(),
                        stderr: err.to_string(),
                    };
                }
            };
            match tokio::time::timeout(
                Duration::from_millis(GIT_TIMEOUT_MS),
                child.wait_with_output(),
            )
            .await
            {
                Ok(Ok(out)) => GitResult {
                    exit: out.status.code(),
                    spawn_failed: false,
                    timed_out: false,
                    stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
                },
                Ok(Err(err)) => GitResult {
                    exit: None,
                    spawn_failed: false,
                    timed_out: false,
                    stdout: String::new(),
                    stderr: err.to_string(),
                },
                Err(_) => GitResult {
                    exit: None,
                    spawn_failed: false,
                    timed_out: true,
                    stdout: String::new(),
                    stderr: String::new(),
                },
            }
        })
    })
}

pub fn check_folder(path: &Path) -> Option<ThreadDataError> {
    let p_str = path.to_string_lossy().into_owned();
    let mut details = Map::new();
    details.insert("path".into(), Value::String(p_str.clone()));
    match std::fs::metadata(path) {
        Ok(meta) => {
            if meta.is_dir() {
                None
            } else {
                Some(folder_failure(
                    FolderFailureReason::NotADirectory,
                    format!("{p_str} is not a folder."),
                    Some(details),
                ))
            }
        }
        Err(err) => {
            let reason = errno_reason(&err).unwrap_or(FolderFailureReason::FolderMissing);
            Some(folder_failure(
                reason,
                format!("Folder {p_str} cannot be used ({}).", reason.as_str()),
                Some(details),
            ))
        }
    }
}

fn generated_branch() -> String {
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let suffix = &uuid::Uuid::new_v4().simple().to_string()[..4];
    format!("omo/thread-{stamp}-{suffix}")
}

fn failed_git(result: &GitResult, mut details: Map<String, Value>) -> ThreadDataError {
    if result.spawn_failed {
        return folder_failure(
            FolderFailureReason::GitUnavailable,
            "git is not installed here.",
            Some(details),
        );
    }
    if result.timed_out {
        return folder_failure(
            FolderFailureReason::GitTimeout,
            "git did not answer in time.",
            Some(details),
        );
    }
    let reason = git_failure_reason(&result.stderr);
    details.insert(
        "stderr".into(),
        Value::String(result.stderr.trim().to_owned()),
    );
    folder_failure(
        reason,
        format!("git refused the worktree ({}).", reason.as_str()),
        Some(details),
    )
}

pub async fn create_thread_worktree(
    repository: &Path,
    request: &WorktreeRequest,
    root: &Path,
    git: Option<GitRun>,
) -> WorktreeOutcome {
    let git = git.unwrap_or_else(run_git);
    if let Some(err) = check_folder(repository) {
        return WorktreeOutcome::Error(err);
    }

    let mut d = Map::new();
    d.insert(
        "path".into(),
        Value::String(repository.to_string_lossy().into_owned()),
    );
    let top = git(&["rev-parse".into(), "--show-toplevel".into()], repository).await;
    if top.exit != Some(0) {
        return WorktreeOutcome::Error(failed_git(&top, d));
    }
    let repo_str = top.stdout.trim().to_owned();
    let repo_path = PathBuf::from(&repo_str);
    let branch = request.branch.clone().unwrap_or_else(generated_branch);
    let base = request.base.as_deref().unwrap_or("HEAD").to_owned();

    if branch.starts_with('-')
        || git(
            &["check-ref-format".into(), "--branch".into(), branch.clone()],
            &repo_path,
        )
        .await
        .exit
            != Some(0)
    {
        let mut bd = Map::new();
        bd.insert("branch".into(), Value::String(branch.clone()));
        return WorktreeOutcome::Error(folder_failure(
            FolderFailureReason::InvalidBranch,
            format!("{branch} is not a valid branch name."),
            Some(bd),
        ));
    }

    let commit = if base.starts_with('-') {
        None
    } else {
        Some(
            git(
                &[
                    "rev-parse".into(),
                    "--verify".into(),
                    "--quiet".into(),
                    format!("{base}^{{commit}}"),
                ],
                &repo_path,
            )
            .await,
        )
    };
    let Some(commit_res) = commit.filter(|r| r.exit == Some(0)) else {
        let mut bd = Map::new();
        bd.insert("base".into(), Value::String(base.clone()));
        bd.insert("repository".into(), Value::String(repo_str.clone()));
        return WorktreeOutcome::Error(folder_failure(
            FolderFailureReason::BaseNotFound,
            format!("{base} names no commit in {repo_str}."),
            Some(bd),
        ));
    };
    let base_commit = commit_res.stdout.trim().to_owned();

    if git(
        &[
            "show-ref".into(),
            "--verify".into(),
            "--quiet".into(),
            format!("refs/heads/{branch}"),
        ],
        &repo_path,
    )
    .await
    .exit
        == Some(0)
    {
        let mut bd = Map::new();
        bd.insert("branch".into(), Value::String(branch.clone()));
        bd.insert("repository".into(), Value::String(repo_str.clone()));
        return WorktreeOutcome::Error(folder_failure(
            FolderFailureReason::BranchExists,
            format!("Branch {branch} already exists in {repo_str}."),
            Some(bd),
        ));
    }

    let repo_base = repo_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("repo");
    let hash = hex::encode(&Sha256::digest(repo_str.as_bytes())[..4]);
    let repo_key = format!("{repo_base}-{hash}");
    let wt_path = root.join(repo_key).join(branch.replace('/', "-"));
    let wt_str = wt_path.to_string_lossy().into_owned();

    if wt_path.exists() {
        let mut bd = Map::new();
        bd.insert("path".into(), Value::String(wt_str.clone()));
        bd.insert("branch".into(), Value::String(branch));
        return WorktreeOutcome::Error(folder_failure(
            FolderFailureReason::WorktreePathExists,
            format!("{wt_str} already exists."),
            Some(bd),
        ));
    }

    let parent = wt_path.parent().unwrap_or(root);
    let parent_existed = parent.exists();
    if let Err(err) = std::fs::create_dir_all(parent) {
        let reason = errno_reason(&err).unwrap_or(FolderFailureReason::PermissionDenied);
        let mut bd = Map::new();
        bd.insert("path".into(), Value::String(wt_str));
        return WorktreeOutcome::Error(folder_failure(
            reason,
            format!(
                "The worktrees folder {} cannot be created ({}).",
                parent.display(),
                reason.as_str()
            ),
            Some(bd),
        ));
    }

    let added = git(
        &[
            "worktree".into(),
            "add".into(),
            "-b".into(),
            branch.clone(),
            wt_str.clone(),
            base_commit.clone(),
        ],
        &repo_path,
    )
    .await;
    if added.exit != Some(0) {
        if let Some(left) = discard_worktree(DiscardWorktreeOptions {
            repository: &repo_path,
            path: &wt_path,
            branch: &branch,
            root,
            git: &git,
            created_folder: (!parent_existed).then_some(parent),
            remove_unregistered_folder: true,
        })
        .await
        {
            return WorktreeOutcome::Error(left);
        }
        let mut bd = Map::new();
        bd.insert("path".into(), Value::String(wt_str));
        bd.insert("branch".into(), Value::String(branch));
        bd.insert("repository".into(), Value::String(repo_str));
        return WorktreeOutcome::Error(failed_git(&added, bd));
    }

    WorktreeOutcome::Ok(ThreadWorktree {
        path: wt_str,
        branch,
        base: base_commit,
        repository: repo_str,
        setup_session_left_open: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[tokio::test]
    async fn creates_and_discards_git_worktree_in_tempdir() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let root = tmp.path().join("worktrees");
        std::fs::create_dir_all(&repo).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(&repo)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .args(["config", "user.email", "t@example.com"])
                .current_dir(&repo)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .args(["config", "user.name", "Test"])
                .current_dir(&repo)
                .status()
                .unwrap()
                .success()
        );
        std::fs::write(repo.join("README.md"), "hi\n").unwrap();
        assert!(
            Command::new("git")
                .args(["add", "."])
                .current_dir(&repo)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .args(["commit", "-q", "-m", "init"])
                .current_dir(&repo)
                .status()
                .unwrap()
                .success()
        );

        let outcome = create_thread_worktree(
            &repo,
            &WorktreeRequest {
                branch: Some("feat/wt-test".into()),
                base: None,
            },
            &root,
            None,
        )
        .await;
        let wt = match outcome {
            WorktreeOutcome::Ok(w) => w,
            WorktreeOutcome::Error(e) => panic!("unexpected error: {e:?}"),
        };
        assert!(Path::new(&wt.path).exists());
        assert_eq!(wt.branch, "feat/wt-test");

        let left = discard_worktree(DiscardWorktreeOptions {
            repository: Path::new(&wt.repository),
            path: Path::new(&wt.path),
            branch: &wt.branch,
            root: &root,
            git: &run_git(),
            created_folder: None,
            remove_unregistered_folder: false,
        })
        .await;
        assert!(left.is_none(), "expected clean discard, got {left:?}");
        assert!(!Path::new(&wt.path).exists());
    }
}
