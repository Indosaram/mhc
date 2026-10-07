pub mod backend;
pub mod backend_marker;
pub mod backends;
pub mod base_dir;
pub mod ensure;
pub mod git;
pub mod merge;
pub mod owner;
pub mod process_identity;
pub mod sweep;
pub mod util;

pub mod test_support;

pub use backend::{
    error_text, is_backend_kind, platform_name, resolve_candidates, BackendKind, Candidates,
    IsolationBackend, IsolationContext, IsolationError, ProbeResult, Result as IsolationResult,
    StartDetail, BACKEND_FILE,
};
pub use backend_marker::mark_started;
pub use base_dir::{choose_base_dir, BaseDirIo, BaseDirSelection, FilesystemBaseDirIo};
pub use ensure::{
    cleanup_isolation, ensure_isolation, relocate_sandbox, retain_isolation, EnsureIsolationOptions,
    IsolationHandle,
};
pub use owner::{
    hostname, read_owner_liveness, read_owner_liveness_now, write_owner_marker,
    write_owner_marker_default, HostOwner, IsolationOwner, OwnerChild, OwnerLiveness, OwnerMarker,
    OwnerMarkerChild, OwnerProbe, OwnerStatus, ProcessOwner, OWNER_FILE,
};
pub use process_identity::{
    get_process_start_identity, process_owner_probe, start_identities_conflict, ProcessOwnerProbe,
};
pub use sweep::{sweep_stale_isolations, BackendRef, SkippedEntry, SweepOptions, SweepResult};

pub use backends::apfs::{clone_with_symbols, ApfsBackend, CloneSymbols, LoadApfs, CLONE_NOFOLLOW};
pub use backends::block_clone::{
    create_windows_clone_api, duplicate_extents, BlockCloneBackend, LoadWindowsApi, WindowsCloneApi,
    WindowsSymbols,
};
pub use backends::btrfs::BtrfsBackend;
pub use backends::copy_tree::{copy_budget, copy_tree, CopyBudget, SpaceFn, DEFAULT_MAX_COPY_BYTES};
pub use backends::overlayfs::OverlayfsBackend;
pub use backends::rcopy::{seed_dirty_state, RcopyBackend};
pub use backends::reflink::{ReflinkBackend, ReflinkIoctl, LoadReflink, FICLONE};
pub use backends::runtime::{
    checked, existing_parent, BackendRuntime, CommandResult, SystemRuntime,
};
pub use backends::zfs::ZfsBackend;
pub use backends::NativeLoadError;

pub use git::baseline::{
    capture_baseline, capture_baseline_default, capture_repo_baseline, discover_nested_repos,
    BaselineCaptureOptions, BaselineReadRetryDetails, NestedBaseline, RepoBaseline,
    WorktreeBaseline, ISOLATION_BASELINE_MAX_CONTENT_BYTES,
};
pub use git::command::{run_git, str_args, GitOptions, GitOutput, OutputLimitError, SpawnObserver};
pub use git::delta::{capture_delta_patch, DeltaPatchResult, NestedPatch};
pub use git::detach_git_dir::{detach_git_dir, scan_nested_git_dirs, DetachOutcome, NestedGitResult};
pub use git::synthetic_tree::{
    parse_diff_git_line_paths, unquote_git_diff_path, write_synthetic_tree,
};

pub use merge::branch_mode::{
    commit_to_branch, commit_to_branch_locked, merge_task_branch, merge_task_branch_locked,
    BranchOptions, CommitMessage, TaskBranch,
};
pub use merge::patch_mode::{
    apply_delta_patch, apply_delta_patch_locked, apply_nested_patches, NestedOutcome,
};
pub use merge::shared::{
    nested_path, stash_pop, stash_push, summarize, task_branch, with_repo_lock, write_artifacts,
    ArtifactOptions, IsolationMergeResult, LockEvent, LockHook, MergeKind, MergeState,
    NestedFailure, WrittenArtifacts,
};
pub use merge::{merge_isolated_changes, IsolationMergeOptions, MergeMode};

pub use util::{
    dirname, home_dir, is_at_or_below, mkdtemp, mkdtemp_in, mtime_ms, now_iso8601, now_ms,
    random_hex, relative_path, resolve_path, sha1_hex,
};
