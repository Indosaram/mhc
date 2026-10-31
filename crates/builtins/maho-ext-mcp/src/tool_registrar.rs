//! Runtime registrar for senpi fe8c564b's MCP `Pick<ExtensionAPI, ...>`.
//! Retain the real API so late installs also publish the SDK's live tool catalog.
use std::sync::Mutex;
use maho_ext_api::{ExtensionApi,ExtensionFailure,ToolDefinition,ToolRenderers};

/// senpi `ToolDefinition.renderCall`/`renderResult` for an MCP-registered tool: JSON args, no render state.
pub type McpToolRenderers=ToolRenderers<(),serde_json::Value>;

pub trait McpToolRegistrar: Send + Sync {
    fn get_active_tools(&self)->Result<Vec<String>,ExtensionFailure>;
    fn set_active_tools(&self,names:Vec<String>)->Result<(),ExtensionFailure>;
    fn register_tool(&self,definition:ToolDefinition)->Result<(),ExtensionFailure>;
    /// `registerTool` of a definition that carries `renderCall`/`renderResult`.
    fn register_tool_with_renderers(&self,definition:ToolDefinition,renderers:McpToolRenderers)->Result<(),ExtensionFailure>;
    /// The live registered definition named `name` (senpi `ctx.tools.find`), when the registrar can see it.
    fn live_tool_definition(&self,_name:&str)->Option<ToolDefinition> {None}
}

/// Adopt the live tool list and its renderers so this handle's publication keeps
/// what another retained handle registered (tools and renderers alike).
fn adopt_live_tools(api:&mut ExtensionApi) {
    let path=api.registered.identity.path.clone();
    let Some(tools)=api.runtime.live_tools(&path) else {return;};
    api.registered.tool_renderers.clear();api.registered.erased_tool_renderers.clear();
    for tool in &tools {
        let name=&tool.definition.name;
        if let Some(Some(renderers))=api.runtime.live_tool_renderer(&path,name) {api.registered.tool_renderers.insert(name.clone(),renderers);}
        if let Some(Some(renderers))=api.runtime.live_erased_tool_renderer(&path,name) {api.registered.erased_tool_renderers.insert(name.clone(),renderers);}
    }
    api.registered.tools=tools;
}

pub struct SessionMcpToolRegistrar {api:Mutex<ExtensionApi>}
impl SessionMcpToolRegistrar {
    pub fn from_api(api:&ExtensionApi)->Self {
        // Tools registered by another retained handle must survive this handle's
        // publication as well (not merely remain installed in the agent).
        let mut retained=ExtensionApi::new(api.registered.clone(),api.profile.clone(),api.events.clone(),api.runtime.clone());
        adopt_live_tools(&mut retained);
        Self {api:Mutex::new(retained)}
    }
}
impl McpToolRegistrar for SessionMcpToolRegistrar {
    fn get_active_tools(&self)->Result<Vec<String>,ExtensionFailure> {
        self.api.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get_active_tools()
    }
    fn set_active_tools(&self,names:Vec<String>)->Result<(),ExtensionFailure> {
        self.api.lock().unwrap_or_else(std::sync::PoisonError::into_inner).set_active_tools(names)
    }
    fn register_tool(&self,definition:ToolDefinition)->Result<(),ExtensionFailure> {
        let mut api=self.api.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        adopt_live_tools(&mut api);
        api.try_register_tool(definition)
    }
    fn register_tool_with_renderers(&self,definition:ToolDefinition,renderers:McpToolRenderers)->Result<(),ExtensionFailure> {
        let mut api=self.api.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        adopt_live_tools(&mut api);
        api.register_tool_with_renderers(definition,renderers)
    }
    fn live_tool_definition(&self,name:&str)->Option<ToolDefinition> {
        let runtime=self.api.lock().unwrap_or_else(std::sync::PoisonError::into_inner).runtime.clone();
        runtime.live_tool_definition(name)
    }
}
