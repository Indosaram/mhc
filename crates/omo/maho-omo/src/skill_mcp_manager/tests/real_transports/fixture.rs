use super::SkillMcpServerContext;
use serde_json::{Value, json};
use std::{
    cell::{Ref, RefCell},
    future::Future,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};

pub(super) const POST_PATH: &str = "/messages?sessionId=legacy-fixture";
const LEGACY_PROTOCOL: &str = "2024-11-05";
pub(super) const INIT: &str = "initialize";
pub(super) const INITIALIZED: &str = "notifications/initialized";
pub(super) const OPERATIONS: [&str; 6] = [
    "tools/list",
    "resources/list",
    "prompts/list",
    "tools/call",
    "resources/read",
    "prompts/get",
];

#[derive(Clone, Copy)]
pub(super) enum Transport {
    Http,
    LegacySse,
}

pub(super) enum Replies {
    Normal,
    WithholdOperations,
    CancelAfterReceipt(Option<oneshot::Sender<()>>),
}

pub(super) struct Fixture {
    listener: TcpListener,
    transport: Transport,
    received: RefCell<Vec<String>>,
}

impl Fixture {
    pub(super) async fn new(transport: Transport) -> Self {
        Self {
            listener: loopback().await,
            transport,
            received: RefCell::default(),
        }
    }

    pub(super) fn context(&self) -> SkillMcpServerContext {
        let (kind, path) = match self.transport {
            Transport::Http => ("http", "/mcp"),
            Transport::LegacySse => ("sse", "/sse"),
        };
        let address = self.listener.local_addr().expect("loopback address");
        SkillMcpServerContext {
            skill_name: "fixture".into(),
            config: json!({
                "type": kind,
                "url": format!("http://{address}{path}"),
                "headers": {"X-Skill-Header": "present"},
            }),
        }
    }

    async fn request(&self) -> (TcpStream, String, Value) {
        let (socket, _) = self
            .listener
            .accept()
            .await
            .expect("accept loopback request");
        let mut reader = BufReader::new(socket);
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .await
            .expect("HTTP request line");
        let route = line.rsplit_once(' ').expect("method and path").0.to_owned();
        let mut length = 0;
        let mut skill_header = None;
        let mut protocol = None;
        loop {
            line.clear();
            assert_ne!(reader.read_line(&mut line).await.expect("HTTP header"), 0);
            if line == "\r\n" {
                break;
            }
            let (name, value) = line.split_once(':').expect("HTTP header pair");
            if name.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().expect("content length");
            } else if name.eq_ignore_ascii_case("x-skill-header") {
                skill_header = Some(value.trim().to_owned());
            } else if name.eq_ignore_ascii_case("mcp-protocol-version") {
                protocol = Some(value.trim().to_owned());
            }
        }
        assert_eq!(skill_header.as_deref(), Some("present"), "{route}");
        let body: Value = if length == 0 {
            Value::Null
        } else {
            let mut bytes = vec![0; length];
            reader
                .read_exact(&mut bytes)
                .await
                .expect("HTTP request body");
            serde_json::from_slice(&bytes).expect("JSON-RPC request")
        };
        if matches!(self.transport, Transport::LegacySse) && body["method"] == "tools/call" {
            assert_eq!(protocol.as_deref(), Some(LEGACY_PROTOCOL));
        }
        (reader.into_inner(), route, body)
    }

    pub(super) async fn open_events(&self, event: &str) -> TcpStream {
        let (mut socket, route, _) = self.request().await;
        assert_eq!(route, "GET /sse");
        self.received.borrow_mut().push(route);
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n")
            .await
            .expect("event stream headers");
        write_event(&mut socket, event).await;
        socket
    }

    async fn serve(&self, mut replies: Replies) {
        let mut events = match self.transport {
            Transport::LegacySse => Some(
                self.open_events(&format!("event: endpoint\ndata: {POST_PATH}\n\n"))
                    .await,
            ),
            Transport::Http => None,
        };
        let mut pending_http = Vec::new();
        let mut cancelled_id = None;
        loop {
            let (mut post, route, request) = self.request().await;
            if matches!(self.transport, Transport::Http) && route == "GET /mcp" {
                respond(&mut post, "405 Method Not Allowed", "").await;
                continue;
            }
            assert_eq!(
                route,
                match self.transport {
                    Transport::Http => "POST /mcp".to_owned(),
                    Transport::LegacySse => format!("POST {POST_PATH}"),
                }
            );
            let method = request["method"].as_str().expect("JSON-RPC method");
            if method == "notifications/cancelled" {
                if matches!(replies, Replies::CancelAfterReceipt(_)) {
                    assert_eq!(
                        request["params"]["requestId"],
                        *cancelled_id.as_ref().expect("cancelled request ID")
                    );
                    self.received.borrow_mut().push(method.to_owned());
                }
                respond(&mut post, "202 Accepted", "").await;
                continue;
            }
            self.received.borrow_mut().push(method.to_owned());
            let result = match method {
                INIT => json!({
                    "protocolVersion": if events.is_some() { LEGACY_PROTOCOL } else { "2025-11-25" },
                    "capabilities": {"tools": {}, "resources": {}, "prompts": {}},
                    "serverInfo": {"name": "loopback", "version": "1"},
                }),
                INITIALIZED => {
                    assert!(request.get("id").is_none());
                    respond(&mut post, "202 Accepted", "").await;
                    continue;
                }
                method
                    if matches!(replies, Replies::WithholdOperations)
                        && OPERATIONS.contains(&method) =>
                {
                    if events.is_some() {
                        respond(&mut post, "202 Accepted", "").await;
                    } else {
                        // Keep the connection alive without sending HTTP response headers.
                        pending_http.push(post);
                    }
                    continue;
                }
                "tools/call" if request["params"]["name"] == "slow" => {
                    let Replies::CancelAfterReceipt(receipt) = &mut replies else {
                        panic!("unexpected slow tool call");
                    };
                    cancelled_id = Some(request["id"].clone());
                    respond(&mut post, "202 Accepted", "").await;
                    receipt
                        .take()
                        .expect("first slow call")
                        .send(())
                        .expect("waiting caller");
                    continue;
                }
                "tools/list" => json!({"tools":[{"name":"echo","inputSchema":{"type":"object"}}]}),
                "tools/call" => {
                    assert_eq!(
                        request["params"],
                        json!({"name":"echo","arguments":{"text":"hello"}})
                    );
                    json!({"content":[{"type":"text","text":"hello"}]})
                }
                _ => panic!("unexpected request: {request}"),
            };
            assert!(request.get("id").is_some(), "{request}");
            let reply = json!({"jsonrpc":"2.0","id":request["id"],"result":result});
            if let Some(events) = &mut events {
                respond(&mut post, "202 Accepted", "").await;
                if let Some(id) = cancelled_id.take() {
                    // Send the abandoned reply while the next request is waiting on the same stream.
                    let late = json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":"stale"}]}});
                    write_event(events, &format!("event: message\ndata: {late}\n\n")).await;
                }
                write_event(events, &format!("event: message\ndata: {reply}\n\n")).await;
            } else {
                respond(&mut post, "200 OK", &reply.to_string()).await;
            }
        }
    }

    pub(super) fn received(&self) -> Ref<'_, Vec<String>> {
        self.received.borrow()
    }

    pub(super) async fn run(&self, replies: Replies, exercise: impl Future<Output = ()>) {
        tokio::select! {
            biased;
            () = self.serve(replies) => panic!("loopback server stopped"),
            () = exercise => {}
        }
    }
}

pub(super) async fn loopback() -> TcpListener {
    TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback listener")
}

async fn write_event(socket: &mut TcpStream, event: &str) {
    socket
        .write_all(format!("{:x}\r\n{event}\r\n", event.len()).as_bytes())
        .await
        .expect("SSE chunk");
}

async fn respond(socket: &mut TcpStream, status: &str, body: &str) {
    socket
        .write_all(
            format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .expect("HTTP response");
}
