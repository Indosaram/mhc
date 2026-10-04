use maho_server::app_server::{models::register_model_list_method, registry::{MethodRegistry, RegistryConnection}};
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn native_model_registry_projects_reasoning_and_validates_integer_limits() {
    let model = maho_ai::providers::all::get_builtin_models("openai").remove(0);
    let id = model.id.clone();
    let mut registry = MethodRegistry::default();
    register_model_list_method(&mut registry, Arc::new(move || vec![model.clone()]));
    let connection = RegistryConnection { initialized:true, ..Default::default() };
    let response = registry.dispatch(connection.clone(),json!({"id":1,"method":"model/list","params":{"limit":1}})).await;
    assert_eq!(response["result"]["data"][0]["model"],id);
    for params in [json!({"limit":1.5}),json!({"limit":-1}),json!({"includeHidden":1}),json!({"cursor":1})] {
        let response = registry.dispatch(connection.clone(),json!({"id":2,"method":"model/list","params":params})).await;
        assert_eq!(response["error"]["code"],-32600);
    }
}

#[tokio::test]
async fn remote_clients_validate_before_reporting_unavailability() {
    let mut registry = MethodRegistry::default();
    register_model_list_method(&mut registry, Arc::new(Vec::new));
    let connection = RegistryConnection { initialized:true, experimental_api:true, ..Default::default() };
    for params in [json!(null), json!({}), json!({"environmentId":"e","limit":1.5}), json!({"environmentId":"e","order":"invalid"})] {
        let response = registry.dispatch(connection.clone(),json!({"id":1,"method":"remoteControl/client/list","params":params})).await;
        assert_eq!(response["error"]["code"],-32600);
    }
    let response = registry.dispatch(connection,json!({"id":2,"method":"remoteControl/client/list","params":{"environmentId":"e"}})).await;
    assert_eq!(response["error"]["code"],-32603);
    assert_eq!(response["error"]["message"],"remote control is unavailable for this app-server");
}

#[tokio::test]
async fn remote_status_persists_private_uuid_and_recovers_dead_owner_lock() {
    use maho_server::app_server::{models::register_remote_status_method,installation_id::ensure_installation_id};
    use std::os::unix::fs::PermissionsExt;
    let directory=tempfile::tempdir().unwrap();let path=directory.path().join("app-server/installation-id");
    let (first,second)=tokio::join!(ensure_installation_id(directory.path()),ensure_installation_id(directory.path()));
    let first=first.unwrap();assert_eq!(first,second.unwrap());assert_eq!(uuid::Uuid::parse_str(&first).unwrap().get_version_num(),4);
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,0o600);
    assert!(!path.with_file_name("installation-id.lock").exists());
    let mut registry=MethodRegistry::default();register_remote_status_method(&mut registry,directory.path().display().to_string());
    let response=registry.dispatch(RegistryConnection {initialized:true,experimental_api:true,..Default::default()},json!({"id":1,"method":"remoteControl/status/read"})).await;
    assert_eq!(response["result"],json!({"status":"disabled","serverName":"senpi app-server","installationId":first,"environmentId":null}));
    std::fs::remove_file(&path).unwrap();let lock=path.with_file_name("installation-id.lock");std::fs::create_dir(&lock).unwrap();
    std::fs::write(lock.join("owner-dead.json"),json!({"ownerToken":"dead","pid":2147483647u64,"createdAtMs":0}).to_string()).unwrap();
    assert_ne!(ensure_installation_id(directory.path()).await.unwrap(),first);assert!(!lock.exists());
}
