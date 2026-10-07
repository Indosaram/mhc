use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use isolation_core::backends::git_fixture::repo;
use isolation_core::test_support::{backend, fixture, Fixture, TestBackend};
use isolation_core::{
    choose_base_dir, cleanup_isolation, ensure_isolation, now_ms, retain_isolation, BackendKind,
    BackendRef, EnsureIsolationOptions, FilesystemBaseDirIo, IsolationError,
};

fn options(f: &Fixture, id: &str, backends: Vec<BackendRef>) -> EnsureIsolationOptions {
    EnsureIsolationOptions {
        repo_root: f.repo_root.clone(),
        id: id.to_string(),
        preferred: None,
        backends,
        platform: None,
        home_dir: Some(f.home_dir.clone()),
        owner: None,
        max_copy_bytes: None,
    }
}

fn preferred(f: &Fixture, id: &str, backends: Vec<BackendRef>, kind: BackendKind) -> EnsureIsolationOptions {
    EnsureIsolationOptions {
        preferred: Some(kind),
        ..options(f, id, backends)
    }
}

fn copy_dir_all(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

#[test]
fn writes_marker_before_start_and_publishes_only_a_complete_directory() {
    let f = fixture();
    let before = now_ms();
    let selection = choose_base_dir(&f.repo_root, &f.home_dir, "one", &FilesystemBaseDirIo)
        .expect("base dir");
    let base_dir = selection.base_dir.clone();
    let probe_base = base_dir.clone();
    let backend: BackendRef = Arc::new(TestBackend {
        start_fn: Arc::new(move |_lower, merged, _ctx| {
            let marker_text = std::fs::read_to_string(
                merged
                    .parent()
                    .expect("parent")
                    .join(".omo-isolation-owner.json"),
            )?;
            let marker: serde_json::Value =
                serde_json::from_str(&marker_text).map_err(|error| IsolationError::other(error.to_string()))?;
            assert_eq!(marker.get("id").and_then(|id| id.as_str()), Some("one"));
            assert!(
                !probe_base.exists(),
                "the published base directory must not exist during start"
            );
            std::fs::create_dir_all(merged)?;
            std::fs::write(merged.join("result"), "ready")?;
            Ok(None)
        }),
        ..TestBackend::new()
    });
    let handle = ensure_isolation(options(&f, "one", vec![backend])).expect("handle");
    assert_eq!(handle.base_dir, base_dir);
    assert_eq!(handle.merged_dir, base_dir.join("m"));
    assert!(!handle.base_dir.to_string_lossy().contains(".creating-"));
    assert_eq!(
        std::fs::read_to_string(handle.merged_dir.join("result")).expect("result"),
        "ready"
    );
    let marker: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(base_dir.join(".omo-isolation-backend.json")).expect("marker"),
    )
    .expect("json");
    assert_eq!(
        marker.get("backend").and_then(|backend| backend.as_str()),
        Some("rcopy")
    );
    let started = marker
        .get("started_at")
        .and_then(|started| started.as_str())
        .expect("started_at");
    let started_ms = chrono::DateTime::parse_from_rfc3339(started)
        .expect("rfc3339")
        .timestamp_millis() as u64;
    assert!(started_ms >= before);
    assert!(started_ms <= now_ms());
}

#[test]
fn unavailable_start_falls_through_and_preserves_the_reason() {
    let f = fixture();
    let apfs: BackendRef = Arc::new(TestBackend {
        kind: BackendKind::Apfs,
        start_fn: Arc::new(|_, _, _| Err(IsolationError::unavailable("no clone support"))),
        ..TestBackend::new()
    });
    let handle = ensure_isolation(EnsureIsolationOptions {
        platform: Some("darwin".to_string()),
        ..options(&f, "one", vec![apfs, backend()])
    })
    .expect("handle");
    assert_eq!(handle.backend, BackendKind::Rcopy);
    assert!(handle.fell_back);
    assert!(handle
        .fallback_reason
        .clone()
        .unwrap_or_default()
        .contains("no clone support"));
    let parent = handle.base_dir.parent().expect("parent").to_path_buf();
    let entries: Vec<String> = std::fs::read_dir(&parent)
        .expect("readdir")
        .map(|entry| entry.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        entries,
        vec![handle
            .base_dir
            .file_name()
            .expect("name")
            .to_string_lossy()
            .into_owned()]
    );
}

#[test]
fn unavailable_probe_skips_start_and_falls_through() {
    let f = fixture();
    let apfs: BackendRef = Arc::new(TestBackend {
        kind: BackendKind::Apfs,
        probe_fn: Arc::new(|_, _| Ok(isolation_core::ProbeResult::unavailable("wrong filesystem"))),
        start_fn: Arc::new(|_, _, _| panic!("must not start")),
        ..TestBackend::new()
    });
    let handle = ensure_isolation(EnsureIsolationOptions {
        platform: Some("darwin".to_string()),
        ..options(&f, "one", vec![apfs, backend()])
    })
    .expect("handle");
    assert_eq!(handle.backend, BackendKind::Rcopy);
    assert!(handle
        .fallback_reason
        .unwrap_or_default()
        .contains("wrong filesystem"));
}

#[test]
fn generic_start_error_propagates_and_removes_the_creating_directory() {
    let f = fixture();
    let backend: BackendRef = Arc::new(TestBackend {
        start_fn: Arc::new(|_, _, _| Err(IsolationError::other("disk failure"))),
        ..TestBackend::new()
    });
    let error = ensure_isolation(preferred(&f, "one", vec![backend], BackendKind::Rcopy))
        .expect_err("must fail");
    assert!(error.to_string().contains("disk failure"));
    assert!(!error.is_unavailable());
    let entries: Vec<String> = std::fs::read_dir(f.home_dir.join(".omo/wt"))
        .expect("readdir")
        .map(|entry| entry.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    assert!(entries.is_empty());
}

#[test]
fn generic_probe_error_propagates_without_falling_through() {
    let f = fixture();
    let apfs: BackendRef = Arc::new(TestBackend {
        kind: BackendKind::Apfs,
        probe_fn: Arc::new(|_, _| Err(IsolationError::other("probe I/O failure"))),
        ..TestBackend::new()
    });
    let error = ensure_isolation(preferred(&f, "one", vec![apfs, backend()], BackendKind::Apfs))
        .expect_err("must fail");
    assert!(error.to_string().contains("probe I/O failure"));
    assert!(!error.is_unavailable());
}

#[test]
fn a_probing_backend_whose_cli_fails_falls_through_instead_of_aborting_the_walk() {
    let f = fixture();
    let block_clone: BackendRef = Arc::new(TestBackend {
        kind: BackendKind::BlockClone,
        probe_fn: Arc::new(|_, _| {
            Ok(isolation_core::ProbeResult::unavailable(
                "fsutil volumeinfo failed (1): The volume does not exist",
            ))
        }),
        ..TestBackend::new()
    });
    let handle = ensure_isolation(EnsureIsolationOptions {
        platform: Some("win32".to_string()),
        ..options(&f, "one", vec![block_clone, backend()])
    })
    .expect("handle");
    assert_eq!(handle.backend, BackendKind::Rcopy);
    assert!(handle.fell_back);
    assert!(handle
        .fallback_reason
        .unwrap_or_default()
        .contains("volume does not exist"));
}

#[test]
fn all_unavailable_yields_a_typed_error() {
    let f = fixture();
    let error = ensure_isolation(options(&f, "one", Vec::new())).expect_err("must fail");
    assert!(error.is_unavailable());
    assert_eq!(error.code(), Some("isolation_unavailable"));
}

#[test]
fn cleanup_stops_before_removing_the_tree() {
    let f = fixture();
    let stopped = Arc::new(Mutex::new(false));
    let sink = Arc::clone(&stopped);
    let backend: BackendRef = Arc::new(TestBackend {
        stop_fn: Arc::new(move |merged| {
            std::fs::metadata(merged)?;
            *sink.lock().unwrap_or_else(|poison| poison.into_inner()) = true;
            Ok(())
        }),
        ..TestBackend::new()
    });
    let handle =
        ensure_isolation(preferred(&f, "one", vec![backend], BackendKind::Rcopy)).expect("handle");
    cleanup_isolation(&handle).expect("cleanup");
    assert!(*stopped.lock().unwrap_or_else(|poison| poison.into_inner()));
    assert!(!handle.base_dir.exists());
}

#[test]
fn stop_failure_preserves_the_tree_instead_of_recursively_removing_a_mount() {
    let f = fixture();
    let backend: BackendRef = Arc::new(TestBackend {
        stop_fn: Arc::new(|_merged| Err(IsolationError::other("cannot unmount"))),
        ..TestBackend::new()
    });
    let handle =
        ensure_isolation(preferred(&f, "one", vec![backend], BackendKind::Rcopy)).expect("handle");
    let error = cleanup_isolation(&handle).expect_err("must fail");
    assert!(error.to_string().contains("cannot unmount"));
    std::fs::metadata(&handle.merged_dir).expect("merged tree preserved");
}

#[test]
fn retention_renames_the_complete_tree_and_stores_the_reason() {
    let f = fixture();
    let handle = ensure_isolation(preferred(&f, "one", vec![backend()], BackendKind::Rcopy))
        .expect("handle");
    let retained = retain_isolation(&handle, "merge conflict").expect("retained");
    assert!(retained
        .to_string_lossy()
        .contains(&format!("{}.retained-", handle.base_dir.to_string_lossy())));
    std::fs::metadata(retained.join("m")).expect("retained merged tree");
    assert!(!handle.base_dir.exists());
    let marker: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(retained.join(".omo-isolation-retained.json")).expect("marker"),
    )
    .expect("json");
    assert_eq!(
        marker.get("reason").and_then(|reason| reason.as_str()),
        Some("merge conflict")
    );
}

#[test]
fn another_ensure_cannot_remove_an_in_flight_creating_directory() {
    let f = fixture();
    let entered = Arc::new((Mutex::new(false), Condvar::new()));
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let entered_signal = Arc::clone(&entered);
    let gate_signal = Arc::clone(&gate);
    let backend: BackendRef = Arc::new(TestBackend {
        start_fn: Arc::new(move |_lower, merged, _ctx| {
            {
                let (flag, condvar) = &*entered_signal;
                *flag.lock().unwrap_or_else(|poison| poison.into_inner()) = true;
                condvar.notify_all();
            }
            {
                let (flag, condvar) = &*gate_signal;
                let mut released = flag.lock().unwrap_or_else(|poison| poison.into_inner());
                while !*released {
                    released = condvar
                        .wait(released)
                        .unwrap_or_else(|poison| poison.into_inner());
                }
            }
            std::fs::create_dir_all(merged)?;
            Ok(None)
        }),
        ..TestBackend::new()
    });
    let first_options = preferred(&f, "one", vec![Arc::clone(&backend)], BackendKind::Rcopy);
    let second_options = preferred(&f, "one", vec![Arc::clone(&backend)], BackendKind::Rcopy);
    let first = std::thread::spawn(move || ensure_isolation(first_options));
    {
        let (flag, condvar) = &*entered;
        let mut started = flag.lock().unwrap_or_else(|poison| poison.into_inner());
        while !*started {
            started = condvar
                .wait(started)
                .unwrap_or_else(|poison| poison.into_inner());
        }
    }
    let second = ensure_isolation(second_options);
    {
        let (flag, condvar) = &*gate;
        *flag.lock().unwrap_or_else(|poison| poison.into_inner()) = true;
        condvar.notify_all();
    }
    assert!(second.is_err());
    let handle = first.join().expect("thread").expect("first ensure");
    std::fs::metadata(handle.base_dir.join(".omo-isolation-owner.json"))
        .expect("owner marker preserved");
}

#[test]
fn fall_through_leaves_no_creating_directory_behind() {
    let f = fixture();
    let apfs: BackendRef = Arc::new(TestBackend {
        kind: BackendKind::Apfs,
        start_fn: Arc::new(|_, _, _| Err(IsolationError::unavailable("no clone support"))),
        ..TestBackend::new()
    });
    let handle = ensure_isolation(EnsureIsolationOptions {
        platform: Some("darwin".to_string()),
        ..options(&f, "one", vec![apfs, backend()])
    })
    .expect("handle");
    let parent = handle.base_dir.parent().expect("parent").to_path_buf();
    let leftovers: Vec<String> = std::fs::read_dir(&parent)
        .expect("readdir")
        .map(|entry| entry.expect("entry").file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".creating-"))
        .collect();
    assert!(leftovers.is_empty());
}

#[test]
fn publication_and_retention_route_through_the_backend_relocation_hook() {
    let f = fixture();
    let calls: Arc<Mutex<Vec<(PathBuf, PathBuf)>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&calls);
    let instance: BackendRef = Arc::new(TestBackend {
        relocate_fn: Some(Arc::new(move |from, to| {
            sink.lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .push((from.to_path_buf(), to.to_path_buf()));
            std::fs::rename(from, to)?;
            Ok(())
        })),
        ..TestBackend::new()
    });
    let handle = ensure_isolation(preferred(&f, "relocate", vec![instance], BackendKind::Rcopy))
        .expect("handle");
    let creating = PathBuf::from(format!(
        "{}.creating-{}",
        handle.base_dir.to_string_lossy(),
        std::process::id()
    ));
    assert_eq!(
        *calls.lock().unwrap_or_else(|poison| poison.into_inner()),
        vec![(creating.clone(), handle.base_dir.clone())]
    );
    let retained = retain_isolation(&handle, "conflict").expect("retained");
    assert_eq!(
        *calls.lock().unwrap_or_else(|poison| poison.into_inner()),
        vec![
            (creating, handle.base_dir.clone()),
            (handle.base_dir.clone(), retained.clone())
        ]
    );
    std::fs::metadata(retained.join("m")).expect("retained tree");
}

#[test]
fn retry_restores_ownership_after_a_mount_backend_removes_its_whole_base() {
    let f = repo();
    let starts = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&starts);
    let backend: BackendRef = Arc::new(TestBackend {
        start_fn: Arc::new(move |lower, merged, _ctx| {
            std::fs::create_dir_all(merged.parent().expect("parent"))?;
            copy_dir_all(lower, merged)?;
            if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                std::fs::write(merged.join(".git").join("index"), "broken index")?;
            }
            Ok(None)
        }),
        stop_fn: Arc::new(|merged| {
            let _ = std::fs::remove_dir_all(merged.parent().expect("parent"));
            Ok(())
        }),
        ..TestBackend::new()
    });
    let handle =
        ensure_isolation(preferred(&f, "retry-owner", vec![backend], BackendKind::Rcopy))
            .expect("handle");
    assert_eq!(starts.load(Ordering::SeqCst), 2);
    let marker: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(handle.base_dir.join(".omo-isolation-owner.json"))
            .expect("marker"),
    )
    .expect("json");
    assert_eq!(
        marker.get("id").and_then(|id| id.as_str()),
        Some("retry-owner")
    );
}

#[test]
fn git_snapshot_inconsistency_after_retry_is_a_hard_failure_not_a_fallback_signal() {
    let f = fixture();
    let backend: BackendRef = Arc::new(TestBackend {
        start_fn: Arc::new(|_lower, merged, _ctx| {
            std::fs::create_dir_all(merged.join(".git"))?;
            Ok(None)
        }),
        ..TestBackend::new()
    });
    let error = ensure_isolation(EnsureIsolationOptions {
        preferred: Some(BackendKind::Rcopy),
        ..options(&f, "broken", vec![backend])
    })
    .expect_err("must fail");
    assert!(!error.is_unavailable());
    assert!(error.to_string().contains("snapshot"));
}
