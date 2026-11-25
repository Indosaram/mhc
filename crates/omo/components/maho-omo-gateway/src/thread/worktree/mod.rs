//! Git worktree creation & rollback for `thread_create` (`thread/worktree/*`).

pub mod discard_worktree;
pub mod failures;
pub mod thread_worktree;
pub mod worktree_rpc;
pub mod worktree_rpc_name;

pub use discard_worktree::{discard_worktree, inside_root};
pub use failures::{FolderFailureReason, errno_reason, folder_failure, git_failure_reason};
pub use thread_worktree::{
    GitResult, GitRun, ThreadWorktree, WorktreeOutcome, WorktreeRequest, check_folder,
    create_thread_worktree, run_git,
};
pub use worktree_rpc::{WorktreeRemoval, WorktreeRpcAnswer, answer_worktree_request};
pub use worktree_rpc_name::THREAD_WORKTREE_RPC;
