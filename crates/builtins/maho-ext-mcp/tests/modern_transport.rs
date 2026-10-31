//! Ports of senpi test/mcp/modern-mrtr-transport.test.ts, modern-subscriptions.test.ts and
//! modern-tasks-http.test.ts surfaces for modern/transport.ts, modern/sdk-bridge.ts and modern/http-fetch.ts
//! (v2026.10.27-2).
use axum::{Json, Router, routing::post};
use maho_ext_mcp::modern::{
    errors::{McpMrtrErrorCode, as_mcp_mrtr_error},
    http_fetch::strip_protocol_headers,
    sdk_bridge::{modern_initialize_result, normalize_modern_result},
    transport::ModernHttpTransport,
    wire::{MODERN_PROTOCOL_VERSION, ModernClientInfo},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

async fn serve(router: Router) -> (url::Url, tokio::sync::oneshot::Sender<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
    });
    (format!("http://{address}/mcp").parse().unwrap(), stop)
}
fn transport(url: url::Url) -> ModernHttpTransport {
    ModernHttpTransport::new(
        "fixture",
        url,
        BTreeMap::new(),
        ModernClientInfo {
            name: "test".into(),
            version: "1".into(),
        },
        json!({}),
        MODERN_PROTOCOL_VERSION.into(),
        None,
        Arc::new(|_| {}),
    )
    .unwrap()
}

#[tokio::test]
async fn an_http_reply_cannot_settle_a_different_in_flight_request_identifier() {
    // Request 1 is answered with id 2; request 2 with its own id.
    let router = Router::new().route(
        "/mcp",
        post(|Json(message): Json<Value>| async move {
            let text = if message["id"] == 1 { "spoofed" } else { "real" };
            Json(json!({"jsonrpc":"2.0","id":2,"result":{"resultType":"complete","content":[{"type":"text","text":text}]}}))
        }),
    );
    let (url, stop) = serve(router).await;
    let transport = transport(url);
    let delivered = Arc::new(Mutex::new(Vec::<Value>::new()));
    let sink = delivered.clone();
    let on_message = move |message: Value| sink.lock().unwrap().push(message);
    let call = |id: u64| json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"fixture"}});
    let failure = transport
        .send(&call(1), None, &on_message)
        .await
        .unwrap_err();
    assert_eq!(
        as_mcp_mrtr_error(&failure).map(|error| error.code),
        Some(McpMrtrErrorCode::InvalidResult)
    );
    let reply = transport
        .send(&call(2), None, &on_message)
        .await
        .unwrap()
        .expect("reply to request 2");
    assert_eq!(reply["id"], 2);
    assert_eq!(reply["result"]["content"][0]["text"], "real");
    assert!(
        delivered.lock().unwrap().is_empty(),
        "a mismatched reply is never delivered as a notification"
    );
    stop.send(()).unwrap();
}

#[tokio::test]
async fn cancellation_notifications_are_never_posted_on_the_stateless_wire() {
    let posts = Arc::new(Mutex::new(0usize));
    let counter = posts.clone();
    let router = Router::new().route(
        "/mcp",
        post(move || {
            let counter = counter.clone();
            async move {
                *counter.lock().unwrap() += 1;
                axum::http::StatusCode::ACCEPTED
            }
        }),
    );
    let (url, stop) = serve(router).await;
    let transport = transport(url);
    let cancelled =
        json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1}});
    assert!(
        transport
            .send(&cancelled, None, &|_| {})
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(*posts.lock().unwrap(), 0);
    stop.send(()).unwrap();
}

#[test]
fn the_local_initialize_answer_requires_the_wire_version_and_object_capabilities() {
    let discovery = json!({
        "supportedVersions":[MODERN_PROTOCOL_VERSION],
        "capabilities":{"tools":{}},
        "instructions":"use tools",
        "_meta":{"io.modelcontextprotocol/serverInfo":{"name":"fx","version":"1"}}
    });
    let discovery = discovery.as_object().unwrap();
    let initialize = modern_initialize_result(discovery, MODERN_PROTOCOL_VERSION).unwrap();
    assert_eq!(initialize["capabilities"], json!({"tools":{}}));
    assert_eq!(initialize["serverInfo"]["name"], "fx");
    assert_eq!(initialize["instructions"], "use tools");
    assert!(modern_initialize_result(discovery, "2099-01-01").is_err());
    let mut malformed = discovery.clone();
    malformed.insert("capabilities".into(), json!({"tools":true}));
    assert!(modern_initialize_result(&malformed, MODERN_PROTOCOL_VERSION).is_err());
}

#[test]
fn a_null_next_cursor_is_dropped_for_the_legacy_list_parsers() {
    let normalized = normalize_modern_result(
        json!({"jsonrpc":"2.0","id":1,"result":{"tools":[],"nextCursor":null}}),
    );
    assert!(normalized["result"].get("nextCursor").is_none());
    let kept = normalize_modern_result(
        json!({"jsonrpc":"2.0","id":1,"result":{"tools":[],"nextCursor":"c"}}),
    );
    assert_eq!(kept["result"]["nextCursor"], "c");
}

#[test]
fn configured_protocol_routing_headers_never_reach_the_probe() {
    let mut headers = BTreeMap::from([
        ("Authorization".to_owned(), "Bearer x".to_owned()),
        ("Mcp-Session-Id".to_owned(), "stale".to_owned()),
        ("MCP-Protocol-Version".to_owned(), "2025-11-25".to_owned()),
        ("Mcp-Method".to_owned(), "tools/call".to_owned()),
    ]);
    strip_protocol_headers(&mut headers);
    assert_eq!(headers.keys().collect::<Vec<_>>(), ["Authorization"]);
}
