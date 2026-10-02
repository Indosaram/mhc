use maho_rpc::host_watchdog::*;

#[tokio::test]
async fn supervisor_pipe_observes_eof_after_ignoring_payload() {
    use tokio::io::AsyncWriteExt;
    let (mut supervisor, host) = tokio::net::UnixStream::pair().unwrap();
    let watch = watch_supervisor_pipe(host);
    let close = async { supervisor.write_all(b"keepalive").await.unwrap(); supervisor.shutdown().await.unwrap(); };
    let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(2), async { tokio::join!(watch, close) }).await.unwrap();
    result.unwrap();
}

#[tokio::test(start_paused = true)]
async fn reparenting_is_detected_on_the_next_tick() {
    let start = tokio::time::Instant::now();
    let reason = watch_supervisor_parent(42, 77, || (1, true)).await;
    assert_eq!(start.elapsed(), std::time::Duration::from_millis(HOST_WATCH_PPID_INTERVAL_MS));
    assert!(reason.contains("ppid=1"));
}
