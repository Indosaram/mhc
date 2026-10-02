use maho_rpc::{host_supersession::*, socket_ownership::stat_socket_identity};

#[tokio::test(start_paused = true)]
async fn replacement_is_observed_through_the_public_socket() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("public.sock");
    let original = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let identity = stat_socket_identity(&path).unwrap();
    let replacement_path = temp.path().join("replacement.sock");
    let replacement = std::os::unix::net::UnixListener::bind(&replacement_path).unwrap();
    std::fs::rename(replacement_path, &path).unwrap();
    let loss = wait_for_supersession(path.to_str().unwrap(), identity, || false).await;
    assert_eq!(loss, Some(EndpointLoss::Replaced));
    assert!(path.exists());
    drop((original, replacement));
}

#[tokio::test(start_paused = true)]
async fn missing_socket_requires_three_timer_observations() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("public.sock");
    let original = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let identity = stat_socket_identity(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    let start = tokio::time::Instant::now();
    let loss = wait_for_supersession(path.to_str().unwrap(), identity, || false).await;
    assert_eq!(loss, Some(EndpointLoss::Absent));
    assert!(start.elapsed() >= std::time::Duration::from_millis(3 * SUPERSESSION_POLL_MS));
    drop(original);
}

#[tokio::test]
async fn unknown_identity_does_not_install_a_watcher() {
    assert_eq!(wait_for_supersession("unused", None, || false).await, None);
}
