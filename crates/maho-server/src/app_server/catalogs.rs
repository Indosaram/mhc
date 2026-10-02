use super::{mcp_wire_status::McpWireStatusRegistry,registry::{JsonRpcError,MethodRegistration,MethodRegistry,MethodScope},thread_registry::ThreadRegistry};
use serde_json::{Value,json};
use std::sync::Arc;
use tokio::sync::Mutex;

pub fn paginate_catalog(items: &[Value],params: &Value,method: &str) -> Result<Value,JsonRpcError> {
    let invalid = |message|JsonRpcError::new(-32600,message);
    let cursor = match params.get("cursor") {None|Some(Value::Null)=>None,Some(Value::String(cursor))=>Some(cursor),_=>return Err(invalid(format!("{method} cursor must be a string or null")))};
    let start = match cursor {None=>0,Some(cursor) if !cursor.is_empty() && cursor.bytes().all(|byte|byte.is_ascii_digit())=>cursor.parse::<usize>().ok().filter(|offset|*offset <= 9_007_199_254_740_991 && *offset <= items.len()).ok_or_else(||invalid(format!("{method} cursor {cursor} exceeds total records {}",items.len())))?,Some(cursor)=>return Err(invalid(format!("{method} received an invalid cursor: {cursor}")))};
    let limit = match params.get("limit") {None|Some(Value::Null)=>items.len(),Some(value)=>value.as_f64().filter(|value|*value >= 0.0 && *value <= 9_007_199_254_740_991.0 && value.fract() == 0.0).map(|value|value as usize).ok_or_else(||invalid(format!("{method} limit must be a non-negative integer or null")))?}.max(1);
    let end = start.saturating_add(limit).min(items.len());
    Ok(json!({"data":items[start..end],"nextCursor":if end < items.len() {Some(end.to_string())} else {None}}))
}
pub fn register_catalog_methods(registry: &mut MethodRegistry,threads: Arc<ThreadRegistry>,inventory: Arc<Mutex<McpWireStatusRegistry>>,agent_dir: String,cwd: String) {
    for method in ["collaborationMode/list","permissionProfile/list","experimentalFeature/list","mcpServerStatus/list"] {
        let threads = threads.clone();let inventory = inventory.clone();let agent_dir = agent_dir.clone();let cwd = cwd.clone();
        registry.register(method.into(),MethodRegistration {requires_init:true,experimental:method == "collaborationMode/list",scope:MethodScope::Global,handler:Arc::new(move |context| {
            let threads = threads.clone();let inventory = inventory.clone();let agent_dir = agent_dir.clone();let cwd = cwd.clone();
            Box::pin(async move {
                let params = &context.request["params"];
                if !params.is_null() && !params.is_object() {return Err(JsonRpcError::new(-32600,format!("{method} params must be an object")));}
                if method == "collaborationMode/list" {
                    let runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {models_path:Some(std::path::Path::new(&agent_dir).join("models.json")),auth_path:Some(std::path::Path::new(&agent_dir).join("auth.json")),..Default::default()});
                    let models = maho_core::model_registry::ModelRegistry::new(runtime).get_available();
                    let model = models.iter().find(|model|maho_core::model_resolver::DEFAULT_MODEL_PER_PROVIDER.iter().any(|(provider,id)|*provider == model.provider && *id == model.id)).or_else(||models.first()).map_or("unknown",|model|model.id.as_str());
                    return Ok(json!({"data":[{"name":"default","mode":null,"model":model,"reasoning_effort":null}]}));
                }
                let field = if method == "permissionProfile/list" {"cwd"} else {"threadId"};
                let scope = match params.get(field) {None|Some(Value::Null)=>None,Some(Value::String(value))=>Some(value.as_str()),_=>return Err(JsonRpcError::new(-32600,format!("{method} {field} must be a string or null")))};
                if field == "threadId" && let Some(id) = scope {threads.get_loaded_thread(id).await.map_err(|_|JsonRpcError::new(-32600,format!("{method} received an unknown threadId: {id}")))?;}
                let items = if method == "permissionProfile/list" {
                    let path = maho_core::paths::resolve_path(scope.unwrap_or(&cwd),&cwd,&maho_core::paths::PathInputOptions {trim:true,..Default::default()});
                    let _settings = maho_core::settings_manager::SettingsManager::create(&path,&agent_dir,&maho_core::config::home_dir(),false);
                    vec![json!({"id":"dangerFullAccess","description":null,"allowed":true})]
                } else if method == "experimentalFeature/list" {Vec::new()} else {
                    let detail = match params.get("detail") {None|Some(Value::Null)=>"full",Some(Value::String(value)) if matches!(value.as_str(),"full"|"toolsAndAuthOnly")=>value,_=>return Err(JsonRpcError::new(-32600,"mcpServerStatus/list detail must be full, toolsAndAuthOnly, or null"))};
                    let inventory = inventory.lock().await;
                    inventory.resolve(scope).map(|adapter|adapter.server_statuses().iter().map(|server| {let mut server = server.clone();if detail == "toolsAndAuthOnly" {server["serverInfo"] = Value::Null;server["resources"] = json!([]);server["resourceTemplates"] = json!([]);}server}).collect::<Vec<_>>()).unwrap_or_default()
                };
                paginate_catalog(&items,params,method)
            })
        })});
    }
}
