use maho_server::app_server::{account::register_account_methods, registry::{MethodRegistry, RegistryConnection}};
use serde_json::json;

#[tokio::test]
async fn account_methods_validate_inputs_and_preserve_correlated_errors() {
    let directory = tempfile::tempdir().unwrap();
    let mut registry = MethodRegistry::default();
    register_account_methods(&mut registry, directory.path().to_string_lossy().into_owned());
    let connection = RegistryConnection { initialized: true, ..Default::default() };
    let response = registry.dispatch(connection.clone(), json!({"id":1,"method":"account/read"})).await;
    assert_eq!(response, json!({"id":1,"result":{"account":null,"requiresOpenaiAuth":false}}));
    for (method, params) in [
        ("account/read", json!({"refreshToken":null})),
        ("account/providerAccounts/read", json!({"provider":""})),
        ("account/providerAccounts/pin", json!({"provider":"anthropic"})),
        ("account/providerAccounts/remove", json!({"provider":"anthropic","name":""})),
        ("account/rateLimits/read", json!(null)),
        ("account/usage/read", json!(null)),
    ] {
        let response = registry.dispatch(connection.clone(), json!({"id":"test","method":method,"params":params})).await;
        assert_eq!(response["id"], "test");
        assert_eq!(response["error"]["code"], -32600);
    }
    let response = registry.dispatch(connection, json!({"id":2,"method":"account/providerAccounts/read","params":{"provider":"claude-sdk-oauth"}})).await;
    assert_eq!(response["result"]["provider"], "anthropic-subscription");
}

#[tokio::test]
async fn account_read_uses_native_stored_credentials() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("auth.json");
    let mut storage = maho_core::auth_storage::AuthStorage::create(&path.to_string_lossy());
    storage.set("anthropic", Some(json!({"type":"api_key","key":"faux-test-only"}))).unwrap();
    let mut registry = MethodRegistry::default();
    register_account_methods(&mut registry, directory.path().to_string_lossy().into_owned());
    let response = registry.dispatch(RegistryConnection { initialized:true, ..Default::default() }, json!({"id":3,"method":"account/read","params":{"refreshToken":true}})).await;
    assert_eq!(response["result"], json!({"account":{"type":"apiKey"},"requiresOpenaiAuth":false}));
}
