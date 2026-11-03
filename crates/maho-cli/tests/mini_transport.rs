use maho_cli::experimental::mini::shared::transport::*;
use serde_json::json;
#[tokio::test]
async fn cancelling_receive_preserves_consumed_partial_frame() {
    use tokio::io::AsyncWriteExt;
    use std::{future::Future, task::Poll};
    let (input, mut output) = tokio::io::duplex(4096);
    let mut connection = JsonConnection::new(input, tokio::io::sink());
    output.write_all(b"{\"value\":").await.unwrap();
    {
        let mut receive = std::pin::pin!(connection.receive());
        std::future::poll_fn(|cx| {
            assert!(matches!(receive.as_mut().poll(cx), Poll::Pending));
            Poll::Ready(())
        }).await;
    }
    output.write_all(b"42}\n").await.unwrap();
    let received = tokio::time::timeout(std::time::Duration::from_secs(1), connection.receive()).await.unwrap().unwrap();
    assert_eq!(received, Some(json!({"value":42})));
}
#[tokio::test]
async fn framed_transport_roundtrips_unicode_and_closes_once() {
    let (left, right) = tokio::io::duplex(4096);
    let (left_read, left_write) = tokio::io::split(left);
    let (right_read, right_write) = tokio::io::split(right);
    let mut left = JsonConnection::new(left_read, left_write);
    let mut right = JsonConnection::new(right_read, right_write);
    left.send(&json!({"text":"한글\ntext"})).await.unwrap();
    assert_eq!(right.receive().await.unwrap(), Some(json!({"text":"한글\ntext"})));
    left.close().await.unwrap();
    assert_eq!(right.receive().await.unwrap(), None);
    assert!(right.closed());
}
#[cfg(unix)]
#[tokio::test]
async fn socket_transport_uses_real_unix_socket() {
    let directory = tempfile::tempdir().unwrap();
    let transport = SocketTransport { path: directory.path().join("socket") };
    let listener = transport.listen().await.unwrap();
    let mut client = transport.connect().await.unwrap();
    let (socket, _) = listener.accept().await.unwrap();
    let (input, output) = socket.into_split();
    let mut server = JsonConnection::new(input, output);
    client.send(&json!({"kind":"ping"})).await.unwrap();
    assert_eq!(server.receive().await.unwrap(), Some(json!({"kind":"ping"})));
    client.close().await.unwrap(); server.close().await.unwrap();
}
/// senpi `childConnection` over a real child's pipes: frames round-trip, and closing signals the started child.
#[cfg(unix)]
#[tokio::test]
async fn child_connection_frames_the_childs_pipes_and_close_signals_it() {
    let mut child = tokio::process::Command::new("/bin/cat").stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).kill_on_drop(true).spawn().unwrap();
    let mut connection = child_connection(&mut child).unwrap();
    connection.send(&json!({"type":"ping"})).await.unwrap();
    assert_eq!(connection.receive().await.unwrap(), Some(json!({"type":"ping"})));
    assert!(signal_spawned_child(&mut child));
    assert!(!child.wait().await.unwrap().success());
    assert!(!signal_spawned_child(&mut child), "a reaped child has no pid and is never signalled");
}
/// senpi#3706: a child spawned without pipes is refused, and the refusal sends no signal.
#[cfg(unix)]
#[tokio::test]
async fn child_connection_requires_pipes() {
    let mut child = tokio::process::Command::new("/bin/true").kill_on_drop(true).spawn().unwrap();
    assert_eq!(child_connection(&mut child).err().as_deref(), Some("Child process was spawned without pipes"));
    child.wait().await.unwrap();
    assert!(!signal_spawned_child(&mut child));
}
#[cfg(unix)]
#[tokio::test]
async fn failed_spawn_yields_no_child_and_reaped_child_sends_no_signal() {
    let root = tempfile::tempdir().expect("missing executable root");
    let failed = tokio::process::Command::new(root.path().join("missing-executable")).spawn();
    assert!(failed.is_err(), "spawning nonexistent binary yields Err");
    let mut child = tokio::process::Command::new("/bin/true").kill_on_drop(true).spawn().expect("spawn true");
    let status = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await.expect("child exit deadline").expect("wait for child");
    assert!(status.success());
    assert!(!signal_spawned_child(&mut child));
}
