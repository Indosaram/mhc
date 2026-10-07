use std::ffi::CStr;
use std::path::Path;
use std::sync::Arc;

use isolation_core::backends::git_fixture::{git, repo};
use isolation_core::test_support::fixture;
use isolation_core::{
    cleanup_isolation, clone_with_symbols, ensure_isolation, ApfsBackend, CloneSymbols,
    EnsureIsolationOptions, IsolationBackend, IsolationError, LoadApfs, NativeLoadError,
    CLONE_NOFOLLOW,
};

struct FakeSymbols {
    errno: i32,
    calls: Arc<std::sync::Mutex<Vec<String>>>,
}

impl CloneSymbols for FakeSymbols {
    fn clonefile(&self, _src: &CStr, _dst: &CStr, flags: u32) -> i32 {
        assert_eq!(flags, CLONE_NOFOLLOW);
        self.calls
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push("clonefile".to_string());
        -1
    }

    fn errno(&self) -> i32 {
        self.calls
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push("__error".to_string());
        self.errno
    }

    fn strerror(&self, errno: i32) -> String {
        self.calls
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push("strerror".to_string());
        format!("errno {errno}")
    }
}

#[test]
fn apfs_clones_many_files_cow_skips_special_entries_and_never_follows_entry_symlinks() {
    if !cfg!(target_os = "macos") {
        return;
    }
    let f = repo();
    let lower = f.repo_root.clone();
    std::fs::write(lower.join("staged"), "stage").expect("staged");
    git(&lower, &["add", "staged"]).expect("add");
    std::fs::write(lower.join("untracked"), "untracked").expect("untracked");
    std::fs::write(lower.join("ignored"), "ignored").expect("ignored");
    std::os::unix::fs::symlink("tracked", lower.join("link")).expect("link");
    std::fs::create_dir_all(lower.join("node_modules/.bin")).expect("node_modules");
    std::os::unix::fs::symlink("../../tracked", lower.join("node_modules/.bin/x")).expect("bin link");
    std::fs::create_dir_all(lower.join("many")).expect("many");
    for index in 0..5000 {
        std::fs::write(lower.join(format!("many/{index}")), format!("file {index}")).expect("file");
    }
    assert!(std::process::Command::new("mkfifo")
        .arg(lower.join("fifo"))
        .status()
        .expect("mkfifo")
        .success());
    let backend = ApfsBackend::default();
    assert!(backend.probe(&lower, None).expect("probe").available);
    let merged = f.root.join("merged");
    let detail = backend
        .start(
            &lower,
            &merged,
            &isolation_core::IsolationContext {
                id: "test".to_string(),
                base_dir: f.root.clone(),
                cross_device: false,
                max_copy_bytes: None,
            },
        )
        .expect("start")
        .expect("detail");
    assert_eq!(detail.strategy_detail, "clone_tree");
    for name in ["tracked", "staged", "untracked", "ignored", "many/4999"] {
        assert_eq!(
            std::fs::read_to_string(merged.join(name)).expect("file"),
            std::fs::read_to_string(lower.join(name)).expect("file")
        );
    }
    for name in ["link", "node_modules/.bin/x"] {
        assert!(std::fs::symlink_metadata(merged.join(name))
            .expect("lstat")
            .file_type()
            .is_symlink());
    }
    for name in ["fifo"] {
        assert!(!merged.join(name).exists());
    }
    std::fs::write(merged.join("tracked"), "changed").expect("write");
    assert_eq!(
        std::fs::read_to_string(lower.join("tracked")).expect("source"),
        "base\n"
    );
    backend.stop(&merged).expect("stop");
}

#[test]
fn clonefile_errno_is_read_before_strerror_and_classifies_exdev_versus_eexist() {
    if !cfg!(target_os = "macos") {
        return;
    }
    for errno in [18, 17] {
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let symbols: Arc<dyn CloneSymbols> = Arc::new(FakeSymbols {
            errno,
            calls: Arc::clone(&calls),
        });
        let error = clone_with_symbols(&symbols, Path::new("source"), Path::new("destination"))
            .expect_err("must fail");
        assert!(error.to_string().contains(&format!("errno {errno}")));
        assert_eq!(error.is_unavailable(), errno == 18);
        assert_eq!(
            *calls.lock().unwrap_or_else(|poison| poison.into_inner()),
            vec!["clonefile", "__error", "strerror"]
        );
    }
}

#[test]
fn ensure_removes_stale_snapshot_locks_before_the_git_consistency_probe() {
    if !cfg!(target_os = "macos") {
        return;
    }
    let f = repo();
    std::fs::write(f.repo_root.join(".git/index.lock"), "stale").expect("lock");
    let handle = ensure_isolation(EnsureIsolationOptions {
        repo_root: f.repo_root.clone(),
        id: "apfs-lock".to_string(),
        preferred: None,
        backends: vec![Arc::new(ApfsBackend::default())],
        platform: None,
        home_dir: Some(f.home_dir.clone()),
        owner: None,
        max_copy_bytes: None,
    })
    .expect("handle");
    assert!(!handle.merged_dir.join(".git/index.lock").exists());
    assert_eq!(git(&handle.merged_dir, &["status", "--porcelain"]).expect("status"), "");
    cleanup_isolation(&handle).expect("cleanup");
}

#[test]
fn apfs_loader_failures_other_than_a_missing_module_propagate() {
    let load: LoadApfs = Arc::new(|| {
        Err(NativeLoadError {
            code: None,
            message: "dlopen exploded".to_string(),
        })
    });
    let backend = ApfsBackend::with_platform(load, "darwin");
    let error = backend.probe(Path::new("unused"), None).expect_err("must fail");
    assert!(!error.is_unavailable());
    assert!(error.to_string().contains("dlopen exploded"));
}

#[test]
fn apfs_treats_a_missing_native_module_as_unavailable() {
    let load: LoadApfs = Arc::new(|| {
        Err(NativeLoadError {
            code: Some("ERR_UNKNOWN_BUILTIN_MODULE".to_string()),
            message: "Cannot find module 'bun:ffi'".to_string(),
        })
    });
    let backend = ApfsBackend::with_platform(load, "darwin");
    let error = backend.probe(Path::new("unused"), None).expect_err("must fail");
    assert!(error.is_unavailable());
    let _ = fixture();
    let _ = IsolationError::other("placeholder");
}
