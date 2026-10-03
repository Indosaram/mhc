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
    let event = tokio::time::timeout(std::time::Duration::from_secs(5), receiver.recv()).await.unwrap().unwrap();
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
