//! Tests for senpi#4828 / #4850: single `startupSettled` inventory announcement after startup.
use maho_ext_api::{EventBus, ExtensionMode};
use maho_ext_mcp::{
    control_inventory::{
        MCP_CONTROL_INVENTORY_CHANGED_EVENT, MCP_CONTROL_INVENTORY_REQUEST_EVENT,
        McpControlInventoryRequest, forward_mcp_control_inventory,
    },
    host_registry::HostMcpRegistry,
    service::{McpAttachOptions, McpService},
    startup_window::{McpStartupWindows, StartupWindowRelease},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
    time::Duration,
};

#[tokio::test]
async fn wire_status_listener_runs_after_service_lock_is_released() {
    let service = Arc::new(tokio::sync::Mutex::new(McpService::new(
        Arc::new(HostMcpRegistry::default()),
        91,
    )));
    let weak = Arc::downgrade(&service);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    {
        let mut guard = service.lock().await;
        guard.bind_self(weak.clone());
        let _unsubscribe = guard.on_wire_status_changed(Arc::new(move |_, _, _| {
            let unlocked = weak
                .upgrade()
                .is_some_and(|service| service.try_lock().is_ok());
            tx.send(unlocked).unwrap();
        }));
        guard
            .release_startup_window("session", StartupWindowRelease::StartupSettled)
            .await;
    }
    let unlocked = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("wire status is delivered")
        .expect("listener reports lock state");
    assert!(unlocked, "listener was called while the service was locked");
}

#[tokio::test]
async fn cancelled_startup_close_releases_the_window() {
    let windows = McpStartupWindows::default();
    let window = windows.open("cancelled");
    assert!(windows.hold(Some("cancelled")));
    let (settle, pending) = tokio::sync::oneshot::channel::<()>();
    let (released, release_event) = tokio::sync::oneshot::channel();
    let mut closing = Box::pin(window.close(
        vec![Box::pin(async move {
            let _ = pending.await;
        })],
        move |kind| async move {
            let _ = released.send(kind);
        },
    ));
    assert!(futures::poll!(&mut closing).is_pending());
    drop(closing);
    assert!(windows.hold(Some("cancelled")));
    settle.send(()).unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), release_event)
            .await
            .expect("startup release arrives")
            .expect("release callback runs"),
        StartupWindowRelease::StartupSettled
    );
    assert!(
        !windows.hold(Some("cancelled")),
        "settled close left the session's startup window open"
    );
}

#[tokio::test]
async fn unbound_service_releases_startup_window_after_connect() {
    let dir = tempfile::tempdir().unwrap();
    let agent_dir = dir.path().join("agent");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("mcp.json"),
        serde_json::json!({
            "mcpServers": {
                "fx": {
                    "type": "stdio",
                    "command": crate::support::node(),
                    "args": [crate::support::senpi("packages/coding-agent/test/mcp/fixtures/stdio-server.ts")],
                    "connectTimeoutMs": 5000
                }
            }
        })
        .to_string(),
    )
    .unwrap();
    let mut service = McpService::new(Arc::new(HostMcpRegistry::default()), 92);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let _unsubscribe = service.on_wire_status_changed(Arc::new(move |session, _, settled| {
        tx.send((session.map(str::to_owned), settled)).unwrap();
    }));
    service
        .attach_session_with_options(McpAttachOptions {
            cwd: dir.path(),
            agent_dir: &agent_dir,
            env: &BTreeMap::new(),
            project_trusted: true,
            declarations: &[],
            mode: Some(ExtensionMode::Rpc),
            session_id: Some("unbound"),
            ui: None,
            has_ui: false,
            extension_connects_mcp_servers: false,
        })
        .await
        .unwrap();
    let event = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("startup event before timeout")
        .expect("listener remains registered");
    assert_eq!(event, (Some("unbound".to_owned()), true));
    service.dispose().await.unwrap();
}

#[tokio::test]
async fn startup_windows_coalesce_overlapping_attaches_and_release_once() {
    let windows = McpStartupWindows::default();
    let w1 = windows.open("s1");
    let w2 = windows.open("s1");
    assert!(windows.hold(Some("s1")));
    assert!(!windows.hold(Some("other")));

    let releases = Arc::new(Mutex::new(Vec::new()));
    let (tx, mut rx) = tokio::sync::watch::channel(false);

    // Close w1 concurrently while waiting on an explicit signal; close w2 first so w1 becomes the final window.
    let r1 = releases.clone();
    let w1_task = tokio::spawn(async move {
        w1.close(
            vec![Box::pin(async move {
                while !*rx.borrow() {
                    if rx.changed().await.is_err() {
                        break;
                    }
                }
            })],
            move |kind| {
                r1.lock().unwrap().push(kind);
                async {}
            },
        )
        .await;
    });

    let r2 = releases.clone();
    w2.close(Vec::new(), move |kind| {
        r2.lock().unwrap().push(kind);
        async {}
    })
    .await;
    assert!(releases.lock().unwrap().is_empty(), "w1 is still open");

    let _ = tx.send(true);
    tokio::time::timeout(Duration::from_secs(5), w1_task)
        .await
        .expect("w1.close timed out")
        .expect("w1 task panicked");
    assert_eq!(
        *releases.lock().unwrap(),
        vec![StartupWindowRelease::StartupSettled]
    );

    // Sequential overlap: a finishes before b, so only b's final close releases.
    let windows2 = McpStartupWindows::default();
    let a = windows2.open("s2");
    let b = windows2.open("s2");
    assert!(windows2.hold(Some("s2")));
    let out = Arc::new(Mutex::new(Vec::new()));
    let out_a = out.clone();
    a.close(Vec::new(), move |kind| {
        out_a.lock().unwrap().push(kind);
        async {}
    })
    .await;
    assert!(out.lock().unwrap().is_empty(), "window b is still open");
    let out_b = out.clone();
    b.close(vec![Box::pin(async {})], move |kind| {
        out_b.lock().unwrap().push(kind);
        async {}
    })
    .await;
    assert_eq!(
        *out.lock().unwrap(),
        vec![StartupWindowRelease::StartupSettled]
    );

    // Zero connects with a withheld change releases `Withheld`.
    let windows3 = McpStartupWindows::default();
    let c = windows3.open("s3");
    assert!(windows3.hold(Some("s3")));
    let out3 = Arc::new(Mutex::new(Vec::new()));
    let out3_c = out3.clone();
    c.close(Vec::new(), move |kind| {
        out3_c.lock().unwrap().push(kind);
        async {}
    })
    .await;
    assert_eq!(*out3.lock().unwrap(), vec![StartupWindowRelease::Withheld]);
    let _ = w2;
}

#[tokio::test]
async fn forward_mcp_control_inventory_filters_by_attached_and_forwards_startup_settled() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("cwd");
    let agent_dir = dir.path().join("agent");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(&agent_dir).unwrap();
    // Write one stdio server config that connects quickly (or fails quickly) so startup connects > 0.
    std::fs::write(
        agent_dir.join("mcp.json"),
        serde_json::json!({
            "mcpServers": {
                "fx": {
                    "type": "stdio",
                    "command": crate::support::node(),
                    "args": [crate::support::senpi("packages/coding-agent/test/mcp/fixtures/stdio-server.ts")],
                    "connectTimeoutMs": 5000
                }
            }
        })
        .to_string(),
    )
    .unwrap();

    let registry = Arc::new(HostMcpRegistry::default());
    let service = McpService::new(registry, 4828);
    let subscriptions = service.wire_status_subscriptions();
    let service = Arc::new(tokio::sync::Mutex::new(service));
    service.lock().await.bind_self(Arc::downgrade(&service));

    let events = EventBus::default();
    let (changed_tx, mut changed_rx) = tokio::sync::mpsc::unbounded_channel();
    let _sub = events.on(
        MCP_CONTROL_INVENTORY_CHANGED_EVENT,
        Arc::new(move |value| {
            let _ = changed_tx.send(value.clone());
        }),
    );

    let attached = Arc::new(Mutex::new(BTreeSet::from(["rpc-session-1".to_owned()])));
    let busy = service.lock().await;
    let forwarder =
        forward_mcp_control_inventory(&events, &service, attached, None, &subscriptions);
    drop(busy);

    service
        .lock()
        .await
        .attach_session_with_options(McpAttachOptions {
            cwd: &cwd,
            agent_dir: &agent_dir,
            env: &BTreeMap::new(),
            project_trusted: true,
            declarations: &[],
            mode: Some(ExtensionMode::Rpc),
            session_id: Some("rpc-session-1"),
            ui: None,
            has_ui: false,
            extension_connects_mcp_servers: false,
        })
        .await
        .unwrap();

    // Await exact startupSettled notification.
    let notification = tokio::time::timeout(Duration::from_secs(10), changed_rx.recv())
        .await
        .expect("startupSettled notification before timeout")
        .expect("channel open");
    assert_eq!(notification["sessionId"], "rpc-session-1");
    assert_eq!(notification["startupSettled"], serde_json::json!(true));
    assert!(
        changed_rx.try_recv().is_err(),
        "exactly one startupSettled event"
    );

    // Attached session responds to MCP_CONTROL_INVENTORY_REQUEST_EVENT; unattached does not.
    let (tx_attached, rx_attached) = tokio::sync::oneshot::channel();
    events.emit_native(
        MCP_CONTROL_INVENTORY_REQUEST_EVENT,
        &McpControlInventoryRequest {
            session_id: "rpc-session-1".into(),
            respond: Mutex::new(Some(tx_attached)),
        },
    );
    let snapshot = tokio::time::timeout(Duration::from_secs(5), rx_attached)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(snapshot["servers"][0]["name"], "fx");

    let (tx_other, rx_other) = tokio::sync::oneshot::channel();
    events.emit_native(
        MCP_CONTROL_INVENTORY_REQUEST_EVENT,
        &McpControlInventoryRequest {
            session_id: "unattached-session".into(),
            respond: Mutex::new(Some(tx_other)),
        },
    );
    assert!(rx_other.await.is_err());

    forwarder.dispose();
    service.lock().await.dispose().await.unwrap();
}
