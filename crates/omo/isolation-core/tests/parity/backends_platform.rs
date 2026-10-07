use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use isolation_core::{
    checked, create_windows_clone_api, duplicate_extents, BtrfsBackend, IsolationBackend,
    IsolationContext, IsolationError, OverlayfsBackend, ReflinkBackend, WindowsCloneApi,
    WindowsSymbols, ZfsBackend, LoadReflink, BlockCloneBackend,
};

use crate::fake::{calls, ok, record, recorded, result, Calls, FakeRuntime};

struct Paths {
    root: PathBuf,
    repo_root: PathBuf,
    base_dir: PathBuf,
    merged: PathBuf,
    _cleanup: TempRoot,
}

struct TempRoot(PathBuf);

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn paths() -> Paths {
    let root = std::env::temp_dir().join(format!(
        "isolation-core-posix-{}",
        isolation_core::random_hex(6)
    ));
    let base_dir = root.join("creating");
    let repo_root = root.join("repo");
    std::fs::create_dir_all(&base_dir).expect("base dir");
    std::fs::create_dir_all(&repo_root).expect("repo dir");
    Paths {
        merged: base_dir.join("m"),
        root: root.clone(),
        repo_root,
        base_dir,
        _cleanup: TempRoot(root),
    }
}

fn context(paths: &Paths) -> IsolationContext {
    IsolationContext {
        id: "test".to_string(),
        base_dir: paths.base_dir.clone(),
        cross_device: false,
        max_copy_bytes: None,
    }
}

fn fake(
    calls: &Calls,
    overrides: impl FnOnce(&mut FakeRuntime),
) -> FakeRuntime {
    let sink = Arc::clone(calls);
    let mut runtime = FakeRuntime {
        platform: "linux".to_string(),
        which: Arc::new(|_| true),
        run: Arc::new(move |argv: &[String]| {
            record(&sink, argv);
            Ok(ok())
        }),
        device: Arc::new(|_| Ok(1)),
        accessible: Arc::new(|_| Ok(true)),
        mounted: Arc::new(|_| Ok(false)),
        wait_mounted: Arc::new(|_| Ok(())),
    };
    overrides(&mut runtime);
    runtime
}

#[test]
fn btrfs_checks_binary_and_subvolume_then_snapshots_and_deletes_with_argv() {
    let p = paths();
    let sink = calls();
    let runtime = fake(&sink, |_| {});
    let backend = BtrfsBackend::new(Arc::new(runtime));
    assert!(backend.probe(&p.repo_root, None).expect("probe").available);
    backend
        .start(&p.repo_root, &p.merged, &context(&p))
        .expect("start");
    std::fs::create_dir_all(&p.merged).expect("merged");
    backend.stop(&p.merged).expect("stop");
    assert_eq!(
        recorded(&sink),
        vec![
            vec!["btrfs", "subvolume", "show", &p.repo_root.to_string_lossy()]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>(),
            vec!["btrfs", "subvolume", "show", &p.repo_root.to_string_lossy()]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>(),
            vec![
                "btrfs",
                "subvolume",
                "snapshot",
                &p.repo_root.to_string_lossy(),
                &p.merged.to_string_lossy()
            ]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>(),
            vec!["btrfs", "subvolume", "delete", &p.merged.to_string_lossy()]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>(),
        ]
    );
    let marker: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(p.base_dir.join(".omo-isolation-backend.json")).expect("marker"),
    )
    .expect("json");
    assert_eq!(
        marker.get("backend").and_then(|backend| backend.as_str()),
        Some("btrfs")
    );
    for overrides in 0..3 {
        let sink = calls();
        let runtime = fake(&sink, |runtime| match overrides {
            0 => runtime.platform = "darwin".to_string(),
            1 => runtime.which = Arc::new(|_| false),
            _ => {
                runtime.run = Arc::new(|_| Ok(result(1, "", "not a subvolume")));
            }
        });
        assert!(!BtrfsBackend::new(Arc::new(runtime))
            .probe(&p.repo_root, None)
            .expect("probe")
            .available);
    }
    assert!(backend
        .start(
            &p.repo_root,
            &p.merged,
            &IsolationContext {
                cross_device: true,
                ..context(&p)
            }
        )
        .is_err());
}

#[test]
fn btrfs_snapshots_a_subvolume_whose_st_dev_differs_from_the_parent_directory() {
    let p = paths();
    let sink = calls();
    let repo_root = p.repo_root.clone();
    let runtime = fake(&sink, move |runtime| {
        runtime.device = Arc::new(move |path: &Path| Ok(if path == repo_root { 7 } else { 9 }));
    });
    let backend = BtrfsBackend::new(Arc::new(runtime));
    backend
        .start(&p.repo_root, &p.merged, &context(&p))
        .expect("start");
    assert!(recorded(&sink).contains(&vec![
        "btrfs".to_string(),
        "subvolume".to_string(),
        "snapshot".to_string(),
        p.repo_root.to_string_lossy().into_owned(),
        p.merged.to_string_lossy().into_owned(),
    ]));
}

#[test]
fn zfs_dataset_root_probe_snapshot_clone_relocation_and_restart_safe_stop() {
    let p = paths();
    let sink = calls();
    let repo_root = p.repo_root.clone();
    let runtime = fake(&sink, move |runtime| {
        runtime.run = Arc::new({
            let sink = Arc::clone(&sink);
            let repo_root = repo_root.clone();
            move |argv: &[String]| {
                record(&sink, argv);
                let stdout = if argv.get(1).map(String::as_str) == Some("list") {
                    format!("pool/repo\t{}\n", repo_root.display())
                } else {
                    String::new()
                };
                Ok(result(0, &stdout, ""))
            }
        });
    });
    assert!(!ZfsBackend::new(Arc::new(runtime.clone()))
        .probe(&p.repo_root.join("subdir"), None)
        .expect("probe")
        .available);
    let backend = ZfsBackend::new(Arc::new(runtime.clone()));
    backend
        .start(&p.repo_root, &p.merged, &context(&p))
        .expect("start");
    std::fs::create_dir_all(&p.merged).expect("merged");
    let final_dir = p.root.join("final");
    backend.relocate(&p.base_dir, &final_dir).expect("relocate");
    ZfsBackend::new(Arc::new(runtime))
        .stop(&final_dir.join("m"))
        .expect("stop");
    let tail = recorded(&sink);
    assert_eq!(
        tail[tail.len() - 5..].to_vec(),
        vec![
            vec!["zfs", "snapshot", "pool/repo@omo-test"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>(),
            vec![
                "zfs",
                "clone",
                "-o",
                &format!("mountpoint={}", p.merged.display()),
                "pool/repo@omo-test",
                "pool/repo/omo-test"
            ]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>(),
            vec![
                "zfs",
                "set",
                &format!("mountpoint={}", final_dir.join("m").display()),
                "pool/repo/omo-test"
            ]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>(),
            vec!["zfs", "destroy", "-r", "pool/repo/omo-test"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>(),
            vec!["zfs", "destroy", "pool/repo@omo-test"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>(),
        ]
    );
}

#[test]
fn overlay_relocates_by_unmount_parent_rename_and_remount_with_new_upper_and_work_paths() {
    let p = paths();
    let sink = calls();
    let runtime = fake(&sink, |runtime| {
        runtime.mounted = Arc::new(|_| Ok(true));
    });
    let backend = OverlayfsBackend::new(Arc::new(runtime));
    backend
        .start(&p.repo_root, &p.merged, &context(&p))
        .expect("start");
    let final_dir = p.root.join("final");
    backend.relocate(&p.base_dir, &final_dir).expect("relocate");
    backend.stop(&final_dir.join("m")).expect("stop");
    assert_eq!(
        recorded(&sink),
        vec![
            vec![
                "fuse-overlayfs",
                "-o",
                &format!(
                    "lowerdir={},upperdir={},workdir={}",
                    p.repo_root.display(),
                    p.base_dir.join("upper").display(),
                    p.base_dir.join("work").display()
                ),
                &p.base_dir.join("m").to_string_lossy()
            ]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>(),
            vec!["fusermount3", "-u", &p.base_dir.join("m").to_string_lossy()]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>(),
            vec![
                "fuse-overlayfs",
                "-o",
                &format!(
                    "lowerdir={},upperdir={},workdir={}",
                    p.repo_root.display(),
                    final_dir.join("upper").display(),
                    final_dir.join("work").display()
                ),
                &final_dir.join("m").to_string_lossy()
            ]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>(),
            vec!["fusermount3", "-u", &final_dir.join("m").to_string_lossy()]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>(),
        ]
    );
    for overrides in 0..3 {
        let sink = calls();
        let runtime = fake(&sink, |runtime| match overrides {
            0 => runtime.which = Arc::new(|_| false),
            1 => runtime.accessible = Arc::new(|_| Ok(false)),
            _ => runtime.platform = "darwin".to_string(),
        });
        assert!(!OverlayfsBackend::new(Arc::new(runtime))
            .probe(&p.repo_root, None)
            .expect("probe")
            .available);
    }
}

#[test]
fn overlay_unmount_failure_retries_three_times_and_leaves_the_source_untouched() {
    let p = paths();
    let sink = calls();
    let runtime = fake(&sink, |runtime| {
        runtime.mounted = Arc::new(|_| Ok(true));
        runtime.run = Arc::new({
            let sink = Arc::clone(&sink);
            move |argv: &[String]| {
                record(&sink, argv);
                Ok(result(
                    if argv.first().map(String::as_str) == Some("fusermount3") {
                        1
                    } else {
                        0
                    },
                    "",
                    "busy",
                ))
            }
        });
    });
    let backend = OverlayfsBackend::new(Arc::new(runtime));
    backend
        .start(&p.repo_root, &p.merged, &context(&p))
        .expect("start");
    std::fs::write(p.merged.join("sentinel"), "untouched").expect("sentinel");
    let error = backend
        .relocate(&p.base_dir, &p.root.join("final"))
        .expect_err("must fail");
    assert!(error.to_string().contains("busy"));
    assert_eq!(
        recorded(&sink)
            .iter()
            .filter(|argv| argv.first().map(String::as_str) == Some("fusermount3"))
            .count(),
        3
    );
    assert_eq!(
        std::fs::read_to_string(p.merged.join("sentinel")).expect("sentinel"),
        "untouched"
    );
    assert!(!p.root.join("final").exists());
}

#[test]
fn reflink_ffi_unavailable_selects_cp_argv_and_classifies_only_unsupported_failures() {
    let p = paths();
    let sink = calls();
    let runtime = fake(&sink, |_| {});
    let load: LoadReflink = Arc::new(|| None);
    let backend = ReflinkBackend::new(Arc::new(runtime), load.clone());
    assert!(backend
        .probe(&p.repo_root, Some(&context(&p)))
        .expect("probe")
        .available);
    backend
        .start(&p.repo_root, &p.merged, &context(&p))
        .expect("start");
    assert_eq!(
        recorded(&sink).last().cloned(),
        Some(vec![
            "cp".to_string(),
            "-a".to_string(),
            "--reflink=always".to_string(),
            p.repo_root.to_string_lossy().into_owned(),
            p.merged.to_string_lossy().into_owned(),
        ])
    );
    for stderr in [
        "failed to clone",
        "Operation not supported",
        "Invalid cross-device link",
        "permission denied",
    ] {
        let sink = calls();
        let runtime = fake(&sink, |runtime| {
            runtime.run = Arc::new(move |_| Ok(result(1, "", stderr)));
        });
        let bad = ReflinkBackend::new(Arc::new(runtime), load.clone());
        let error = bad
            .start(&p.repo_root, &p.merged, &context(&p))
            .expect_err("must fail");
        assert!(error.to_string().contains(stderr));
        assert_eq!(error.is_unavailable(), stderr != "permission denied");
    }
}

#[test]
fn reflink_probe_rejects_eopnotsupp_and_disables_start() {
    let p = paths();
    let sink = calls();
    let runtime = fake(&sink, |_| {});
    let load: LoadReflink = Arc::new(|| {
        Some(Arc::new(FakeIoctl {
            ioctl: Arc::new(|_, _, _| -1),
            errno: 95,
        }) as Arc<dyn isolation_core::ReflinkIoctl>)
    });
    let unsupported = ReflinkBackend::new(Arc::new(runtime), load);
    assert!(!unsupported
        .probe(&p.repo_root, Some(&context(&p)))
        .expect("probe")
        .available);
    std::fs::write(p.repo_root.join("data"), "source").expect("data");
    assert!(unsupported
        .start(&p.repo_root, &p.merged, &context(&p))
        .is_err());
    assert!(!p.merged.exists());
}

struct FakeIoctl {
    ioctl: Arc<dyn Fn(i32, u64, i32) -> i32 + Send + Sync>,
    errno: i32,
}

impl isolation_core::ReflinkIoctl for FakeIoctl {
    fn ioctl(&self, dst: i32, request: u64, src: i32) -> i32 {
        (self.ioctl)(dst, request, src)
    }

    fn errno(&self) -> i32 {
        self.errno
    }
}

#[test]
fn refs_probe_rejects_non_windows_missing_binary_and_wrong_filesystem() {
    let p = paths();
    for overrides in 0..3 {
        let sink = calls();
        let runtime = fake(&sink, |runtime| match overrides {
            0 => {}
            1 => {
                runtime.platform = "win32".to_string();
                runtime.which = Arc::new(|_| false);
            }
            _ => runtime.platform = "win32".to_string(),
        });
        assert!(!BlockCloneBackend::new(Arc::new(runtime), Arc::new(|| {
            Err(isolation_core::NativeLoadError {
                code: None,
                message: "unused".to_string(),
            })
        }))
        .probe(&p.repo_root, None)
        .expect("probe")
        .available);
    }
}

#[test]
fn refs_duplicate_extents_uses_aligned_range_allocated_eof_and_closes_native_handles() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&calls);
    let api = RecordingApi { calls: sink };
    duplicate_extents(&api, "C:\\repo\\src", "C:\\out\\dst", 8192).expect("duplicate");
    assert_eq!(
        *calls.lock().unwrap_or_else(|poison| poison.into_inner()),
        vec![
            serde_json::json!(["open", "\\\\?\\C:\\repo\\src", false]),
            serde_json::json!(["open", "\\\\?\\C:\\out\\dst", true]),
            serde_json::json!(["resize", 2, 8192u64]),
            serde_json::json!(["ioctl", 2, 1, 0x00098344u32, 8192u64]),
            serde_json::json!(["resize", 2, 8192u64]),
            serde_json::json!(["close", 2]),
            serde_json::json!(["close", 1]),
        ]
    );
}

struct RecordingApi {
    calls: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl WindowsCloneApi for RecordingApi {
    fn open(&self, path: &str, write: bool) -> isolation_core::IsolationResult<u64> {
        self.calls
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(serde_json::json!(["open", path, write]));
        Ok(if write { 2 } else { 1 })
    }

    fn resize(&self, handle: u64, size: u64) -> isolation_core::IsolationResult<()> {
        self.calls
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(serde_json::json!(["resize", handle, size]));
        Ok(())
    }

    fn duplicate(
        &self,
        destination: u64,
        source: u64,
        bytes: u64,
    ) -> isolation_core::IsolationResult<()> {
        self.calls
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(serde_json::json!(["ioctl", destination, source, 0x00098344u32, bytes]));
        Ok(())
    }

    fn close(&self, handle: u64) -> isolation_core::IsolationResult<()> {
        self.calls
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(serde_json::json!(["close", handle]));
        Ok(())
    }

    fn cluster_size(&self, _path: &str) -> isolation_core::IsolationResult<u64> {
        Ok(4096)
    }
}

#[test]
fn zfs_failed_relocation_restores_the_source_parent_and_marker_identity() {
    let p = paths();
    let sink = calls();
    let repo_root = p.repo_root.clone();
    let runtime = fake(&sink, move |runtime| {
        runtime.run = Arc::new({
            let sink = Arc::clone(&sink);
            let repo_root = repo_root.clone();
            move |argv: &[String]| {
                record(&sink, argv);
                let stdout = if argv.get(1).map(String::as_str) == Some("list") {
                    format!("pool/repo\t{}\n", repo_root.display())
                } else {
                    String::new()
                };
                Ok(result(
                    if argv.get(1).map(String::as_str) == Some("set") {
                        1
                    } else {
                        0
                    },
                    &stdout,
                    "mount busy",
                ))
            }
        });
    });
    let backend = ZfsBackend::new(Arc::new(runtime));
    backend
        .start(&p.repo_root, &p.merged, &context(&p))
        .expect("start");
    std::fs::create_dir_all(&p.merged).expect("merged");
    std::fs::write(p.merged.join("sentinel"), "untouched").expect("sentinel");
    let final_dir = p.root.join("final");
    let error = backend.relocate(&p.base_dir, &final_dir).expect_err("must fail");
    assert!(error.to_string().contains("mount busy"));
    assert_eq!(
        std::fs::read_to_string(p.merged.join("sentinel")).expect("sentinel"),
        "untouched"
    );
    let marker: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(p.base_dir.join(".omo-isolation-backend.json")).expect("marker"),
    )
    .expect("json");
    assert_eq!(
        marker.get("dataset").and_then(|dataset| dataset.as_str()),
        Some("pool/repo/omo-test")
    );
    assert!(!final_dir.exists());
    assert_eq!(
        recorded(&sink).last().cloned(),
        Some(vec![
            "zfs".to_string(),
            "set".to_string(),
            format!("mountpoint={}", final_dir.join("m").display()),
            "pool/repo/omo-test".to_string(),
        ])
    );
}

#[test]
fn zfs_clone_failure_leaves_a_snapshot_marker_that_restart_safe_stop_reclaims() {
    let p = paths();
    let sink = calls();
    let repo_root = p.repo_root.clone();
    let runtime = fake(&sink, move |runtime| {
        runtime.run = Arc::new({
            let sink = Arc::clone(&sink);
            let repo_root = repo_root.clone();
            move |argv: &[String]| {
                record(&sink, argv);
                let stdout = if argv.get(1).map(String::as_str) == Some("list") {
                    format!("pool/repo\t{}\n", repo_root.display())
                } else {
                    String::new()
                };
                Ok(result(
                    if argv.get(1).map(String::as_str) == Some("clone") {
                        1
                    } else {
                        0
                    },
                    &stdout,
                    "clone failed",
                ))
            }
        });
    });
    let error = ZfsBackend::new(Arc::new(runtime.clone()))
        .start(&p.repo_root, &p.merged, &context(&p))
        .expect_err("must fail");
    assert!(error.to_string().contains("clone failed"));
    ZfsBackend::new(Arc::new(runtime))
        .stop(&p.merged)
        .expect("stop");
    let tail = recorded(&sink);
    assert_eq!(
        tail.last().cloned(),
        Some(vec![
            "zfs".to_string(),
            "destroy".to_string(),
            "pool/repo@omo-test".to_string(),
        ])
    );
    assert!(!tail.iter().any(|argv| {
        argv.get(1).map(String::as_str) == Some("destroy") && argv.get(2).map(String::as_str) == Some("-r")
    }));
}

#[test]
fn cp_unsupported_stderr_with_exit_two_remains_a_generic_error() {
    let p = paths();
    let sink = calls();
    let runtime = fake(&sink, |runtime| {
        runtime.run = Arc::new(|_| Ok(result(2, "", "failed to clone")));
    });
    let load: LoadReflink = Arc::new(|| None);
    let backend = ReflinkBackend::new(Arc::new(runtime), load);
    let error = backend
        .start(&p.repo_root, &p.merged, &context(&p))
        .expect_err("must fail");
    assert!(!error.is_unavailable());
    assert!(error.to_string().contains("cp exited 2"));
}

#[test]
fn missing_cp_probe_cannot_leave_the_fallback_tier_enabled() {
    let p = paths();
    let sink = calls();
    let runtime = fake(&sink, |runtime| {
        runtime.which = Arc::new(|_| false);
    });
    let load: LoadReflink = Arc::new(|| None);
    let backend = ReflinkBackend::new(Arc::new(runtime), load);
    assert!(!backend.probe(&p.repo_root, None).expect("probe").available);
    assert!(backend
        .start(&p.repo_root, &p.merged, &context(&p))
        .is_err());
    assert!(recorded(&sink).is_empty());
}

#[test]
fn btrfs_probe_propagates_unexpected_subvolume_show_failures() {
    let p = paths();
    let sink = calls();
    let runtime = fake(&sink, |runtime| {
        runtime.run = Arc::new(|_| Ok(result(1, "", "Input/output error")));
    });
    let error = BtrfsBackend::new(Arc::new(runtime))
        .probe(&p.repo_root, None)
        .expect_err("must fail");
    assert!(error.to_string().contains("Input/output error"));
}

#[test]
fn btrfs_probe_still_classifies_not_a_subvolume_as_unavailable() {
    let p = paths();
    let sink = calls();
    let runtime = fake(&sink, |runtime| {
        runtime.run = Arc::new(|_| Ok(result(1, "", "Not a Btrfs subvolume: /x")));
    });
    assert!(!BtrfsBackend::new(Arc::new(runtime))
        .probe(&p.repo_root, None)
        .expect("probe")
        .available);
}

#[test]
fn zfs_list_failures_other_than_missing_pools_propagate_from_the_probe() {
    let p = paths();
    let sink = calls();
    let runtime = fake(&sink, |runtime| {
        runtime.run = Arc::new(|_| Ok(result(1, "", "Input/output error")));
    });
    let error = ZfsBackend::new(Arc::new(runtime))
        .probe(&p.repo_root, None)
        .expect_err("must fail");
    assert!(error.to_string().contains("Input/output error"));
}

#[test]
fn zfs_snapshot_permission_failures_fall_through_as_unavailable() {
    let p = paths();
    let sink = calls();
    let repo_root = p.repo_root.clone();
    let runtime = fake(&sink, move |runtime| {
        runtime.run = Arc::new(move |argv: &[String]| {
            if argv.get(1).map(String::as_str) == Some("list") {
                Ok(result(0, &format!("pool/repo\t{}\n", repo_root.display()), ""))
            } else {
                Ok(result(1, "", "cannot create snapshot: permission denied"))
            }
        });
    });
    let error = ZfsBackend::new(Arc::new(runtime))
        .start(&p.repo_root, &p.merged, &context(&p))
        .expect_err("must fail");
    assert!(error.is_unavailable());
}

#[test]
fn zfs_unexpected_snapshot_failures_remain_hard_errors() {
    let p = paths();
    let sink = calls();
    let repo_root = p.repo_root.clone();
    let runtime = fake(&sink, move |runtime| {
        runtime.run = Arc::new(move |argv: &[String]| {
            if argv.get(1).map(String::as_str) == Some("list") {
                Ok(result(0, &format!("pool/repo\t{}\n", repo_root.display()), ""))
            } else {
                Ok(result(1, "", "internal error: Input/output error"))
            }
        });
    });
    let error = ZfsBackend::new(Arc::new(runtime))
        .start(&p.repo_root, &p.merged, &context(&p))
        .expect_err("must fail");
    assert!(!error.is_unavailable());
}

#[test]
fn refs_probe_classifies_fsutil_failures_as_unavailable_so_the_walk_can_fall_through() {
    let sink = calls();
    let runtime = fake(&sink, |runtime| {
        runtime.platform = "win32".to_string();
        runtime.run = Arc::new(|_| Ok(result(1, "", "The volume does not exist")));
    });
    let result = BlockCloneBackend::new(Arc::new(runtime), Arc::new(|| {
        Err(isolation_core::NativeLoadError {
            code: None,
            message: "unused".to_string(),
        })
    }))
    .probe(Path::new("C:\\repo"), None)
    .expect("probe");
    assert!(!result.available);
    assert!(result.reason.unwrap_or_default().contains("volume does not exist"));
}

#[test]
fn overlayfs_mount_capability_failures_fall_through_as_unavailable() {
    let p = paths();
    let sink = calls();
    let runtime = fake(&sink, |runtime| {
        runtime.run = Arc::new(|argv: &[String]| {
            Ok(if argv.first().map(String::as_str) == Some("fuse-overlayfs") {
                result(
                    1,
                    "",
                    "fusermount3: failed to access /dev/fuse: Permission denied",
                )
            } else {
                ok()
            })
        });
    });
    let error = OverlayfsBackend::new(Arc::new(runtime))
        .start(&p.repo_root, &p.merged, &context(&p))
        .expect_err("must fail");
    assert!(error.is_unavailable());
}

#[test]
fn overlayfs_unexpected_mount_failures_remain_hard_errors() {
    let p = paths();
    let sink = calls();
    let runtime = fake(&sink, |runtime| {
        runtime.run = Arc::new(|argv: &[String]| {
            Ok(if argv.first().map(String::as_str) == Some("fuse-overlayfs") {
                result(1, "", "Input/output error")
            } else {
                ok()
            })
        });
    });
    let error = OverlayfsBackend::new(Arc::new(runtime))
        .start(&p.repo_root, &p.merged, &context(&p))
        .expect_err("must fail");
    assert!(!error.is_unavailable());
}

#[test]
fn btrfs_probe_treats_unprivileged_b_tree_search_as_inconclusive_and_lets_start_snapshot() {
    let p = paths();
    let sink = calls();
    let runtime = fake(&sink, |runtime| {
        runtime.run = Arc::new({
            let sink = Arc::clone(&sink);
            move |argv: &[String]| {
                record(&sink, argv);
                Ok(if argv.get(2).map(String::as_str) == Some("show") {
                    result(1, "", "ERROR: Could not search B-tree: Operation not permitted")
                } else {
                    ok()
                })
            }
        });
    });
    let backend = BtrfsBackend::new(Arc::new(runtime));
    assert!(backend.probe(&p.repo_root, None).expect("probe").available);
    backend
        .start(&p.repo_root, &p.merged, &context(&p))
        .expect("start");
    assert!(recorded(&sink).contains(&vec![
        "btrfs".to_string(),
        "subvolume".to_string(),
        "snapshot".to_string(),
        p.repo_root.to_string_lossy().into_owned(),
        p.merged.to_string_lossy().into_owned(),
    ]));
}

#[test]
fn btrfs_probe_trusts_the_filesystem_uuid_over_subvolume_st_dev() {
    let p = paths();
    let sink = calls();
    let repo_root = p.repo_root.clone();
    let runtime = fake(&sink, move |runtime| {
        runtime.device = Arc::new(move |path: &Path| Ok(if path == repo_root { 7 } else { 9 }));
        runtime.run = Arc::new(|argv: &[String]| {
            Ok(match argv.get(0).map(String::as_str) {
                Some("btrfs") => ok(),
                Some("findmnt") if argv.get(2).map(String::as_str) == Some("FSTYPE") => {
                    result(0, "btrfs\n", "")
                }
                Some("findmnt") => result(0, "uuid-same\n", ""),
                _ => ok(),
            })
        });
    });
    assert!(BtrfsBackend::new(Arc::new(runtime))
        .probe(&p.repo_root, Some(&context(&p)))
        .expect("probe")
        .available);
}

#[test]
fn btrfs_probe_rejects_a_base_directory_on_a_different_btrfs_filesystem() {
    let p = paths();
    let sink = calls();
    let repo_root = p.repo_root.clone();
    let runtime = fake(&sink, move |runtime| {
        runtime.run = Arc::new(move |argv: &[String]| {
            Ok(match argv.get(0).map(String::as_str) {
                Some("btrfs") => ok(),
                Some("findmnt") if argv.get(2).map(String::as_str) == Some("FSTYPE") => {
                    result(0, "btrfs\n", "")
                }
                Some("findmnt") => {
                    if argv.get(4) == Some(&repo_root.to_string_lossy().into_owned()) {
                        result(0, "uuid-a\n", "")
                    } else {
                        result(0, "uuid-b\n", "")
                    }
                }
                _ => ok(),
            })
        });
    });
    assert!(!BtrfsBackend::new(Arc::new(runtime))
        .probe(&p.repo_root, Some(&context(&p)))
        .expect("probe")
        .available);
    let _ = checked(&FakeRuntime::linux(), &[]);
    let _ = IsolationError::other("placeholder");
    let _: Arc<dyn WindowsSymbols> = Arc::new(NoopSymbols);
    let _ = create_windows_clone_api(Arc::new(NoopSymbols));
}

struct NoopSymbols;

impl WindowsSymbols for NoopSymbols {
    fn create_file_w(&self, _path: &[u8], _access: u32, _share: u32, _disposition: u32, _flags: u32) -> i64 {
        -1
    }

    fn set_file_pointer_ex(&self, _handle: i64, _offset: u64) -> i32 {
        0
    }

    fn set_end_of_file(&self, _handle: i64) -> i32 {
        0
    }

    fn device_io_control(&self, _handle: i64, _code: u32, _input: &[u8], _returned: &mut [u8]) -> i32 {
        0
    }

    fn close_handle(&self, _handle: i64) -> i32 {
        0
    }

    fn get_last_error(&self) -> u32 {
        0
    }

    fn get_disk_free_space_w(&self, _root: &[u8], _sectors: &mut [u8], _bytes: &mut [u8]) -> i32 {
        0
    }
}
