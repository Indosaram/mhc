//! Worktree extension RPC request handler (`thread/worktree/worktree-rpc.ts`).

use std::path::Path;

use serde_json::{Map, Value};

use super::super::contracts_results::ThreadDataError;
use super::discard_worktree::{DiscardWorktreeOptions, discard_worktree, inside_root};
use super::failures::{FolderFailureReason, folder_failure};
use super::thread_worktree::{
    GitRun, WorktreeOutcome, WorktreeRequest, create_thread_worktree, run_git,
};

#[derive(Debug, Clone, PartialEq)]
pub enum WorktreeRemoval {
    Removed,
    Error(ThreadDataError),
}

fn optional_nonempty(val: Option<&Value>) -> Result<Option<&str>, ()> {
    match val {
        None => Ok(None),
        Some(Value::String(s)) if !s.is_empty() => Ok(Some(s.as_str())),
        _ => Err(()),
    }
}

fn not_absolute(path: Option<&str>) -> ThreadDataError {
    let mut details = Map::new();
    if let Some(p) = path {
        details.insert("path".into(), Value::String(p.to_owned()));
    }
    folder_failure(
        FolderFailureReason::RelativePath,
        "The worktree request must name an absolute folder of this device.",
        Some(details),
    )
}

async fn is_linked_worktree_top(path: &Path, git: &GitRun) -> bool {
    let Ok(real) = std::fs::canonicalize(path) else {
        return false;
    };
    let top = git(&["rev-parse".into(), "--show-toplevel".into()], path).await;
    if top.exit != Some(0) {
        return false;
    }
    let Ok(top_real) = std::fs::canonicalize(top.stdout.trim()) else {
        return false;
    };
    if top_real != real {
        return false;
    }
    let list = git(
        &["worktree".into(), "list".into(), "--porcelain".into()],
        path,
    )
    .await;
    if list.exit != Some(0) {
        return false;
    }
    list.stdout
        .lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .skip(1)
        .any(|p| std::fs::canonicalize(p).ok().as_ref() == Some(&real))
}

async fn remove_requested(
    record: &Map<String, Value>,
    root: &Path,
    git: &GitRun,
) -> WorktreeRemoval {
    let Ok(Some(path_str)) = optional_nonempty(record.get("path")) else {
        return WorktreeRemoval::Error(not_absolute(record.get("path").and_then(Value::as_str)));
    };
    let path = Path::new(path_str);
    let Ok(Some(branch)) = optional_nonempty(record.get("branch")) else {
        return WorktreeRemoval::Error(not_absolute(Some(path_str)));
    };
    if !path.is_absolute() {
        return WorktreeRemoval::Error(not_absolute(Some(path_str)));
    }
    let left_behind = |msg: String| {
        let mut details = Map::new();
        details.insert("path".into(), Value::String(path_str.to_owned()));
        details.insert("branch".into(), Value::String(branch.to_owned()));
        WorktreeRemoval::Error(folder_failure(
            FolderFailureReason::WorktreeLeftBehind,
            msg,
            Some(details),
        ))
    };
    if !inside_root(root, path) {
        return left_behind(format!(
            "{path_str} is not a worktree OmO made here; it was not removed."
        ));
    }
    if !is_linked_worktree_top(path, git).await {
        return left_behind(format!(
            "{path_str} is not the top folder of a worktree; it was not removed."
        ));
    }
    let head = git(
        &["symbolic-ref".into(), "--short".into(), "HEAD".into()],
        path,
    )
    .await;
    let common = git(
        &[
            "rev-parse".into(),
            "--path-format=absolute".into(),
            "--git-common-dir".into(),
        ],
        path,
    )
    .await;
    if head.exit != Some(0) || head.stdout.trim() != branch || common.exit != Some(0) {
        return left_behind(format!(
            "{path_str} is not the worktree of branch {branch}; it was not removed."
        ));
    }
    let status = git(
        &[
            "status".into(),
            "--porcelain".into(),
            "--ignored".into(),
            "--untracked-files=all".into(),
        ],
        path,
    )
    .await;
    if status.exit != Some(0) || !status.stdout.trim().is_empty() {
        return left_behind(format!(
            "{path_str} has uncommitted work; it was not removed."
        ));
    }
    let reflog = git(
        &[
            "reflog".into(),
            "show".into(),
            "--format=%H".into(),
            format!("refs/heads/{branch}"),
        ],
        path,
    )
    .await;
    let count = if reflog.exit == Some(0) {
        Some(
            reflog
                .stdout
                .lines()
                .filter(|l| !l.trim().is_empty())
                .count(),
        )
    } else {
        None
    };
    if count != Some(1) {
        let why = match count {
            None => format!(
                "the reflog of branch {branch} could not be read, so its work cannot be ruled out"
            ),
            Some(0) => format!(
                "branch {branch} has no reflog entry (reflogs may be off in this repository), so its work cannot be ruled out"
            ),
            Some(_) => format!("branch {branch} has commits of its own"),
        };
        return left_behind(format!("{path_str} was not removed: {why}."));
    }
    let common_dir = Path::new(common.stdout.trim());
    let repository = if common_dir.file_name().and_then(|s| s.to_str()) == Some(".git") {
        common_dir.parent().unwrap_or(common_dir)
    } else {
        common_dir
    };
    match discard_worktree(DiscardWorktreeOptions {
        repository,
        path,
        branch,
        root,
        git,
        created_folder: None,
        remove_unregistered_folder: false,
    })
    .await
    {
        None => WorktreeRemoval::Removed,
        Some(err) => WorktreeRemoval::Error(err),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum WorktreeRpcAnswer {
    Created(WorktreeOutcome),
    Removed(WorktreeRemoval),
}

pub async fn answer_worktree_request(
    data: &Value,
    root: &Path,
    git: Option<GitRun>,
) -> WorktreeRpcAnswer {
    let git = git.unwrap_or_else(run_git);
    let Some(obj) = data.as_object() else {
        return WorktreeRpcAnswer::Created(WorktreeOutcome::Error(not_absolute(None)));
    };
    if let Some(rem) = obj.get("remove").and_then(Value::as_object) {
        return WorktreeRpcAnswer::Removed(remove_requested(rem, root, &git).await);
    }
    let Ok(Some(cwd_str)) = optional_nonempty(obj.get("cwd")) else {
        return WorktreeRpcAnswer::Created(WorktreeOutcome::Error(not_absolute(
            obj.get("cwd").and_then(Value::as_str),
        )));
    };
    let cwd = Path::new(cwd_str);
    if !cwd.is_absolute() {
        return WorktreeRpcAnswer::Created(WorktreeOutcome::Error(not_absolute(Some(cwd_str))));
    }
    let Ok(branch) = optional_nonempty(obj.get("branch")) else {
        return WorktreeRpcAnswer::Created(WorktreeOutcome::Error(not_absolute(Some(cwd_str))));
    };
    let Ok(base) = optional_nonempty(obj.get("base")) else {
        return WorktreeRpcAnswer::Created(WorktreeOutcome::Error(not_absolute(Some(cwd_str))));
    };
    let req = WorktreeRequest {
        branch: branch.map(str::to_owned),
        base: base.map(str::to_owned),
    };
    WorktreeRpcAnswer::Created(create_thread_worktree(cwd, &req, root, Some(git)).await)
}
