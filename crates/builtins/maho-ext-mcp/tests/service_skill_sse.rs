use axum::{
    Json, Router,
    http::StatusCode,
    response::sse::{Event, Sse},
    routing::{get, post},
};
use maho_ext_mcp::{
    errors::McpErrorKind,
    host_registry::HostMcpRegistry,
    service::{McpService, McpServiceError},
    skills::{SkillLike, parse_skill_mcp_declarations},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    convert::Infallible,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::broadcast;

/// Accept the legacy handshake, but acknowledge tools/list without ever replying on the stream.
/// The service must use the skill deadline, not the 30 s native default or the connect deadline.
#[tokio::test]
async fn skill_service_uses_sse_without_discovery_and_times_out_real_catalog_requests() {
    for scope in ["user", "project"] {
        let wire = Arc::new(Mutex::new(Vec::new()));
        let (tx, _) = broadcast::channel::<String>(8);
        let app = Router::new()
            .route(
                "/events",
                get({
                    let tx = tx.clone();
                    let wire = wire.clone();
                    move || async move {
                        wire.lock().unwrap().push("GET".to_owned());
                        let rx = tx.subscribe();
                        let events =
                            futures::stream::unfold((true, rx), |(first, mut rx)| async move {
                                let event = if first {
                                    Event::default().event("endpoint").data("/rpc")
                                } else {
                                    Event::default()
                                        .event("message")
                                        .data(rx.recv().await.ok()?)
                                };
                                Some((Ok::<_, Infallible>(event), (false, rx)))
                            });
                        Sse::new(events)
                    }
                })
                .post({
                    let wire = wire.clone();
                    move || async move {
                        wire.lock()
                            .unwrap()
                            .push("unexpected discovery POST".into());
                        StatusCode::METHOD_NOT_ALLOWED
                    }
                }),
            )
            .route(
                "/rpc",
                post({
                    let wire = wire.clone();
                    move |Json(request): Json<Value>| async move {
                        let method = request["method"].as_str().unwrap();
                        wire.lock().unwrap().push(method.to_owned());
                        if method == "initialize" {
                            tx.send(
                                json!({"jsonrpc":"2.0","id":request["id"],"result":{
                                    "protocolVersion":"2025-03-26","capabilities":{"tools":{}},
                                    "serverInfo":{"name":"fixture","version":"1"}
                                }})
                                .to_string(),
                            )
                            .unwrap();
                        }
                        StatusCode::ACCEPTED
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/events", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let agent = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let skill = cwd.path().join("SKILL.md");
        // This loopback server is anonymous; a bare remote declaration defaults to OAuth.
        let config =
            json!({"mcpServers":{"fixture":{"url":url,"type":"sse","auth":false,"timeout":250}}});
        if scope == "user" {
            std::fs::write(&skill, "---\nname: skill\n---\n").unwrap();
            std::fs::write(cwd.path().join("mcp.json"), config.to_string()).unwrap();
        } else {
            std::fs::write(&skill, format!("---\nname: skill\nmcp: {config}\n---\n")).unwrap();
        }
        let declarations = parse_skill_mcp_declarations(&[SkillLike {
            name: "skill".into(),
            scope: Some(scope.into()),
            file_path: skill,
            base_dir: cwd.path().into(),
            ..Default::default()
        }]);
        let registry = Arc::new(HostMcpRegistry::default());
        let mut service = McpService::new(registry.clone(), 1);
        service
            .attach_session(cwd.path(), agent.path(), &BTreeMap::new(), false, &[])
            .await
            .unwrap();
        service
            .attach_skill_mcp_servers(&declarations)
            .await
            .unwrap();
        assert!(service.connections["fixture"].shared.is_some());
        let error = tokio::time::timeout(Duration::from_secs(8), service.connect_server("fixture"))
            .await
            .expect("skill timeout was ignored")
            .unwrap_err();
        assert!(
            matches!(error, McpServiceError::Connection(ref e)
                if e.kind == McpErrorKind::Timeout
                    && e.phase.as_deref() == Some("request")
                    && e.message.contains("tools/list")),
            "{error}"
        );
        let calls = wire.lock().unwrap().clone();
        assert!(
            calls.starts_with(&[
                "GET".into(),
                "initialize".into(),
                "notifications/initialized".into(),
                "tools/list".into()
            ]),
            "{calls:?}"
        );
        service.dispose().await.unwrap();
        registry.dispose().await.unwrap();
        server.abort();
    }
}

#[tokio::test]
async fn sse_server_request_receives_a_json_rpc_response() {
    use maho_ext_mcp::{
        log::McpLogger,
        transport_sdk::{McpClient, McpTransportSpec},
    };

    let (events, _) = broadcast::channel::<String>(8);
    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let app = Router::new()
        .route(
            "/events",
            get({
                let events = events.clone();
                move || async move {
                    let rx = events.subscribe();
                    let stream =
                        futures::stream::unfold((true, rx), |(first, mut rx)| async move {
                            let event = if first {
                                Event::default().event("endpoint").data("/rpc")
                            } else {
                                Event::default()
                                    .event("message")
                                    .data(rx.recv().await.ok()?)
                            };
                            Some((Ok::<_, Infallible>(event), (false, rx)))
                        });
                    Sse::new(stream)
                }
            }),
        )
        .route(
            "/rpc",
            post({
                let events = events.clone();
                move |Json(request): Json<Value>| {
                    let events = events.clone();
                    let reply_tx = reply_tx.clone();
                    async move {
                        if request["method"] == "initialize" {
                            events
                                .send(
                                    json!({"jsonrpc":"2.0","id":request["id"],"result":{
                                        "protocolVersion":"2025-03-26","capabilities":{},
                                        "serverInfo":{"name":"fixture","version":"1"}
                                    }})
                                    .to_string(),
                                )
                                .unwrap();
                        } else if request.get("id").is_some() && request.get("method").is_none() {
                            reply_tx.send(request).unwrap();
                        }
                        StatusCode::ACCEPTED
                    }
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/events", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let root = tempfile::tempdir().unwrap();
    let logger = Arc::new(Mutex::new(
        McpLogger::new("fixture", root.path(), None).unwrap(),
    ));
    let client = McpClient::materialize_sse(
        "fixture",
        &McpTransportSpec::Http {
            url: url.parse().unwrap(),
            headers: Default::default(),
        },
        logger,
    )
    .await
    .unwrap();
    let handler = client.install_elicitation(None);
    client.initialize(Duration::from_secs(3)).await.unwrap();

    events
        .send(json!({"jsonrpc":"2.0","id":"server-1","method":"unknown/method"}).to_string())
        .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(3), reply_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response["id"], "server-1");
    assert_eq!(response["error"]["code"], -32601);
    client.close().await.unwrap();
    handler.abort();
    server.abort();
}
