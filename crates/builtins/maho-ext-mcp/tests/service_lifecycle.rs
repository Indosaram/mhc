//! Startup-race clause and `list_changed` re-registration lifecycle.
//!
//! Covers the two pinned registration triggers the native port had not wired:
//! `syncFromConfig`'s `cachedCatalog === undefined` race clause and the coalesced
//! `#handleServerToolsChanged` re-registration from `#wireListChanged`.

use std::sync::{Arc, Mutex};

use crate::support;
use maho_ext_api::{ExtensionFailure, ToolDefinition};
use maho_ext_mcp::connection::ServerConnectionState;
use maho_ext_mcp::host_registry::HostMcpRegistry;
use maho_ext_mcp::service::McpService;
use maho_ext_mcp::startup_race::McpStartupRaceResult;
use maho_ext_mcp::tool_registrar::McpToolRegistrar;

const FIXTURE: &str = "packages/coding-agent/test/mcp/fixtures/stdio-server.ts";

#[derive(Default)]
struct RecordingRegistrar {
    registered: Mutex<Vec<String>>,
    active: Mutex<Vec<String>>,
}
impl McpToolRegistrar for RecordingRegistrar {
    fn get_active_tools(&self) -> Result<Vec<String>, ExtensionFailure> {
        Ok(self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone())
    }
    fn set_active_tools(&self, names: Vec<String>) -> Result<(), ExtensionFailure> {
        *self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = names;
        Ok(())
    }
    fn register_tool(&self, definition: ToolDefinition) -> Result<(), ExtensionFailure> {
        self.registered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(definition.name);
        Ok(())
    }
    fn register_tool_with_renderers(
        &self,
        definition: ToolDefinition,
        _renderers: maho_ext_mcp::tool_registrar::McpToolRenderers,
    ) -> Result<(), ExtensionFailure> {
        self.register_tool(definition)
    }
}

fn http_declaration(
    name: &str,
    cwd: &std::path::Path,
    url: String,
    startup_timeout_ms: f64,
) -> maho_ext_api::RegisteredMcpServerDeclaration {
    maho_ext_api::RegisteredMcpServerDeclaration {
        name: name.into(),
        config: maho_ext_api::McpServerDeclaration {
            transport: Some(maho_ext_api::McpTransport::Http),
            url: Some(url),
            auth: Some(maho_ext_api::McpAuth::Disabled),
            exposure: Some(maho_ext_api::McpExposure::Direct),
            lifecycle: Some(maho_ext_api::McpLifecycle::Eager),
            startup_timeout_ms: Some(startup_timeout_ms),
            connect_timeout_ms: Some(5000.0),
            ..Default::default()
        },
        extension_path: "fixture".into(),
        registration_cwd: cwd.into(),
    }
}

fn declaration(
    name: &str,
    cwd: &std::path::Path,
    extra_args: &[&str],
) -> maho_ext_api::RegisteredMcpServerDeclaration {
    let mut args = vec![support::senpi(FIXTURE)];
    args.extend(extra_args.iter().map(|arg| (*arg).to_owned()));
    maho_ext_api::RegisteredMcpServerDeclaration {
        name: name.into(),
        config: maho_ext_api::McpServerDeclaration {
            transport: Some(maho_ext_api::McpTransport::Stdio),
            command: Some(support::node()),
            args: Some(args),
            exposure: Some(maho_ext_api::McpExposure::Direct),
            ..Default::default()
        },
        extension_path: "fixture".into(),
        registration_cwd: cwd.into(),
    }
}

/// Seed a valid on-disk catalog so the lazy server is cached and does not race at attach
/// (upstream: `shouldRaceMcpStartup(lifecycle) || cachedCatalog === undefined`).
fn seed_cache(
    agent_dir: &std::path::Path,
    name: &str,
    declaration: &maho_ext_api::RegisteredMcpServerDeclaration,
) {
    let mut wire = maho_ext_mcp::config_schema::ServerConfigWire::from(&declaration.config);
    wire.cwd
        .get_or_insert_with(|| declaration.registration_cwd.to_string_lossy().into_owned());
    let hash = maho_ext_mcp::config::hash_config(
        &maho_ext_mcp::config::normalize_server(wire),
        Some(agent_dir),
    )
    .expect("hash the seeded server config");
    maho_ext_mcp::catalog_cache::write_mcp_cached_server(
        agent_dir,
        name,
        maho_ext_mcp::catalog_cache::McpCachedServerCatalog {
            modern: false,
            credential_identity: None,
            config_hash: hash,
            fetched_at: chrono::Utc::now().timestamp_millis() as f64,
            tools: vec![serde_json::json!({"name":"tool_1","inputSchema":{"type":"object"}})],
            resources: vec![],
            prompts: vec![],
            instructions: None,
        },
    )
    .expect("write the seeded mcp catalog");
}

#[tokio::test]
async fn deferred_registration_completes_before_deferred_registration_wait_returns() {
    use axum::{Json, Router, http::StatusCode, response::IntoResponse, routing::post};
    let release = Arc::new(tokio::sync::Notify::new());
    let release_gate = release.clone();
    let app = Router::new().route(
        "/mcp",
        post(move |Json(request): Json<serde_json::Value>| {
            let release = release_gate.clone();
            async move {
                if request.get("id").is_none() {
                    return StatusCode::ACCEPTED.into_response();
                }
                let method = request["method"].as_str().unwrap_or_default();
                match method {
                    "initialize" => {
                        release.notified().await;
                        Json(serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": request["id"],
                            "result": {
                                "protocolVersion": "2025-11-25",
                                "capabilities": { "tools": {} },
                                "serverInfo": { "name": "delayed", "version": "1" }
                            }
                        }))
                        .into_response()
                    }
                    "tools/list" => Json(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": request["id"],
                        "result": {
                            "tools": [{ "name": "tool_1", "inputSchema": { "type": "object" } }]
                        }
                    }))
                    .into_response(),
                    "resources/list" => Json(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": request["id"],
                        "result": { "resources": [] }
                    }))
                    .into_response(),
                    "prompts/list" => Json(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": request["id"],
                        "result": { "prompts": [] }
                    }))
                    .into_response(),
                    _ => Json(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": request["id"],
                        "error": { "code": -32601, "message": format!("unsupported method {method}") }
                    }))
                    .into_response(),
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
    });
    let root = tempfile::tempdir().unwrap();
    let registry = Arc::new(HostMcpRegistry::default());
    let handle = Arc::new(tokio::sync::Mutex::new(McpService::new(
        registry.clone(),
        1,
    )));
    let recording = Arc::new(RecordingRegistrar::default());
    let registrar: Arc<dyn McpToolRegistrar> = recording.clone();
    {
        let mut service = handle.lock().await;
        service.bind_self(Arc::downgrade(&handle));
        service.bind_registration(registrar, None);
    }
    handle
        .lock()
        .await
        .attach_session(
            root.path(),
            root.path(),
            &Default::default(),
            true,
            &[http_declaration(
                "delayed",
                root.path(),
                format!("http://{address}/mcp"),
                10.0,
            )],
        )
        .await
        .unwrap();
    assert_eq!(
        handle
            .lock()
            .await
            .deferred_attach()
            .wait(std::time::Duration::ZERO)
            .await,
        McpStartupRaceResult::Timeout
    );
    release.notify_one();
    let (deferred, deferred_reg) = {
        let service = handle.lock().await;
        (service.deferred_attach(), service.deferred_registration())
    };
    let timeout = std::time::Duration::from_secs(10);
    assert_eq!(deferred.wait(timeout).await, McpStartupRaceResult::Settled);
    assert_eq!(
        deferred_reg.wait(timeout).await,
        McpStartupRaceResult::Settled
    );
    assert!(
        recording
            .registered
            .lock()
            .unwrap()
            .contains(&"mcp_delayed_tool_1".to_owned())
    );
    assert!(
        recording
            .active
            .lock()
            .unwrap()
            .contains(&"mcp_delayed_tool_1".to_owned())
    );
    handle.lock().await.dispose().await.unwrap();
    registry.dispose().await.unwrap();
    stop.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn shared_http_startup_connect_error_logs_warning_and_marks_needs_auth() {
    use axum::{
        Router,
        http::{HeaderValue, StatusCode, header},
        routing::post,
    };
    let app = Router::new().route(
        "/mcp",
        post(|| async {
            (
                StatusCode::UNAUTHORIZED,
                [(
                    header::WWW_AUTHENTICATE,
                    HeaderValue::from_static("Bearer realm=\"mcp\""),
                )],
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
    });
    let root = tempfile::tempdir().unwrap();
    let registry = Arc::new(HostMcpRegistry::default());
    let handle = Arc::new(tokio::sync::Mutex::new(McpService::new(
        registry.clone(),
        1,
    )));
    let declaration = http_declaration("auth", root.path(), format!("http://{address}/mcp"), 10.0);
    handle
        .lock()
        .await
        .attach_session(
            root.path(),
            root.path(),
            &Default::default(),
            true,
            &[declaration],
        )
        .await
        .unwrap();
    let deferred = handle.lock().await.deferred_attach();
    assert_eq!(
        deferred.wait(std::time::Duration::from_secs(10)).await,
        McpStartupRaceResult::Settled
    );
    let (state, logs) = {
        let service = handle.lock().await;
        let entry = service.connections["auth"].entry.clone();
        let state = entry.lock().await.connection.state();
        let logs = service.log_lines("auth", 20).await;
        (state, logs)
    };
    assert_eq!(state, ServerConnectionState::NeedsAuth);
    assert!(
        logs.iter().any(|line| line.contains("warning")),
        "expected warning log, got {logs:?}"
    );
    handle.lock().await.dispose().await.unwrap();
    registry.dispose().await.unwrap();
    stop.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn startup_oauth_token_rotation_resyncs_so_connection_stays_current() {
    use axum::{Json, Router, http::StatusCode, response::IntoResponse, routing::post};
    use maho_ext_mcp::auth::token_store::{McpStoredAuth, McpTokenStore};
    use std::sync::atomic::{AtomicBool, Ordering};

    let root = tempfile::tempdir().unwrap();
    let rotated = Arc::new(AtomicBool::new(false));
    let url_slot: Arc<std::sync::Mutex<Option<String>>> = Arc::new(std::sync::Mutex::new(None));
    let root_path = root.path().to_path_buf();
    let app = Router::new().route(
        "/mcp",
        post({
            let rotated = rotated.clone();
            let url_slot = url_slot.clone();
            move |Json(request): Json<serde_json::Value>| {
                let rotated = rotated.clone();
                let url_slot = url_slot.clone();
                let root_path = root_path.clone();
                async move {
                    if request.get("id").is_none() {
                        return StatusCode::ACCEPTED.into_response();
                    }
                    let method = request["method"].as_str().unwrap_or_default();
                    match method {
                        "initialize" => {
                            if !rotated.swap(true, Ordering::SeqCst)
                                && let Some(url) = url_slot.lock().unwrap().clone()
                            {
                                // Simulate an OAuth token refresh writing a new access token during startup connect.
                                McpTokenStore::new(&root_path, "oauth_srv", &url)
                                    .write(McpStoredAuth {
                                        access_token: Some("refreshed-access-token".into()),
                                        expires_at: Some(
                                            (chrono::Utc::now().timestamp_millis() + 600_000)
                                                as f64,
                                        ),
                                        ..Default::default()
                                    })
                                    .unwrap();
                            }
                            Json(serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": request["id"],
                                "result": {
                                    "protocolVersion": "2025-11-25",
                                    "capabilities": { "tools": {} },
                                    "serverInfo": { "name": "oauth_srv", "version": "1" }
                                }
                            }))
                            .into_response()
                        }
                        "tools/list" => Json(serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": request["id"],
                            "result": {
                                "tools": [{ "name": "issue_get", "inputSchema": { "type": "object" } }]
                            }
                        }))
                        .into_response(),
                        "resources/list" => Json(serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": request["id"],
                            "result": { "resources": [] }
                        }))
                        .into_response(),
                        "prompts/list" => Json(serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": request["id"],
                            "result": { "prompts": [] }
                        }))
                        .into_response(),
                        _ => Json(serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": request["id"],
                            "error": { "code": -32601, "message": format!("unsupported method {method}") }
                        }))
                        .into_response(),
                    }
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server_url = format!("http://{address}/mcp");
    *url_slot.lock().unwrap() = Some(server_url.clone());
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
    });

    McpTokenStore::new(root.path(), "oauth_srv", &server_url)
        .write(McpStoredAuth {
            access_token: Some("initial-access-token".into()),
            expires_at: Some((chrono::Utc::now().timestamp_millis() + 600_000) as f64),
            ..Default::default()
        })
        .unwrap();

    let registry = Arc::new(HostMcpRegistry::default());
    let handle = Arc::new(tokio::sync::Mutex::new(McpService::new(
        registry.clone(),
        1,
    )));
    let recording = Arc::new(RecordingRegistrar::default());
    let registrar: Arc<dyn McpToolRegistrar> = recording.clone();
    {
        let mut service = handle.lock().await;
        service.bind_self(Arc::downgrade(&handle));
        service.bind_registration(registrar, None);
    }
    let decl = maho_ext_api::RegisteredMcpServerDeclaration {
        name: "oauth_srv".into(),
        config: maho_ext_api::McpServerDeclaration {
            transport: Some(maho_ext_api::McpTransport::Http),
            url: Some(server_url),
            exposure: Some(maho_ext_api::McpExposure::Direct),
            startup_timeout_ms: Some(10.0),
            connect_timeout_ms: Some(5000.0),
            ..Default::default()
        },
        extension_path: "fixture".into(),
        registration_cwd: root.path().into(),
    };
    handle
        .lock()
        .await
        .attach_session(root.path(), root.path(), &Default::default(), true, &[decl])
        .await
        .unwrap();
    let (deferred, deferred_reg) = {
        let service = handle.lock().await;
        (service.deferred_attach(), service.deferred_registration())
    };
    let timeout = std::time::Duration::from_secs(10);
    assert_eq!(deferred.wait(timeout).await, McpStartupRaceResult::Settled);
    assert_eq!(
        deferred_reg.wait(timeout).await,
        McpStartupRaceResult::Settled
    );
    assert!(
        handle.lock().await.get_connection("oauth_srv").is_some(),
        "connection must be resynced under the refreshed OAuth credential instead of hidden as stale"
    );
    assert!(
        recording
            .active
            .lock()
            .unwrap()
            .contains(&"mcp_oauth_srv_issue_get".to_owned()),
        "active tools: {:?}",
        recording.active.lock().unwrap()
    );
    handle.lock().await.dispose().await.unwrap();
    registry.dispose().await.unwrap();
    stop.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn lazy_server_without_a_cache_races_at_attach() {
    let root = tempfile::tempdir().unwrap();
    let registry = Arc::new(HostMcpRegistry::default());
    let mut service = McpService::new(registry.clone(), 1);
    let declaration = declaration("fx", root.path(), &[]);
    service
        .attach_session(
            root.path(),
            root.path(),
            &Default::default(),
            true,
            &[declaration],
        )
        .await
        .unwrap();
    assert_eq!(
        service
            .wait_for_deferred_attach(std::time::Duration::from_secs(15))
            .await,
        McpStartupRaceResult::Settled
    );
    let entry = service.connections["fx"].entry.clone();
    assert_eq!(
        entry.lock().await.connection.state(),
        ServerConnectionState::Connected
    );
    assert!(entry.lock().await.cached_catalog.is_some());
    service.dispose().await.unwrap();
    registry.dispose().await.unwrap();
}

#[tokio::test]
async fn lazy_server_with_a_valid_cache_skips_the_startup_race() {
    let root = tempfile::tempdir().unwrap();
    let registry = Arc::new(HostMcpRegistry::default());
    let mut service = McpService::new(registry.clone(), 1);
    let declaration = declaration("fx", root.path(), &[]);
    seed_cache(root.path(), "fx", &declaration);
    service
        .attach_session(
            root.path(),
            root.path(),
            &Default::default(),
            true,
            &[declaration],
        )
        .await
        .unwrap();
    let entry = service.connections["fx"].entry.clone();
    let connection = entry.lock().await.connection.clone();
    assert_eq!(connection.state(), ServerConnectionState::Idle);
    assert!(entry.lock().await.cached_catalog.is_some());
    assert!(
        connection.get_root_pid().is_none(),
        "a cached lazy server must not spawn a process at attach"
    );
    service.dispose().await.unwrap();
    registry.dispose().await.unwrap();
}

#[tokio::test]
async fn list_changed_reregisters_and_records_the_delta() {
    let root = tempfile::tempdir().unwrap();
    let registry = Arc::new(HostMcpRegistry::default());
    let handle = Arc::new(tokio::sync::Mutex::new(McpService::new(
        registry.clone(),
        1,
    )));
    let registrar: Arc<dyn McpToolRegistrar> = Arc::new(RecordingRegistrar::default());
    {
        let mut service = handle.lock().await;
        service.bind_self(Arc::downgrade(&handle));
        service.bind_registration(registrar.clone(), None);
    }
    let declaration = declaration("fx", root.path(), &["--tools", "2"]);
    handle
        .lock()
        .await
        .attach_session(
            root.path(),
            root.path(),
            &Default::default(),
            true,
            &[declaration],
        )
        .await
        .unwrap();
    assert_eq!(
        handle
            .lock()
            .await
            .wait_for_deferred_attach(std::time::Duration::from_secs(15))
            .await,
        McpStartupRaceResult::Settled
    );
    let entry = handle.lock().await.connections["fx"].entry.clone();
    {
        let entry = entry.lock().await;
        assert_eq!(entry.list_changed_tasks.len(), 2);
        assert!(entry.list_changed_coalescer.is_some());
    }
    assert_eq!(
        entry.lock().await.connection.state(),
        ServerConnectionState::Connected
    );
    handle
        .lock()
        .await
        .handle_server_tools_changed("fx")
        .await
        .unwrap();
    let guard = entry.lock().await;
    assert_eq!(guard.last_list_changed_delta.as_deref(), Some("no change"));
    assert_eq!(guard.known_tool_names.as_ref().map(Vec::len), Some(2));
    drop(guard);
    handle.lock().await.dispose().await.unwrap();
    assert!(entry.lock().await.list_changed_coalescer.is_none());
    assert!(entry.lock().await.list_changed_tasks.is_empty());
    registry.dispose().await.unwrap();
}

/// service-tools-changed.ts: a connect-only refresh leaves an unchanged registration in place (#2177), while a
/// reported change re-registers and records the catalog cache it refreshed (#2188).
#[tokio::test]
async fn connect_only_refresh_skips_an_unchanged_registration() {
    let root = tempfile::tempdir().unwrap();
    let registry = Arc::new(HostMcpRegistry::default());
    let handle = Arc::new(tokio::sync::Mutex::new(McpService::new(
        registry.clone(),
        1,
    )));
    let recording = Arc::new(RecordingRegistrar::default());
    let registrar: Arc<dyn McpToolRegistrar> = recording.clone();
    {
        let mut service = handle.lock().await;
        service.bind_self(Arc::downgrade(&handle));
        service.bind_registration(registrar, None);
    }
    let declaration = declaration("fx", root.path(), &["--tools", "2"]);
    handle
        .lock()
        .await
        .attach_session(
            root.path(),
            root.path(),
            &Default::default(),
            true,
            &[declaration],
        )
        .await
        .unwrap();
    assert_eq!(
        handle
            .lock()
            .await
            .wait_for_deferred_attach(std::time::Duration::from_secs(15))
            .await,
        McpStartupRaceResult::Settled
    );
    let entry = handle.lock().await.connections["fx"].entry.clone();
    assert_eq!(
        entry.lock().await.connection.state(),
        ServerConnectionState::Connected
    );
    // Registered once the startup race settled: the session's identity matches the live catalog.
    let registered = handle
        .lock()
        .await
        .registration()
        .and_then(|registration| registration.registered_identities.get("fx").cloned());
    assert!(registered.is_some());
    let before = recording.registered.lock().unwrap().len();
    handle
        .lock()
        .await
        .handle_server_tools_changed_with("fx", true)
        .await
        .unwrap();
    assert_eq!(
        recording.registered.lock().unwrap().len(),
        before,
        "an unchanged catalog is not registered again"
    );
    assert_eq!(
        entry.lock().await.last_list_changed_delta.as_deref(),
        Some("no change")
    );
    // A reported change always re-registers.
    handle
        .lock()
        .await
        .handle_server_tools_changed_with("fx", false)
        .await
        .unwrap();
    assert!(recording.registered.lock().unwrap().len() > before);
    assert_eq!(
        entry
            .lock()
            .await
            .cached_catalog
            .as_ref()
            .map(|catalog| catalog.tools.len()),
        Some(2)
    );
    handle.lock().await.dispose().await.unwrap();
    registry.dispose().await.unwrap();
}

#[test]
fn registration_identity_tracks_tool_contracts_resources_and_prompts() {
    use maho_ext_mcp::catalog::{McpToolCatalogEntry, mcp_registration_identity};
    let tool = McpToolCatalogEntry {
        server: "fx".into(),
        tool: "t".into(),
        schema: serde_json::json!({"type":"object"}),
        output_schema: None,
        modern: false,
        description: Some("d".into()),
        annotations: None,
        request_timeout: std::time::Duration::from_secs(1),
        client: None,
        runtime: None,
        connection: None,
        ensure_connected: None,
        ensure_fresh: None,
        agent_dir: None,
        artifacts: None,
        output_guard: None,
        request_origin: None,
        invocation: None,
    };
    // catalog.ts: JSON.stringify([[tool, description, schema, outputSchema, modern, annotations]], resources, prompts).
    assert_eq!(
        mcp_registration_identity(std::slice::from_ref(&tool), None),
        r#"[[["t","d",{"type":"object"},null,false,null]],[],[]]"#
    );
    let mut changed = tool.clone();
    changed.description = Some("other".into());
    assert_ne!(
        mcp_registration_identity(&[changed], None),
        mcp_registration_identity(std::slice::from_ref(&tool), None)
    );
    let catalog = maho_ext_mcp::catalog_cache::McpCachedServerCatalog {
        modern: false,
        config_hash: "h".into(),
        credential_identity: None,
        fetched_at: 0.0,
        tools: vec![],
        resources: vec![serde_json::json!({"uri":"r"})],
        prompts: vec![],
        instructions: None,
    };
    assert_eq!(
        mcp_registration_identity(std::slice::from_ref(&tool), Some(&catalog)),
        r#"[[["t","d",{"type":"object"},null,false,null]],[{"uri":"r"}],[]]"#
    );
}
