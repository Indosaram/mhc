use crate::support;
use maho_ext_mcp::{
    config_schema::{McpServerConfig, Transport},
    host_registry::HostMcpRegistry,
    service_connection::*,
    startup_race::connect_and_refresh_mcp_catalog,
};
#[tokio::test]
async fn session_connection_refreshes_catalog_and_disposes_registry_owner() {
    let root = tempfile::tempdir().unwrap();
    let registry = HostMcpRegistry::default();
    let config = McpServerConfig {
        enabled: Some(true),
        transport: Some(Transport::Stdio),
        command: Some(support::node()),
        args: Some(vec![support::senpi(
            "packages/coding-agent/test/mcp/fixtures/stdio-server.ts",
        )]),
        ..Default::default()
    };
    let session = create_mcp_session_connection(SessionConnectionOptions {
        registry: &registry,
        owner: 1,
        key: "key",
        name: "fixture",
        config_hash: "hash",
        config: config.clone(),
        declared: None,
        agent_dir: root.path(),
        env: None,
        elicitation_ui: None,
        elicitation_enabled: true,
    })
    .unwrap();
    {
        let mut entry = session.entry.lock().await;
        connect_and_refresh_mcp_catalog(&mut entry, &config)
            .await
            .unwrap();
        assert_eq!(
            entry.connection.state(),
            maho_ext_mcp::connection::ServerConnectionState::Connected
        );
        assert!(entry.cache_refreshed_after_connect);
        assert!(!entry.cached_catalog.as_ref().unwrap().tools.is_empty());
        let cache = maho_ext_mcp::catalog_cache::read_mcp_catalog_cache(root.path());
        assert_eq!(cache.servers["fixture"].config_hash, "hash");
    }
    dispose_entry_connection(&session, &registry, 1)
        .await
        .unwrap();
    assert_eq!(registry.size(), 0);
}
#[tokio::test]
async fn literal_secret_warnings_read_the_declaration_not_the_resolved_generation() {
    // service-connection.ts: detectLiteralBearerWarnings(name, options.declared).
    let root = tempfile::tempdir().unwrap();
    let registry = HostMcpRegistry::default();
    let secret = "Bearer sk-resolvedreferencevalue0123456789";
    let mut declared = McpServerConfig {
        enabled: Some(true),
        transport: Some(Transport::Http),
        url: Some("http://127.0.0.1:1/mcp".into()),
        headers: Some(std::collections::BTreeMap::from([(
            "Authorization".to_owned(),
            String::new(),
        )])),
        ..Default::default()
    };
    declared.references.insert(
        "headers.Authorization".into(),
        serde_json::from_value(serde_json::json!({"$env":"SVC_CONN_TOKEN"})).unwrap(),
    );
    let mut resolved = declared.clone();
    resolved.references.clear();
    resolved.headers = Some(std::collections::BTreeMap::from([(
        "Authorization".to_owned(),
        secret.to_owned(),
    )]));
    let warned = |session: &McpSessionConnection| {
        let entry = session.entry.try_lock().unwrap();
        let lines = entry.logger.lock().unwrap().get_ring_buffer();
        lines
            .iter()
            .any(|line| line.contains("appears to contain a literal secret"))
    };
    let session = create_mcp_session_connection(SessionConnectionOptions {
        registry: &registry,
        owner: 1,
        key: "declared",
        name: "fixture",
        config_hash: "hash",
        config: resolved.clone(),
        declared: Some(&declared),
        agent_dir: root.path(),
        env: None,
        elicitation_ui: None,
        elicitation_enabled: true,
    })
    .unwrap();
    assert!(
        !warned(&session),
        "a resolved reference is not a literal in the declaration"
    );
    dispose_entry_connection(&session, &registry, 1)
        .await
        .unwrap();
    // The same header written literally in the declaration still warns.
    let session = create_mcp_session_connection(SessionConnectionOptions {
        registry: &registry,
        owner: 1,
        key: "literal",
        name: "fixture",
        config_hash: "hash",
        config: resolved.clone(),
        declared: Some(&resolved),
        agent_dir: root.path(),
        env: None,
        elicitation_ui: None,
        elicitation_enabled: true,
    })
    .unwrap();
    assert!(warned(&session));
    dispose_entry_connection(&session, &registry, 1)
        .await
        .unwrap();
}

/// service.ts: print and json sessions declare `elicitationEnabled: false`, which reaches the connection and
/// keys pooled connections apart from interactive ones (host-registry.ts).
#[tokio::test]
async fn a_non_interactive_session_withholds_elicitation_from_its_connection() {
    let root = tempfile::tempdir().unwrap();
    let registry = HostMcpRegistry::default();
    let config = McpServerConfig {
        enabled: Some(true),
        transport: Some(Transport::Stdio),
        command: Some(support::node()),
        args: Some(vec!["-e".into(), String::new()]),
        ..Default::default()
    };
    let session = create_mcp_session_connection(SessionConnectionOptions {
        registry: &registry,
        owner: 1,
        key: "print",
        name: "fixture",
        config_hash: "hash",
        config,
        declared: None,
        agent_dir: root.path(),
        env: None,
        elicitation_ui: None,
        elicitation_enabled: false,
    })
    .unwrap();
    assert!(!session.entry.lock().await.connection.elicitation_enabled());
    assert_ne!(
        mcp_legacy_form_policy_key(None, false),
        mcp_legacy_form_policy_key(None, true)
    );
    dispose_entry_connection(&session, &registry, 1)
        .await
        .unwrap();
}
