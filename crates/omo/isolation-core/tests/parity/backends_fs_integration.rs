use std::path::Path;

use isolation_core::{
    checked, cleanup_isolation, ensure_isolation, BackendKind, BackendRef, BtrfsBackend,
    EnsureIsolationOptions, OverlayfsBackend, RcopyBackend, ReflinkBackend, SystemRuntime,
    ZfsBackend,
};

fn env_path(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn skip(what: &str) {
    eprintln!("fs-integration {what}: skipped (filesystem environment not configured)");
}

fn roundtrip(root: &Path, kind: &str) {
    let base = isolation_core::mkdtemp_in(root, "integration-").expect("base");
    let lower = base.join("repo");
    let home_dir = base.join("home");
    std::fs::create_dir_all(&home_dir).expect("home");
    let btrfs = BtrfsBackend::default();
    if kind == "btrfs" {
        checked(
            &SystemRuntime,
            &[
                "btrfs".to_string(),
                "subvolume".to_string(),
                "create".to_string(),
                lower.to_string_lossy().into_owned(),
            ],
        )
        .expect("subvolume create");
    } else {
        std::fs::create_dir_all(&lower).expect("lower");
    }
    std::fs::create_dir_all(lower.join("nested")).expect("nested");
    std::fs::write(lower.join("nested/file"), "unchanged").expect("file");
    let backends: Vec<BackendRef> = vec![
        std::sync::Arc::new(btrfs),
        std::sync::Arc::new(ReflinkBackend::default()),
        std::sync::Arc::new(OverlayfsBackend::default()),
        std::sync::Arc::new(RcopyBackend),
    ];
    let preferred = match kind {
        "btrfs" => None,
        "reflink" => None,
        "overlayfs" => Some(BackendKind::Overlayfs),
        _ => Some(BackendKind::Rcopy),
    };
    let handle = ensure_isolation(EnsureIsolationOptions {
        repo_root: lower.clone(),
        id: format!("integration-{kind}"),
        preferred,
        backends,
        platform: None,
        home_dir: Some(home_dir),
        owner: None,
        max_copy_bytes: None,
    })
    .expect("handle");
    assert_eq!(handle.backend.as_str(), kind);
    assert!(!handle.base_dir.to_string_lossy().contains(".creating-"));
    assert_eq!(
        std::fs::read_to_string(handle.merged_dir.join("nested/file")).expect("file"),
        "unchanged"
    );
    std::fs::write(handle.merged_dir.join("nested/file"), "sandbox").expect("write");
    assert_eq!(
        std::fs::read_to_string(lower.join("nested/file")).expect("file"),
        "unchanged"
    );
    cleanup_isolation(&handle).expect("cleanup");
    assert!(!handle.base_dir.exists());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn fs_integration_btrfs_publication_cow_parity_and_teardown() {
    let Some(root) = env_path("ISOLATION_TEST_BTRFS_ROOT") else {
        skip("btrfs/reflink/rcopy");
        return;
    };
    roundtrip(Path::new(&root), "btrfs");
}

#[test]
fn fs_integration_reflink_publication_cow_parity_and_teardown() {
    let Some(root) = env_path("ISOLATION_TEST_BTRFS_ROOT") else {
        skip("btrfs/reflink/rcopy");
        return;
    };
    roundtrip(Path::new(&root), "reflink");
}

#[test]
fn fs_integration_rcopy_publication_cow_parity_and_teardown() {
    let fixture = isolation_core::test_support::fixture();
    roundtrip(&fixture.root, "rcopy");
}

#[test]
fn fs_integration_overlayfs_publication_and_teardown() {
    let root = env_path("ISOLATION_TEST_BTRFS_ROOT");
    let overlay = std::env::var("ISOLATION_TEST_OVERLAY").ok().as_deref() == Some("1");
    let Some(root) = root.filter(|_| overlay) else {
        skip("overlayfs");
        return;
    };
    roundtrip(Path::new(&root), "overlayfs");
}

#[test]
fn fs_integration_ext4_probe_rejects_eopnotsupp() {
    let Some(root) = env_path("ISOLATION_TEST_EXT4_ROOT") else {
        skip("ext4 unsupported probe");
        return;
    };
    let lower = isolation_core::mkdtemp_in(Path::new(&root), "reflink-unsupported-").expect("lower");
    let context = isolation_core::IsolationContext {
        id: "ext4".to_string(),
        base_dir: lower.clone(),
        cross_device: false,
        max_copy_bytes: None,
    };
    assert!(!ReflinkBackend::default()
        .probe(&lower, Some(&context))
        .expect("probe")
        .available);
    let _ = std::fs::remove_dir_all(&lower);
}

fn zfs_lists(name: &str) -> bool {
    let argv = vec![
        "zfs".to_string(),
        "list".to_string(),
        "-H".to_string(),
        "-o".to_string(),
        "name".to_string(),
        name.to_string(),
    ];
    match checked(&SystemRuntime, &argv) {
        Ok(result) => result.stdout.lines().any(|line| line.trim() == name),
        Err(_) => false,
    }
}

#[test]
fn fs_integration_zfs_clone_publication_cow_and_restart_safe_teardown() {
    let Some(dataset) = env_path("ISOLATION_TEST_ZFS_DATASET") else {
        skip("zfs");
        return;
    };
    if env_path("ISOLATION_TEST_ZFS_DISPOSABLE").as_deref() != Some("1") {
        skip("zfs (set ISOLATION_TEST_ZFS_DISPOSABLE=1 for a disposable dataset)");
        return;
    }
    let list = checked(
        &SystemRuntime,
        &[
            "zfs".to_string(),
            "list".to_string(),
            "-H".to_string(),
            "-o".to_string(),
            "mountpoint".to_string(),
            dataset.clone(),
        ],
    )
    .expect("zfs list");
    let lower = std::path::PathBuf::from(list.stdout.trim());
    let clone = format!("{dataset}/omo-integration-zfs");
    let snapshot = format!("{dataset}@omo-integration-zfs");
    let home_dir = lower.join("home");
    std::fs::create_dir_all(&home_dir).expect("home");
    std::fs::write(lower.join("content"), "lower").expect("content");
    let handle = ensure_isolation(EnsureIsolationOptions {
        repo_root: lower.clone(),
        id: "integration-zfs".to_string(),
        preferred: Some(BackendKind::Zfs),
        backends: vec![std::sync::Arc::new(ZfsBackend::default())],
        platform: None,
        home_dir: Some(home_dir.clone()),
        owner: None,
        max_copy_bytes: None,
    })
    .expect("handle");
    assert_eq!(handle.backend.as_str(), "zfs");
    assert_eq!(
        std::fs::read_to_string(handle.merged_dir.join("content")).expect("content"),
        "lower"
    );
    std::fs::write(handle.merged_dir.join("content"), "merged").expect("write");
    assert_eq!(
        std::fs::read_to_string(lower.join("content")).expect("content"),
        "lower"
    );
    assert!(zfs_lists(&clone), "clone dataset missing before teardown");
    assert!(zfs_lists(&snapshot), "snapshot missing before teardown");
    let mount = checked(
        &SystemRuntime,
        &[
            "zfs".to_string(),
            "get".to_string(),
            "-H".to_string(),
            "-o".to_string(),
            "value".to_string(),
            "mountpoint".to_string(),
            clone.clone(),
        ],
    )
    .expect("zfs get mountpoint");
    assert_eq!(
        mount.stdout.trim().to_string(),
        handle.merged_dir.to_string_lossy().into_owned()
    );
    cleanup_isolation(&handle).expect("cleanup");
    assert!(!handle.base_dir.exists());
    assert!(!handle.merged_dir.exists());
    assert!(!zfs_lists(&clone), "clone dataset survived teardown");
    assert!(!zfs_lists(&snapshot), "snapshot survived teardown");
    assert!(zfs_lists(&dataset), "fixture dataset must survive teardown");
    let _ = std::fs::remove_file(lower.join("content"));
    let _ = std::fs::remove_dir_all(&home_dir);
}
