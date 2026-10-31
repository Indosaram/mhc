//! Ports `thread-tools.test.ts` / `discovery-validation.test.ts` (omo v5.1.52) against a loopback
//! MCP stub spoken over raw TCP, so no extra HTTP server dependency is needed.

use maho_omo_desktop_toolkit::ToolkitGrant;
use maho_omo_desktop_toolkit::runtime::RETRY_GAPS_IN_TURNS;
use maho_omo_desktop_toolkit::thread_tools::{call_thread_tool, list_thread_tools};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

struct Stub {
    grant: ToolkitGrant,
    token: String,
    methods: Arc<Mutex<Vec<String>>>,
    _dir: tempfile::TempDir,
}

async fn read_request(stream: &mut tokio::net::TcpStream) -> Option<(String, String, Vec<u8>)> {
    let mut data = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        data.extend_from_slice(&chunk[..read]);
        if let Some(split) = data.windows(4).position(|window| window == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&data[..split]).into_owned();
            let length = head
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                })
                .unwrap_or(0);
            while data.len() < split + 4 + length {
                let read = stream.read(&mut chunk).await.ok()?;
                if read == 0 {
                    return None;
                }
                data.extend_from_slice(&chunk[..read]);
            }
            let first = head.lines().next().unwrap_or("").to_owned();
            return Some((first, head, data[split + 4..split + 4 + length].to_vec()));
        }
    }
}

async fn stub(tools: Value, sse: bool, echo_token: bool) -> Stub {
    let dir = tempfile::tempdir().expect("dir");
    let token = format!("test-credential-{}", std::process::id());
    let token_file = dir.path().join("bearer");
    std::fs::write(&token_file, &token).expect("token");
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let methods = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&methods);
    let secret = token.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let seen = Arc::clone(&seen);
            let tools = tools.clone();
            let secret = secret.clone();
            tokio::spawn(async move {
                while let Some((first, head, body)) = read_request(&mut stream).await {
                    let authorized = head.contains(&format!("Bearer {secret}"));
                    if !authorized {
                        let _ = stream
                            .write_all(b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\n\r\n")
                            .await;
                        continue;
                    }
                    if first.starts_with("DELETE") {
                        let _ = stream
                            .write_all(b"HTTP/1.1 204 No Content\r\ncontent-length: 0\r\n\r\n")
                            .await;
                        continue;
                    }
                    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                    let method = request["method"].as_str().unwrap_or("").to_owned();
                    seen.lock().expect("seen").push(method.clone());
                    if method == "notifications/initialized" {
                        let _ = stream
                            .write_all(b"HTTP/1.1 202 Accepted\r\ncontent-length: 0\r\n\r\n")
                            .await;
                        continue;
                    }
                    let result = match method.as_str() {
                        "initialize" => {
                            json!({"protocolVersion":"2025-03-26","capabilities":{"tools":{}}})
                        }
                        "tools/list" => json!({"tools": tools}),
                        "tools/call" if echo_token => {
                            json!({"content":[{"type":"text","text":secret}]})
                        }
                        _ => json!({"content":[{"type":"text","text":"organized"}]}),
                    };
                    let payload =
                        json!({"jsonrpc":"2.0","id":request["id"],"result":result}).to_string();
                    let (content_type, body) = if sse {
                        (
                            "text/event-stream",
                            format!(": keepalive\r\nevent: message\r\ndata: {payload}\r\n\r\n"),
                        )
                    } else {
                        ("application/json", payload)
                    };
                    let session = if method == "initialize" {
                        "mcp-session-id: s1\r\n"
                    } else {
                        ""
                    };
                    let response = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\n{session}content-length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                }
            });
        }
    });
    Stub {
        grant: ToolkitGrant {
            endpoint: format!("http://127.0.0.1:{port}/mcp"),
            token_file: token_file.to_string_lossy().into_owned(),
        },
        token,
        methods,
        _dir: dir,
    }
}

fn schema() -> Value {
    json!({"type":"object","properties":{"title":{"type":"string"}}})
}

#[tokio::test]
async fn json_and_sse_replies_list_and_call_with_the_bearer_from_the_token_file() {
    for sse in [false, true] {
        let stub = stub(
            json!([{"name":"thread_organize","description":"Organize","inputSchema":schema()}]),
            sse,
            false,
        )
        .await;
        let listed = list_thread_tools(&stub.grant, 2_000, None)
            .await
            .expect("list");
        assert_eq!(listed.tools.len(), 1);
        assert_eq!(listed.tools[0].name, "thread_organize");
        assert!(listed.invalid.is_empty());
        let result = call_thread_tool(
            Some(&stub.grant),
            "thread_organize",
            json!({"title":"x"}),
            2_000,
            None,
        )
        .await;
        assert_ne!(result.is_error, Some(true));
        assert!(
            stub.methods
                .lock()
                .expect("m")
                .iter()
                .any(|m| m == "tools/call")
        );
    }
}

#[tokio::test]
async fn invalid_descriptors_are_skipped_with_nonsecret_reasons() {
    let long = "a".repeat(57);
    let tools = json!([
        {"name":"", "inputSchema":schema()},
        {"name":"a.b", "inputSchema":schema()},
        {"name":long, "inputSchema":schema()},
        {"name":"bad_schema", "inputSchema":{"type":"array"}},
        {"name":"bad_description", "description":42, "inputSchema":schema()},
        null,
        {"name":"a".repeat(56), "inputSchema":schema()},
        {"name":"Mixed-123_Name", "inputSchema":schema()},
    ]);
    let stub = stub(tools, false, false).await;
    let listed = list_thread_tools(&stub.grant, 2_000, None)
        .await
        .expect("list");
    let names: Vec<_> = listed.tools.iter().map(|tool| tool.name.clone()).collect();
    assert_eq!(names, vec!["a".repeat(56), "Mixed-123_Name".to_owned()]);
    assert_eq!(
        listed.invalid,
        vec![
            "invalid_name",
            "invalid_name",
            "invalid_name",
            "invalid_schema",
            "invalid_descriptor",
            "invalid_descriptor"
        ]
    );
}

#[tokio::test]
async fn a_response_echoing_the_bearer_or_a_missing_grant_never_discloses_it() {
    let stub = stub(json!([]), false, true).await;
    let result = call_thread_tool(Some(&stub.grant), "t", json!({}), 2_000, None).await;
    assert_eq!(result.is_error, Some(true));
    assert!(!result.details.to_string().contains(&stub.token));
    assert!(!format!("{:?}", result.content).contains(&stub.token));
    let refused = call_thread_tool(None, "t", json!({}), 2_000, None).await;
    assert_eq!(refused.is_error, Some(true));
    assert_eq!(refused.details, json!({"reason":"no_session_grant"}));
}

#[test]
fn background_retries_back_off_over_three_gaps_then_stop() {
    assert_eq!(RETRY_GAPS_IN_TURNS, [1, 2, 4]);
}
