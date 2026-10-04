use maho_ext_pi_webfetch::webfetch::fetcher::discard_body;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn discard_fixture(wire: &'static [u8]) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut bytes = [0; 4096];
        assert!(socket.read(&mut bytes).await.expect("request") > 0);
        socket.write_all(wire).await.expect("response");
    });
    let response = reqwest::get(format!("http://{address}")).await.expect("headers");
    tokio::time::timeout(std::time::Duration::from_secs(5), discard_body(response)).await.expect("bounded discard");
    server.await.expect("joined fixture");
}

#[tokio::test]
async fn discard_drains_small_body() {
    discard_fixture(b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nConnection: close\r\n\r\nhello world").await;
}
#[tokio::test]
async fn discard_swallows_midstream_failure() {
    discard_fixture(b"HTTP/1.1 200 OK\r\nContent-Length: 20\r\nConnection: close\r\n\r\npartial").await;
}
#[tokio::test]
async fn discard_declared_body_above_dump_limit() {
    discard_fixture(b"HTTP/1.1 200 OK\r\nContent-Length: 1025\r\nConnection: close\r\n\r\n").await;
}
