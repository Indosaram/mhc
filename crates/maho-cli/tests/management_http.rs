use maho_cli::utils::management_http::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[tokio::test]
async fn retries_transient_status_immediately_and_returns_success() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for status in ["503 Service Unavailable", "200 OK"] {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut byte = [0];
            while !request.ends_with(b"\r\n\r\n") { socket.read_exact(&mut byte).await.unwrap(); request.push(byte[0]); }
            socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
        }
    });
    let client = reqwest::Client::new();
    let request = client.get(format!("http://{address}/")).build().unwrap();
    let response = fetch_with_retry(&client, request, None, FetchRetryOptions { timeout_ms: Some(5000), ..Default::default() }).await.unwrap();
    assert_eq!(response.status(), 200);
    server.await.unwrap();
}
#[tokio::test]
async fn cancellation_prevents_any_request() {
    let controller = maho_ai::utils::abort::AbortController::new();
    controller.abort(None);
    let client = reqwest::Client::new();
    let request = client.get("http://127.0.0.1:1/").build().unwrap();
    assert!(fetch_with_retry(&client, request, Some(&controller.signal()), Default::default()).await.is_err());
}
