use std::path::{Path, PathBuf};
use std::sync::Arc;

use maho_ext_api::*;
use maho_ext_mcp::config::{hash_config, normalize_server};
use maho_ext_mcp::config_schema::ServerConfigWire;
use maho_ext_mcp::host_registry::HostMcpRegistry;
use serde_json::json;
use crate::support;

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
struct TestUi;
impl ExtensionUi for TestUi {
    fn select<'a>(&'a self, _: &'a str, _: &'a [String], _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> { Box::pin(async { None }) }
    fn confirm<'a>(&'a self, _: &'a str, _: &'a str, _: ExtensionUiDialogOptions) -> UiFuture<'a, bool> { Box::pin(async { false }) }
    fn input<'a>(&'a self, _: &'a str, _: Option<&'a str>, _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> { Box::pin(async { None }) }
    fn notify(&self, _: &str, _: NotificationType) {}
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
fn context() -> ExtensionContext {
    ExtensionContext { ui: Arc::new(TestUi), mode: ExtensionMode::Print, has_ui: false, cwd: "/tmp".into(), agent_dir: "/tmp/agent".into(),
        session_manager: Arc::new(TestSession), model_registry: Arc::new(TestRegistry), model: None, thinking_level: None,
        service_tier: None, effective_service_tier: None, scoped_models: Vec::new(), goal_store_file: None,
        loaded_extension_paths: Vec::new(), signal: None, steering_signal: None,
        is_idle_fn: Arc::new(|| true), wait_for_idle_fn: Arc::new(|| Box::pin(async {})), is_project_trusted_fn: Arc::new(|| true),
        is_compacting_fn: Arc::new(|| false), get_system_prompt_fn: Arc::new(|| "base".into()),
        get_system_prompt_options_fn: Arc::new(|| BuildSystemPromptOptions { cwd: "/tmp".into(), ..Default::default() }),
        registered_mcp_servers: Vec::new(), update_tool_hook_status: None, idle_coordinator:None, logger:None, defer_macrotask:None, compaction_signal: Default::default() }
}

struct McpExtension {registry: Arc<HostMcpRegistry>}
impl Extension for McpExtension {
    fn register(&self, api: &mut ExtensionApi) {
        maho_ext_mcp::index::register_mcp_lifecycle(api, self.registry.clone(), 1);
    }
}

fn seed_direct_server(agent_dir: &Path) {
    let declaration = json!({"type":"stdio","command":support::node(),"args":[support::senpi("packages/coding-agent/test/mcp/fixtures/stdio-server.ts"),"--tools","2"],"exposure":"direct"});
    std::fs::write(agent_dir.join("mcp.json"), json!({"mcpServers":{"fx":declaration.clone()}}).to_string()).expect("write mcp.json");
    let config: ServerConfigWire = serde_json::from_value(declaration).expect("parse the mcp server declaration");
    let hash = hash_config(&normalize_server(config), Some(agent_dir)).expect("hash the seeded server config");
    let cache_dir = agent_dir.join("cache");
    std::fs::create_dir_all(&cache_dir).expect("create the cache dir");
    let catalog = json!({"version":1,"servers":{"fx":{"configHash":hash,"fetchedAt":chrono::Utc::now().timestamp_millis() as f64,
        "tools":[{"name":"alpha","inputSchema":{"type":"object","properties":{"q":{"type":"string"}}}}],"resources":[],"prompts":[]}}});
    std::fs::write(cache_dir.join("mcp-cache.json"), catalog.to_string()).expect("write mcp-cache.json");
}

#[tokio::test]
async fn session_start_registers_direct_mode_mcp_tools_into_the_real_session() {
    use maho_ai::providers::faux::{FauxAssistantMessageOptions, RegisterFauxProviderOptions, faux_assistant_message, faux_provider, faux_streams};
    use maho_core::agent_session::{AgentSession, AgentSessionConfig, ExtensionBindings, PromptOptions};
    let temp = tempfile::tempdir().unwrap();
    let cwd: PathBuf = temp.path().to_path_buf();
    seed_direct_server(&cwd);
    let provider = faux_provider(RegisterFauxProviderOptions { api: Some("faux".into()), tokens_per_second: Some(0.0), ..Default::default() }).expect("valid faux provider");
    let model = provider.get_model(Some("faux-1")).expect("faux-1 model");
    provider.set_responses(vec![faux_assistant_message(vec![ContentBlock::text("ok")], FauxAssistantMessageOptions { timestamp: Some(0), ..Default::default() }).into()]);
    let streams = faux_streams(provider.core.clone());
    let stream_fn: maho_agent::types::StreamFn = Arc::new(move |model, context, options| streams.stream_simple(model, context, options.map(|options| options.simple)));
    let runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {
        models_path: Some(cwd.join("models.json")), auth_path: Some(cwd.join("auth.json")), providers: Some(vec![provider.provider.clone()]), ..Default::default()
    }).expect("valid model runtime");
    let cwd_string = cwd.to_string_lossy().into_owned();
    let session = AgentSession::new(AgentSessionConfig {
        agent: maho_agent::Agent::new(maho_agent::AgentOptions {
            initial_state: Some(maho_agent::agent::PartialAgentState { model: Some(model), ..Default::default() }),
            stream_fn: Some(stream_fn), ..Default::default()
        }),
        session_manager: maho_core::session_manager::SessionManager::in_memory(&cwd_string, None, None),
        settings_manager: maho_core::settings_manager::SettingsManager::from_storage(Box::new(maho_core::settings_manager::InMemorySettingsStorage::default()), false),
        cwd: cwd_string.clone(), agent_dir: Some(cwd_string), fallback_now: Some(Arc::new(|| 0.0)), retry_random: Some(Arc::new(|| 0.5)),
        scoped_models: Vec::new(), favorite_models: Vec::new(), flag_values: Default::default(), custom_tools: Vec::new(),
        model_runtime: Some(runtime), model_registry: None, uses_default_stream_function: Some(false),
        initial_active_tool_names: None, default_tool_names: None, eval_only_tool_names: None,
        allowed_tool_names: None, excluded_tool_names: None, base_tools_override: None,
        session_start_event: None, auto_title_sessions: Some(false),
    }).unwrap();
    let registry = Arc::new(HostMcpRegistry::default());
    session.set_extension_runner(maho_ext_host::ExtensionRunner::from_static(vec![Box::new(McpExtension {registry: registry.clone()})], context())).await;
    session.bind_extensions(ExtensionBindings::default()).await;
    session.prompt("hi", PromptOptions::default()).await.unwrap();
    let exposed = session.get_active_tool_names();
    assert!(exposed.iter().any(|name| name == "mcp_fx_alpha"), "expected the cached direct-mode MCP tool to be exposed through the real session, saw {exposed:?}");
    session.dispose().await;
    registry.dispose().await.unwrap();
}

#[tokio::test]
async fn session_start_attach_publishes_live_mcp_status_to_the_bound_subscriber_and_stops_after_unsubscribe() {
    use maho_ai::providers::faux::{FauxAssistantMessageOptions, RegisterFauxProviderOptions, faux_assistant_message, faux_provider, faux_streams};
    use maho_core::agent_session::{AgentSession, AgentSessionConfig, ExtensionBindings, PromptOptions};
    use maho_ext_mcp::service_types::McpWireStatusSnapshot;
    let temp = tempfile::tempdir().unwrap();
    let cwd: PathBuf = temp.path().to_path_buf();
    seed_direct_server(&cwd);
    let provider = faux_provider(RegisterFauxProviderOptions { api: Some("faux".into()), tokens_per_second: Some(0.0), ..Default::default() }).expect("valid faux provider");
    let model = provider.get_model(Some("faux-1")).expect("faux-1 model");
    provider.set_responses(vec![faux_assistant_message(vec![ContentBlock::text("ok")], FauxAssistantMessageOptions { timestamp: Some(0), ..Default::default() }).into()]);
    let streams = faux_streams(provider.core.clone());
    let stream_fn: maho_agent::types::StreamFn = Arc::new(move |model, context, options| streams.stream_simple(model, context, options.map(|options| options.simple)));
    let runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {
        models_path: Some(cwd.join("models.json")), auth_path: Some(cwd.join("auth.json")), providers: Some(vec![provider.provider.clone()]), ..Default::default()
    }).expect("valid model runtime");
    let cwd_string = cwd.to_string_lossy().into_owned();
    let session = AgentSession::new(AgentSessionConfig {
        agent: maho_agent::Agent::new(maho_agent::AgentOptions {
            initial_state: Some(maho_agent::agent::PartialAgentState { model: Some(model), ..Default::default() }),
            stream_fn: Some(stream_fn), ..Default::default()
        }),
        session_manager: maho_core::session_manager::SessionManager::in_memory(&cwd_string, None, None),
        settings_manager: maho_core::settings_manager::SettingsManager::from_storage(Box::new(maho_core::settings_manager::InMemorySettingsStorage::default()), false),
        cwd: cwd_string.clone(), agent_dir: Some(cwd_string), fallback_now: Some(Arc::new(|| 0.0)), retry_random: Some(Arc::new(|| 0.5)),
        scoped_models: Vec::new(), favorite_models: Vec::new(), flag_values: Default::default(), custom_tools: Vec::new(),
        model_runtime: Some(runtime), model_registry: None, uses_default_stream_function: Some(false),
        initial_active_tool_names: None, default_tool_names: None, eval_only_tool_names: None,
        allowed_tool_names: None, excluded_tool_names: None, base_tools_override: None,
        session_start_event: None, auto_title_sessions: Some(false),
    }).unwrap();
    let registry = Arc::new(HostMcpRegistry::default());
    session.set_extension_runner(maho_ext_host::ExtensionRunner::from_static(vec![Box::new(McpExtension {registry: registry.clone()})], context())).await;
    let (send, mut receive) = tokio::sync::mpsc::unbounded_channel::<McpWireStatusSnapshot>();
    let subscription = session.bind_mcp_wire_status::<McpWireStatusSnapshot>(Arc::new(move |snapshot| { let _ = send.send(snapshot.clone()); })).await.expect("runner bound");
    session.bind_extensions(ExtensionBindings::default()).await;
    session.prompt("hi", PromptOptions::default()).await.unwrap();
    let snapshot = tokio::time::timeout(std::time::Duration::from_secs(5), receive.recv()).await.expect("attach publish within deadline").expect("subscription alive");
    assert!(snapshot.servers.iter().any(|server| server.name == "fx"), "expected the attach publisher to deliver the session snapshot, saw {:?}", snapshot.servers.iter().map(|server| server.name.clone()).collect::<Vec<_>>());
    // The live receive proves the erased publication landed; the session's retention subscription
    // (installed at runner bind) must also have caught the same erased Arc, so a late bind replays it.
    let retained = session.mcp_wire_status_snapshot::<McpWireStatusSnapshot>().expect("retention caught the erased publication");
    assert!(retained.servers.iter().any(|server| server.name == "fx"));
    let (late_send, mut late_receive) = tokio::sync::mpsc::unbounded_channel::<McpWireStatusSnapshot>();
    let late_subscription = session.bind_mcp_wire_status::<McpWireStatusSnapshot>(Arc::new(move |snapshot| { let _ = late_send.send(snapshot.clone()); })).await.expect("runner bound");
    let replayed = late_receive.try_recv().expect("a late bind replays the retained snapshot synchronously");
    assert!(replayed.servers.iter().any(|server| server.name == "fx"));
    drop(late_subscription);
    drop(subscription);
    let _ = session.publish_mcp_wire_status(&McpWireStatusSnapshot::default()).await;
    assert!(receive.try_recv().is_err(), "an unsubscribed consumer must receive nothing");
    session.dispose().await;
    registry.dispose().await.unwrap();
}

/// Records the service the lifecycle registers, so a test can inspect whether session_start attached it.
struct RecordingMcpExtension {
    registry: Arc<HostMcpRegistry>,
    service: Arc<std::sync::Mutex<Option<Arc<tokio::sync::Mutex<maho_ext_mcp::service::McpService>>>>>,
}
impl Extension for RecordingMcpExtension {
    fn register(&self, api: &mut ExtensionApi) {
        let service = maho_ext_mcp::index::register_mcp_lifecycle(api, self.registry.clone(), 1);
        *self.service.lock().unwrap() = Some(service);
    }
}

/// Registers the MCP lifecycle and records every bus a factory run received, so a test can keep the
/// pre-reload runtime's bus the way the old runtime's background workers do.
struct CapturingMcpExtension {registry: Arc<HostMcpRegistry>, buses: Arc<std::sync::Mutex<Vec<EventBus>>>}
impl Extension for CapturingMcpExtension {
    fn register(&self, api: &mut ExtensionApi) {
        self.buses.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(api.events.clone());
        maho_ext_mcp::index::register_mcp_lifecycle(api, self.registry.clone(), 1);
    }
}

#[tokio::test]
async fn sizes_only_session_start_skips_the_builtin_mcp_attach() {
    // agent-session.ts emits `sizesOnly: true` for a sizes-only startup; index.ts returns before attach.
    use maho_core::agent_session::{AgentSession, AgentSessionConfig, ExtensionBindings};
    for sizes_only in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let cwd: PathBuf = temp.path().to_path_buf();
        seed_direct_server(&cwd);
        let cwd_string = cwd.to_string_lossy().into_owned();
        use maho_ai::providers::faux::{RegisterFauxProviderOptions, faux_provider, faux_streams};
        let provider = faux_provider(RegisterFauxProviderOptions { api: Some("faux".into()), tokens_per_second: Some(0.0), ..Default::default() }).expect("valid faux provider");
        let model = provider.get_model(Some("faux-1")).expect("faux-1 model");
        let streams = faux_streams(provider.core.clone());
        let stream_fn: maho_agent::types::StreamFn = Arc::new(move |model, context, options| streams.stream_simple(model, context, options.map(|options| options.simple)));
        let runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {
            models_path: Some(cwd.join("models.json")), auth_path: Some(cwd.join("auth.json")), providers: Some(vec![provider.provider.clone()]), ..Default::default()
        }).expect("valid model runtime");
        let session = AgentSession::new(AgentSessionConfig {
            agent: maho_agent::Agent::new(maho_agent::AgentOptions {
                initial_state: Some(maho_agent::agent::PartialAgentState { model: Some(model), ..Default::default() }),
                stream_fn: Some(stream_fn),
                ..Default::default()
            }),
            session_manager: maho_core::session_manager::SessionManager::in_memory(&cwd_string, None, None),
            settings_manager: maho_core::settings_manager::SettingsManager::from_storage(Box::new(maho_core::settings_manager::InMemorySettingsStorage::default()), false),
            cwd: cwd_string.clone(), agent_dir: Some(cwd_string), fallback_now: Some(Arc::new(|| 0.0)), retry_random: Some(Arc::new(|| 0.5)),
            scoped_models: Vec::new(), favorite_models: Vec::new(), flag_values: Default::default(), custom_tools: Vec::new(),
            model_runtime: Some(runtime), model_registry: None, uses_default_stream_function: Some(false),
            initial_active_tool_names: None, default_tool_names: None, eval_only_tool_names: None,
            allowed_tool_names: None, excluded_tool_names: None, base_tools_override: None,
            session_start_event: None, auto_title_sessions: Some(false),
        }).unwrap();
        let registry = Arc::new(HostMcpRegistry::default());
        let recorded = Arc::new(std::sync::Mutex::new(None));
        session.set_extension_runner(maho_ext_host::ExtensionRunner::from_static(vec![Box::new(RecordingMcpExtension {registry: registry.clone(), service: recorded.clone()})], context())).await;
        session.bind_extensions(ExtensionBindings {sizes_only, ..Default::default()}).await;
        let service = recorded.lock().unwrap().clone().expect("lifecycle registered");
        let attached = service.lock().await.config.is_some();
        assert_eq!(attached, !sizes_only, "sizes_only={sizes_only}");
        session.dispose().await;
        registry.dispose().await.unwrap();
    }
}

#[tokio::test]
async fn wire_status_published_by_the_pre_reload_runtime_is_dropped_without_panicking() {
    use maho_ai::providers::faux::{RegisterFauxProviderOptions, faux_provider, faux_streams};
    use maho_core::agent_session::{AgentSession, AgentSessionConfig};
    use maho_ext_mcp::service_types::{McpWireAuthStatus, McpWireStatusServer, McpWireStatusSnapshot};
    let temp = tempfile::tempdir().unwrap();
    let cwd_string = temp.path().to_string_lossy().into_owned();
    let provider = faux_provider(RegisterFauxProviderOptions { api: Some("faux".into()), tokens_per_second: Some(0.0), ..Default::default() }).expect("valid faux provider");
    let model = provider.get_model(Some("faux-1")).expect("faux-1 model");
    let streams = faux_streams(provider.core.clone());
    let stream_fn: maho_agent::types::StreamFn = Arc::new(move |model, context, options| streams.stream_simple(model, context, options.map(|options| options.simple)));
    let runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {
        models_path: Some(temp.path().join("models.json")), auth_path: Some(temp.path().join("auth.json")), providers: Some(vec![provider.provider.clone()]), ..Default::default()
    }).expect("valid model runtime");
    let session = AgentSession::new(AgentSessionConfig {
        agent: maho_agent::Agent::new(maho_agent::AgentOptions {
            initial_state: Some(maho_agent::agent::PartialAgentState { model: Some(model), ..Default::default() }),
            stream_fn: Some(stream_fn), ..Default::default()
        }),
        session_manager: maho_core::session_manager::SessionManager::in_memory(&cwd_string, None, None),
        settings_manager: maho_core::settings_manager::SettingsManager::from_storage(Box::new(maho_core::settings_manager::InMemorySettingsStorage::default()), false),
        cwd: cwd_string.clone(), agent_dir: Some(cwd_string), fallback_now: Some(Arc::new(|| 0.0)), retry_random: Some(Arc::new(|| 0.5)),
        scoped_models: Vec::new(), favorite_models: Vec::new(), flag_values: Default::default(), custom_tools: Vec::new(),
        model_runtime: Some(runtime), model_registry: None, uses_default_stream_function: Some(false),
        initial_active_tool_names: None, default_tool_names: None, eval_only_tool_names: None,
        allowed_tool_names: None, excluded_tool_names: None, base_tools_override: None,
        session_start_event: None, auto_title_sessions: Some(false),
    }).unwrap();
    let registry = Arc::new(HostMcpRegistry::default());
    let buses = Arc::new(std::sync::Mutex::new(Vec::new()));
    session.set_extension_runner(maho_ext_host::ExtensionRunner::from_static(vec![Box::new(CapturingMcpExtension {registry: registry.clone(), buses: buses.clone()})], context())).await;
    // Given a session whose MCP runtime was replaced by a config reload
    assert!(!session.reload().await.expect("reload").cancelled);
    let (stale, _live) = {
        let buses = buses.lock().unwrap();
        assert_eq!(buses.len(), 2, "the reload must re-run the MCP factory on a fresh runtime");
        (buses[0].clone(), buses[1].clone())
    };
    let (send, mut receive) = tokio::sync::mpsc::unbounded_channel::<McpWireStatusSnapshot>();
    let subscription = session.bind_mcp_wire_status::<McpWireStatusSnapshot>(Arc::new(move |snapshot| { let _ = send.send(snapshot.clone()); })).await.expect("runner bound");
    // When an old-runtime worker publishes its wire status after the reload
    let snapshot = McpWireStatusSnapshot {servers: vec![McpWireStatusServer {name: "stale".into(), server_info: None, last_error: None, tools: Vec::new(), resources: Vec::new(), resource_templates: Vec::new(), auth_status: McpWireAuthStatus::Unsupported, status: None}]};
    let worker = tokio::spawn(async move { maho_ext_mcp::index::publish_wire_status(&stale, &snapshot) });
    // Then the worker finishes without panicking, reports the stale runtime as its typed outcome,
    // and the stale snapshot reaches no live consumer
    let joined = worker.await;
    assert!(joined.is_ok(), "publishing on the stale runtime panicked: {joined:?}");
    assert!(format!("{joined:?}").contains("stale after session replacement or reload"), "expected the stale-runtime failure as the publish outcome, got {joined:?}");
    assert!(receive.try_recv().is_err(), "a stale-runtime snapshot must not reach the live session");
    assert!(session.mcp_wire_status_snapshot::<McpWireStatusSnapshot>().is_none_or(|retained| retained.servers.iter().all(|server| server.name != "stale")));
    drop(subscription);
    session.dispose().await;
    registry.dispose().await.unwrap();
}
