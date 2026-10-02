use super::registry::{JsonRpcError, MethodRegistration, MethodRegistry, MethodScope};
use maho_core::settings_manager::SettingsManager;
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

pub fn register_config_methods(registry: &mut MethodRegistry, agent_dir: String, server_cwd: String) {
    for method in ["config/read", "configRequirements/read"] {
        let agent_dir = agent_dir.clone();
        let server_cwd = server_cwd.clone();
        registry.register(method.into(), MethodRegistration { requires_init:true, experimental:false, scope:MethodScope::Global, handler:Arc::new(move |context| {
            let agent_dir = agent_dir.clone(); let server_cwd = server_cwd.clone();
            Box::pin(async move {
                if method == "configRequirements/read" { return Ok(json!({"requirements":null})); }
                let params = &context.request["params"];
                if !params.is_null() && !params.is_object() { return Err(JsonRpcError::new(-32600,"config/read params must be an object")); }
                if params.get("includeLayers").is_some_and(|value| !value.is_boolean()) { return Err(JsonRpcError::new(-32600,"config/read includeLayers must be a boolean")); }
                if params.get("cwd").is_some_and(|value| !value.is_null() && !value.is_string()) { return Err(JsonRpcError::new(-32600,"config/read cwd must be a string or null")); }
                let include_layers = params["includeLayers"] == true;
                let cwd = params["cwd"].as_str().unwrap_or(&server_cwd).trim();
                let cwd = maho_core::paths::resolve_path(cwd, &server_cwd, &maho_core::paths::PathInputOptions { trim:true, ..Default::default() });
                tokio::task::spawn_blocking(move || {
                    let settings = SettingsManager::create(&cwd, &agent_dir, &maho_core::config::home_dir(), true);
                    let user_source = json!({"type":"user","file":Path::new(&agent_dir).join("settings.json"),"profile":null});
                    let project_source = json!({"type":"project","dotCodexFolder":Path::new(&cwd).join(".maho")});
                    let mut origins = json!({}); let mut user = json!({}); let mut project = json!({});
                    for (key, wire) in [("defaultModel","model"),("defaultProvider","model_provider"),("defaultThinkingLevel","model_reasoning_effort")] {
                        if let Some(value) = settings.get_global().get(key).filter(|value| value.is_string()) {
                            user[wire] = value.clone(); origins[wire] = json!({"name":user_source,"version":"unversioned"});
                        }
                        if let Some(value) = settings.get_project().get(key).filter(|value| value.is_string()) {
                            project[wire] = value.clone(); origins[wire] = json!({"name":project_source,"version":"unversioned"});
                        }
                    }
                    json!({"config":{"model":settings.get_string("defaultModel"),"model_provider":settings.get_string("defaultProvider"),"model_reasoning_effort":settings.get_string("defaultThinkingLevel"),"approval_policy":"never","sandbox_mode":"danger-full-access"},"origins":origins,"layers":if include_layers { json!([{"name":user_source,"version":"unversioned","config":user,"disabledReason":null},{"name":project_source,"version":"unversioned","config":project,"disabledReason":null}]) } else { Value::Null }})
                }).await.map_err(|error| JsonRpcError::new(-32603,error.to_string()))
            })
        }) });
    }
}
