use std::path::PathBuf;

use isolation_core::{
    choose_base_dir, cleanup_isolation, ensure_isolation, read_owner_liveness, resolve_candidates,
    retain_isolation, sweep_stale_isolations, write_owner_marker, BackendKind, BaseDirIo,
    EnsureIsolationOptions, IsolationError, IsolationHandle, IsolationResult, OwnerProbe,
    SweepOptions, SweepResult,
};

#[test]
fn public_surface_exposes_the_whole_isolation_lifecycle_from_the_crate_root() {
    let _ensure: fn(EnsureIsolationOptions) -> IsolationResult<IsolationHandle> = ensure_isolation;
    let _cleanup: fn(&IsolationHandle) -> IsolationResult<()> = cleanup_isolation;
    let _retain: fn(&IsolationHandle, &str) -> IsolationResult<PathBuf> = retain_isolation;
    let _sweep: fn(&[PathBuf], &SweepOptions) -> IsolationResult<SweepResult> = sweep_stale_isolations;
    let _liveness: fn(
        &std::path::Path,
        &dyn OwnerProbe,
        u64,
    ) -> IsolationResult<isolation_core::OwnerLiveness> = read_owner_liveness;
    let _marker: fn(
        &std::path::Path,
        &str,
        &isolation_core::IsolationOwner,
        &dyn Fn(u32) -> Option<String>,
    ) -> IsolationResult<()> = write_owner_marker;
    let _base: fn(
        &std::path::Path,
        &std::path::Path,
        &str,
        &dyn BaseDirIo,
    ) -> IsolationResult<isolation_core::BaseDirSelection> = choose_base_dir;
    assert_eq!(
        resolve_candidates("darwin", None).candidates,
        vec![BackendKind::Apfs, BackendKind::Zfs, BackendKind::Rcopy]
    );
    assert_eq!(
        IsolationError::unavailable("unsupported").code(),
        Some("isolation_unavailable")
    );
}
