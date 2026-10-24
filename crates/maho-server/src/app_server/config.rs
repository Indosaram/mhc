use super::registry::{JsonRpcError, MethodRegistration, MethodRegistry, MethodScope};
use maho_core::settings_manager::SettingsManager;
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path, sync::Arc};
use maho_ext_api::FlagValue;

/// The approval and OS-isolation state a client is shown for a session.
// SAFETY-DIVERGENCE(SD-3): the previous port hardcoded `never` / `danger-full-access`; the app-server now reports the
// resolved permission config and the actual `bashSandbox` isolation. The upstream app-server is the Codex protocol port.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EffectiveSafety { pub approval_policy: &'static str, pub sandbox_mode: &'static str }
impl EffectiveSafety {
    /// The `SandboxPolicy` wire object (`protocol/generated/v2/sandbox_policy.rs`).
    pub fn sandbox_policy(&self, cwd: &str) -> Value {
        match self.sandbox_mode {
            "workspace-write" => json!({"type":"workspaceWrite","writableRoots":[cwd],"networkAccess":true,"excludeTmpdirEnvVar":false,"excludeSlashTmp":true}),
            "read-only" => json!({"type":"readOnly","networkAccess":true}),
            _ => json!({"type":"dangerFullAccess"}),
        }
    }
}
fn bash_sandbox_tool_present() -> bool {
    let tool = if cfg!(target_os = "macos") { "sandbox-exec" } else if cfg!(target_os = "linux") { "bwrap" } else { return false };
    std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(tool).is_file()))
}
/// `get_flag` supplies the `--permission-preset` / `--permission` values (session flags or server CLI flags).
pub fn effective_safety(settings: &SettingsManager, get_flag: &dyn Fn(&str) -> Option<FlagValue>) -> EffectiveSafety {
    use maho_ext_permission_system::{cli, settings::{effective_approval_summary, resolve_permission_config}};
    let flag = |name: &str| match get_flag(name) { Some(FlagValue::String(value)) => Some(value), _ => None };
    let rules = flag("permission").map(|value| cli::parse_permission_flag(&value)).unwrap_or_default();
    let preset = flag("permission-preset").and_then(|value| cli::parse_permission_preset_flag(&value));
    let approval_policy = resolve_permission_config(settings, &rules, &maho_core::config::home_dir(), preset)
        .map(|resolved| effective_approval_summary(&resolved).as_str())
        .unwrap_or("on-request");
    let sandbox_mode = match settings.get_string("bashSandbox").as_deref() {
        Some("required") => "workspace-write",
        Some("auto") if bash_sandbox_tool_present() => "workspace-write",
        _ => "danger-full-access",
    };
    EffectiveSafety { approval_policy, sandbox_mode }
}
pub fn session_effective_safety(session: &maho_core::agent_session::AgentSession, cwd: &str, agent_dir: &str) -> EffectiveSafety {
    let settings = SettingsManager::create(cwd, agent_dir, &maho_core::config::home_dir(), true);
    effective_safety(&settings, &|name| session.flag_value(name))
}
pub fn register_config_methods(registry: &mut MethodRegistry, agent_dir: String, server_cwd: String) {
    register_config_methods_with_flags(registry, agent_dir, server_cwd, BTreeMap::new());
}
pub fn register_config_methods_with_flags(registry: &mut MethodRegistry, agent_dir: String, server_cwd: String, flags: BTreeMap<String, FlagValue>) {
    for method in ["config/read", "configRequirements/read"] {
        let agent_dir = agent_dir.clone();
        let server_cwd = server_cwd.clone();
        let flags = flags.clone();
        registry.register(method.into(), MethodRegistration { requires_init:true, experimental:false, scope:MethodScope::Global, handler:Arc::new(move |context| {
            let agent_dir = agent_dir.clone(); let server_cwd = server_cwd.clone(); let flags = flags.clone();
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
                    let project_source = json!({"type":"project","dotCodexFolder":Path::new(&cwd).join(".omo")});
                    let mut origins = json!({}); let mut user = json!({}); let mut project = json!({});
                    for (key, wire) in [("defaultModel","model"),("defaultProvider","model_provider"),("defaultThinkingLevel","model_reasoning_effort")] {
                        if let Some(value) = settings.get_global().get(key).filter(|value| value.is_string()) {
                            user[wire] = value.clone(); origins[wire] = json!({"name":user_source,"version":"unversioned"});
                        }
                        if let Some(value) = settings.get_project().get(key).filter(|value| value.is_string()) {
                            project[wire] = value.clone(); origins[wire] = json!({"name":project_source,"version":"unversioned"});
                        }
                    }
                    let safety = effective_safety(&settings, &|name| flags.get(name).cloned());
                    json!({"config":{"model":settings.get_string("defaultModel"),"model_provider":settings.get_string("defaultProvider"),"model_reasoning_effort":settings.get_string("defaultThinkingLevel"),"approval_policy":safety.approval_policy,"sandbox_mode":safety.sandbox_mode},"origins":origins,"layers":if include_layers { json!([{"name":user_source,"version":"unversioned","config":user,"disabledReason":null},{"name":project_source,"version":"unversioned","config":project,"disabledReason":null}]) } else { Value::Null }})
                }).await.map_err(|error| JsonRpcError::new(-32603,error.to_string()))
            })
        }) });
    }
}
