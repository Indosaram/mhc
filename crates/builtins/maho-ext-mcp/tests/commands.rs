use std::path::Path;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use maho_ext_api::*;
use maho_ext_mcp::{
    auth::{
        commands_auth::{AuthCommandDeps, McpAuthReconnect, run_auth_start_command},
        token_store::{McpStoredAuth, McpTokenStore},
    },
    commands::*,
    config_schema::{McpServerConfig, Transport},
    host_registry::HostMcpRegistry,
    service::McpService,
};

#[test]
fn quoted_endpoints_preserve_arguments_and_receive_production_defaults() {
    let args = split_command_args(
        r#"fixture node "argument with spaces" 'single quoted' "escaped\"quote""#,
    )
    .unwrap();
    assert_eq!(
        args,
        vec![
            "fixture",
            "node",
            "argument with spaces",
            "single quoted",
            "escaped\"quote"
        ]
    );
    let config = parse_server_config(&args[1..]);
    assert_eq!(config.transport, Some(Transport::Stdio));
    assert_eq!(config.connect_timeout_ms, Some(15000.0));
    assert_eq!(config.args.unwrap().len(), 3);
    let http = parse_server_config(&["https://example.test/mcp".into()]);
    assert_eq!(http.transport, Some(Transport::Http));
    assert_eq!(http.url.as_deref(), Some("https://example.test/mcp"));
}

#[test]
fn argument_completions_filter_the_upstream_subcommand_list() {
    assert_eq!(
        complete_mcp_subcommands("auth"),
        vec!["auth", "auth-start", "auth-complete"]
    );
    assert_eq!(complete_mcp_subcommands("").len(), 11);
    assert!(complete_mcp_subcommands("x").is_empty());
}

struct TestSession;
impl ToolSessionManager for TestSession {
    fn session_id(&self) -> &str {
        "session"
    }
    fn session_file(&self) -> Option<&Path> {
        None
    }
}
impl SessionManager for TestSession {
    fn get_entries(&self) -> Vec<SessionEntry> {
        Vec::new()
    }
    fn get_branch(&self) -> Vec<SessionEntry> {
        Vec::new()
    }
    fn get_leaf_id(&self) -> Option<String> {
        None
    }
    fn get_session_name(&self) -> Option<String> {
        None
    }
}
struct TestRegistry;
impl ModelRegistry for TestRegistry {
    fn get_all(&self) -> Vec<Model> {
        Vec::new()
    }
    fn get_available(&self) -> Vec<Model> {
        Vec::new()
    }
    fn find(&self, _: &str, _: &str) -> Option<Model> {
        None
    }
    fn has_configured_auth(&self, _: &Model) -> bool {
        false
    }
    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<String>> {
        Box::pin(async { Ok(None) })
    }
}
#[derive(Default)]
struct RecordingUi {
    notes: Mutex<Vec<(String, NotificationType)>>,
    confirm: bool,
}
impl ExtensionUi for RecordingUi {
    fn select<'a>(
        &'a self,
        _: &'a str,
        _: &'a [String],
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn confirm<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, bool> {
        let confirm = self.confirm;
        Box::pin(async move { confirm })
    }
    fn input<'a>(
        &'a self,
        _: &'a str,
        _: Option<&'a str>,
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn notify(&self, message: &str, kind: NotificationType) {
        self.notes.lock().unwrap().push((message.into(), kind));
    }
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(&self, _: &str, _: Option<WidgetContent>, _: ExtensionWidgetOptions) {}
    fn set_header(&self, _: Option<ComponentFactory>) {}
    fn set_footer(&self, _: Option<ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String {
        String::new()
    }
    fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> {
        Box::pin(async { Err("UI not available".into()) })
    }
    fn theme(&self) -> Theme {
        Theme::default()
    }
}
fn context(ui: Arc<RecordingUi>, root: &Path, has_ui: bool) -> ExtensionContext {
    ExtensionContext {
        ui,
        mode: ExtensionMode::Print,
        has_ui,
        cwd: root.into(),
        agent_dir: root.into(),
        session_manager: Arc::new(TestSession),
        model_registry: Arc::new(TestRegistry),
        model: None,
        thinking_level: None,
        service_tier: None,
        effective_service_tier: None,
        scoped_models: Vec::new(),
        goal_store_file: None,
        loaded_extension_paths: Vec::new(),
        signal: None,
        steering_signal: None,
        is_idle_fn: Arc::new(|| true),
        wait_for_idle_fn: Arc::new(|| Box::pin(async {})),
        is_project_trusted_fn: Arc::new(|| true),
        is_compacting_fn: Arc::new(|| false),
        get_system_prompt_fn: Arc::new(|| "base".into()),
        get_system_prompt_options_fn: Arc::new(|| BuildSystemPromptOptions {
            cwd: "/tmp".into(),
            ..Default::default()
        }),
        registered_mcp_servers: Vec::new(),
        update_tool_hook_status: None,
        idle_coordinator: None,
        logger: None,
        defer_macrotask: None,
        compaction_signal: Default::default(),
    }
}
async fn host(root: &Path, servers: serde_json::Value) -> McpCommandHost {
    std::fs::write(
        root.join("mcp.json"),
        serde_json::json!({"mcpServers": servers}).to_string(),
    )
    .unwrap();
    let mut service = McpService::new(Arc::new(HostMcpRegistry::default()), 1);
    service
        .attach_session(root, root, &Default::default(), true, &[])
        .await
        .unwrap();
    McpCommandHost {
        service: Arc::new(tokio::sync::Mutex::new(service)),
        pending_attach: Arc::new(|| Box::pin(async {})),
        append_entry: None,
        after_reconnect: None,
        open_browser: None,
    }
}
async fn run(raw: &str, ctx: &ExtensionContext, host: &McpCommandHost) {
    handle_mcp_command(&split_command_args(raw).unwrap(), ctx, host).await;
}
fn last(ui: &RecordingUi) -> (String, NotificationType) {
    ui.notes.lock().unwrap().last().cloned().unwrap()
}

// commands.test.ts "enables, disables, tails logs, and returns helpful unknown-server errors".
#[tokio::test]
async fn unknown_servers_list_the_known_ones_and_unknown_subcommands_are_named() {
    let root = tempfile::tempdir().unwrap();
    let host = host(
        root.path(),
        serde_json::json!({"fx": {"command": "/missing/fx", "enabled": false}}),
    )
    .await;
    let ui = Arc::new(RecordingUi::default());
    let ctx = context(ui.clone(), root.path(), true);
    run("test missing", &ctx, &host).await;
    assert_eq!(
        last(&ui),
        (
            "Unknown MCP server: missing\nKnown MCP servers: fx".into(),
            NotificationType::Error
        )
    );
    run("logs", &ctx, &host).await;
    assert_eq!(
        last(&ui).0,
        "Unknown MCP server: <missing>\nKnown MCP servers: fx"
    );
    run("bogus", &ctx, &host).await;
    assert_eq!(
        last(&ui),
        (
            "Unknown /mcp subcommand: bogus".into(),
            NotificationType::Error
        )
    );
    run("logs fx", &ctx, &host).await;
    assert_eq!(last(&ui).0, "MCP logs for fx: (empty)");
}

// commands.test.ts "round-trips /mcp add into global config after confirmation and aborts on cancel".
#[tokio::test]
async fn add_requires_ui_and_reports_a_cancelled_confirmation_as_a_warning() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path(), serde_json::json!({})).await;
    let headless = Arc::new(RecordingUi::default());
    run(
        "add docs https://example.test/mcp",
        &context(headless.clone(), root.path(), false),
        &host,
    )
    .await;
    assert_eq!(
        last(&headless),
        (
            "Cannot add MCP server without UI confirmation.".into(),
            NotificationType::Error
        )
    );
    let cancel = Arc::new(RecordingUi::default());
    run(
        "add skipped node server.js",
        &context(cancel.clone(), root.path(), true),
        &host,
    )
    .await;
    assert_eq!(
        last(&cancel),
        ("MCP add cancelled".into(), NotificationType::Warning)
    );
    run("add", &context(cancel.clone(), root.path(), true), &host).await;
    assert_eq!(last(&cancel).0, "Usage: /mcp add <name> <command...|url>");
    let yes = Arc::new(RecordingUi {
        confirm: true,
        ..Default::default()
    });
    // A missing stdio command fails its startup connect locally, without network.
    run(
        "add docs /missing/docs-server",
        &context(yes.clone(), root.path(), true),
        &host,
    )
    .await;
    assert_eq!(
        last(&yes),
        ("Added MCP server docs".into(), NotificationType::Info)
    );
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.path().join("mcp.json")).unwrap())
            .unwrap();
    assert_eq!(
        written["mcpServers"]["docs"]["command"],
        "/missing/docs-server"
    );
    host.service.lock().await.dispose().await.unwrap();
}

#[tokio::test]
async fn auth_start_with_valid_token_reports_already_authorized_and_reconnects() {
    use rustls_pki_types::{CertificateDer, PrivateKeyDer};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_rustls::{TlsAcceptor, rustls::ServerConfig};

    let key_pair = rcgen::KeyPair::generate().expect("key pair");
    let params = rcgen::CertificateParams::new(vec!["127.0.0.1".into()]).expect("params");
    let cert = params.self_signed(&key_pair).expect("cert");
    let cert_der = CertificateDer::from(cert.der().to_vec());
    let key_der = PrivateKeyDer::Pkcs8(key_pair.serialize_der().into());
    let crypto = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
    let tls_config = ServerConfig::builder_with_provider(crypto)
        .with_safe_default_protocol_versions()
        .expect("versions")
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
        .expect("single cert");
    let acceptor = TlsAcceptor::from(Arc::new(tls_config));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let issuer = format!("https://{address}");
    let server_url = format!("{issuer}/mcp");
    let metadata = serde_json::json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{issuer}/authorize"),
        "token_endpoint": format!("{issuer}/token"),
        "code_challenge_methods_supported": ["S256"]
    });
    let metadata_body = serde_json::to_vec(&metadata).unwrap();
    let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                _ = &mut stop_rx => break,
                accepted = listener.accept() => accepted,
            };
            let Ok((stream, _)) = accepted else {
                break;
            };
            let acceptor = acceptor.clone();
            let metadata_body = metadata_body.clone();
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(stream).await else {
                    return;
                };
                let mut request = vec![0; 4096];
                let Ok(read) = tls.read(&mut request).await else {
                    return;
                };
                let head = String::from_utf8_lossy(&request[..read]);
                let response = if head.starts_with("GET /.well-known/oauth-authorization-server") {
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        metadata_body.len()
                    )
                } else {
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_owned()
                };
                let _ = tls.write_all(response.as_bytes()).await;
                if head.starts_with("GET /.well-known/oauth-authorization-server") {
                    let _ = tls.write_all(&metadata_body).await;
                }
                let _ = tls.shutdown().await;
            });
        }
    });

    let root = tempfile::tempdir().unwrap();
    McpTokenStore::new(root.path(), "remote", &server_url)
        .write(McpStoredAuth {
            access_token: Some("valid-access-token".into()),
            expires_at: Some((chrono::Utc::now().timestamp_millis() + 60_000) as f64),
            ..Default::default()
        })
        .unwrap();
    let config = McpServerConfig {
        transport: Some(Transport::Http),
        url: Some(server_url),
        ..Default::default()
    };

    let reconnects = Arc::new(AtomicUsize::new(0));
    let reconnect_count = reconnects.clone();
    let on_reconnect: McpAuthReconnect = Arc::new(move || {
        let reconnect_count = reconnect_count.clone();
        Box::pin(async move {
            reconnect_count.fetch_add(1, Ordering::SeqCst);
        })
    });
    let notes = Arc::new(Mutex::new(Vec::new()));
    let recorded_notes = notes.clone();
    let notify = Arc::new(move |message: &str, kind| {
        recorded_notes
            .lock()
            .unwrap()
            .push((message.to_owned(), kind));
    });
    let client = reqwest::Client::builder()
        .no_proxy()
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap();
    let deps = AuthCommandDeps {
        server_name: "remote".into(),
        config,
        agent_dir: root.path().into(),
        has_ui: true,
        notify,
        callback_url: None,
        open_browser: None,
        on_reconnect,
        pending: Arc::new(Mutex::new(Default::default())),
        interactive_guard: None,
        client,
    };

    let result = run_auth_start_command(&deps).await;
    let _ = stop_tx.send(());
    server.await.unwrap();
    assert_eq!(result.unwrap(), "");
    assert_eq!(reconnects.load(Ordering::SeqCst), 1);
    let notes = notes.lock().unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].1, NotificationType::Info);
    assert!(notes[0].0.contains("already authorized"));
}

// commands.test.ts disable/enable: the connecting notice precedes the result.
#[tokio::test]
async fn disable_notifies_connecting_then_the_result() {
    let root = tempfile::tempdir().unwrap();
    let host = host(
        root.path(),
        serde_json::json!({"fx": {"command": "/missing/fx", "enabled": false}}),
    )
    .await;
    let ui = Arc::new(RecordingUi::default());
    run("disable fx", &context(ui.clone(), root.path(), true), &host).await;
    let notes = ui
        .notes
        .lock()
        .unwrap()
        .iter()
        .map(|(text, _)| text.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        notes,
        vec!["MCP server fx connecting", "Disabled MCP server fx"]
    );
}

// oauth-headless.test.ts "fails fast in non-UI mode with a headless hint and no browser attempt".
#[tokio::test]
async fn auth_without_ui_gives_the_headless_hint_and_unknown_servers_fail() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path(), serde_json::json!({"remote": {"type": "http", "url": "https://example.test/mcp", "enabled": false}})).await;
    let ui = Arc::new(RecordingUi::default());
    let ctx = context(ui.clone(), root.path(), false);
    run("auth remote", &ctx, &host).await;
    assert_eq!(last(&ui), ("MCP server remote needs interactive OAuth. Run senpi in a terminal, then: /mcp auth-start remote  and  /mcp auth-complete remote <redirect-url>".into(), NotificationType::Error));
    run("auth-complete remote", &ctx, &host).await;
    assert_eq!(
        last(&ui).0,
        "Usage: /mcp auth-complete remote <redirect-url>"
    );
    run("logout missing", &ctx, &host).await;
    assert_eq!(
        last(&ui),
        (
            "Unknown MCP server: missing".into(),
            NotificationType::Error
        )
    );
}

// token-store-per-server / oauth-headless "logout clears credentials so the next use needs auth again".
#[tokio::test]
async fn logout_clears_stored_credentials_and_reports_it() {
    let root = tempfile::tempdir().unwrap();
    let url = "https://example.test/mcp";
    let host = host(
        root.path(),
        serde_json::json!({"remote": {"type": "http", "url": url, "enabled": false}}),
    )
    .await;
    let store = maho_ext_mcp::auth::token_store::McpTokenStore::new(root.path(), "remote", url);
    store
        .write(maho_ext_mcp::auth::token_store::McpStoredAuth {
            access_token: Some("token".into()),
            ..Default::default()
        })
        .unwrap();
    let ui = Arc::new(RecordingUi::default());
    run(
        "logout remote",
        &context(ui.clone(), root.path(), false),
        &host,
    )
    .await;
    assert_eq!(
        last(&ui),
        (
            "MCP server remote logged out".into(),
            NotificationType::Info
        )
    );
    assert!(store.read().unwrap().is_none());
}
