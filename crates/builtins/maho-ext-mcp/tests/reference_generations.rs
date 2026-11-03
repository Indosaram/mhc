//! Port of senpi test/mcp/3858-reference-generations.test.ts: every connection generation re-reads its
//! references, a rotated value re-keys the server, and a failing reference never blocks its siblings.

use crate::reference_support::HeaderStub;
use maho_ext_mcp::{
    config_generation::{
        McpGenerationScope, is_mcp_config_generation_changed_error, mcp_generation_renewal,
        resolve_mcp_config_generation,
    },
    config_schema::{LoadMcpConfigOptions, McpServerSource},
    connection::ServerConnection,
    host_registry::HostMcpRegistry,
    log::McpLogger,
    service::McpService,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

fn write_private(path: &Path, value: &str) {
    std::fs::write(path, value).expect("reference file");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .expect("private reference file");
    }
}

fn write_config(agent_dir: &Path, servers: Value) {
    std::fs::write(
        agent_dir.join("mcp.json"),
        json!({ "mcpServers": servers }).to_string(),
    )
    .expect("global mcp.json");
}

fn header_server(url: &str, reference: Value) -> Value {
    json!({
        "type": "http",
        "url": url,
        "auth": false,
        "headers": {"Authorization": reference},
        "exposure": "direct",
        "lifecycle": "eager",
        "connectTimeoutMs": 5000,
    })
}

/// The declared server as a trusted loader admits it (origin remembered on each reference).
fn declared(agent_dir: &Path) -> maho_ext_mcp::config_schema::McpServerDeclaredConfig {
    let env = BTreeMap::new();
    let config = maho_ext_mcp::config::load_mcp_config(LoadMcpConfigOptions {
        cwd: agent_dir,
        agent_dir,
        env: &env,
        project_trusted: false,
        home_dir: Some(agent_dir),
        claude_config_dir: Some(agent_dir),
    })
    .expect("load");
    assert_eq!(config.servers["fx"].source, McpServerSource::Global);
    config.servers["fx"].config.clone().expect("declared")
}

/// Unshared half of `uses rotated header bytes on reconnect`: the connection's renewal re-reads the reference
/// for the new transport; a rotated value opens no transport under the stale identity and asks its owner to re-key.
#[tokio::test]
async fn reconnect_rereads_the_reference_and_rekeys_on_rotation() {
    let secret = "synthetic/3858 generation?+alpha";
    let rotated = "synthetic/3858 generation?+beta";
    let stub = HeaderStub::start().await;
    let agent = tempfile::tempdir().expect("agent dir");
    let key = agent.path().join("key");
    write_private(&key, secret);
    write_config(
        agent.path(),
        json!({"fx": header_server(&stub.url, json!({"$file": key}))}),
    );
    let declared = declared(agent.path());
    let scope = McpGenerationScope {
        server_name: "fx".into(),
        declared: declared.clone(),
        env: Some(BTreeMap::new()),
        agent_dir: Some(agent.path().into()),
    };
    let first = resolve_mcp_config_generation(&scope).expect("first generation");
    assert_eq!(
        first
            .config
            .headers
            .as_ref()
            .and_then(|headers| headers.get("Authorization"))
            .map(String::as_str),
        Some(secret)
    );
    assert!(
        first.config.references.is_empty(),
        "a generation is a resolved snapshot"
    );
    let rekeys = Arc::new(AtomicUsize::new(0));
    let counter = rekeys.clone();
    let renewal = mcp_generation_renewal(
        scope.clone(),
        &first.config_hash,
        Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }),
    )
    .expect("a server with references renews per generation");
    let logger = Arc::new(Mutex::new(
        McpLogger::new("fx", agent.path(), None).expect("logger"),
    ));
    let connection = ServerConnection::new("fx", first.config.clone(), None, logger);
    connection.set_resolve_config(Some(renewal));
    connection.connect().await.expect("first connect");
    let seen = stub.authorizations();
    assert!(!seen.is_empty());
    assert!(
        seen.iter().all(|value| value.as_deref() == Some(secret)),
        "{seen:?}"
    );

    write_private(&key, rotated);
    let error = connection
        .renew()
        .await
        .err()
        .expect("a rotated snapshot never reuses the stale identity");
    assert!(is_mcp_config_generation_changed_error(&error), "{error:?}");
    assert_eq!(rekeys.load(Ordering::SeqCst), 1);
    assert_eq!(
        stub.authorizations().len(),
        seen.len(),
        "no request carried stale or rotated bytes"
    );

    let second = resolve_mcp_config_generation(&scope).expect("rotated generation");
    assert_ne!(second.config_hash, first.config_hash);
    assert_eq!(
        second
            .config
            .headers
            .as_ref()
            .and_then(|headers| headers.get("Authorization"))
            .map(String::as_str),
        Some(rotated)
    );
    connection.dispose().await.expect("dispose");
    stub.close().await;
}

/// Shared half: the service re-keys a rotated server, and the new pooled connection sends the rotated bytes.
#[tokio::test]
async fn service_resync_uses_rotated_header_bytes() {
    let secret = "synthetic/3858 service?+alpha";
    let rotated = "synthetic/3858 service?+beta";
    let stub = HeaderStub::start().await;
    let agent = tempfile::tempdir().expect("agent dir");
    let key = agent.path().join("key");
    write_private(&key, secret);
    write_config(
        agent.path(),
        json!({"fx": header_server(&stub.url, json!({"$file": key}))}),
    );
    let registry = Arc::new(HostMcpRegistry::default());
    let mut service = McpService::new(registry.clone(), 3858);
    service
        .attach_session(agent.path(), agent.path(), &BTreeMap::new(), true, &[])
        .await
        .expect("attach");
    service.connect_server("fx").await.expect("first connect");
    let first_hash = service.server_snapshots().await[0].config_hash.clone();
    let seen = stub.authorizations();
    assert!(!seen.is_empty());
    assert!(
        seen.iter().all(|value| value.as_deref() == Some(secret)),
        "{seen:?}"
    );

    write_private(&key, rotated);
    service.resync_current_config().await.expect("resync");
    service.connect_server("fx").await.expect("rotated connect");
    let after = stub.authorizations();
    assert!(
        after.len() > seen.len(),
        "the re-keyed server opened a new transport"
    );
    assert!(
        after[seen.len()..]
            .iter()
            .all(|value| value.as_deref() == Some(rotated)),
        "{after:?}"
    );
    assert_ne!(service.server_snapshots().await[0].config_hash, first_hash);
    service.dispose().await.expect("dispose");
    registry.dispose().await.expect("registry dispose");
    stub.close().await;
}

/// `reports a missing %s reference ... while another server works`, minus the status-row half (below).
async fn missing_reference_is_contained(kind: &str) {
    let stub = HeaderStub::start().await;
    let agent = tempfile::tempdir().expect("agent dir");
    let reference = if kind == "file" {
        json!({"$file": agent.path().join("missing")})
    } else {
        json!({"$env": "UNSET_3858"})
    };
    let mut good = header_server(&stub.url, json!("Bearer plain"));
    good["headers"] = json!({});
    write_config(
        agent.path(),
        json!({"bad": header_server(&stub.url, reference.clone()), "good": good}),
    );
    let registry = Arc::new(HostMcpRegistry::default());
    let mut service = McpService::new(registry.clone(), 3859);
    service
        .attach_session(agent.path(), agent.path(), &BTreeMap::new(), true, &[])
        .await
        .expect("a failing reference never aborts the attach");
    let reason = if kind == "file" { "missing" } else { "unset" };
    let message = format!("mcpServers.bad.headers.Authorization: configuration reference {reason}");
    let error = &service.reference_errors["bad"];
    assert_eq!(error.to_string(), message);
    assert!(
        service
            .config
            .as_ref()
            .expect("config")
            .diagnostics
            .contains(&format!("MCP server 'bad' skipped: {message}"))
    );
    assert!(!service.connections.contains_key("bad"));
    service
        .connect_server("good")
        .await
        .expect("sibling connects");
    assert!(!stub.authorizations().is_empty());
    let log = std::fs::read_to_string(agent.path().join("bad.log")).expect("bad.log");
    let line: Value =
        serde_json::from_str(log.lines().next().expect("one line")).expect("json line");
    assert_eq!(line["message"], "ConfigReferenceError");
    assert_eq!(
        line["data"]["field"],
        "mcpServers.bad.headers.Authorization"
    );
    assert_eq!(line["data"]["reference"], reference);
    assert_eq!(line["data"]["reason"], reason);
    service.dispose().await.expect("dispose");
    registry.dispose().await.expect("registry dispose");
    stub.close().await;
}

#[tokio::test]
async fn reports_a_missing_file_reference_while_another_server_works() {
    tokio::time::timeout(
        Duration::from_secs(30),
        missing_reference_is_contained("file"),
    )
    .await
    .expect("bounded");
}

#[tokio::test]
async fn reports_a_missing_env_reference_while_another_server_works() {
    tokio::time::timeout(
        Duration::from_secs(30),
        missing_reference_is_contained("env"),
    )
    .await
    .expect("bounded");
}

/// Upstream's status half: `/mcp status` names the failing server as degraded with the reference error.
#[tokio::test]
async fn status_names_the_failing_reference() {
    let agent = tempfile::tempdir().expect("agent dir");
    write_config(
        agent.path(),
        json!({"bad": header_server("http://127.0.0.1:9/mcp", json!({"$env": "UNSET_3858"}))}),
    );
    let registry = Arc::new(HostMcpRegistry::default());
    let mut service = McpService::new(registry.clone(), 3860);
    service
        .attach_session(agent.path(), agent.path(), &BTreeMap::new(), true, &[])
        .await
        .expect("attach");
    let message = "mcpServers.bad.headers.Authorization: configuration reference unset";
    let status = service.status("MCP status").await;
    assert!(status.contains("bad enabled state=degraded"), "{status}");
    assert!(status.contains(message), "{status}");
    service.dispose().await.expect("dispose");
    registry.dispose().await.expect("registry dispose");
}
