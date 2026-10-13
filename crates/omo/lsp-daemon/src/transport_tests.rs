use super::*;
use pretty_assertions::assert_eq;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn endpoint(root: &Path) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(format!(
            r"\\.\pipe\maho-lsp-transport-test-{}",
            std::process::id()
        ))
    } else {
        root.join("daemon.sock")
    }
}

#[tokio::test]
async fn bound_endpoint_accepts_a_client_and_carries_bytes_both_ways() {
    let root = tempfile::tempdir().expect("tempdir");
    let path = endpoint(root.path());
    let listener = Listener::bind(&path).expect("bind");
    let server = tokio::spawn(async move {
        let mut stream = listener.accept().await.expect("accept");
        let mut buffer = [0_u8; 4];
        stream.read_exact(&mut buffer).await.expect("server read");
        stream.write_all(&buffer).await.expect("server write");
        stream.flush().await.expect("server flush");
    });
    let exchange = async {
        let mut client = connect(&path).await.expect("connect");
        client.write_all(b"ping").await.expect("client write");
        let mut echoed = [0_u8; 4];
        client.read_exact(&mut echoed).await.expect("client read");
        echoed
    };
    let echoed = tokio::time::timeout(Duration::from_secs(10), exchange)
        .await
        .expect("bounded exchange");
    assert_eq!(&echoed, b"ping");
    server.await.expect("server task");
}
