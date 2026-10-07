//! SC-U2 registered OAuth TLS acceptance.
//!
//! Drives the real registered `/mcp auth-start`, `/mcp auth-complete` and `/mcp logout`
//! commands through the production `ExtensionRunner` against a controlled HTTPS
//! authorization server: discovery, DCR, authorize redirect, token exchange, a real TLS
//! reconnect, replay and logout. No McpService reply is mocked and TLS is never disabled.
//!
//! Role branching: the default (parent) role generates a throwaway CA + loopback leaf and
//! re-executes this same test binary (current_exe --exact registered_command_auth
//! --nocapture) with an isolated agent/TLS directory and SSL_CERT_FILE pointing at the
//! fixture CA. The child role runs the real TLS server and the registered commands.
//! The chain and hostname checks stay enabled and production require_https stays true.

use std::collections::BTreeMap;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::Router;
use futures::FutureExt;
use serde_json::{json, Value};
use tokio::io::AsyncReadExt;

use maho_ext_api::*;
use maho_ext_mcp::auth::token_store::McpTokenStore;
use maho_ext_mcp::host_registry::HostMcpRegistry;

const ROLE_ENV: &str = "SC_U2_TLS_ROLE";
const DIR_ENV: &str = "SC_U2_TLS_DIR";
const ACCESS_TOKEN: &str = "sc-u2-access-token";
const REFRESH_TOKEN: &str = "sc-u2-refresh-token";
const DCR_CLIENT_ID: &str = "sc-u2-dcr-client";
const AUTH_CODE: &str = "sc-u2-authorization-code";

fn redact(text: &str) -> String {
    text.replace(ACCESS_TOKEN, "[redacted]").replace(REFRESH_TOKEN, "[redacted]").replace(AUTH_CODE, "[redacted]")
}

/// Minimal, dependency-free PEM encoder: the fixture CA is written for SSL_CERT_FILE without
/// pulling the optional `pem` feature into rcgen.
fn pem_from_der(label: &str, der: &[u8]) -> String {
    use base64::Engine;
    let body = base64::engine::general_purpose::STANDARD.encode(der);
    let mut out = format!("-----BEGIN {label}-----\n");
    for chunk in body.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(chunk).expect("base64 is ascii"));
        out.push('\n');
    }
    out.push_str(&format!("-----END {label}-----\n"));
    out
}

fn generate_fixture_certificates() -> (String, Vec<u8>, Vec<u8>) {
    use rcgen::{BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair, KeyUsagePurpose};
    let ca_key = KeyPair::generate().expect("fixture CA key");
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).expect("fixture CA params");
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.distinguished_name.push(DnType::CommonName, "SC-U2 fixture CA");
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca_cert = ca_params.self_signed(&ca_key).expect("fixture CA certificate");
    let issuer = Issuer::new(ca_params.clone(), &ca_key);
    let leaf_key = KeyPair::generate().expect("fixture leaf key");
    let mut leaf_params = CertificateParams::new(vec!["127.0.0.1".to_string(), "localhost".to_string()]).expect("fixture leaf params");
    leaf_params.distinguished_name.push(DnType::CommonName, "127.0.0.1");
    leaf_params.use_authority_key_identifier_extension = true;
    leaf_params.key_usages = vec![KeyUsagePurpose::DigitalSignature, KeyUsagePurpose::KeyEncipherment];
    leaf_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let leaf_cert = leaf_params.signed_by(&leaf_key, &issuer).expect("fixture leaf certificate");
    (pem_from_der("CERTIFICATE", ca_cert.der().as_ref()), leaf_cert.der().to_vec(), leaf_key.serialize_der())
}

async fn run_parent() {
    let root = tempfile::tempdir().expect("fixture root");
    let (ca_pem, leaf_der, key_der) = generate_fixture_certificates();
    std::fs::write(root.path().join("ca.pem"), &ca_pem).expect("write fixture CA");
    std::fs::write(root.path().join("leaf.der"), &leaf_der).expect("write fixture leaf");
    std::fs::write(root.path().join("leaf-key.der"), &key_der).expect("write fixture key");
    let executable = std::env::current_exe().expect("current test executable");
    let mut child = tokio::process::Command::new(&executable)
        .arg("--exact")
        .arg("registered_command_auth")
        .arg("--nocapture")
        .env(ROLE_ENV, "child")
        .env(DIR_ENV, root.path())
        .env("SSL_CERT_FILE", root.path().join("ca.pem"))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn the child role");
    let mut child_stdout = child.stdout.take().expect("child stdout");
    let mut child_stderr = child.stderr.take().expect("child stderr");
    let stdout_task = tokio::spawn(async move { let mut buffer = Vec::new(); let _ = child_stdout.read_to_end(&mut buffer).await; buffer });
    let stderr_task = tokio::spawn(async move { let mut buffer = Vec::new(); let _ = child_stderr.read_to_end(&mut buffer).await; buffer });
    let status = match tokio::time::timeout(Duration::from_secs(120), child.wait()).await {
        Ok(status) => status.expect("child wait"),
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            let stdout = redact(&String::from_utf8_lossy(&stdout_task.await.unwrap_or_default()));
            let stderr = redact(&String::from_utf8_lossy(&stderr_task.await.unwrap_or_default()));
            panic!("the child role exceeded the bounded lifetime and was killed\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}");
        }
    };
    let stdout = redact(&String::from_utf8_lossy(&stdout_task.await.unwrap_or_default()));
    let stderr = redact(&String::from_utf8_lossy(&stderr_task.await.unwrap_or_default()));
    assert!(status.success(), "the registered OAuth TLS child must pass; status {status:?}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}");
}

fn build_tls_server_config(dir: &Path) -> Arc<rustls::ServerConfig> {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    let leaf = CertificateDer::from(std::fs::read(dir.join("leaf.der")).expect("fixture leaf der"));
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(std::fs::read(dir.join("leaf-key.der")).expect("fixture key der")));
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("fixture protocol versions")
        .with_no_client_auth()
        .with_single_cert(vec![leaf], key)
        .expect("fixture single certificate");
    Arc::new(config)
}

#[derive(Clone, Default)]
struct AuthServerState {
    base: String,
    authorize_queries: Arc<Mutex<Vec<BTreeMap<String, String>>>>,
    dcr_bodies: Arc<Mutex<Vec<Value>>>,
    token_requests: Arc<Mutex<Vec<BTreeMap<String, String>>>>,
    initialize_authorization: Arc<Mutex<Vec<Option<String>>>>,
}

async fn protected_resource(State(state): State<AuthServerState>) -> Json<Value> {
    let base = state.base.clone();
    Json(json!({ "resource": format!("{base}/mcp"), "authorization_servers": [base] }))
}

async fn authorization_server(State(state): State<AuthServerState>) -> Json<Value> {
    let base = state.base.clone();
    Json(json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/authorize"),
        "token_endpoint": format!("{base}/token"),
        "registration_endpoint": format!("{base}/register"),
        "code_challenge_methods_supported": ["S256"],
    }))
}

async fn register(State(state): State<AuthServerState>, body: String) -> Json<Value> {
    let value: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    state.dcr_bodies.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(value);
    Json(json!({ "client_id": DCR_CLIENT_ID }))
}

async fn authorize(State(state): State<AuthServerState>, Query(params): Query<BTreeMap<String, String>>) -> Response {
    state.authorize_queries.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(params.clone());
    let redirect = params.get("redirect_uri").cloned().unwrap_or_default();
    let state_value = params.get("state").cloned().unwrap_or_default();
    let location = format!("{redirect}?code={AUTH_CODE}&state={state_value}");
    Response::builder().status(StatusCode::FOUND).header("location", location).body(axum::body::Body::empty()).expect("authorize redirect")
}

async fn token(State(state): State<AuthServerState>, body: String) -> Json<Value> {
    let params: BTreeMap<String, String> = url::form_urlencoded::parse(body.as_bytes()).into_owned().collect();
    state.token_requests.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(params);
    Json(json!({ "access_token": ACCESS_TOKEN, "refresh_token": REFRESH_TOKEN, "token_type": "Bearer", "expires_in": 3600 }))
}

async fn mcp(State(state): State<AuthServerState>, headers: HeaderMap, body: String) -> Response {
    let value: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    if value.get("id").is_none() {
        return StatusCode::ACCEPTED.into_response();
    }
    let method = value.get("method").and_then(Value::as_str).unwrap_or("").to_owned();
    let authorization = headers.get("authorization").and_then(|header| header.to_str().ok()).map(str::to_owned);
    if method == "initialize" {
        state.initialize_authorization.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(authorization);
        return Json(json!({ "jsonrpc": "2.0", "id": value["id"], "result": { "protocolVersion": "2025-11-25", "capabilities": {}, "serverInfo": { "name": "sc-u2", "version": "1" } } })).into_response();
    }
    if method == "tools/list" {
        return Json(json!({ "jsonrpc": "2.0", "id": value["id"], "result": { "tools": [ { "name": "ping", "inputSchema": { "type": "object", "properties": {} } } ] } })).into_response();
    }
    Json(json!({ "jsonrpc": "2.0", "id": value["id"], "result": {} })).into_response()
}

async fn get_mcp() -> Response {
    // The transport probes the same URL with an SSE GET; METHOD_NOT_ALLOWED stops the stream.
    StatusCode::METHOD_NOT_ALLOWED.into_response()
}

/// TLS listener seam for axum::serve: the real TLS chain (rustls) wraps every accepted socket,
/// so no plaintext path exists for the fixture server.
struct TlsListener {
    listener: tokio::net::TcpListener,
    acceptor: tokio_rustls::TlsAcceptor,
}

impl axum::serve::Listener for TlsListener {
    type Io = tokio_rustls::server::TlsStream<tokio::net::TcpStream>;
    type Addr = std::net::SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            let Ok((tcp, address)) = self.listener.accept().await else { continue };
            match self.acceptor.accept(tcp).await {
                Ok(stream) => return (stream, address),
                Err(_) => continue,
            }
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.listener.local_addr()
    }
}

struct TestSession;
impl ToolSessionManager for TestSession {
    fn session_id(&self) -> &str { "session" }
    fn session_file(&self) -> Option<&Path> { None }
}
impl SessionManager for TestSession {
    fn get_entries(&self) -> Vec<SessionEntry> { Vec::new() }
    fn get_branch(&self) -> Vec<SessionEntry> { Vec::new() }
    fn get_leaf_id(&self) -> Option<String> { None }
    fn get_session_name(&self) -> Option<String> { None }
}

struct TestRegistry;
impl ModelRegistry for TestRegistry {
    fn get_all(&self) -> Vec<Model> { Vec::new() }
    fn get_available(&self) -> Vec<Model> { Vec::new() }
    fn find(&self, _: &str, _: &str) -> Option<Model> { None }
    fn has_configured_auth(&self, _: &Model) -> bool { false }
    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<String>> { Box::pin(async { Ok(Some("faux".into())) }) }
}

/// Recording UI: the registered command reports through notifications because invoke_command
/// returns unit, so the observables are read back from here.
#[derive(Default)]
struct RecordingUi {
    notifications: Mutex<Vec<(String, NotificationType)>>,
}
impl RecordingUi {
    fn messages(&self) -> Vec<(String, NotificationType)> {
        self.notifications.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }
    fn errors(&self) -> Vec<String> {
        self.messages().into_iter().filter(|(_, kind)| matches!(kind, NotificationType::Error)).map(|(message, _)| message).collect()
    }
}
impl ExtensionUi for RecordingUi {
    fn select<'a>(&'a self, _: &'a str, _: &'a [String], _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> { Box::pin(async { None }) }
    fn confirm<'a>(&'a self, _: &'a str, _: &'a str, _: ExtensionUiDialogOptions) -> UiFuture<'a, bool> { Box::pin(async { false }) }
    fn input<'a>(&'a self, _: &'a str, _: Option<&'a str>, _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> { Box::pin(async { None }) }
    fn notify(&self, message: &str, kind: NotificationType) {
        self.notifications.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((message.to_owned(), kind));
    }
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(&self, _: &str, _: Option<WidgetContent>, _: ExtensionWidgetOptions) {}
    fn set_header(&self, _: Option<ComponentFactory>) {}
    fn set_footer(&self, _: Option<ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String { String::new() }
    fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> { Box::pin(async { Err("UI not available".into()) }) }
    fn theme(&self) -> Theme { Theme::default() }
}

struct StubActions;
impl ExtensionCommandContextActions for StubActions {
    fn wait_for_idle(&self) -> ExtensionFuture<'_, ()> { Box::pin(async { Ok(()) }) }
    fn new_session(&self, _: NewSessionOptions) -> ExtensionFuture<'_, SessionNavigationResult> { Box::pin(async { Ok(SessionNavigationResult { cancelled: false }) }) }
    fn fork<'a>(&'a self, _: &'a str, _: ForkOptions) -> ExtensionFuture<'a, SessionNavigationResult> { Box::pin(async { Ok(SessionNavigationResult { cancelled: false }) }) }
    fn navigate_tree<'a>(&'a self, _: &'a str, _: ExtensionTreeNavigationOptions) -> ExtensionFuture<'a, SessionNavigationResult> { Box::pin(async { Ok(SessionNavigationResult { cancelled: false }) }) }
    fn edit_assistant_message<'a>(&'a self, _: &'a str, _: &'a str, _: EditMessageOptions) -> ExtensionFuture<'a, EditMessageResult> { Box::pin(async { Ok(EditMessageResult::default()) }) }
    fn edit_user_message<'a>(&'a self, _: &'a str, _: &'a str, _: EditMessageOptions) -> ExtensionFuture<'a, EditMessageResult> { Box::pin(async { Ok(EditMessageResult::default()) }) }
    fn switch_session<'a>(&'a self, _: &'a str, _: SwitchSessionOptions) -> ExtensionFuture<'a, SessionNavigationResult> { Box::pin(async { Ok(SessionNavigationResult { cancelled: false }) }) }
    fn reload(&self) -> ExtensionFuture<'_, ()> { Box::pin(async { Ok(()) }) }
}

fn extension_context(ui: Arc<RecordingUi>, cwd: PathBuf, agent_dir: PathBuf) -> ExtensionContext {
    ExtensionContext {
        ui, mode: ExtensionMode::Print, has_ui: true, cwd, agent_dir,
        session_manager: Arc::new(TestSession), model_registry: Arc::new(TestRegistry), model: None, thinking_level: None,
        service_tier: None, effective_service_tier: None, scoped_models: Vec::new(), goal_store_file: None,
        loaded_extension_paths: Vec::new(), signal: None, steering_signal: None,
        is_idle_fn: Arc::new(|| true), wait_for_idle_fn: Arc::new(|| Box::pin(async {})), is_project_trusted_fn: Arc::new(|| true),
        is_compacting_fn: Arc::new(|| false), get_system_prompt_fn: Arc::new(|| "base".into()),
        get_system_prompt_options_fn: Arc::new(|| BuildSystemPromptOptions { cwd: "/tmp".into(), ..Default::default() }),
        registered_mcp_servers: Vec::new(), update_tool_hook_status: None, idle_coordinator: None, logger: None,
        defer_macrotask: None, compaction_signal: Default::default(),
    }
}

struct McpExtension { registry: Arc<HostMcpRegistry> }
impl Extension for McpExtension {
    fn register(&self, api: &mut ExtensionApi) {
        maho_ext_mcp::index::register_mcp_lifecycle(api, self.registry.clone(), 1);
    }
}

fn query_map(url: &url::Url) -> BTreeMap<String, String> {
    url.query_pairs().map(|(key, value)| (key.into_owned(), value.into_owned())).collect()
}

fn index_has_fx(agent_dir: &Path) -> bool {
    let path = agent_dir.join("mcp-auth/index.json");
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str::<Value>(&text).ok().and_then(|value| value.get("fx").map(|_| ())).is_some(),
        Err(_) => false,
    }
}

async fn run_scenario(
    runner: &maho_ext_host::ExtensionRunner,
    command_context: &ExtensionCommandContext,
    ui: &Arc<RecordingUi>,
    state: &AuthServerState,
    store: &McpTokenStore,
    mcp_url: &str,
    agent_dir: &Path,
) {
    runner.invoke_command("mcp", "auth-start fx", command_context).await.expect("auth-start dispatches");
    let start_message = ui.messages().into_iter().rev().find(|(message, _)| message.contains("/mcp auth-complete")).map(|(message, _)| message).expect("auth-start surfaces the authorization URL");
    let authorization_url = url::Url::parse(start_message.rsplit('\n').next().expect("authorization URL line").trim()).expect("authorization URL parses");
    assert_eq!(authorization_url.scheme(), "https");
    assert_eq!(authorization_url.path(), "/authorize");
    let authorization = query_map(&authorization_url);
    assert_eq!(authorization.get("response_type").map(String::as_str), Some("code"));
    assert_eq!(authorization.get("client_id").map(String::as_str), Some(DCR_CLIENT_ID));
    assert_eq!(authorization.get("code_challenge_method").map(String::as_str), Some("S256"));
    assert!(authorization.get("code_challenge").is_some_and(|challenge| !challenge.is_empty()));
    assert!(authorization.get("state").is_some_and(|state| !state.is_empty()));
    assert_eq!(authorization.get("resource").map(String::as_str), Some(mcp_url));
    assert_eq!(authorization.get("scope").map(String::as_str), Some("mcp:read"));

    let dcr = state.dcr_bodies.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    assert_eq!(dcr.len(), 1, "exactly one DCR request");
    assert_eq!(dcr[0]["redirect_uris"], json!(["http://127.0.0.1:0/callback"]));
    assert_eq!(dcr[0]["grant_types"], json!(["authorization_code", "refresh_token"]));
    assert_eq!(dcr[0]["response_types"], json!(["code"]));
    assert_eq!(dcr[0]["token_endpoint_auth_method"], json!("none"));
    assert_eq!(dcr[0]["client_name"], json!("oh-my-openagent"));
    assert!(dcr[0].get("scope").is_none(), "the pinned DCR body carries no scope field");

    // Follow the authorization redirect manually; the port-zero callback is never dialed.
    let browser = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().expect("redirect-none client");
    let response = browser.get(authorization_url.clone()).send().await.expect("authorize request over TLS");
    assert_eq!(response.status().as_u16(), 302);
    let redirect = response.headers().get("location").and_then(|header| header.to_str().ok()).expect("authorize redirect Location").to_owned();
    assert!(redirect.starts_with("http://127.0.0.1:0/callback?"), "unexpected redirect target {redirect}");

    let errors_before_complete = ui.errors().len();
    runner.invoke_command("mcp", &format!("auth-complete fx {redirect}"), command_context).await.expect("auth-complete dispatches");
    assert_eq!(ui.errors().len(), errors_before_complete, "auth-complete must succeed without a UI error, saw {:?}", ui.errors());

    let stored = store.read().expect("token store read").expect("stored auth after completion");
    assert_eq!(stored.access_token.as_deref(), Some(ACCESS_TOKEN));
    assert_eq!(stored.resource.as_deref(), Some(mcp_url));

    let initializes = state.initialize_authorization.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    assert!(
        initializes.iter().any(|authorization| authorization.as_deref() == Some(&format!("Bearer {ACCESS_TOKEN}"))),
        "the post-completion reconnect initialize must carry the bearer token, saw {initializes:?}"
    );

    // Replay: the pending provider was consumed, so the registered UI reports StateMismatch and
    // no second token exchange runs.
    let errors_before_replay = ui.errors().len();
    let tokens_before_replay = state.token_requests.lock().unwrap_or_else(std::sync::PoisonError::into_inner).len();
    let stored_before_replay = store.read().expect("token store read before replay");
    runner.invoke_command("mcp", &format!("auth-complete fx {redirect}"), command_context).await.expect("replay dispatches");
    assert_eq!(ui.errors().len(), errors_before_replay + 1, "the replay must add exactly one registered UI error, saw {:?}", ui.errors());
    assert_eq!(state.token_requests.lock().unwrap_or_else(std::sync::PoisonError::into_inner).len(), tokens_before_replay, "a replay must not exchange a second token");
    assert!(store.read().expect("token store read after replay") == stored_before_replay, "a replay must not change the stored credentials");

    runner.invoke_command("mcp", "logout fx", command_context).await.expect("logout dispatches");
    assert!(store.read().expect("token store read after logout").is_none(), "logout removes the stored credentials");
    assert!(!store.dir().exists(), "logout removes the URL-hash token directory");
    assert!(!index_has_fx(agent_dir), "logout removes the fx mapping from mcp-auth/index.json");

    let leaked = ui.messages().into_iter().any(|(message, _)| message.contains(ACCESS_TOKEN) || message.contains(REFRESH_TOKEN));
    assert!(!leaked, "no fixture secret may appear in a notification");
}

async fn run_child() {
    let dir = PathBuf::from(std::env::var(DIR_ENV).expect("SC_U2_TLS_DIR"));
    let agent_dir = dir.join("agent");
    let cwd = dir.join("cwd");
    std::fs::create_dir_all(&agent_dir).expect("fixture agent dir");
    std::fs::create_dir_all(&cwd).expect("fixture cwd");

    let tls_config = build_tls_server_config(&dir);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("fixture listener");
    let address = listener.local_addr().expect("fixture address");
    let base = format!("https://{address}");
    let mcp_url = format!("{base}/mcp");
    std::fs::write(
        agent_dir.join("mcp.json"),
        json!({ "mcpServers": { "fx": { "type": "http", "url": mcp_url, "oauth": { "scopes": ["mcp:read"] } } } }).to_string(),
    ).expect("seed mcp.json");

    let state = AuthServerState { base: base.clone(), ..Default::default() };
    let router = Router::new()
        .route("/.well-known/oauth-protected-resource", get(protected_resource))
        .route("/.well-known/oauth-authorization-server", get(authorization_server))
        .route("/register", post(register))
        .route("/authorize", get(authorize))
        .route("/token", post(token))
        .route("/mcp", post(mcp).get(get_mcp))
        .with_state(state.clone());

    // Exact shutdown trigger registered before the server starts, so cleanup can always fire.
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let tls_listener = TlsListener { listener, acceptor: tokio_rustls::TlsAcceptor::from(tls_config) };
    let mut server = tokio::spawn(async move {
        axum::serve(tls_listener, router)
            .with_graceful_shutdown(async { let _ = stopped.await; })
            .await
            .expect("fixture TLS server");
    });

    let ui = Arc::new(RecordingUi::default());
    let registry = Arc::new(HostMcpRegistry::default());
    let mut runner = maho_ext_host::ExtensionRunner::from_static(
        vec![Box::new(McpExtension { registry: registry.clone() })],
        extension_context(ui.clone(), cwd.clone(), agent_dir.clone()),
    );
    let store = McpTokenStore::new(&agent_dir, "fx", &mcp_url);

    let outcome = AssertUnwindSafe(async {
        runner.emit(ExtensionEvent::SessionStart(SessionStartEvent { reason: SessionReason::Startup, initial_model_provenance: None, previous_session_file: None }))
            .await
            .expect("session start attaches the MCP session");
        let command_context = runner.create_command_context(Arc::new(StubActions)).expect("command context");
        run_scenario(&runner, &command_context, &ui, &state, &store, &mcp_url, &agent_dir).await;
    }).catch_unwind().await;

    // Teardown always runs so no listener or connection leaks, even when the scenario panicked;
    // it is bounded and its failures are reported rather than swallowed on the success path.
    let teardown = tokio::time::timeout(Duration::from_secs(10), AssertUnwindSafe(async {
        let shutdown = runner.emit(ExtensionEvent::SessionShutdown(SessionShutdownEvent { reason: SessionReason::Quit, target_session_file: None, signal: None })).await;
        let registry_dispose = registry.dispose().await;
        (shutdown, registry_dispose)
    }).catch_unwind()).await;
    let _ = stop.send(());
    let server_result = match tokio::time::timeout(Duration::from_secs(5), &mut server).await {
        Ok(joined) => joined.map_err(|error| error.to_string()),
        Err(_) => { server.abort(); let _ = server.await; Err("the fixture TLS server did not stop within the bound".to_string()) }
    };

    match outcome {
        Ok(()) => {
            let (shutdown, registry_dispose) = match teardown {
                Ok(Ok(results)) => results,
                Ok(Err(_)) => panic!("teardown must not panic"),
                Err(_) => panic!("teardown must complete within the bound"),
            };
            shutdown.expect("session shutdown must dispose the MCP session");
            registry_dispose.expect("registry dispose must succeed");
            server_result.expect("the fixture TLS server must stop gracefully within the bound");
            assert!(std::net::TcpListener::bind(address).is_ok(), "the fixture listener must be released");
        }
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

#[tokio::test]
async fn registered_command_auth() {
    if std::env::var(ROLE_ENV).as_deref() == Ok("child") {
        run_child().await;
    } else {
        run_parent().await;
    }
}
