use maho_server::app_server::{config::register_config_methods, registry::{MethodRegistry, RegistryConnection}};
use serde_json::json;

#[tokio::test]
async fn config_read_maps_settings_and_project_origins_with_nullable_layers() {
    let root = tempfile::tempdir().unwrap();
    let agent = root.path().join("agent");
    let project = root.path().join("project");
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::create_dir_all(project.join(".maho")).unwrap();
    std::fs::write(agent.join("settings.json"), r#"{"defaultModel":"global","defaultProvider":"faux"}"#).unwrap();
    std::fs::write(project.join(".maho/settings.json"), r#"{"defaultModel":"local","defaultThinkingLevel":"high"}"#).unwrap();
    let mut registry = MethodRegistry::default();
    register_config_methods(&mut registry, agent.to_string_lossy().into_owned(), project.to_string_lossy().into_owned());
    let connection = RegistryConnection { initialized:true, ..Default::default() };
    let response = registry.dispatch(connection.clone(), json!({"id":1,"method":"config/read","params":{"includeLayers":true}})).await;
    assert_eq!(response["result"]["config"]["model"], "local");
    assert_eq!(response["result"]["config"]["model_provider"], "faux");
    assert_eq!(response["result"]["origins"]["model"]["name"]["type"], "project");
    assert_eq!(response["result"]["layers"].as_array().unwrap().len(), 2);
    let response = registry.dispatch(connection.clone(), json!({"id":2,"method":"config/read"})).await;
    assert!(response["result"]["layers"].is_null());
    let response = registry.dispatch(connection.clone(), json!({"id":3,"method":"config/read","params":{"includeLayers":null}})).await;
    assert_eq!(response["error"]["code"], -32600);
    let response = registry.dispatch(connection, json!({"id":4,"method":"configRequirements/read"})).await;
    assert_eq!(response["result"], json!({"requirements":null}));
}
