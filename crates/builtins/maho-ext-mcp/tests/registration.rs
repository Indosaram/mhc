use std::{sync::{Arc, Mutex}, time::Duration};

use maho_ext_api::{CustomMessage, ExtensionActions, ExtensionFailure, ExtensionRuntime, JsonValue, SendMessageOptions, SendUserMessageOptions, ToolDefinition, ToolInfo, UserMessageContent};
use maho_ext_mcp::catalog::McpToolCatalogEntry;
use maho_ext_mcp::config_schema::default_settings;
use maho_ext_mcp::expose::tier_b::*;
use maho_ext_mcp::guard::output_guard::McpOutputArtifacts;
use maho_ext_mcp::tool_registrar::McpToolRegistrar;
use maho_ext_tool_search::service::ToolSearchService;
use serde_json::json;
use maho_ext_api::{ExtensionSessionActions,ExecuteToolOptions,ExecuteToolFuture,LazyToolActivator,SlashCommandInfo,Model,ExtensionFuture,ThinkingLevel,ExecOptions,ExecResult};

struct SessionActions(Arc<RecordingRegistrar>);
impl ExtensionSessionActions for SessionActions {
    fn set_session_name(&self,_:&str)->Result<(),ExtensionFailure> {Err("unused".into())}
    fn get_session_name(&self)->Result<Option<String>,ExtensionFailure> {Err("unused".into())}
    fn set_label(&self,_:&str,_:Option<&str>)->Result<(),ExtensionFailure> {Err("unused".into())}
    fn execute_tool<'a>(&'a self,name:&'a str,_:JsonValue,_:ExecuteToolOptions)->ExecuteToolFuture<'a> {Box::pin(async move {Err(maho_ext_api::ExecuteToolError {code:maho_ext_api::ExecuteToolErrorCode::UnknownTool,tool_name:name.into(),message:"unused".into(),active_tools:vec![]})})}
    fn get_active_tools(&self)->Result<Vec<String>,ExtensionFailure> {self.0.get_active_tools()}
    fn set_active_tools(&self,names:Vec<String>)->Result<(),ExtensionFailure> {self.0.set_active_tools(names)}
    fn refresh_tools(&self)->Result<(),ExtensionFailure> {Ok(())}
    fn install_registered_tool(&self,tool:maho_ext_api::RegisteredTool)->Result<(),ExtensionFailure> {self.0.register_tool(tool.definition)}
    fn register_removed_tool_hint(&self,_:&str,_:&str)->Result<(),ExtensionFailure> {Err("unused".into())}
    fn register_lazy_tool_activator(&self,_:LazyToolActivator)->Result<(),ExtensionFailure> {Err("unused".into())}
    fn get_commands(&self)->Result<Vec<SlashCommandInfo>,ExtensionFailure> {Err("unused".into())}
    fn set_model(&self,_:Model)->ExtensionFuture<'_,bool> {Box::pin(async {Err("unused".into())})}
    fn get_thinking_level(&self)->Result<ThinkingLevel,ExtensionFailure> {Err("unused".into())}
    fn set_thinking_level(&self,_:ThinkingLevel)->Result<(),ExtensionFailure> {Err("unused".into())}
    fn set_session_model(&self,_:Model)->ExtensionFuture<'_,bool> {Box::pin(async {Err("unused".into())})}
    fn set_session_thinking_level(&self,_:ThinkingLevel)->Result<(),ExtensionFailure> {Err("unused".into())}
    fn set_session_fast_mode(&self,_:bool)->Result<(),ExtensionFailure> {Err("unused".into())}
    fn exec<'a>(&'a self,_:&'a str,_:&'a [String],_:&'a std::path::Path,_:ExecOptions)->ExtensionFuture<'a,ExecResult> {Box::pin(async {Err("unused".into())})}
}
fn search_service(registrar:Arc<RecordingRegistrar>)->ToolSearchService {
    let runtime=ExtensionRuntime::default();runtime.bind_session_actions(Arc::new(SessionActions(registrar)));
    ToolSearchService::new(runtime,Arc::new(NoToolsActions))
}

#[derive(Default)]
struct RecordingRegistrar {registered: Mutex<Vec<String>>, active: Mutex<Vec<String>>}
impl McpToolRegistrar for RecordingRegistrar {
    fn get_active_tools(&self) -> Result<Vec<String>, ExtensionFailure> {Ok(self.active.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone())}
    fn set_active_tools(&self, names: Vec<String>) -> Result<(), ExtensionFailure> {*self.active.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = names; Ok(())}
    fn register_tool(&self, definition: ToolDefinition) -> Result<(), ExtensionFailure> {self.registered.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(definition.name); Ok(())}
}

struct NoToolsActions;
impl ExtensionActions for NoToolsActions {
    fn send_message(&self, _message: CustomMessage, _options: SendMessageOptions) -> Result<(), ExtensionFailure> {Err(ExtensionFailure::new("unused"))}
    fn send_user_message(&self, _content: UserMessageContent, _options: SendUserMessageOptions) -> Result<(), ExtensionFailure> {Err(ExtensionFailure::new("unused"))}
    fn append_entry(&self, _custom_type: &str, _data: Option<JsonValue>) -> Result<(), ExtensionFailure> {Err(ExtensionFailure::new("unused"))}
    fn get_all_tools(&self) -> Result<Vec<ToolInfo>, ExtensionFailure> {Ok(Vec::new())}
}

fn entry(server: &str, tool: &str) -> McpToolCatalogEntry {
    McpToolCatalogEntry {server: server.into(), tool: tool.into(), schema: json!({"type":"object"}), description: Some(format!("{tool} tool")), annotations: None, request_timeout: Duration::from_secs(1), client: None, runtime: None, connection: None, ensure_connected: None, ensure_fresh: None, agent_dir: None, artifacts: None, output_guard: None}
}

fn input(registered: Vec<McpToolCatalogEntry>, active: Vec<McpToolCatalogEntry>, search_mode: bool, proxy_gateways: Vec<(String, Vec<McpToolCatalogEntry>)>) -> McpTierBRegistrationInput {
    McpTierBRegistrationInput {registered_entries: registered, active_entries: active, search_mode, proxy_gateways, utility_tools: Vec::new(), settings: default_settings(), agent_dir: std::env::temp_dir(), artifacts: Arc::new(McpOutputArtifacts::default()), output_guard: None}
}

fn registrar_with_base() -> Arc<RecordingRegistrar> {
    let registrar = RecordingRegistrar::default();
    registrar.set_active_tools(vec!["base".into()]).expect("seed active set");
    Arc::new(registrar)
}

/// Seed a valid on-disk catalog so the lazy server is cached and does not race at attach
/// (upstream: `shouldRaceMcpStartup(lifecycle) || cachedCatalog === undefined`).
fn seed_cache(agent_dir: &std::path::Path, name: &str, declaration: &maho_ext_api::RegisteredMcpServerDeclaration) {
    let mut wire = maho_ext_mcp::config_schema::ServerConfigWire::from(&declaration.config);
    wire.cwd.get_or_insert_with(|| declaration.registration_cwd.to_string_lossy().into_owned());
    let hash = maho_ext_mcp::config::hash_config(&maho_ext_mcp::config::normalize_server(wire)).unwrap();
    maho_ext_mcp::catalog_cache::write_mcp_cached_server(agent_dir, name, maho_ext_mcp::catalog_cache::McpCachedServerCatalog {config_hash: hash, fetched_at: chrono::Utc::now().timestamp_millis() as f64, tools: vec![json!({"name":"tool_1","inputSchema":{"type":"object"}})], resources: vec![], prompts: vec![], instructions: None}).unwrap();
}

#[test]
fn direct_mode_registers_catalog_and_keeps_it_active_after_base() {
    let registrar = registrar_with_base();
    let mut tool_search = search_service(registrar.clone());
    let registration = register_mcp_tier_b_tools(registrar.clone(), input(vec![entry("fx", "alpha")], vec![entry("fx", "alpha")], false, Vec::new()), Some(&mut tool_search), &mut McpTierBRegistry::default(), None).expect("registration");
    let registered = registrar.registered.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    assert_eq!(registered.len(), 1);
    assert!(registered[0].starts_with("mcp_"));
    assert_eq!(registrar.get_active_tools().expect("active"), vec!["base".to_owned(), registered[0].clone()]);
    assert_eq!(registration.searchable.len(), 1);
    assert_eq!(registration.searchable[0].tool_name, "alpha");
}

#[test]
fn direct_and_proxy_registration_does_not_require_the_shared_service() {
    let registrar = registrar_with_base();
    let registration = register_mcp_tier_b_tools(registrar.clone(), input(vec![entry("fx", "alpha")], vec![entry("fx", "alpha")], false, vec![("px".into(), vec![entry("px", "beta")])]), None, &mut McpTierBRegistry::default(), None).expect("registration without the shared tool-search service");
    let registered = registrar.registered.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    assert_eq!(registered.len(), 2);
    assert!(registered.iter().any(|name| name == "mcp_px"));
    assert_eq!(registration.searchable.len(), 1);
    let active = registrar.get_active_tools().expect("active");
    assert_eq!(active.first().map(String::as_str), Some("base"));
    assert!(active.contains(&registered[0]) && active.contains(&registered[1]));
}

#[test]
fn search_mode_without_the_shared_service_is_an_explicit_error() {
    let registrar = registrar_with_base();
    let error = match register_mcp_tier_b_tools(registrar.clone(), input(vec![entry("fx", "alpha")], Vec::new(), true, Vec::new()), None, &mut McpTierBRegistry::default(), None) { Err(error) => error, Ok(_) => panic!("search mode must not silently register nothing") };
    assert!(error.message.contains("tool_search"));
    assert!(registrar.registered.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_empty());
}

#[test]
fn search_mode_registers_catalog_but_leaves_it_inactive_and_feeds_documents() {
    let registrar = registrar_with_base();
    let mut tool_search = search_service(registrar.clone());
    register_mcp_tier_b_tools(registrar.clone(), input(vec![entry("fx", "alpha")], Vec::new(), true, Vec::new()), Some(&mut tool_search), &mut McpTierBRegistry::default(), None).expect("registration");
    assert_eq!(registrar.registered.lock().unwrap_or_else(std::sync::PoisonError::into_inner).len(), 1);
    assert_eq!(registrar.get_active_tools().expect("active"), vec!["base".to_owned(),"tool_search".to_owned()]);
    let names = tool_search.get_catalog().expect("catalog").into_iter().map(|document| document.name).collect::<Vec<_>>();
    assert_eq!(names, vec![registrar.registered.lock().unwrap_or_else(std::sync::PoisonError::into_inner)[0].clone()]);
}

#[test]
fn proxy_mode_registers_only_the_gateway_and_hides_the_catalog() {
    let registrar = registrar_with_base();
    let registration = register_mcp_tier_b_tools(registrar.clone(), input(Vec::new(), Vec::new(), false, vec![("fx".into(), vec![entry("fx", "beta")])]), None, &mut McpTierBRegistry::default(), None).expect("registration");
    let registered = registrar.registered.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    assert_eq!(registered, vec!["mcp_fx".to_owned()]);
    assert!(registration.searchable.is_empty());
    assert_eq!(registrar.get_active_tools().expect("active"), vec!["base".to_owned(), "mcp_fx".to_owned()]);

}

#[test]
fn repeated_registration_does_not_drop_base_tools_from_the_active_set() {
    let registrar = registrar_with_base();
    let mut tool_search = search_service(registrar.clone());
    let mut registry = McpTierBRegistry::default();
    register_mcp_tier_b_tools(registrar.clone(), input(vec![entry("fx", "alpha")], vec![entry("fx", "alpha")], false, Vec::new()), Some(&mut tool_search), &mut registry, None).expect("first");
    register_mcp_tier_b_tools(registrar.clone(), input(vec![entry("fx", "alpha")], vec![entry("fx", "alpha")], false, Vec::new()), Some(&mut tool_search), &mut registry, None).expect("second");
    let active = registrar.get_active_tools().expect("active");
    assert_eq!(active.first().map(String::as_str), Some("base"));
    assert_eq!(active.len(), 2);
}

#[test]
fn retained_registrar_publishes_replacements_and_rejects_stale_sdk_scope() {
    let runtime=ExtensionRuntime::default();let observed=registrar_with_base();
    runtime.bind_session_actions(Arc::new(SessionActions(observed.clone())));
    let scope=runtime.registration_scope();scope.commit_registration().unwrap();
    let api=maho_ext_api::ExtensionApi::new(maho_ext_api::LoadedExtension::new("<builtin:mcp>",Default::default(),maho_ext_api::SourceInfo {source:"builtin".into(),..Default::default()}),Default::default(),Default::default(),scope.clone());
    let registrar=maho_ext_mcp::tool_registrar::SessionMcpToolRegistrar::from_api(&api);
    let tool=|description:&str|ToolDefinition::new("mcp_fx_alpha",description,json!({"type":"object"}),Arc::new(|_|Box::pin(async {Ok(maho_ext_api::ToolResult::text("ok"))})));
    registrar.register_tool(tool("first")).unwrap();registrar.register_tool(tool("replacement")).unwrap();
    let published=runtime.live_tools("<builtin:mcp>").unwrap();
    assert_eq!(published.len(),1);assert_eq!(published[0].definition.description,"replacement");
    assert_eq!(published[0].source_info.path,"<builtin:mcp>");
    scope.invalidate_registration("removed");
    assert!(registrar.register_tool(tool("stale")).is_err());
    assert_eq!(observed.registered.lock().unwrap().len(),2);
}

#[test]
fn empty_generation_removes_managed_tools_and_shared_search_feed() {
    let registrar=registrar_with_base();let mut search=search_service(registrar.clone());let mut registry=McpTierBRegistry::default();
    register_mcp_tier_b_tools(registrar.clone(),input(vec![entry("fx","alpha")],Vec::new(),true,Vec::new()),Some(&mut search),&mut registry,None).unwrap();
    register_mcp_tier_b_tools(registrar.clone(),input(Vec::new(),Vec::new(),false,Vec::new()),Some(&mut search),&mut registry,None).unwrap();
    assert_eq!(registrar.get_active_tools().unwrap(),vec!["base"]);assert!(search.get_catalog().unwrap().is_empty());
}

#[test]
fn search_mode_re_registration_restores_base_and_direct_tools() {
    // Upstream `registerMcpTierBTools` does NOT retain a non-stubSwap search-mode
    // promotion across re-registration: a catalog refresh (a raced startup connect
    // or `list_changed`) re-derives the active set from the base tools plus the
    // direct entries, so a previously activated tool returns to inactive.
    let registrar=registrar_with_base();let mut search=search_service(registrar.clone());let mut registry=McpTierBRegistry::default();
    let registration=register_mcp_tier_b_tools(registrar.clone(),input(vec![entry("fx","alpha")],Vec::new(),true,Vec::new()),Some(&mut search),&mut registry,None).unwrap();
    let name=registration.searchable[0].name.clone();(registration.activate)(std::slice::from_ref(&name)).unwrap();
    assert_eq!(registrar.get_active_tools().unwrap(),vec!["base".to_owned(),name.clone()]);
    register_mcp_tier_b_tools(registrar.clone(),input(vec![entry("fx","alpha")],Vec::new(),true,Vec::new()),Some(&mut search),&mut registry,None).unwrap();
    assert_eq!(registrar.get_active_tools().unwrap(),vec!["base".to_owned()]);
}

#[test]
fn active_set_registration_sorts_definitions_without_activating_them() {
    let registrar=registrar_with_base();
    let tool=|name:&str|ToolDefinition::new(name,"fixture",json!({"type":"object"}),Arc::new(|_|Box::pin(async {Ok(maho_ext_api::ToolResult::text("ok"))})));
    maho_ext_mcp::active_set::register_tools_preserving_active_set_with(registrar.as_ref(),vec![tool("zebra"),tool("alpha")],None).unwrap();
    assert_eq!(*registrar.registered.lock().unwrap(),vec!["alpha","zebra"]);assert_eq!(registrar.get_active_tools().unwrap(),vec!["base"]);
}

#[test]
fn cloned_native_gate_observes_settings_changes_and_reset() {
    use maho_ext_mcp::{service::McpNativeToolSearchGate,config_schema::NativeToolSearch};
    let gate=McpNativeToolSearchGate::default();let observer=gate.clone();
    gate.publish(Some(&NativeToolSearch::Auto("auto".into())));assert!(observer.enabled());
    gate.publish(Some(&NativeToolSearch::Enabled(false)));assert!(!observer.enabled());
    gate.publish(Some(&NativeToolSearch::Enabled(true)));assert!(observer.enabled());
    gate.publish(None);assert!(!observer.enabled());
}

#[tokio::test]
async fn cached_service_registration_connects_on_first_call_and_reads_current_client() {
    use maho_ext_mcp::{service::McpService,host_registry::HostMcpRegistry,catalog_cache::McpCachedServerCatalog};
    let root=tempfile::tempdir().unwrap();let registry=Arc::new(HostMcpRegistry::default());let mut service=McpService::new(registry.clone(),1);
    let declaration=maho_ext_api::RegisteredMcpServerDeclaration {name:"fx".into(),config:maho_ext_api::McpServerDeclaration {command:Some("/usr/bin/node".into()),args:Some(vec!["/home/indo/code/senpi/packages/coding-agent/test/mcp/fixtures/stdio-server.ts".into(),"--tools".into(),"1".into()]),exposure:Some(maho_ext_api::McpExposure::Direct),..Default::default()},extension_path:"fixture".into(),registration_cwd:root.path().into()};
    seed_cache(root.path(),"fx",&declaration);
    service.attach_session(root.path(),root.path(),&Default::default(),true,&[declaration]).await.unwrap();
    let entry=service.connections["fx"].entry.clone();let connection=entry.lock().await.connection.clone();
    entry.lock().await.cached_catalog=Some(McpCachedServerCatalog {config_hash:"fixture".into(),fetched_at:0.0,tools:vec![json!({"name":"tool_1","inputSchema":{"type":"object"}})],resources:vec![],prompts:vec![],instructions:None});
    let prepared=maho_ext_mcp::service_register::prepare_mcp_service_registration_entries(service.config.as_ref().unwrap(),&[entry]).await;
    let current=prepared[0].connection.clone();let connect=prepared[0].ensure_cached_tool_connected.clone();
    let mut catalog_entry=entry("fx","tool_1");catalog_entry.connection=Some(current);catalog_entry.ensure_connected=Some(connect);
    let tools=maho_ext_mcp::expose::register::build_mcp_tool_definitions(&[catalog_entry],root.path().into(),Arc::new(McpOutputArtifacts::default()),None);
    assert_eq!(connection.state(),maho_ext_mcp::connection::ServerConnectionState::Idle);
    let result=(tools[0].execute)(maho_ext_api::ToolCall {id:"lazy",params:json!({"value":"lazy"}),signal:Default::default(),on_update:None,context:None}).await.unwrap();
    assert_eq!(result.details.as_ref().unwrap()["tool"],"tool_1");assert_eq!(connection.state(),maho_ext_mcp::connection::ServerConnectionState::Connected);
    service.dispose().await.unwrap();registry.dispose().await.unwrap();
}
