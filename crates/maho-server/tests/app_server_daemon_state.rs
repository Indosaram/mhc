use maho_server::app_server::{cli_args::DaemonVerb, daemon::{DaemonPaths,run_daemon_command}};
use serde_json::json;

#[tokio::test]
async fn daemon_status_uses_sqlite_lock_and_releases_it_after_request() {
    let directory = tempfile::tempdir().unwrap();
    let paths = DaemonPaths::new(directory.path());
    let response = run_daemon_command(&paths,DaemonVerb::Status,&json!({"kind":"stdio","url":"stdio://"}),"1",std::path::Path::new("/unused"),&[]).await.unwrap();
    assert_eq!(response["status"],"not-running");
    let connection = rusqlite::Connection::open(&paths.lock_file).unwrap();
    connection.busy_timeout(std::time::Duration::ZERO).unwrap();
    connection.execute_batch("BEGIN EXCLUSIVE; COMMIT;").unwrap();
}

#[tokio::test]
async fn daemon_command_preserves_and_rejects_legacy_lock_directory() {
    let directory = tempfile::tempdir().unwrap();
    let paths = DaemonPaths::new(directory.path());
    std::fs::create_dir_all(&paths.lock_file).unwrap();
    let error = run_daemon_command(&paths,DaemonVerb::Status,&json!({"kind":"stdio","url":"stdio://"}),"1",std::path::Path::new("/unused"),&[]).await.unwrap_err();
    assert!(error.to_string().contains("Legacy lock directory"));
    assert!(paths.lock_file.is_dir());
}
