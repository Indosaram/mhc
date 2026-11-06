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
    BACKEND_FILE, BackendKind, Candidates, IsolationBackend, IsolationContext, IsolationError,
    ProbeResult, Result as IsolationResult, StartDetail, error_text, is_backend_kind,
    platform_name, resolve_candidates,
};
pub use backend_marker::mark_started;
pub use base_dir::{BaseDirIo, BaseDirSelection, FilesystemBaseDirIo, choose_base_dir};
pub use ensure::{
    EnsureIsolationOptions, IsolationHandle, cleanup_isolation, ensure_isolation, relocate_sandbox,
    retain_isolation,
};
pub use owner::{
    HostOwner, IsolationOwner, OWNER_FILE, OwnerChild, OwnerLiveness, OwnerMarker,
    OwnerMarkerChild, OwnerProbe, OwnerStatus, ProcessOwner, hostname, read_owner_liveness,
    read_owner_liveness_now, write_owner_marker, write_owner_marker_default,
};
pub use process_identity::{
    ProcessOwnerProbe, get_process_start_identity, process_owner_probe, start_identities_conflict,
};
pub use sweep::{BackendRef, SkippedEntry, SweepOptions, SweepResult, sweep_stale_isolations};

pub use backends::NativeLoadError;
pub use backends::apfs::{ApfsBackend, CLONE_NOFOLLOW, CloneSymbols, LoadApfs, clone_with_symbols};
pub use backends::block_clone::{
    BlockCloneBackend, LoadWindowsApi, WindowsCloneApi, WindowsSymbols, create_windows_clone_api,
    duplicate_extents,
};
pub use backends::btrfs::BtrfsBackend;
pub use backends::copy_tree::{
    CopyBudget, DEFAULT_MAX_COPY_BYTES, SpaceFn, copy_budget, copy_tree,
};
pub use backends::overlayfs::OverlayfsBackend;
pub use backends::rcopy::{RcopyBackend, seed_dirty_state};
pub use backends::reflink::{FICLONE, LoadReflink, ReflinkBackend, ReflinkIoctl};
pub use backends::runtime::{
    BackendRuntime, CommandResult, SystemRuntime, checked, existing_parent,
};
pub use backends::zfs::ZfsBackend;

pub use git::baseline::{
    BaselineCaptureOptions, BaselineReadRetryDetails, ISOLATION_BASELINE_MAX_CONTENT_BYTES,
    NestedBaseline, RepoBaseline, WorktreeBaseline, capture_baseline, capture_baseline_default,
    capture_repo_baseline, discover_nested_repos,
};
pub use git::command::{GitOptions, GitOutput, OutputLimitError, SpawnObserver, run_git, str_args};
pub use git::delta::{DeltaPatchResult, NestedPatch, capture_delta_patch};
pub use git::detach_git_dir::{
    DetachOutcome, NestedGitResult, detach_git_dir, scan_nested_git_dirs,
};
pub use git::synthetic_tree::{
    parse_diff_git_line_paths, unquote_git_diff_path, write_synthetic_tree,
};

pub use merge::branch_mode::{
    BranchOptions, CommitMessage, TaskBranch, commit_to_branch, commit_to_branch_locked,
    merge_task_branch, merge_task_branch_locked,
};
pub use merge::patch_mode::{
    NestedOutcome, apply_delta_patch, apply_delta_patch_locked, apply_nested_patches,
};
pub use merge::shared::{
    ArtifactOptions, IsolationMergeResult, LockEvent, LockHook, MergeKind, MergeState,
    NestedFailure, WrittenArtifacts, nested_path, stash_pop, stash_push, summarize, task_branch,
    with_repo_lock, write_artifacts,
};
pub use merge::{IsolationMergeOptions, MergeMode, merge_isolated_changes};

pub use util::{
    dirname, home_dir, is_at_or_below, mkdtemp, mkdtemp_in, mtime_ms, now_iso8601, now_ms,
    random_hex, relative_path, resolve_path, sha1_hex,
};
