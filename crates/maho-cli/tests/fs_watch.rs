use maho_cli::utils::fs_watch::*;
#[tokio::test]
async fn directory_probe_opens_real_directory_and_rejects_missing_or_file() {
    let directory = tempfile::tempdir().unwrap();
    probe_directory_openable(directory.path()).await.unwrap();
    assert!(probe_directory_openable(&directory.path().join("missing")).await.is_err());
    let file = directory.path().join("file");
    std::fs::write(&file, b"").unwrap();
    assert!(probe_directory_openable(&file).await.is_err());
}
#[cfg(unix)]
#[test]
fn canonical_watch_path_does_not_touch_unix_filesystem() {
    let path = std::path::Path::new("/missing/autofs/../trigger");
    assert_eq!(canonical_watch_path(path), path);
}
#[tokio::test]
async fn watcher_subscribes_before_write_and_delivers_filename() {
    let directory = tempfile::tempdir().unwrap();
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let watcher = watch_with_error_handler(directory.path(), move |kind, name| { let _ = sender.send((kind.to_owned(), name)); }, || panic!("watch error"), false).unwrap();
    std::fs::write(directory.path().join("changed"), b"data").unwrap();
    // FSEvents can also report the watched root itself (an empty relative name); wait for the file's event.
    let event = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let event = receiver.recv().await.unwrap();
            if event.1.as_deref() != Some("") { break event; }
        }
    }).await.unwrap();
    assert!(matches!(event.0.as_str(), "rename" | "change"));
    assert_eq!(event.1.as_deref(), Some("changed"));
    close_watcher(Some(watcher));
}
#[cfg(unix)]
#[tokio::test]
async fn watcher_symlink_root_delivers_relative_filename() {
    let directory = tempfile::tempdir().unwrap();
    let real_directory = directory.path().join("real");
    std::fs::create_dir(&real_directory).unwrap();
    let symlink = directory.path().join("link");
    std::os::unix::fs::symlink(&real_directory, &symlink).unwrap();
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let watcher = watch_with_error_handler(&symlink, move |kind, name| { let _ = sender.send((kind.to_owned(), name)); }, || panic!("watch error"), false).unwrap();
    std::fs::write(symlink.join("changed"), b"data").unwrap();
    // FSEvents can first report the watched root itself (an empty relative name), e.g. the `real`
    // directory created just before the watch started; wait for the file's own event.
    let event = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let event = receiver.recv().await.unwrap();
            if event.1.as_deref() != Some("") { break event; }
        }
    }).await.unwrap();
    assert!(matches!(event.0.as_str(), "rename" | "change"));
    assert_eq!(event.1.as_deref(), Some("changed"));
    close_watcher(Some(watcher));
}
#[test]
fn missing_watch_path_reports_error_once() {
    let directory = tempfile::tempdir().unwrap();
    let errors = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = errors.clone();
    assert!(watch_with_error_handler(&directory.path().join("missing"), |_, _| {}, move || { observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst); }, false).is_none());
    assert_eq!(errors.load(std::sync::atomic::Ordering::SeqCst), 1);
}
// senpi issue-1229-win-fswatch-noncanonical-abort.test.ts: a directory watched through a
// non-canonical (8.3 short-name) path delivers its entry events instead of failing.
#[cfg(windows)]
#[tokio::test]
async fn upstream_a_non_canonical_directory_path_still_delivers_events() {
    let directory = tempfile::Builder::new().prefix("senpi-1229-long-directory-name-").tempdir().unwrap();
    let long = directory.path().to_string_lossy().into_owned();
    let output = std::process::Command::new("cmd.exe").args(["/d", "/s", "/c", &format!("for %I in (\"{long}\") do @echo %~sI")]).output().unwrap();
    let alias = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if alias.is_empty() || alias == long {
        return; // the volume exposes no 8.3 alias (upstream skips this case too)
    }
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let watcher = watch_with_error_handler(std::path::Path::new(&alias), move |_, name| { let _ = sender.send(name); }, || panic!("watch error"), false).expect("watcher");
    std::fs::write(directory.path().join("regression-write-1229.txt"), b"change").unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), receiver.recv()).await.expect("event delivered").expect("open channel");
    close_watcher(Some(watcher));
}
