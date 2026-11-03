//! Materialising and removing an isolated directory. Port of the MIT oh-my-pi `pi-iso` rcopy
//! backend (`rcopy.rs`): a detached `git worktree` seeded with the source's staged, unstaged and
//! untracked state when the source is a git work tree, a recursive copy otherwise. An explicit
//! copy-on-write choice (APFS, btrfs, ZFS, reflink, overlayfs, ProjFS, block clone) runs the ported
//! `pi-iso` backend ([`crate::iso`]) and falls back to the worktree/copy pair when the backend is
//! unavailable, as upstream `ensureIsolation` walks its candidates.
use crate::iso::{self, IsoError};
use crate::settings::BackendChoice;
use crate::tree;
use maho_git::{ApplyOptions, Git};
use std::path::Path;

/// The backend actually used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendKind {
    /// `git worktree add --detach` + seeded dirty state.
    Worktree,
    /// Recursive copy (of the whole repository, `.git` included, inside git).
    Copy,
    /// A copy-on-write `pi-iso` backend (never [`iso::BackendKind::Rcopy`], which is
    /// `Worktree`/`Copy`). Like `Copy`, the whole repository, `.git` included, is materialised.
    Cow(iso::BackendKind),
}

impl BackendKind {
    /// The `task.isolation.backend` label (upstream `formatIsolationBackend`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Worktree => "worktree",
            Self::Copy => "copy",
            Self::Cow(kind) => setting_label(kind),
        }
    }

    /// Inverse of [`Self::as_str`] (the owner marker stores the label); unknown labels are `None`.
    pub fn from_label(label: &str) -> Option<Self> {
        match label {
            "worktree" => Some(Self::Worktree),
            "copy" => Some(Self::Copy),
            other => match iso::BackendKind::from_str(other)? {
                iso::BackendKind::Rcopy => None,
                kind => Some(Self::Cow(kind)),
            },
        }
    }
}

/// Upstream `formatIsolationBackend`: the setting label of a native backend.
pub const fn setting_label(kind: iso::BackendKind) -> &'static str {
    match kind {
        iso::BackendKind::Apfs => "apfs",
        iso::BackendKind::Btrfs => "btrfs",
        iso::BackendKind::Zfs => "zfs",
        iso::BackendKind::LinuxReflink => "reflink",
        iso::BackendKind::Overlayfs => "overlayfs",
        iso::BackendKind::Projfs => "projfs",
        iso::BackendKind::WindowsBlockClone => "block-clone",
        iso::BackendKind::Rcopy => "rcopy",
    }
}

/// The `auto` choice: a git worktree inside git, a copy outside.
const fn auto_kind(is_git: bool) -> BackendKind {
    if is_git {
        BackendKind::Worktree
    } else {
        BackendKind::Copy
    }
}

/// Picks the backend for `choice`; the second value explains a fallback.
///
/// A copy-on-write choice whose host probe fails (upstream `isoResolve` with a preferred kind), or
/// that would copy a linked worktree's `.git` pointer file (upstream detaches the git dir first,
/// which is not ported), falls back to the `auto` choice with the reason.
///
/// # Errors
/// `copy` was asked for a linked git worktree (its `.git` is a pointer file a copy cannot carry).
pub fn resolve(
    choice: &BackendChoice,
    root: &Path,
    is_git: bool,
) -> Result<(BackendKind, Option<String>), String> {
    match choice {
        BackendChoice::Auto => Ok((auto_kind(is_git), None)),
        BackendChoice::Unsupported(name) => Ok((
            auto_kind(is_git),
            Some(format!("backend `{name}` is not available in this build")),
        )),
        BackendChoice::Cow(kind) => {
            let label = setting_label(*kind);
            if is_git && !root.join(".git").is_dir() {
                return Ok((
                    auto_kind(is_git),
                    Some(format!(
                        "backend `{label}` cannot isolate {}: it is a linked git worktree",
                        root.display()
                    )),
                ));
            }
            let probe = iso::backend(*kind).probe();
            if probe.available {
                Ok((BackendKind::Cow(*kind), None))
            } else {
                Ok((
                    auto_kind(is_git),
                    Some(probe.reason.unwrap_or_else(|| {
                        format!("backend `{label}` is not available on this host")
                    })),
                ))
            }
        }
        BackendChoice::Copy => {
            if is_git && !root.join(".git").is_dir() {
                return Err(format!(
                    "the copy backend cannot isolate {}: it is a linked git worktree (use backend `auto`)",
                    root.display()
                ));
            }
            Ok((BackendKind::Copy, None))
        }
    }
}

/// Why [`start_with_fallback`] could not materialise `merged`.
#[derive(Debug)]
pub enum StartError {
    /// The copy-on-write backend cannot serve this source/destination pair (upstream
    /// `isoIsUnavailableError`); the worktree/copy pair can still be tried.
    Unavailable(String),
    Failed(String),
}

/// [`start`] with upstream `ensureIsolation`'s candidate walk: when a copy-on-write backend is
/// unavailable for this path pair (a non-subvolume btrfs source, a cross-device reflink, a source
/// that is not a ZFS mountpoint, a refused overlay mount), its partial state is torn down and the
/// `auto` choice is used instead. Returns the backend used and the fallback reason.
///
/// # Errors
/// The final backend failed.
pub fn start_with_fallback(
    kind: BackendKind,
    root: &Path,
    merged: &Path,
    is_git: bool,
) -> Result<(BackendKind, Option<String>), String> {
    match start_checked(kind, root, merged) {
        Ok(()) => Ok((kind, None)),
        Err(StartError::Unavailable(reason)) => {
            stop(kind, root, merged);
            let fallback = auto_kind(is_git);
            if let Err(error) = start(fallback, root, merged) {
                stop(fallback, root, merged);
                return Err(error);
            }
            Ok((fallback, Some(reason)))
        }
        Err(StartError::Failed(error)) => Err(error),
    }
}

fn start_checked(kind: BackendKind, root: &Path, merged: &Path) -> Result<(), StartError> {
    match kind {
        BackendKind::Cow(cow) => {
            if let Some(parent) = merged.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| StartError::Failed(format!("create {}: {e}", parent.display())))?;
            }
            iso::backend(cow)
                .start(root, merged)
                .map_err(|error| match error {
                    IsoError::Unavailable(reason) => StartError::Unavailable(reason),
                    IsoError::Other(message) => StartError::Failed(message),
                })
        }
        other => start(other, root, merged).map_err(StartError::Failed),
    }
}

/// Creates `merged` from `root` with `kind`.
///
/// # Errors
/// Any git or filesystem step failed (the caller removes the partial directory).
pub fn start(kind: BackendKind, root: &Path, merged: &Path) -> Result<(), String> {
    if let BackendKind::Cow(cow) = kind {
        return iso::backend(cow)
            .start(root, merged)
            .map_err(|error| error.to_string());
    }
    if let Some(parent) = merged.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    tree::remove_existing(merged).map_err(|e| format!("clear {}: {e}", merged.display()))?;
    match kind {
        BackendKind::Worktree => {
            let source = Git::new(root);
            if source.head().is_none() {
                return Err(
                    "the repository has no commit yet; a worktree needs one (use backend `copy`)"
                        .into(),
                );
            }
            let target = merged.to_string_lossy().into_owned();
            source
                .run(&["worktree", "add", "--detach", &target, "HEAD"])
                .map_err(|e| e.to_string())?;
            seed_dirty_state(root, merged)
        }
        BackendKind::Cow(_) => Ok(()),
        BackendKind::Copy => {
            std::fs::create_dir_all(merged)
                .map_err(|e| format!("create {}: {e}", merged.display()))?;
            tree::copy_dir_contents(root, merged, &[])
                .map_err(|e| format!("copy {}: {e}", root.display()))
        }
    }
}

/// Makes the fresh worktree at `merged` mirror `root`'s live state: staged changes into index and
/// tree, unstaged changes into the tree, untracked (non-ignored) paths copied.
fn seed_dirty_state(root: &Path, merged: &Path) -> Result<(), String> {
    let source = Git::new(root);
    let target = Git::new(merged);
    let staged = source
        .run(&["diff", "--binary", "--no-color", "--cached"])
        .map_err(|e| e.to_string())?;
    if !staged.is_empty() {
        target
            .apply(
                &staged,
                ApplyOptions {
                    index: true,
                    ..ApplyOptions::default()
                },
            )
            .map_err(|e| e.to_string())?;
    }
    let unstaged = source
        .run(&["diff", "--binary", "--no-color"])
        .map_err(|e| e.to_string())?;
    if !unstaged.is_empty() {
        target
            .apply(&unstaged, ApplyOptions::default())
            .map_err(|e| e.to_string())?;
    }
    for rel in source.untracked().map_err(|e| e.to_string())? {
        tree::copy_path(&root.join(&rel), &merged.join(&rel))
            .map_err(|e| format!("copy {rel}: {e}"))?;
    }
    Ok(())
}

/// Removes `merged` (unregistering a worktree, or unmounting/destroying a copy-on-write view,
/// first).
pub fn stop(kind: BackendKind, root: &Path, merged: &Path) {
    if let BackendKind::Cow(cow) = kind {
        let _ = iso::backend(cow).stop(merged);
    }
    if kind == BackendKind::Worktree {
        let target = merged.to_string_lossy().into_owned();
        let _ = Git::new(root).run(&["worktree", "remove", "--force", &target]);
    }
    let _ = tree::remove_existing(merged);
    if kind == BackendKind::Worktree {
        let _ = Git::new(root).run(&["worktree", "prune"]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_round_trip_through_the_owner_marker() {
        for kind in [
            BackendKind::Worktree,
            BackendKind::Copy,
            BackendKind::Cow(iso::BackendKind::Apfs),
            BackendKind::Cow(iso::BackendKind::Btrfs),
            BackendKind::Cow(iso::BackendKind::Zfs),
            BackendKind::Cow(iso::BackendKind::LinuxReflink),
            BackendKind::Cow(iso::BackendKind::Overlayfs),
            BackendKind::Cow(iso::BackendKind::Projfs),
            BackendKind::Cow(iso::BackendKind::WindowsBlockClone),
        ] {
            assert_eq!(BackendKind::from_label(kind.as_str()), Some(kind));
        }
        assert_eq!(BackendKind::from_label("rcopy"), None);
        assert_eq!(BackendKind::from_label("hammerfs"), None);
        assert_eq!(
            BackendKind::Cow(iso::BackendKind::LinuxReflink).as_str(),
            "reflink"
        );
        assert_eq!(
            BackendKind::Cow(iso::BackendKind::WindowsBlockClone).as_str(),
            "block-clone"
        );
    }

    /// A backend whose host probe fails (upstream `isoResolve` with a preferred kind) falls back
    /// to the `auto` choice and carries the probe's reason.
    #[test]
    fn unavailable_copy_on_write_backend_falls_back_with_the_probe_reason() {
        #[cfg(not(windows))]
        let (kind, reason) = (
            iso::BackendKind::Projfs,
            "ProjFS isolation is only available on Windows",
        );
        #[cfg(windows)]
        let (kind, reason) = (
            iso::BackendKind::Apfs,
            "APFS clonefile isolation is only available on macOS",
        );
        let dir = std::env::temp_dir();
        let (picked, fallback) = resolve(&BackendChoice::Cow(kind), &dir, false).expect("resolve");
        assert_eq!(picked, BackendKind::Copy);
        assert_eq!(fallback.as_deref(), Some(reason));
    }

    /// A start-time unavailability (btrfs on a directory that is no subvolume root) falls back to
    /// the `auto` choice after tearing down the partial view.
    #[cfg(target_os = "linux")]
    #[test]
    fn start_time_unavailability_falls_back_to_a_copy() {
        let root = tempfile::tempdir().expect("tempdir");
        let lower = root.path().join("lower");
        std::fs::create_dir_all(lower.join("nested")).expect("mkdir");
        std::fs::write(lower.join("nested/file"), "data").expect("write");
        let merged = root.path().join("base/m");
        let (used, reason) = start_with_fallback(
            BackendKind::Cow(iso::BackendKind::Btrfs),
            &lower,
            &merged,
            false,
        )
        .expect("start");
        assert_eq!(used, BackendKind::Copy);
        let reason = reason.expect("reason");
        assert!(
            reason.contains("not a btrfs subvolume") || reason.contains("btrfs"),
            "{reason}"
        );
        assert_eq!(
            std::fs::read_to_string(merged.join("nested/file")).expect("read"),
            "data"
        );
    }
}
