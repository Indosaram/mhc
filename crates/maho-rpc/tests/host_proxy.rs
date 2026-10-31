#![cfg(unix)]
#[tokio::test(start_paused = true)]
async fn installed_idle_timer_exits_after_a_continuous_idle_window() {
    let (_activity, receiver) =
        tokio::sync::watch::channel(maho_rpc::host_lifecycle::HostActivity::default());
    let start = tokio::time::Instant::now();
    maho_rpc::host_lifecycle::wait_for_idle_exit(100., receiver)
        .await
        .unwrap();
    assert_eq!(start.elapsed(), std::time::Duration::from_millis(100));
}

/// The POSIX supervisor path: `run_public_proxy` with a memory-only internal key reaches the host only through the
/// sealed hop, and a drain keeps the attached client until the host ends it.
#[tokio::test]
async fn draining_public_proxy_preserves_attached_connection_over_the_sealed_internal_hop() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    const KEY: [u8; 32] = [7; 32];
    let dir = tempfile::tempdir().unwrap();
    let public = dir.path().join("public");
    let internal = dir.path().join("internal");
    let listener = tokio::net::UnixListener::bind(&public).unwrap();
    let backend = tokio::net::UnixListener::bind(&internal).unwrap();
    let (drain, draining) = tokio::sync::watch::channel(false);
    let (count, mut counts) = tokio::sync::watch::channel(0);
    let options = maho_rpc::host_lifecycle_proxy::PublicProxyOptions {
        internal_socket: internal,
        internal_key: Some(KEY),
        public_endpoint: None,
        require_token: false,
        admission: std::sync::Arc::new(maho_rpc::host_admission::HostAdmission::new(
            serde_json::Map::new(),
        )),
    };
    let proxy = tokio::spawn(maho_rpc::host_lifecycle_proxy::run_public_proxy(
        listener, options, draining, count,
    ));
    let scenario = async {
        let mut client = tokio::net::UnixStream::connect(&public).await.unwrap();
        let (socket, _) = backend.accept().await.unwrap();
        // A raw (unsealed) internal side fails this handshake with auth_required.
        let mut host = maho_rpc::internal_channel::accept_internal_connection(
            socket,
            &KEY,
            serde_json::Map::new(),
        )
        .await
        .unwrap();
        counts.wait_for(|count| *count == 1).await.unwrap();
        drain.send(true).unwrap();
        client
            .write_all(b"{\"id\":\"r\",\"type\":\"ping\"}\n")
            .await
            .unwrap();
        client.shutdown().await.unwrap();
        let mut request = String::new();
        host.read_to_string(&mut request).await.unwrap();
        assert_eq!(request, "{\"id\":\"r\",\"type\":\"ping\"}\n");
        host.write_all(b"{\"type\":\"final_lifecycle\"}\n")
            .await
            .unwrap();
        host.shutdown().await.unwrap();
        let mut output = String::new();
        client.read_to_string(&mut output).await.unwrap();
        assert_eq!(output, "{\"type\":\"final_lifecycle\"}\n");
        proxy.await.unwrap().unwrap();
        assert_eq!(*counts.borrow(), 0);
    };
    // Hang guard only; nothing in the scenario waits on time.
    tokio::time::timeout(std::time::Duration::from_secs(30), scenario)
        .await
        .unwrap();
}
