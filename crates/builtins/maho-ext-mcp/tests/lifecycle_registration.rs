use std::path::{Path, PathBuf};
use std::sync::Arc;

use maho_ext_api::*;
use maho_ext_mcp::config::{hash_config, normalize_server};
use maho_ext_mcp::config_schema::ServerConfigWire;
use maho_ext_mcp::host_registry::HostMcpRegistry;
use serde_json::json;

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
        registered_mcp_servers: Vec::new(), update_tool_hook_status: None, idle_coordinator:None, logger:None, defer_macrotask:None }
}

struct McpExtension {registry: Arc<HostMcpRegistry>}
impl Extension for McpExtension {
    fn register(&self, api: &mut ExtensionApi) {
        maho_ext_mcp::index::register_mcp_lifecycle(api, self.registry.clone(), 1);
    }
}

fn seed_direct_server(agent_dir: &Path) {
    let declaration = json!({"type":"stdio","command":"/usr/bin/node","args":["/home/indo/code/senpi/packages/coding-agent/test/mcp/fixtures/stdio-server.ts","--tools","2"],"exposure":"direct"});
    std::fs::write(agent_dir.join("mcp.json"), json!({"mcpServers":{"fx":declaration.clone()}}).to_string()).unwrap();
    let config: ServerConfigWire = serde_json::from_value(declaration).unwrap();
    let hash = hash_config(&normalize_server(config)).unwrap();
    let cache_dir = agent_dir.join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();
    let catalog = json!({"version":1,"servers":{"fx":{"configHash":hash,"fetchedAt":chrono::Utc::now().timestamp_millis() as f64,
        "tools":[{"name":"alpha","inputSchema":{"type":"object","properties":{"q":{"type":"string"}}}}],"resources":[],"prompts":[]}}});
    std::fs::write(cache_dir.join("mcp-cache.json"), catalog.to_string()).unwrap();
}

#[tokio::test]
async fn session_start_registers_direct_mode_mcp_tools_into_the_real_session() {
    use maho_ai::providers::faux::{FauxAssistantMessageOptions, RegisterFauxProviderOptions, faux_assistant_message, faux_provider, faux_streams};
    use maho_core::agent_session::{AgentSession, AgentSessionConfig, ExtensionBindings, PromptOptions};
    let temp = tempfile::tempdir().unwrap();
    let cwd: PathBuf = temp.path().to_path_buf();
    seed_direct_server(&cwd);
    let provider = faux_provider(RegisterFauxProviderOptions { api: Some("faux".into()), tokens_per_second: Some(0.0), ..Default::default() });
    let model = provider.get_model(Some("faux-1")).expect("faux-1 model");
    provider.set_responses(vec![faux_assistant_message(vec![ContentBlock::text("ok")], FauxAssistantMessageOptions { timestamp: Some(0), ..Default::default() }).into()]);
    let streams = faux_streams(provider.core.clone());
    let stream_fn: maho_agent::types::StreamFn = Arc::new(move |model, context, options| streams.stream_simple(model, context, options.map(|options| options.simple)));
    let runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {
        models_path: Some(cwd.join("models.json")), auth_path: Some(cwd.join("auth.json")), providers: Some(vec![provider.provider.clone()]), ..Default::default()
    });
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
