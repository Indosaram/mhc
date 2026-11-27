//! Typed worktree & folder failure mapping (`thread/worktree/failures.ts`).

use serde_json::{Map, Value};

use super::super::contracts_results::ThreadDataError;
use super::super::errors::thread_tool_failure;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderFailureReason {
    FolderMissing,
    NotADirectory,
    RelativePath,
    NotAGitRepo,
    InvalidBranch,
    BaseNotFound,
    BranchExists,
    BranchCheckedOut,
    WorktreePathExists,
    NoSpace,
    PermissionDenied,
    GitUnavailable,
    GitTimeout,
    GitFailed,
    WorktreeLeftBehind,
}

impl FolderFailureReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FolderMissing => "folder_missing",
            Self::NotADirectory => "not_a_directory",
            Self::RelativePath => "relative_path",
            Self::NotAGitRepo => "not_a_git_repo",
            Self::InvalidBranch => "invalid_branch",
            Self::BaseNotFound => "base_not_found",
            Self::BranchExists => "branch_exists",
            Self::BranchCheckedOut => "branch_checked_out",
            Self::WorktreePathExists => "worktree_path_exists",
            Self::NoSpace => "no_space",
            Self::PermissionDenied => "permission_denied",
            Self::GitUnavailable => "git_unavailable",
            Self::GitTimeout => "git_timeout",
            Self::GitFailed => "git_failed",
            Self::WorktreeLeftBehind => "worktree_left_behind",
        }
    }

    fn code_and_next(self) -> (&'static str, &'static str) {
        match self {
            Self::FolderMissing => (
                "invalid_arguments",
                "Pass an existing folder as cwd; nothing was created.",
            ),
            Self::NotADirectory => (
                "invalid_arguments",
                "Pass a folder, not a file, as cwd; nothing was created.",
            ),
            Self::RelativePath => (
                "invalid_arguments",
                "Pass an absolute folder; on another device use a path of that device (thread_list with all_scope, tool.thread_list({ all_scope: true }) in an eval cell, shows its folders).",
            ),
            Self::NotAGitRepo => (
                "invalid_arguments",
                "Pass a folder inside a git repository as cwd, or drop worktree to start the thread in that folder.",
            ),
            Self::InvalidBranch => (
                "invalid_arguments",
                "Pass a valid git branch name as worktree.branch.",
            ),
            Self::BaseNotFound => (
                "invalid_arguments",
                "Pass an existing commit, branch or tag as worktree.base, or drop it to start at HEAD.",
            ),
            Self::BranchExists => (
                "name_conflict",
                "Pass a new branch name as worktree.branch, or drop it to get a generated one.",
            ),
            Self::BranchCheckedOut => (
                "name_conflict",
                "That branch is checked out in another worktree; pass a new branch name as worktree.branch.",
            ),
            Self::WorktreePathExists => (
                "name_conflict",
                "A folder already exists where the worktree would go; pass another worktree.branch.",
            ),
            Self::NoSpace => (
                "host_unavailable",
                "Free disk space on that device, then retry; nothing was created.",
            ),
            Self::PermissionDenied => (
                "scope_denied",
                "The device user cannot write there; fix the folder's permissions or pick another cwd, then retry.",
            ),
            Self::GitUnavailable => (
                "unsupported",
                "Install git on that device, or drop worktree.",
            ),
            Self::GitTimeout => (
                "host_unavailable",
                "git did not answer in time on that device; retry once it is idle.",
            ),
            Self::GitFailed => (
                "invalid_arguments",
                "Read details.stderr, fix the repository state, then retry.",
            ),
            Self::WorktreeLeftBehind => (
                "partial_commit",
                "Remove what details.left names: git worktree remove --force <details.path> and git branch -D <details.branch> in details.repository; then retry.",
            ),
        }
    }
}

pub fn folder_failure(
    reason: FolderFailureReason,
    message: impl Into<String>,
    extra: Option<Map<String, Value>>,
) -> ThreadDataError {
    let (code, next) = reason.code_and_next();
    let mut details = extra.unwrap_or_default();
    details.insert(
        "reason".to_owned(),
        Value::String(reason.as_str().to_owned()),
    );
    ThreadDataError::of(thread_tool_failure(
        code,
        message,
        next,
        Some(Value::Object(details)),
    ))
}

pub fn git_failure_reason(stderr: &str) -> FolderFailureReason {
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("no space left on device") {
        FolderFailureReason::NoSpace
    } else if lower.contains("permission denied")
        || lower.contains("operation not permitted")
        || lower.contains("read-only file system")
    {
        FolderFailureReason::PermissionDenied
    } else if lower.contains("already checked out")
        || lower.contains("is already used by worktree")
        || lower.contains("already used by worktree")
    {
        FolderFailureReason::BranchCheckedOut
    } else if lower.contains("a branch named ") && lower.contains(" already exists") {
        FolderFailureReason::BranchExists
    } else if lower.contains("already exists") {
        FolderFailureReason::WorktreePathExists
    } else if lower.contains("not a git repository") {
        FolderFailureReason::NotAGitRepo
    } else if lower.contains("invalid reference")
        || lower.contains("not a valid object name")
        || lower.contains("unknown revision")
    {
        FolderFailureReason::BaseNotFound
    } else {
        FolderFailureReason::GitFailed
    }
}

pub fn errno_reason(error: &std::io::Error) -> Option<FolderFailureReason> {
    use std::io::ErrorKind;
    match error.kind() {
        ErrorKind::NotFound => Some(FolderFailureReason::FolderMissing),
        ErrorKind::NotADirectory => Some(FolderFailureReason::NotADirectory),
        ErrorKind::StorageFull => Some(FolderFailureReason::NoSpace),
        ErrorKind::PermissionDenied => Some(FolderFailureReason::PermissionDenied),
        _ => match error.raw_os_error() {
            Some(28 | 122) => Some(FolderFailureReason::NoSpace),
            Some(1 | 13 | 30) => Some(FolderFailureReason::PermissionDenied),
            Some(20) => Some(FolderFailureReason::NotADirectory),
            Some(2) => Some(FolderFailureReason::FolderMissing),
            _ => None,
        },
    }
}
