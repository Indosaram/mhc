//! isolation/runtime.rs: the isolation capability the task manager consumes, as a port.
//!
//! The production object below is the real maho-isolation-core; tests inject the same trait with a
//! temp home_dir so a clone never lands in the developer's ~/.omo/wt, and fake OwnerProbes so a
//! sweep decision is deterministic.
//!
//! ## Producer API (maho-isolation-core @ the isolation worktree, owner dag_5a0b8081)
//!
//! Bound to the ACTUAL published crate root (src/lib.rs):
//!   - ensure::{ensure_isolation(EnsureIsolationOptions) -> Result<IsolationHandle>,
//!     cleanup_isolation(&IsolationHandle) -> Result<()>, retain_isolation(&IsolationHandle, &str)
//!     -> Result<PathBuf>, relocate_sandbox, EnsureIsolationOptions, IsolationHandle}
//!   - merge::{merge_isolated_changes(IsolationMergeOptions) -> Result<IsolationMergeResult>,
//!     IsolationMergeOptions, MergeMode}
//!   - sweep::{sweep_stale_isolations(&[PathBuf], &SweepOptions) -> Result<SweepResult>,
//!     SweepOptions { backends, probe: Option<Arc<dyn OwnerProbe>>, now }}
//!   - git::baseline::{capture_baseline_default, WorktreeBaseline}, git::delta::{capture_delta_patch,
//!     DeltaPatchResult}, git::command::{run_git, GitOptions, str_args}
//!   - owner::{IsolationOwner, HostOwner, OwnerChild, OwnerProbe, write_owner_marker_default}
//!   - the 7 backend impls (RcopyBackend is a unit struct; the rest impl Default)
//!
//! ## The producer's merge contract (source-read)
//!
//! merge_isolated_changes NEVER tears down isolation: it writes artifacts under
//! <artifacts_dir>/isolation/<id>/, and when apply=false it returns Retained WITHOUT replay. The
//! caller retains the sandbox on an artifact-write or replay failure. The delta is optional; when
//! absent the producer captures it from isolation_dir against the baseline.

use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use isolation_core::{
    BackendKind, DeltaPatchResult, IsolationBackend, IsolationError, IsolationHandle,
    IsolationMergeOptions, IsolationMergeResult, IsolationOwner, OwnerProbe, SweepResult,
    WorktreeBaseline,
};

/// EnsureInput: the request the manager makes before a child launch.
#[derive(Clone, Debug)]
pub struct EnsureInput {
    pub repo_root: PathBuf,
    pub id: String,
    /// The caller's preferred backend. 'None' is the config's "auto" sentinel - let the producer
    /// choose - and is forwarded unchanged to the producer's optional 'EnsureIsolationOptions.preferred'.
    pub preferred: Option<BackendKind>,
    pub owner: Option<IsolationOwner>,
}

/// The isolation capability the task manager consumes, as a port (TS IsolationRuntime).
///
/// The TS surface is promise-returning; the native manager is synchronous (runners block until the
/// child exists and outcome tracking runs on a watcher thread), so every method here blocks and
/// returns its value directly.
pub trait IsolationRuntime: Send + Sync {
    /// The git checkout that owns cwd, or None when cwd is not inside a git repository.
    fn resolve_repo_root(&self, cwd: &Path) -> Option<PathBuf>;
    fn capture_baseline(&self, repo_root: &Path) -> Result<WorktreeBaseline, IsolationError>;
    fn ensure(&self, input: &EnsureInput) -> Result<IsolationHandle, IsolationError>;
    fn capture_delta(
        &self,
        merged_dir: &Path,
        baseline: &WorktreeBaseline,
    ) -> Result<DeltaPatchResult, IsolationError>;
    /// Merges (or, with apply=false, writes artifacts and returns Retained). Takes options by value,
    /// matching the producer. Never tears down isolation.
    fn merge(
        &self,
        options: IsolationMergeOptions,
    ) -> Result<IsolationMergeResult, IsolationError>;
    fn cleanup(&self, handle: &IsolationHandle);
    /// Rename the clone aside; returns the retained path.
    fn retain(&self, handle: &IsolationHandle, reason: &str) -> Result<PathBuf, IsolationError>;
    /// Re-stamp ownership once the child's identity (pid or host session) is known.
    fn write_owner(&self, base_dir: &Path, id: &str, owner: &IsolationOwner);
    fn sweep(
        &self,
        roots: &[PathBuf],
        probe: Arc<dyn OwnerProbe>,
    ) -> Result<SweepResult, IsolationError>;
    /// Where THIS runtime places clones; the startup sweep scans these plus any root on a record.
    fn sweep_roots(&self) -> Vec<PathBuf>;
}

/// Every backend this platform could pick, so a sweep can also tear down a clone it did not create.
pub fn isolation_backends() -> Vec<Arc<dyn IsolationBackend>> {
    vec![
        Arc::new(isolation_core::ApfsBackend::default()),
        Arc::new(isolation_core::BtrfsBackend::default()),
        Arc::new(isolation_core::ZfsBackend::default()),
        Arc::new(isolation_core::ReflinkBackend::default()),
        Arc::new(isolation_core::OverlayfsBackend::default()),
        Arc::new(isolation_core::BlockCloneBackend::default()),
        Arc::new(isolation_core::RcopyBackend),
    ]
}

/// Options of create_isolation_runtime (TS { homeDir? }).
#[derive(Clone, Debug, Default)]
pub struct IsolationRuntimeOptions {
    /// The OS home directory (`HOME` / `USERPROFILE`); the sandbox lands at `<home_dir>/.omo/wt`.
    /// Defaults to [`os_home_dir`]. This is NOT the `.maho` profile root.
    pub home_dir: Option<PathBuf>,
}

/// The production object over maho-isolation-core (TS createIsolationRuntime).
pub struct CoreIsolationRuntime {
    home_dir: PathBuf,
    backends: Vec<Arc<dyn IsolationBackend>>,
}

impl CoreIsolationRuntime {
    pub fn new(options: &IsolationRuntimeOptions) -> Self {
        Self {
            home_dir: options.home_dir.clone().unwrap_or_else(os_home_dir),
            backends: isolation_backends(),
        }
    }
}

impl IsolationRuntime for CoreIsolationRuntime {
    fn resolve_repo_root(&self, cwd: &Path) -> Option<PathBuf> {
        // git rev-parse --show-toplevel; any failure is not-a-git-checkout.
        let output = isolation_core::run_git(
            &isolation_core::str_args(&["rev-parse", "--show-toplevel"]),
            &isolation_core::GitOptions::new(cwd.to_path_buf()),
        )
        .ok()?;
        let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
        (!root.is_empty()).then(|| PathBuf::from(root))
    }

    fn capture_baseline(&self, repo_root: &Path) -> Result<WorktreeBaseline, IsolationError> {
        isolation_core::capture_baseline_default(repo_root)
    }

    fn ensure(&self, input: &EnsureInput) -> Result<IsolationHandle, IsolationError> {
        isolation_core::ensure_isolation(isolation_core::EnsureIsolationOptions {
            repo_root: input.repo_root.clone(),
            id: input.id.clone(),
            preferred: input.preferred,
            backends: self.backends.clone(),
            platform: None,
            home_dir: Some(self.home_dir.clone()),
            owner: input.owner.clone(),
            max_copy_bytes: None,
        })
    }

    fn capture_delta(
        &self,
        merged_dir: &Path,
        baseline: &WorktreeBaseline,
    ) -> Result<DeltaPatchResult, IsolationError> {
        isolation_core::capture_delta_patch(merged_dir, baseline)
    }

    fn merge(
        &self,
        options: IsolationMergeOptions,
    ) -> Result<IsolationMergeResult, IsolationError> {
        isolation_core::merge_isolated_changes(options)
    }

    fn cleanup(&self, handle: &IsolationHandle) {
        // A cleanup failure is logged, not propagated: the caller's terminal path must still land.
        if let Err(error) = isolation_core::cleanup_isolation(handle) {
            utils::logger::log(
                "senpi-task isolation cleanup failed",
                Some(&serde_json::json!({ "base_dir": handle.base_dir.display().to_string(), "error": error.to_string() })),
            );
        }
    }

    fn retain(&self, handle: &IsolationHandle, reason: &str) -> Result<PathBuf, IsolationError> {
        isolation_core::retain_isolation(handle, reason)
    }

    fn write_owner(&self, base_dir: &Path, id: &str, owner: &IsolationOwner) {
        // Ownership re-stamping is best-effort: a failed marker must not fail the launch.
        let _ = isolation_core::write_owner_marker_default(base_dir, id, owner);
    }

    fn sweep(
        &self,
        roots: &[PathBuf],
        probe: Arc<dyn OwnerProbe>,
    ) -> Result<SweepResult, IsolationError> {
        isolation_core::sweep_stale_isolations(
            roots,
            &isolation_core::SweepOptions {
                backends: self.backends.clone(),
                probe: Some(probe),
                now: None,
            },
        )
    }

    fn sweep_roots(&self) -> Vec<PathBuf> {
        // The producer's choose_base_dir(repo_root, home_dir, id) places a SAME-DEVICE sandbox at
        // `<home_dir>/.omo/wt/<segment>`, so the startup sweep scans that parent. `home_dir` here is
        // the OS home (HOME / USERPROFILE) unless the caller overrode
        // IsolationRuntimeOptions.home_dir; it is NOT the `.maho` profile root (that is the
        // agent/config home, a separate concept). The `.omo` segment is the SOURCE IDENTITY and is
        // not renamed, and no path relocation is performed here - this mirrors upstream
        // createIsolationRuntime's `sweepRoots: [join(homeDir, ".omo", "wt")]`. A cross-device
        // sandbox lives at a volume root's `.omo-wt/<segment>`; those roots come from a record's
        // base_dir via sweep_roots_for, never from this fixed root.
        vec![self.home_dir.join(".omo").join("wt")]
    }
}

/// The production object (TS createIsolationRuntime).
pub fn create_isolation_runtime(options: &IsolationRuntimeOptions) -> Arc<dyn IsolationRuntime> {
    Arc::new(CoreIsolationRuntime::new(options))
}

/// The OS home directory (`HOME` / `USERPROFILE`); the default for `IsolationRuntimeOptions.home_dir`
/// (the producer's `util::home_dir()`). This is the OS home, NOT the `.maho` profile root.
fn os_home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map_or_else(PathBuf::new, PathBuf::from)
}
