//! senpi-mcp-17 (expose/call.ts): tool-call progress updates and error text pass through
//! `redactResolvedReferenceValues` / `redactResolvedReferenceData` before they reach the model or the display.

use crate::{reference_support::resolve_env, support};
use maho_ext_api::{AbortSignal, ToolCall, ToolContent};
use maho_ext_mcp::{
    catalog::collect_tool_catalog,
    config_schema::{McpServerConfig, Transport},
    errors::{McpError, McpErrorKind},
    expose::register::build_mcp_tool_definitions,
    guard::output_guard::McpOutputArtifacts,
    log::McpLogger,
    transport::{connect_mcp_transport, create_mcp_transport, shutdown_mcp_transport},
};
use serde_json::json;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

#[tokio::test]
async fn progress_updates_redact_resolved_values_in_text_and_details() {
    // The server label is a resolved value, so the progress line `<server>/<tool> progress ...` quotes it.
    let secret = resolve_env("VALUE", "synthetic3858progressalpha");
    let root = tempfile::tempdir().expect("root");
    let config = McpServerConfig {
        transport: Some(Transport::Stdio),
        command: Some(support::node()),
        args: Some(vec![
            support::senpi("packages/coding-agent/test/mcp/fixtures/stdio-server.ts"),
            "--tools".into(),
            "1".into(),
            "--slow-tool-call".into(),
            "1".into(),
        ]),
        connect_timeout_ms: Some(5000.0),
        ..Default::default()
    };
    let transport = create_mcp_transport(
        "progress",
        &config,
        None,
        Arc::new(Mutex::new(
            McpLogger::new("progress", root.path(), None).expect("logger"),
        )),
    )
    .expect("transport");
    let client = connect_mcp_transport(&transport).await.expect("connect");
    let catalog = collect_tool_catalog(&secret, client, Duration::from_secs(3))
        .await
        .expect("catalog");
    let artifacts = Arc::new(McpOutputArtifacts::default());
    let tools = build_mcp_tool_definitions(&catalog, root.path().into(), artifacts.clone(), None);
    let updates = Arc::new(Mutex::new(Vec::new()));
    let capture = updates.clone();
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        (tools[0].execute)(ToolCall {
            id: "progress",
            params: json!({"value": "progress"}),
            signal: AbortSignal::default(),
            on_update: Some(Arc::new(move |update| {
                capture
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(update);
                Ok(())
            })),
            context: None,
        }),
    )
    .await
    .expect("bounded call");
    shutdown_mcp_transport(&transport).await.expect("shutdown");
    artifacts.cleanup().expect("cleanup");
    result.expect("tool call");

    let updates = updates
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    assert_eq!(updates.len(), 1);
    let ToolContent::Text { text, .. } = &updates[0].content[0] else {
        panic!("progress text");
    };
    assert!(!text.contains(&secret), "{text}");
    assert!(text.starts_with("<redacted>/tool_1 progress 1/2"), "{text}");
    let details = updates[0].details.as_ref().expect("progress details");
    assert_eq!(details["server"], "<redacted>");
    assert_eq!(details["tool"], "tool_1");
    assert!(!details.to_string().contains(&secret), "{details}");
}

#[tokio::test]
async fn tool_exec_error_text_redacts_resolved_values() {
    let secret = resolve_env("VALUE", "synthetic3858errortextalpha");
    let root = tempfile::tempdir().expect("root");
    let config = McpServerConfig {
        enabled: Some(true),
        transport: Some(Transport::Stdio),
        command: Some("/missing/never-spawn".into()),
        ..Default::default()
    };
    let connection = maho_ext_mcp::connection::ServerConnection::new(
        "errors",
        config.clone(),
        None,
        Arc::new(Mutex::new(
            McpLogger::new("errors", root.path(), None).expect("logger"),
        )),
    );
    let lifecycle =
        maho_ext_mcp::idle::McpConnectionLifecycle::configure(connection.clone(), config);
    let cached = maho_ext_mcp::catalog_cache::McpCachedServerCatalog {
        modern: false,
        credential_identity: None,
        config_hash: "hash".into(),
        fetched_at: 0.0,
        tools: vec![json!({"name": "tool", "inputSchema": {"type": "object"}})],
        resources: vec![],
        prompts: vec![],
        instructions: None,
    };
    let runtime = Arc::new(maho_ext_mcp::catalog::McpCatalogRuntime {
        connection: connection.clone(),
        lifecycle: lifecycle.clone(),
        health: Default::default(),
    });
    let mut entries = maho_ext_mcp::catalog::cached_mcp_catalog_entries(
        "errors",
        &cached,
        runtime,
        Duration::from_secs(2),
        Arc::new(|| Box::pin(async { Ok(()) })),
    );
    let reflected = format!("server reflected {secret}");
    entries[0].ensure_fresh = Some(Arc::new(move || {
        let reflected = reflected.clone();
        Box::pin(async move { Err(McpError::new(McpErrorKind::Connect, reflected)) })
    }));
    let artifacts = Arc::new(McpOutputArtifacts::default());
    let tools = build_mcp_tool_definitions(&entries, root.path().into(), artifacts.clone(), None);
    let result = (tools[0].execute)(ToolCall {
        id: "error",
        params: json!({}),
        signal: Default::default(),
        on_update: None,
        context: None,
    })
    .await;
    lifecycle.dispose();
    connection.dispose().await.expect("dispose");
    artifacts.cleanup().expect("cleanup");

    let message = result.expect_err("refresh failure").to_string();
    assert!(!message.contains(&secret), "{message}");
    assert_eq!(message, "ToolExecError: server reflected <redacted>");
}
