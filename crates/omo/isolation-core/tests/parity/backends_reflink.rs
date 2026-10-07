use std::path::Path;
use std::sync::{Arc, Mutex};

use isolation_core::test_support::fixture;
use isolation_core::{
    IsolationBackend, IsolationContext, IsolationError, LoadReflink, ReflinkBackend, ReflinkIoctl,
    FICLONE,
};

use crate::fake::{record, FakeRuntime};

struct FakeIoctl {
    ioctl: Arc<dyn Fn(i32, u64, i32) -> i32 + Send + Sync>,
    errno: i32,
}

impl ReflinkIoctl for FakeIoctl {
    fn ioctl(&self, dst: i32, request: u64, src: i32) -> i32 {
        (self.ioctl)(dst, request, src)
    }

    fn errno(&self) -> i32 {
        self.errno
    }
}

fn loader(ioctl: Arc<dyn Fn(i32, u64, i32) -> i32 + Send + Sync>, errno: i32) -> LoadReflink {
    Arc::new(move || {
        let value: Arc<dyn ReflinkIoctl> = Arc::new(FakeIoctl {
            ioctl: Arc::clone(&ioctl),
            errno,
        });
        Some(value)
    })
}

fn copy_fd(src: i32, dst: i32) {
    use std::io::{Read, Write};
    use std::os::fd::FromRawFd;
    let mut reader = unsafe { std::fs::File::from_raw_fd(libc::dup(src)) };
    let mut writer = unsafe { std::fs::File::from_raw_fd(libc::dup(dst)) };
    let mut buffer = Vec::new();
    let _ = reader.read_to_end(&mut buffer);
    let _ = writer.write_all(&buffer);
}

fn context(id: &str, base_dir: &Path, max_copy_bytes: Option<u64>) -> IsolationContext {
    IsolationContext {
        id: id.to_string(),
        base_dir: base_dir.to_path_buf(),
        cross_device: false,
        max_copy_bytes,
    }
}

#[test]
fn ficlone_walks_fd_pairs_preserves_metadata_and_symlinks_and_isolates_writes() {
    let f = fixture();
    let merged = f.root.join("merged");
    std::fs::create_dir_all(f.repo_root.join("dir")).expect("dir");
    std::fs::write(f.repo_root.join("dir/data"), "original").expect("data");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            f.repo_root.join("dir/data"),
            std::fs::Permissions::from_mode(0o751),
        )
        .expect("mode");
    }
    filetime::set_file_mtime(
        f.repo_root.join("dir/data"),
        filetime::FileTime::from_unix_time(1234567890, 0),
    )
    .expect("mtime");
    std::os::unix::fs::symlink("dir/data", f.repo_root.join("link")).expect("symlink");
    let requests: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&requests);
    let backend = ReflinkBackend::new(
        Arc::new(FakeRuntime::linux()),
        loader(
            Arc::new(move |dst, request, src| {
                sink.lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .push(request);
                copy_fd(src, dst);
                0
            }),
            0,
        ),
    );
    assert!(backend
        .probe(&f.repo_root, Some(&context("probe", &f.root, None)))
        .expect("probe")
        .available);
    backend
        .start(
            &f.repo_root,
            &merged,
            &context("walk", &f.root, None),
        )
        .expect("start");
    assert_eq!(
        *requests.lock().unwrap_or_else(|poison| poison.into_inner()),
        vec![FICLONE, FICLONE]
    );
    let mut entries: Vec<String> = std::fs::read_dir(&merged)
        .expect("readdir")
        .map(|entry| entry.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    entries.sort();
    assert_eq!(entries, vec!["dir", "link"]);
    assert_eq!(
        std::fs::read_link(merged.join("link")).expect("readlink"),
        Path::new("dir").join("data")
    );
    assert!(std::fs::symlink_metadata(merged.join("link"))
        .expect("lstat")
        .file_type()
        .is_symlink());
    let mtime = std::fs::metadata(merged.join("dir/data"))
        .expect("stat")
        .modified()
        .expect("modified")
        .duration_since(std::time::UNIX_EPOCH)
        .expect("epoch")
        .as_millis();
    assert_eq!(mtime, 1_234_567_890_000);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(merged.join("dir/data"))
                .expect("stat")
                .permissions()
                .mode()
                & 0o777,
            0o751
        );
    }
    std::fs::write(merged.join("dir/data"), "changed").expect("write");
    assert_eq!(
        std::fs::read_to_string(f.repo_root.join("dir/data")).expect("source"),
        "original"
    );
    backend.stop(&merged).expect("stop");
    assert!(!merged.exists());
}

#[test]
fn first_mid_walk_eopnotsupp_aborts_and_removes_all_partial_output() {
    let f = fixture();
    let merged = f.root.join("merged");
    std::fs::write(f.repo_root.join("a"), "a").expect("a");
    std::fs::write(f.repo_root.join("b"), "b").expect("b");
    let clones = Arc::new(Mutex::new(0usize));
    let sink = Arc::clone(&clones);
    let backend = ReflinkBackend::new(
        Arc::new(FakeRuntime::linux()),
        loader(
            Arc::new(move |dst, _request, src| {
                let mut count = sink.lock().unwrap_or_else(|poison| poison.into_inner());
                *count += 1;
                if *count == 3 {
                    return -1;
                }
                drop(count);
                copy_fd(src, dst);
                0
            }),
            95,
        ),
    );
    assert!(backend
        .probe(&f.repo_root, Some(&context("probe", &f.root, None)))
        .expect("probe")
        .available);
    let error = backend
        .start(&f.repo_root, &merged, &context("walk", &f.root, None))
        .expect_err("must fail");
    assert!(error.is_unavailable());
    assert_eq!(*clones.lock().unwrap_or_else(|poison| poison.into_inner()), 3);
    assert!(!merged.exists());
}

#[test]
fn probe_rejects_a_different_target_device_without_loading_ffi() {
    let f = fixture();
    let loads = Arc::new(Mutex::new(0usize));
    let sink = Arc::clone(&loads);
    let repo_root = f.repo_root.clone();
    let runtime = FakeRuntime {
        device: Arc::new(move |path: &Path| Ok(if path == repo_root { 1 } else { 2 })),
        ..FakeRuntime::linux()
    };
    let load: LoadReflink = Arc::new(move || {
        *sink.lock().unwrap_or_else(|poison| poison.into_inner()) += 1;
        None
    });
    let backend = ReflinkBackend::new(Arc::new(runtime), load);
    let result = backend
        .probe(&f.repo_root, Some(&context("probe", &f.home_dir, None)))
        .expect("probe");
    assert!(!result.available);
    assert_eq!(*loads.lock().unwrap_or_else(|poison| poison.into_inner()), 0);
}

#[test]
fn failed_probe_disables_direct_start_rather_than_retrying_an_unsupported_ioctl() {
    let f = fixture();
    let clones = Arc::new(Mutex::new(0usize));
    let sink = Arc::clone(&clones);
    let backend = ReflinkBackend::new(
        Arc::new(FakeRuntime::linux()),
        loader(
            Arc::new(move |_dst, _request, _src| {
                *sink.lock().unwrap_or_else(|poison| poison.into_inner()) += 1;
                -1
            }),
            95,
        ),
    );
    assert!(!backend
        .probe(&f.repo_root, Some(&context("probe", &f.root, None)))
        .expect("probe")
        .available);
    let error = backend
        .start(
            &f.repo_root,
            &f.root.join("merged"),
            &context("probe", &f.root, None),
        )
        .expect_err("must fail");
    assert!(error.is_unavailable());
    assert_eq!(*clones.lock().unwrap_or_else(|poison| poison.into_inner()), 1);
}

#[test]
fn reflink_byte_ceiling_aborts_before_cloning_oversized_input() {
    let f = fixture();
    let merged = f.root.join("merged");
    std::fs::write(f.repo_root.join("data"), "123456").expect("data");
    let clones = Arc::new(Mutex::new(0usize));
    let sink = Arc::clone(&clones);
    let backend = ReflinkBackend::new(
        Arc::new(FakeRuntime::linux()),
        loader(
            Arc::new(move |_dst, _request, _src| {
                *sink.lock().unwrap_or_else(|poison| poison.into_inner()) += 1;
                0
            }),
            0,
        ),
    );
    assert!(backend
        .probe(&f.repo_root, Some(&context("probe", &f.root, None)))
        .expect("probe")
        .available);
    let error = backend
        .start(&f.repo_root, &merged, &context("cap", &f.root, Some(5)))
        .expect_err("must fail");
    assert!(error.to_string().contains("6 bytes exceeds maxCopyBytes 5"));
    assert_eq!(*clones.lock().unwrap_or_else(|poison| poison.into_inner()), 1);
    assert!(!merged.exists());
}

#[test]
fn reflink_walk_skips_sockets_and_fifos_rather_than_opening_them() {
    if cfg!(windows) {
        return;
    }
    let f = fixture();
    let merged = f.root.join("merged");
    let status = std::process::Command::new("mkfifo")
        .arg(f.repo_root.join("fifo"))
        .status()
        .expect("mkfifo");
    assert!(status.success());
    std::fs::write(f.repo_root.join("data"), "source").expect("data");
    let listener = std::os::unix::net::UnixListener::bind(f.repo_root.join("sock")).expect("socket");
    let clones = Arc::new(Mutex::new(0usize));
    let sink = Arc::clone(&clones);
    let backend = ReflinkBackend::new(
        Arc::new(FakeRuntime::linux()),
        loader(
            Arc::new(move |dst, _request, src| {
                *sink.lock().unwrap_or_else(|poison| poison.into_inner()) += 1;
                copy_fd(src, dst);
                0
            }),
            0,
        ),
    );
    backend
        .start(&f.repo_root, &merged, &context("special", &f.root, None))
        .expect("start");
    let entries: Vec<String> = std::fs::read_dir(&merged)
        .expect("readdir")
        .map(|entry| entry.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(entries, vec!["data"]);
    assert_eq!(*clones.lock().unwrap_or_else(|poison| poison.into_inner()), 2);
    drop(listener);
    record(&crate::fake::calls(), &[]);
    let _ = IsolationError::other("placeholder");
}
