use maho_rpc::socket_ownership::*;
use std::time::Duration;

#[tokio::test]
async fn published_identity_is_read_without_waiting() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("owner");
    let identity = SocketFileIdentity { dev: 7, ino: 19 };
    write_socket_identity_file(&path, identity).unwrap();
    assert_eq!(wait_for_socket_identity_file(&path, Duration::ZERO, Duration::from_millis(25)).await.unwrap(), Some(identity));
}

#[tokio::test(start_paused = true)]
async fn absent_identity_stops_at_the_bounded_deadline() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("absent");
    assert_eq!(wait_for_socket_identity_file(&path, Duration::from_millis(50), Duration::from_millis(25)).await.unwrap(), None);
}

#[tokio::test]
async fn truncated_identity_is_not_an_ownership_token() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("owner");
    std::fs::write(&path, "{").unwrap();
    assert_eq!(wait_for_socket_identity_file(&path, Duration::ZERO, Duration::from_millis(25)).await.unwrap(), None);
}
