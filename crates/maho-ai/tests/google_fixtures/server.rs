//! A recording HTTP server for the google adapter tests.
//!
//! The TS suites mock `@google/genai` so no request leaves the process; the Rust suites drive the
//! real `reqwest` path against this server, which records the request head (method line + headers)
//! and body and replies with fixed SSE bytes. That keeps every assertion the TS suites make —
//! payload, headers, event order, stop reasons — while exercising the wire code instead of a stub.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Debug, Clone, Default)]
pub struct RecordedRequest {
    pub request_line: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl RecordedRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub fn url_path(&self) -> &str {
        self.request_line.split(' ').nth(1).unwrap_or_default()
    }
}

pub struct RecordingServer {
    addr: SocketAddr,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    handle: tokio::task::JoinHandle<()>,
}

impl RecordingServer {
    /// Serves `body` with `content_type` for every request, recording each request.
    pub async fn start(body: Vec<u8>, content_type: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind recording server");
        let addr = listener.local_addr().expect("recording server addr");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&requests);
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else { return };
                let body = body.clone();
                let recorder = Arc::clone(&recorder);
                tokio::spawn(async move {
                    let mut buffer = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let header_end = loop {
                        match socket.read(&mut chunk).await {
                            Ok(0) | Err(_) => break None,
                            Ok(read) => {
                                buffer.extend_from_slice(&chunk[..read]);
                                if let Some(index) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                                    break Some(index + 4);
                                }
                            }
                        }
                    };
                    let Some(header_end) = header_end else { return };
                    let head = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
                    let mut lines = head.split("\r\n");
                    let request_line = lines.next().unwrap_or_default().to_owned();
                    let headers: Vec<(String, String)> = lines
                        .filter(|line| !line.is_empty())
                        .filter_map(|line| line.split_once(':'))
                        .map(|(name, value)| (name.trim().to_owned(), value.trim().to_owned()))
                        .collect();
                    let content_length = headers
                        .iter()
                        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                        .and_then(|(_, value)| value.parse::<usize>().ok())
                        .unwrap_or(0);
                    let mut body_bytes = buffer[header_end..].to_vec();
                    while body_bytes.len() < content_length {
                        match socket.read(&mut chunk).await {
                            Ok(0) | Err(_) => break,
                            Ok(read) => body_bytes.extend_from_slice(&chunk[..read]),
                        }
                    }
                    recorder.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(RecordedRequest {
                        request_line,
                        headers,
                        body: String::from_utf8_lossy(&body_bytes).into_owned(),
                    });
                    let response = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.write_all(&body).await;
                    let _ = socket.flush().await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        Self { addr, requests, handle }
    }

    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
    }

    pub fn last_request(&self) -> RecordedRequest {
        self.requests().pop().expect("a recorded request")
    }
}

impl Drop for RecordingServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// One `data:` SSE frame per chunk, the shape the SDK's stream reader parses.
pub fn sse_body(chunks: &[serde_json::Value]) -> Vec<u8> {
    let mut body = String::new();
    for chunk in chunks {
        body.push_str("data: ");
        body.push_str(&chunk.to_string());
        body.push_str("\n\n");
    }
    body.into_bytes()
}
