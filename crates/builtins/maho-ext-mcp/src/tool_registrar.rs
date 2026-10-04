//! Runtime registrar for senpi fe8c564b's MCP `Pick<ExtensionAPI, ...>`.
//! Retain the real API so late installs also publish the SDK's live tool catalog.
use std::sync::Mutex;
use maho_ext_api::{ExtensionApi,ExtensionFailure,ToolDefinition};

pub trait McpToolRegistrar: Send + Sync {
    fn get_active_tools(&self)->Result<Vec<String>,ExtensionFailure>;
    fn set_active_tools(&self,names:Vec<String>)->Result<(),ExtensionFailure>;
    fn register_tool(&self,definition:ToolDefinition)->Result<(),ExtensionFailure>;
}

pub struct SessionMcpToolRegistrar {api:Mutex<ExtensionApi>}
impl SessionMcpToolRegistrar {
    pub fn from_api(api:&ExtensionApi)->Self {
        let mut registered=api.registered.clone();
        // Tools registered by another retained handle must survive this handle's
        // publication as well (not merely remain installed in the agent).
        if let Some(tools)=api.runtime.live_tools(&registered.identity.path) {registered.tools=tools;}
        Self {api:Mutex::new(ExtensionApi::new(registered,api.profile.clone(),api.events.clone(),api.runtime.clone()))}
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
        if let Some(tools)=api.runtime.live_tools(&api.registered.identity.path) {api.registered.tools=tools;}
        api.try_register_tool(definition)
    }
}
