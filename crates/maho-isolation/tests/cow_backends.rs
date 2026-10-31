//! The ported `pi-iso` copy-on-write backends through their public dispatch, and the isolation
//! lifecycle with a copy-on-write choice. A filesystem the host lacks makes a backend report
//! `Unavailable`; those cases print why they were skipped instead of passing silently.
#![cfg_attr(
    not(target_os = "linux"),
    allow(unused_imports, dead_code, reason = "several cases are Linux-only")
)]
use maho_isolation::iso::{self, BackendKind, IsoError, IsolationBackend as _};
use maho_isolation::{BackendChoice, IsolationSettings, TaskIsolation};
use maho_task::isolation::WorkspaceIsolation;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    git(dir.path(), &["init", "-q", "-b", "main"]);
    git(dir.path(), &["config", "user.email", "t@example.com"]);
    git(dir.path(), &["config", "user.name", "T"]);
    git(dir.path(), &["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.path().join("a.txt"), "one\ntwo\n").expect("write");
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "init"]);
    dir
}

fn lower_tree(root: &Path) -> std::path::PathBuf {
    let lower = root.join("lower");
    std::fs::create_dir_all(lower.join("nested")).expect("mkdir");
    std::fs::create_dir_all(lower.join(".git")).expect("mkdir");
    std::fs::write(lower.join("file"), "file").expect("write");
    std::fs::write(lower.join("nested/child"), "child").expect("write");
    std::fs::write(lower.join(".git/config"), "skip").expect("write");
    #[cfg(unix)]
    std::os::unix::fs::symlink("file", lower.join("link")).expect("symlink");
    lower
}

/// Upstream `BackendKind::from_str`/`as_str`, `clones_tree` and the per-target auto order.
#[test]
fn kinds_parse_and_auto_order_ends_with_rcopy() {
    for kind in [
        BackendKind::Apfs,
        BackendKind::Btrfs,
        BackendKind::Zfs,
        BackendKind::LinuxReflink,
        BackendKind::Overlayfs,
        BackendKind::WindowsBlockClone,
        BackendKind::Projfs,
        BackendKind::Rcopy,
    ] {
        assert_eq!(BackendKind::from_str(kind.as_str()), Some(kind));
        assert_eq!(iso::backend(kind).kind(), kind);
    }
    assert_eq!(
        BackendKind::from_str("reflink"),
        Some(BackendKind::LinuxReflink)
    );
    assert_eq!(
        BackendKind::from_str("block-clone"),
        Some(BackendKind::WindowsBlockClone)
    );
    assert_eq!(BackendKind::from_str("nope"), None);
    assert!(BackendKind::Apfs.clones_tree());
    assert!(BackendKind::LinuxReflink.clones_tree());
    assert!(BackendKind::WindowsBlockClone.clones_tree());
    assert!(!BackendKind::Overlayfs.clones_tree());
    assert_eq!(iso::auto_order().last(), Some(&BackendKind::Rcopy));
    #[cfg(target_os = "linux")]
    assert_eq!(
        iso::auto_order(),
        &[
            BackendKind::Btrfs,
            BackendKind::Zfs,
            BackendKind::LinuxReflink,
            BackendKind::Overlayfs,
            BackendKind::Rcopy
        ]
    );
    #[cfg(target_os = "macos")]
    assert_eq!(
        iso::auto_order(),
        &[BackendKind::Apfs, BackendKind::Zfs, BackendKind::Rcopy]
    );
    #[cfg(windows)]
    assert_eq!(
        iso::auto_order(),
        &[
            BackendKind::WindowsBlockClone,
            BackendKind::Projfs,
            BackendKind::Rcopy
        ]
    );
    let resolution = iso::resolve(None);
    assert_eq!(resolution.candidates.first(), Some(&resolution.kind));
    assert_eq!(resolution.candidates.last(), Some(&BackendKind::Rcopy));
    assert!(iso::clone_candidates(None).iter().all(|k| k.clones_tree()));
}

/// Backends compiled out for this target are platform stubs: unavailable probe and start.
#[test]
fn foreign_platform_backends_are_unavailable_stubs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut foreign = Vec::new();
    #[cfg(not(target_os = "macos"))]
    foreign.push(BackendKind::Apfs);
    #[cfg(not(target_os = "linux"))]
    foreign.extend([
        BackendKind::Btrfs,
        BackendKind::LinuxReflink,
        BackendKind::Overlayfs,
    ]);
    #[cfg(not(windows))]
    foreign.extend([BackendKind::Projfs, BackendKind::WindowsBlockClone]);
    #[cfg(not(unix))]
    foreign.push(BackendKind::Zfs);
    assert!(!foreign.is_empty());
    for kind in foreign {
        let backend = iso::backend(kind);
        let probe = backend.probe();
        assert!(!probe.available, "{kind}");
        assert!(probe.reason.is_some(), "{kind}");
        let error = backend
            .start(dir.path(), &dir.path().join("m"))
            .expect_err("stub start");
        assert!(error.is_unavailable(), "{kind}: {error}");
        assert!(backend.stop(&dir.path().join("m")).is_ok());
    }
}

/// `cow::clone_file` refuses a destination that is not a regular file before touching it.
#[test]
fn clone_file_refuses_a_directory_destination() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("src"), "data").expect("write");
    std::fs::create_dir(dir.path().join("dst")).expect("mkdir");
    let error = iso::cow::clone_file(&dir.path().join("src"), &dir.path().join("dst"))
        .expect_err("directory destination");
    assert!(iso::cow::is_unsupported(&error), "{error}");
    assert!(dir.path().join("dst").is_dir());
}

/// A clone shares the source's data; a filesystem without cloning reports an unsupported error.
#[test]
fn clone_file_clones_or_reports_unsupported() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    std::fs::write(&src, "copy-on-write").expect("write");
    match iso::cow::clone_file(&src, &dst) {
        Ok(()) => {
            assert_eq!(
                std::fs::read_to_string(&dst).expect("read"),
                "copy-on-write"
            );
            std::fs::write(&dst, "changed").expect("write");
            assert_eq!(
                std::fs::read_to_string(&src).expect("read"),
                "copy-on-write"
            );
        }
        Err(error) if iso::cow::is_unsupported(&error) => {
            eprintln!(
                "skipped: {} cannot clone files ({error})",
                dir.path().display()
            );
            assert!(!dst.exists());
        }
        Err(error) => panic!("clone_file: {error}"),
    }
}

/// ZFS only accepts an exact dataset mountpoint; a plain temp directory is unavailable.
#[cfg(unix)]
#[test]
fn zfs_rejects_a_directory_that_is_not_a_dataset_mountpoint() {
    let dir = tempfile::tempdir().expect("tempdir");
    let lower = lower_tree(dir.path());
    let error = iso::backend(BackendKind::Zfs)
        .start(&lower, &dir.path().join("m"))
        .expect_err("not a dataset");
    assert!(error.is_unavailable(), "{error}");
}

/// btrfs only snapshots a subvolume root; a fresh temp directory never is one.
#[cfg(target_os = "linux")]
#[test]
fn btrfs_rejects_a_directory_that_is_not_a_subvolume_root() {
    let dir = tempfile::tempdir().expect("tempdir");
    let lower = lower_tree(dir.path());
    let merged = dir.path().join("m");
    let error = iso::backend(BackendKind::Btrfs)
        .start(&lower, &merged)
        .expect_err("not a subvolume");
    assert!(error.is_unavailable(), "{error}");
    assert!(error.message().contains("not a btrfs subvolume"), "{error}");
    assert!(!merged.exists());
}

/// `FICLONE` tree: directories and symlinks recreated, files cloned, `.git` skipped at the top.
#[cfg(target_os = "linux")]
#[test]
fn reflink_clone_tree_skips_top_level_entry_and_keeps_symlinks() {
    let dir = tempfile::tempdir().expect("tempdir");
    let lower = lower_tree(dir.path());
    let merged = dir.path().join("merged");
    let backend = iso::backend(BackendKind::LinuxReflink);
    match backend.clone_tree(&lower, &merged, &[std::ffi::OsStr::new(".git")]) {
        Err(IsoError::Unavailable(reason)) => {
            eprintln!(
                "skipped: {} does not support FICLONE ({reason})",
                dir.path().display()
            );
            assert!(!merged.exists(), "a failed clone leaves nothing behind");
            return;
        }
        Err(error) => panic!("clone_tree: {error}"),
        Ok(()) => {}
    }
    assert!(!merged.join(".git").exists());
    assert_eq!(
        std::fs::read_to_string(merged.join("file")).expect("read"),
        "file"
    );
    assert_eq!(
        std::fs::read_to_string(merged.join("nested/child")).expect("read"),
        "child"
    );
    assert_eq!(
        std::fs::read_link(merged.join("link")).expect("link"),
        Path::new("file")
    );
    backend.stop(&merged).expect("stop");
    assert!(!merged.exists());
}

/// Kernel overlay (or `fuse-overlayfs`): writes land in `upper`, `lower` stays untouched.
#[cfg(target_os = "linux")]
#[test]
fn overlayfs_mounts_a_writable_view_or_reports_unavailable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let lower = lower_tree(dir.path());
    let merged = dir.path().join("base/m");
    let backend = iso::backend(BackendKind::Overlayfs);
    match backend.start(&lower, &merged) {
        Err(IsoError::Unavailable(reason)) => {
            eprintln!("skipped: overlay mounts are not permitted here ({reason})");
            return;
        }
        Err(error) => {
            eprintln!("skipped: overlay mount failed on this host ({error})");
            let _ = backend.stop(&merged);
            return;
        }
        Ok(()) => {}
    }
    assert_eq!(
        std::fs::read_to_string(merged.join("file")).expect("read"),
        "file"
    );
    std::fs::write(merged.join("file"), "changed").expect("write");
    assert_eq!(
        std::fs::read_to_string(lower.join("file")).expect("read"),
        "file"
    );
    backend.stop(&merged).expect("stop");
    assert!(!merged.exists());
    assert!(!dir.path().join("base/upper").exists());
}

/// Lifecycle with `task.isolation.backend: reflink`: the change comes back either from a reflink
/// clone or, where the filesystem cannot clone, from the worktree fallback that names the reason.
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread")]
async fn reflink_choice_isolates_or_falls_back_with_the_reason() {
    let repo = repo();
    let wt = tempfile::tempdir().expect("wt");
    let settings = IsolationSettings {
        backend: BackendChoice::Cow(BackendKind::LinuxReflink),
        ..IsolationSettings::default()
    };
    let iso = TaskIsolation::with_settings(wt.path(), Arc::new(move |_| settings.clone()));
    let ws = iso.prepare("Cow", repo.path()).await.expect("prepare");
    std::fs::write(ws.cwd.join("a.txt"), "one\ntwo\nthree\n").expect("write");
    let note = iso.finish(&ws, true).await.expect("finish").expect("note");
    if note.starts_with("Isolation (reflink): applied 1 changed file") {
        assert!(!ws.cwd.exists());
    } else {
        assert!(
            note.starts_with("Isolation (worktree), fell back: FICLONE unsupported"),
            "{note}"
        );
        eprintln!("skipped reflink path: the temp filesystem cannot clone ({note})");
    }
    assert_eq!(
        std::fs::read_to_string(repo.path().join("a.txt")).expect("read"),
        "one\ntwo\nthree\n"
    );
}

/// Lifecycle with a backend this target cannot run: the `auto` worktree with the probe reason.
#[cfg(not(windows))]
#[tokio::test(flavor = "multi_thread")]
async fn platform_foreign_choice_falls_back_to_the_worktree() {
    let repo = repo();
    let wt = tempfile::tempdir().expect("wt");
    let settings = IsolationSettings {
        backend: BackendChoice::Cow(BackendKind::Projfs),
        ..IsolationSettings::default()
    };
    let iso = TaskIsolation::with_settings(wt.path(), Arc::new(move |_| settings.clone()));
    let ws = iso.prepare("Projfs", repo.path()).await.expect("prepare");
    let note = iso.finish(&ws, true).await.expect("finish").expect("note");
    assert!(
        note.starts_with(
            "Isolation (worktree), fell back: ProjFS isolation is only available on Windows: no changes."
        ),
        "{note}"
    );
}
