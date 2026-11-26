use super::{
    SKILL_MCP_ABORTED_MESSAGE, SkillMcpClientInfo, SkillMcpClientOptions, SkillMcpManager,
    SkillMcpServerContext,
};
use serde_json::json;
use std::time::Duration;
use tokio::{
    io::AsyncReadExt,
    time::{Instant, timeout},
};

mod fixture;
use fixture::{Fixture, INIT, INITIALIZED, OPERATIONS, POST_PATH, Replies, Transport, loopback};

const WATCHDOG: Duration = Duration::from_secs(5);
const REQUEST_MS: u64 = 500;

fn manager_fixture() -> (tempfile::TempDir, SkillMcpManager, SkillMcpClientInfo) {
    let root = tempfile::tempdir().expect("config root");
    let manager = SkillMcpManager::new(Some(root.path()), None, None);
    let info = SkillMcpClientInfo {
        session_id: "real-transport".into(),
        skill_name: "fixture".into(),
        server_name: "transport-fixture".into(),
        scope: Some("user".into()),
        ..Default::default()
    };
    (root, manager, info)
}

async fn assert_echo(
    manager: &SkillMcpManager,
    info: &SkillMcpClientInfo,
    context: &SkillMcpServerContext,
) {
    let result = manager
        .call_tool(info, context, "echo", json!({"text":"hello"}), None)
        .await
        .expect("echo");
    assert_eq!(result, json!({"content":[{"type":"text","text":"hello"}]}));
}

#[tokio::test]
async fn legacy_sse_get_advertises_post_and_streams_tool_replies_with_configured_headers() {
    let fixture = Fixture::new(Transport::LegacySse).await;
    let context = fixture.context();
    let (_root, manager, info) = manager_fixture();
    timeout(
        WATCHDOG,
        fixture.run(Replies::Normal, async {
            let tools = manager
                .list_tools(&info, &context, None)
                .await
                .expect("list tools");
            assert_eq!(
                tools,
                [json!({"name":"echo","inputSchema":{"type":"object"}})]
            );
            assert_echo(&manager, &info, &context).await;
            manager.disconnect_all().await;
        }),
    )
    .await
    .expect("legacy SSE round trip");
    assert_eq!(
        *fixture.received(),
        ["GET /sse", INIT, INITIALIZED, "tools/list", "tools/call"]
    );
}

#[tokio::test]
async fn a_new_operation_opens_a_fresh_session_after_sse_eof() {
    let fixture = Fixture::new(Transport::LegacySse).await;
    let context = fixture.context();
    let (_root, manager, info) = manager_fixture();
    timeout(WATCHDOG, async {
        let mut closed = None;
        fixture
            .run(Replies::Normal, async {
                let client = manager
                    .get_or_create_client(&info, &context.config, None)
                    .await
                    .expect("first connection");
                closed = Some(client.closed.subscribe());
                assert_echo(&manager, &info, &context).await;
            })
            .await;
        // Returning from run drops the server's event socket without asking the
        // manager to disconnect. Wait for its reader, not for an idle sweep.
        let mut closed = closed.expect("first stream");
        drop(
            closed
                .wait_for(|closed| *closed)
                .await
                .expect("reader closed"),
        );
        fixture
            .run(Replies::Normal, async {
                assert_echo(&manager, &info, &context).await;
                manager.disconnect_all().await;
            })
            .await;
    })
    .await
    .expect("second operation negotiated a new session");
    assert_eq!(
        *fixture.received(),
        ["GET /sse", INIT, INITIALIZED, "tools/call"].repeat(2)
    );
}

#[tokio::test]
async fn legacy_sse_rejects_a_cross_origin_post_endpoint_without_contacting_it() {
    let fixture = Fixture::new(Transport::LegacySse).await;
    let foreign = loopback().await;
    let address = foreign.local_addr().expect("foreign loopback address");
    let context = fixture.context();
    let (_root, manager, info) = manager_fixture();
    let serve = async {
        let _events = fixture
            .open_events(&format!(
                "event: endpoint\ndata: http://{address}{POST_PATH}\n\n"
            ))
            .await;
        std::future::pending::<()>().await;
    };
    let error = timeout(WATCHDOG, async {
        tokio::select! {
            biased;
            _ = foreign.accept() => panic!("client contacted the cross-origin endpoint"),
            () = serve => unreachable!("fixture keeps the event stream open"),
            result = manager.list_tools(&info, &context, None) => result.expect_err("cross-origin endpoint accepted"),
        }
    })
    .await
    .expect("cross-origin endpoint rejected before the request deadline");
    assert!(error.to_lowercase().contains("origin"), "{error}");
    assert_eq!(*fixture.received(), ["GET /sse"]);
}

async fn assert_configured_timeouts(transport: Transport, options: Option<&SkillMcpClientOptions>) {
    let fixture = Fixture::new(transport).await;
    let mut context = fixture.context();
    context.config["timeout"] = json!(REQUEST_MS);
    let (_root, manager, info) = manager_fixture();
    fixture
        .run(Replies::WithholdOperations, async {
            timeout(
                WATCHDOG,
                manager.get_or_create_client(&info, &context.config, options),
            )
            .await
            .expect("fixture handshake deadline")
            .unwrap_or_else(|error| panic!("fixture handshake: {error}"));
            for method in OPERATIONS {
                let started = Instant::now();
                let error = timeout(WATCHDOG, async {
                    match method {
                        "tools/list" => manager
                            .list_tools(&info, &context, options)
                            .await
                            .map(|_| ()),
                        "resources/list" => manager
                            .list_resources(&info, &context, options)
                            .await
                            .map(|_| ()),
                        "prompts/list" => manager
                            .list_prompts(&info, &context, options)
                            .await
                            .map(|_| ()),
                        "tools/call" => manager
                            .call_tool(&info, &context, "slow", json!({}), options)
                            .await
                            .map(|_| ()),
                        "resources/read" => manager
                            .read_resource(&info, &context, "fixture://slow", options)
                            .await
                            .map(|_| ()),
                        "prompts/get" => manager
                            .get_prompt(&info, &context, "slow", json!({}), options)
                            .await
                            .map(|_| ()),
                        _ => unreachable!("known MCP operation"),
                    }
                })
                .await
                .unwrap_or_else(|_| panic!("{method} ignored configured {REQUEST_MS} ms timeout"))
                .expect_err("fixture never answers this operation");
                assert!(
                    error.contains("timed out") && error.contains(method),
                    "{error}"
                );
                assert!(
                    started.elapsed() >= Duration::from_millis(REQUEST_MS),
                    "{method} timed out early: {:?}",
                    started.elapsed()
                );
            }
            timeout(WATCHDOG, manager.disconnect_all())
                .await
                .expect("disconnect");
        })
        .await;
    assert!(fixture.received().ends_with(&OPERATIONS.map(str::to_owned)));
}

#[tokio::test]
async fn streamable_http_applies_configured_timeout_to_all_direct_operations() {
    assert_configured_timeouts(Transport::Http, None).await;
}

#[tokio::test]
async fn legacy_sse_applies_configured_timeout_with_a_non_aborted_caller_signal() {
    let options = SkillMcpClientOptions {
        signal: Some(maho_ext_api::AbortSignal::default()),
        ..Default::default()
    };
    assert_configured_timeouts(Transport::LegacySse, Some(&options)).await;
}

#[tokio::test]
async fn legacy_sse_without_endpoint_hits_connect_deadline_and_closes_get() {
    let fixture = Fixture::new(Transport::LegacySse).await;
    let context = fixture.context();
    let (_root, manager, info) = manager_fixture();
    let connecting = manager.list_tools(&info, &context, None);
    tokio::pin!(connecting);
    let mut stream = tokio::select! {
        result = &mut connecting => panic!("connect settled before the GET opened: {result:?}"),
        stream = timeout(WATCHDOG, fixture.open_events(": open\n\n")) => stream.expect("GET opened"),
    };
    // The connect budget must cover waiting for `endpoint`, not just `initialize`.
    // Drive real I/O before pausing, so Tokio's idle auto-advance cannot race the GET.
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(61)).await;
    tokio::time::resume();
    let error = timeout(WATCHDOG, &mut connecting)
        .await
        .expect("SSE endpoint wait must be covered by the connect budget")
        .expect_err("server never advertised an endpoint");
    assert!(error.contains("timed out"), "{error}");
    let mut byte = [0];
    assert_eq!(
        timeout(WATCHDOG, stream.read(&mut byte))
            .await
            .expect("client closed the GET stream")
            .expect("socket read"),
        0
    );
}

#[tokio::test]
async fn legacy_sse_abort_keeps_session_and_discards_late_tool_reply() {
    let fixture = Fixture::new(Transport::LegacySse).await;
    let context = fixture.context();
    let (_root, manager, info) = manager_fixture();
    let (receipt, received) = tokio::sync::oneshot::channel();
    let signal = maho_ext_api::AbortSignal::default();
    let options = SkillMcpClientOptions {
        signal: Some(signal.clone()),
        ..Default::default()
    };
    timeout(
        WATCHDOG,
        fixture.run(Replies::CancelAfterReceipt(Some(receipt)), async {
            let call = manager.call_tool(&info, &context, "slow", json!({}), Some(&options));
            tokio::pin!(call);
            tokio::select! {
                result = &mut call => panic!("tool completed before abort: {result:?}"),
                received = received => received.expect("server acknowledged the tool POST"),
            }
            signal.abort();
            assert_eq!(
                call.await.expect_err("caller cancelled tool"),
                SKILL_MCP_ABORTED_MESSAGE
            );
            assert_echo(&manager, &info, &context).await;
            manager.disconnect_all().await;
        }),
    )
    .await
    .expect("cancelled tool and follow-up on the original SSE stream");
    assert_eq!(
        *fixture.received(),
        [
            "GET /sse",
            INIT,
            INITIALIZED,
            "tools/call",
            "notifications/cancelled",
            "tools/call",
        ]
    );
}
