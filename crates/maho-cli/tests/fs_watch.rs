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
