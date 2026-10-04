use maho_ext_mcp::{auth::context::{ServerAuthMode,resolve_auth_mode},config::load_mcp_config,config_schema::{LoadMcpConfigOptions,McpServerSource},service_types::{McpWireAuthStatus,McpWireStatusServer,McpWireStatusSnapshot}};
use serde_json::{Value,json};
use std::{collections::BTreeMap,path::Path};

pub struct McpWireStatusAdapter {
    servers: Vec<Value>,
    unsubscribe: Option<Box<dyn FnOnce() + Send + Sync>>,
}
impl McpWireStatusAdapter {
    pub fn new(snapshot: McpWireStatusSnapshot) -> Self {Self {servers:map_snapshot(snapshot),unsubscribe:None}}
    pub fn server_statuses(&self) -> &[Value] {&self.servers}
    pub fn update(&mut self,snapshot: McpWireStatusSnapshot) {self.servers = map_snapshot(snapshot);}
    pub fn bind_live_updates(&mut self,unsubscribe: impl FnOnce() + Send + Sync + 'static) {self.dispose();self.unsubscribe = Some(Box::new(unsubscribe));}
    pub fn dispose(&mut self) {if let Some(unsubscribe) = self.unsubscribe.take() {unsubscribe();}}
}
impl Drop for McpWireStatusAdapter {fn drop(&mut self) {self.dispose();}}
#[derive(Default)]
pub struct McpWireStatusRegistry {global:Option<Arc<std::sync::Mutex<McpWireStatusAdapter>>>,threads:BTreeMap<String,Arc<std::sync::Mutex<McpWireStatusAdapter>>>}
impl McpWireStatusRegistry {
    pub fn new(global: Option<McpWireStatusAdapter>) -> Self {Self {global:global.map(|adapter|Arc::new(std::sync::Mutex::new(adapter))),threads:BTreeMap::new()}}
    pub fn set_global(&mut self,adapter: McpWireStatusAdapter) {self.global = Some(Arc::new(std::sync::Mutex::new(adapter)));}
    pub fn register_thread(&mut self,id: String,adapter: Arc<std::sync::Mutex<McpWireStatusAdapter>>) {self.threads.insert(id,adapter);}
    pub fn remove_thread(&mut self,id: &str) {self.threads.remove(id);}
    pub fn resolve(&self,id: Option<&str>) -> Option<Arc<std::sync::Mutex<McpWireStatusAdapter>>> {match id {None=>self.global.clone(),Some(id)=>self.threads.get(id).cloned()}}
    pub fn thread_ids(&self) -> Vec<String> {self.threads.keys().cloned().collect()}
}
pub fn create_process_mcp_wire_status_adapter(agent_dir: &Path,cwd: &Path,env: &BTreeMap<String,String>) -> Result<McpWireStatusAdapter,maho_ext_mcp::config::McpConfigValidationError> {
    let config = load_mcp_config(LoadMcpConfigOptions {agent_dir,cwd,env,project_trusted:false})?;
    let servers = config.servers.into_values().filter(|server|server.source == McpServerSource::Global).map(|server| {
        let auth_status = match server.config.as_ref().map(resolve_auth_mode).unwrap_or(ServerAuthMode::None) {ServerAuthMode::None=>McpWireAuthStatus::Unsupported,ServerAuthMode::Bearer=>McpWireAuthStatus::BearerToken,ServerAuthMode::OAuth=>McpWireAuthStatus::NotLoggedIn};
        McpWireStatusServer {name:server.name,server_info:None,tools:Vec::new(),resources:Vec::new(),resource_templates:Vec::new(),auth_status,status:None}
    }).collect();
    Ok(McpWireStatusAdapter::new(McpWireStatusSnapshot {servers}))
}
fn map_snapshot(mut snapshot: McpWireStatusSnapshot) -> Vec<Value> {
    snapshot.servers.sort_by(|left,right|left.name.cmp(&right.name));
    snapshot.servers.into_iter().map(|server| {
        let tools = server.tools.into_iter().map(|tool| {
            let mut mapped = serde_json::Map::new();mapped.insert("name".into(),json!(tool.name));mapped.insert("inputSchema".into(),tool.input_schema);
            for (key,value) in tool.extra {if matches!(key.as_str(),"title"|"description"|"outputSchema"|"annotations"|"icons"|"_meta") {mapped.insert(key,value);}}
            (tool.name,Value::Object(mapped))
        }).collect::<serde_json::Map<_,_>>();
        let resources = server.resources.into_iter().map(|resource| {let mut mapped = serde_json::Map::from_iter([("uri".into(),json!(resource.uri)),("name".into(),json!(resource.name))]);for (key,value) in resource.extra {if matches!(key.as_str(),"title"|"description"|"mimeType"|"size"|"annotations"|"icons"|"_meta") {mapped.insert(key,value);}}Value::Object(mapped)}).collect::<Vec<_>>();
        let templates = server.resource_templates.into_iter().map(|template| {let mut mapped = serde_json::Map::from_iter([("uriTemplate".into(),json!(template.uri_template)),("name".into(),json!(template.name))]);for (key,value) in template.extra {if matches!(key.as_str(),"title"|"description"|"mimeType"|"annotations"|"icons"|"_meta") {mapped.insert(key,value);}}Value::Object(mapped)}).collect::<Vec<_>>();
        json!({"name":server.name,"serverInfo":server.server_info,"tools":tools,"resources":resources,"resourceTemplates":templates,"authStatus":server.auth_status})
    }).collect()
}
