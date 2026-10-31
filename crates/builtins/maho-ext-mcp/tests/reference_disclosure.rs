//! Port of senpi test/mcp/3858-reference-disclosure.test.ts and the ext-mcp case of
//! 3858-reference-structure.test.ts: resolved reference values never leave the process through logs, snapshots,
//! diagnostics, the catalog cache, tool output, spill files or error text. Redaction is an outbound projection;
//! the wire-facing original is never mutated.

use crate::reference_support::{encode_uri_component, resolve_env};
use maho_ext_mcp::{
    catalog_cache::{McpCachedServerCatalog, read_mcp_catalog_cache, write_mcp_cached_server},
    config_schema::{McpServerConfig, OutputGuardSettings, Transport},
    connection::{ServerConnection, ServerConnectionState},
    diagnose::diagnose_captured_mcp_connect_failure,
    errors::{McpError, McpErrorKind},
    guard::output_guard::{McpOutputArtifacts, McpOutputGuardOptions, apply_mcp_output_guard},
    log::McpLogger,
    service_snapshot::build_mcp_server_snapshot,
};
use serde_json::json;
use std::sync::{Arc, Mutex};

#[test]
fn redacts_raw_url_encoded_and_nested_values_from_logs_after_rotation() {
    let secret = resolve_env("VALUE", "synthetic/3858 log?+alpha");
    let rotated = resolve_env("VALUE", "synthetic/3858 log?+beta");
    let root = tempfile::tempdir().expect("log dir");
    let mut logger = McpLogger::new("fixture", root.path(), None).expect("logger");
    logger
        .log(
            "info",
            &format!("{secret} {}", encode_uri_component(&secret)),
            Some(&json!({"error": {"message": rotated, "cause": {"message": secret}}})),
            None,
        )
        .expect("log");
    let output = logger.get_ring_buffer().join("\n")
        + &std::fs::read_to_string(&logger.file_path).expect("log file");
    for value in [secret.clone(), rotated, encode_uri_component(&secret)] {
        assert!(!output.contains(&value), "{value} leaked: {output}");
    }
    assert!(output.contains("redacted"));
}

#[test]
fn retains_all_257_active_values_and_an_old_value_after_later_rotations() {
    let values = (0..258)
        .map(|index| resolve_env("VALUE", &format!("synthetic-3858-entry-{index}-end")))
        .collect::<Vec<_>>();
    let root = tempfile::tempdir().expect("log dir");
    let mut logger = McpLogger::new("retired", root.path(), None).expect("logger");
    logger
        .log("info", &values.join(" "), None, None)
        .expect("log");
    let output = logger.get_ring_buffer().join("\n");
    for value in &values {
        assert!(!output.contains(value.as_str()), "{value} leaked");
    }
}

#[test]
fn persists_a_redacted_catalog_without_changing_the_callers_resources() {
    let secret = resolve_env("VALUE", "synthetic/3858 catalog?+alpha");
    let root = tempfile::tempdir().expect("agent dir");
    let uri = format!("fixture://{secret}");
    let catalog = McpCachedServerCatalog {
        modern: false,
        config_hash: "synthetic-hmac".into(),
        credential_identity: Some("none".into()),
        fetched_at: 1.0,
        tools: vec![],
        resources: vec![json!({"name": "resource", "uri": uri})],
        prompts: vec![],
        instructions: None,
    };
    write_mcp_cached_server(root.path(), "fixture", catalog.clone()).expect("write cache");
    assert_eq!(
        catalog.resources[0]["uri"], uri,
        "the subscription identifier stays raw"
    );
    let persisted =
        std::fs::read_to_string(root.path().join("cache/mcp-cache.json")).expect("cache file");
    assert!(!persisted.contains(&secret), "{persisted}");
    assert_eq!(
        read_mcp_catalog_cache(root.path()).servers["fixture"].resources[0]["uri"],
        "fixture://<redacted>"
    );
}

/// 3858-reference-structure: `persists catalog boolean annotations and numeric schema values unchanged`.
#[test]
fn persists_catalog_booleans_and_numbers_unchanged_while_redacting_text() {
    let secret = resolve_env("VALUE", "synthetic/3858 structure?+alpha");
    // DIVERGENCE: upstream also remembers "true" and "8080"; the registry is process global and this binary hosts
    // every test module, so remembering them would redact unrelated JSON elsewhere. Non-string leaves are still
    // checked to survive the redacting projection unchanged.
    let root = tempfile::tempdir().expect("agent dir");
    write_mcp_cached_server(
        root.path(),
        "fixture",
        McpCachedServerCatalog {
            modern: false,
            config_hash: "synthetic-hmac".into(),
            credential_identity: Some("none".into()),
            fetched_at: 8080.0,
            tools: vec![json!({
                "name": "fixture",
                "annotations": {"readOnlyHint": true},
                "inputSchema": {"type": "object", "properties": {"text": {"type": "number", "default": 8080}}},
                "description": format!("nested {secret}"),
            })],
            resources: vec![],
            prompts: vec![],
            instructions: None,
        },
    )
    .expect("write cache");
    let cached = &read_mcp_catalog_cache(root.path()).servers["fixture"];
    assert_eq!(cached.fetched_at, 8080.0);
    assert_eq!(
        cached.tools[0]["annotations"],
        json!({"readOnlyHint": true})
    );
    assert_eq!(
        cached.tools[0]["inputSchema"]["properties"]["text"]["default"],
        json!(8080)
    );
    assert_eq!(cached.tools[0]["inputSchema"]["type"], "object");
    assert_eq!(cached.tools[0]["description"], "nested <redacted>");
}

#[test]
fn sanitizes_snapshot_and_diagnostic_errors_without_changing_the_original_error() {
    let secret = resolve_env("VALUE", "synthetic/3858 snapshot?+alpha");
    let root = tempfile::tempdir().expect("log dir");
    let config = McpServerConfig {
        enabled: Some(true),
        transport: Some(Transport::Stdio),
        command: Some("fixture".into()),
        ..Default::default()
    };
    let logger = Arc::new(Mutex::new(
        McpLogger::new("diagnostic", root.path(), None).expect("logger"),
    ));
    let connection = ServerConnection::new("fixture", config.clone(), None, logger.clone());
    let error = McpError::new(
        McpErrorKind::Connect,
        format!("reflected {secret} {}", encode_uri_component(&secret)),
    );
    connection.mark_failure(ServerConnectionState::Degraded, Some(error.clone()));
    let snapshot = build_mcp_server_snapshot("fixture", None, Some(connection.as_ref()), None, 0.0);
    let last_error = snapshot.last_error.expect("snapshot error");
    assert!(!last_error.contains(&secret), "{last_error}");
    assert!(
        !last_error.contains(&encode_uri_component(&secret)),
        "{last_error}"
    );
    assert!(last_error.contains("<redacted>"));

    logger
        .lock()
        .expect("logger lock")
        .log("info", &secret, None, Some("stderr"))
        .expect("stderr line");
    let diagnostic = diagnose_captured_mcp_connect_failure(
        "fixture",
        &config,
        &error,
        &logger.lock().expect("logger lock"),
    )
    .expect("captured stderr diagnostic");
    assert!(
        !diagnostic.message.contains(&secret),
        "{}",
        diagnostic.message
    );
    assert!(
        error.message.contains(&secret),
        "the original error is unchanged"
    );
    assert!(
        connection
            .last_error()
            .expect("last error")
            .message
            .contains(&secret)
    );
}

/// The output half of `sanitizes server tool and resource text before model, display and spill sinks`.
#[test]
fn sanitizes_tool_output_and_spill_files_without_changing_the_wire_reply() {
    let secret = resolve_env("VALUE", "synthetic/3858?+output");
    let root = tempfile::tempdir().expect("agent dir");
    let artifacts = McpOutputArtifacts::default();
    let reply = vec![json!({"type": "text", "text": format!("echo {secret}")})];
    let inline = apply_mcp_output_guard(
        &reply,
        McpOutputGuardOptions {
            agent_dir: root.path(),
            artifacts: Some(&artifacts),
            server: "fx",
            output_guard: None,
        },
    );
    assert_eq!(
        inline,
        vec![json!({"type": "text", "text": "echo <redacted>"})]
    );
    assert_eq!(
        reply[0]["text"],
        format!("echo {secret}"),
        "the wire reply stays raw"
    );

    let large = vec![json!({"type": "text", "text": format!("{secret}\n").repeat(800)})];
    let spilled = apply_mcp_output_guard(
        &large,
        McpOutputGuardOptions {
            agent_dir: root.path(),
            artifacts: Some(&artifacts),
            server: "fx",
            output_guard: Some(&OutputGuardSettings {
                max_bytes: Some(1024.0),
                max_lines: Some(10.0),
                max_tokens: None,
            }),
        },
    );
    let text = spilled[0]["text"].as_str().expect("spill notice");
    let path = text
        .lines()
        .find_map(|line| line.strip_prefix("Full output saved to: "))
        .expect("spill path");
    let spill = std::fs::read_to_string(path).expect("spill file");
    for outward in [text, spill.as_str()] {
        assert!(!outward.contains(&secret));
        assert!(!outward.contains(&encode_uri_component(&secret)));
        assert!(outward.contains("<redacted>"));
    }
    artifacts.cleanup().expect("cleanup");
}

/// `redacts needs_auth errors in real service snapshots and RPC inventory snapshots`.
#[tokio::test]
async fn redacts_needs_auth_errors_in_service_and_wire_status_snapshots() {
    let secret = resolve_env("VALUE", "synthetic/3858 needs-auth?+alpha");
    let stub = crate::reference_support::HeaderStub::start().await;
    let agent = tempfile::tempdir().expect("agent dir");
    std::fs::write(
        agent.path().join("mcp.json"),
        json!({"mcpServers": {"fx": {
            "type": "http",
            "url": stub.url,
            "auth": false,
            "exposure": "direct",
            "connectTimeoutMs": 5000,
        }}})
        .to_string(),
    )
    .expect("global mcp.json");
    let registry = Arc::new(maho_ext_mcp::host_registry::HostMcpRegistry::default());
    let mut service = maho_ext_mcp::service::McpService::new(registry.clone(), 3861);
    service
        .attach_session(
            agent.path(),
            agent.path(),
            &std::collections::BTreeMap::new(),
            true,
            &[],
        )
        .await
        .expect("attach");
    tokio::time::timeout(
        std::time::Duration::from_secs(30),
        service.connect_server("fx"),
    )
    .await
    .expect("bounded connect")
    .expect("connect");
    let connection = service.connections["fx"]
        .entry
        .lock()
        .await
        .connection
        .clone();
    let error = McpError::new(McpErrorKind::Connect, format!("authenticate {secret}"));
    connection.mark_failure(ServerConnectionState::NeedsAuth, Some(error.clone()));

    let snapshots = service.server_snapshots().await;
    assert_eq!(
        snapshots[0].last_error.as_deref(),
        Some("authenticate <redacted>")
    );
    let wire = service.wire_status_snapshot().await;
    assert_eq!(
        wire.servers[0].status,
        Some(
            maho_ext_mcp::service_types::McpWireServerStatus::Connection(
                ServerConnectionState::NeedsAuth
            )
        )
    );
    assert_eq!(
        wire.servers[0].last_error.as_deref(),
        Some("authenticate <redacted>")
    );
    let serialized = serde_json::to_value(&wire).expect("wire json");
    assert_eq!(
        serialized["servers"][0]["lastError"],
        "authenticate <redacted>"
    );
    assert_eq!(error.message, format!("authenticate {secret}"));
    service.dispose().await.expect("dispose");
    registry.dispose().await.expect("registry dispose");
    stub.close().await;
}
