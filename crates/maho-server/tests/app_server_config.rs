use maho_server::app_server::{config::{register_config_methods, register_config_methods_with_flags}, registry::{MethodRegistry, RegistryConnection}};
use serde_json::json;

#[tokio::test]
async fn config_read_maps_settings_and_project_origins_with_nullable_layers() {
    let root = tempfile::tempdir().expect("test");
    let agent = root.path().join("agent");
    let project = root.path().join("project");
    std::fs::create_dir_all(&agent).expect("test");
    std::fs::create_dir_all(project.join(".omo")).expect("test");
    std::fs::write(agent.join("settings.json"), r#"{"defaultModel":"global","defaultProvider":"faux"}"#).expect("test");
    std::fs::write(project.join(".omo/settings.json"), r#"{"defaultModel":"local","defaultThinkingLevel":"high"}"#).expect("test");
    let mut registry = MethodRegistry::default();
    register_config_methods(&mut registry, agent.to_string_lossy().into_owned(), project.to_string_lossy().into_owned());
    let connection = RegistryConnection { initialized:true, ..Default::default() };
    let response = registry.dispatch(connection.clone(), json!({"id":1,"method":"config/read","params":{"includeLayers":true}})).await;
    assert_eq!(response["result"]["config"]["model"], "local");
    assert_eq!(response["result"]["config"]["model_provider"], "faux");
    assert_eq!(response["result"]["config"]["approval_policy"], "never");
    assert_eq!(response["result"]["config"]["sandbox_mode"], "danger-full-access");
    assert_eq!(response["result"]["origins"]["model"]["name"]["type"], "project");
    assert_eq!(response["result"]["layers"].as_array().expect("test").len(), 2);
    let response = registry.dispatch(connection.clone(), json!({"id":2,"method":"config/read"})).await;
    assert!(response["result"]["layers"].is_null());
    let response = registry.dispatch(connection.clone(), json!({"id":3,"method":"config/read","params":{"includeLayers":null}})).await;
    assert_eq!(response["error"]["code"], -32600);
    let response = registry.dispatch(connection, json!({"id":4,"method":"configRequirements/read"})).await;
    assert_eq!(response["result"], json!({"requirements":null}));
}

async fn safety(settings: &str, flags: &[(&str, &str)]) -> (String, String) {
    let root = tempfile::tempdir().expect("test");
    let agent = root.path().join("agent");
    std::fs::create_dir_all(&agent).expect("test");
    std::fs::write(agent.join("settings.json"), settings).expect("test");
    let flags = flags.iter().map(|(name, value)| ((*name).to_owned(), maho_ext_api::FlagValue::String((*value).to_owned()))).collect();
    let mut registry = MethodRegistry::default();
    register_config_methods_with_flags(&mut registry, agent.to_string_lossy().into_owned(), root.path().to_string_lossy().into_owned(), flags);
    let response = registry.dispatch(RegistryConnection { initialized:true, ..Default::default() }, json!({"id":1,"method":"config/read"})).await;
    let config = &response["result"]["config"];
    (config["approval_policy"].as_str().expect("test").to_owned(), config["sandbox_mode"].as_str().expect("test").to_owned())
}

// SAFETY-DIVERGENCE(SD-3): config/read reports the resolved permission config and OS isolation, not constants.
#[tokio::test]
async fn config_read_reports_effective_approval_and_sandbox() {
    assert_eq!(safety("{}", &[]).await, ("never".into(), "danger-full-access".into()));
    assert_eq!(safety(r#"{"permissionPreset":"guarded"}"#, &[]).await, ("on-request".into(), "danger-full-access".into()));
    assert_eq!(safety(r#"{"permissionPreset":"full-access"}"#, &[]).await, ("never".into(), "danger-full-access".into()));
    assert_eq!(safety(r#"{"permissionPreset":"read-only"}"#, &[]).await, ("untrusted".into(), "danger-full-access".into()));
    assert_eq!(safety(r#"{"permissionPreset":"full-access"}"#, &[("permission-preset", "ask")]).await, ("untrusted".into(), "danger-full-access".into()));
    assert_eq!(safety("{}", &[("permission-preset", "full-access")]).await, ("never".into(), "danger-full-access".into()));
    assert_eq!(safety(r#"{"permissionPreset":"full-access"}"#, &[("permission", "bash=ask")]).await, ("on-request".into(), "danger-full-access".into()));
    assert_eq!(safety(r#"{"bashSandbox":"required"}"#, &[]).await, ("never".into(), "workspace-write".into()));
    assert_eq!(safety(r#"{"bashSandbox":"off"}"#, &[]).await, ("never".into(), "danger-full-access".into()));
}
