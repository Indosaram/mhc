use maho_server::app_server::connection::*;
use serde_json::json;
#[test]
fn initialize_validation_normalizes_only_accepted_capability_fields() {
    let params=parse_initialize_params(&json!({"clientInfo":{"name":"client","version":"1"},"capabilities":{"experimentalApi":true,"optOutNotificationMethods":["turn/started","turn/started"],"ignored":true}})).unwrap();
    assert_eq!(params["clientInfo"]["title"],json!(null));assert_eq!(params["capabilities"]["requestAttestation"],false);assert!(params["capabilities"].get("ignored").is_none());
    let mut connection=InitializedConnection::default();assert!(!connection.registry_connection().initialized);
    assert!(connection.initialize(&params,"2","Linux","test","x64"));assert!(connection.registry_connection().experimental_api);assert_eq!(connection.opt_out_notification_methods.len(),1);
    assert!(!connection.initialize(&json!({}),"3","Linux","test","x64"));assert_eq!(connection.state.unwrap()["clientInfo"]["name"],"client");
}
#[test]
fn initialize_rejects_invalid_null_booleans_and_array_entries() {
    for params in [json!(null),json!({"clientInfo":{"name":"","version":"1"}}),json!({"clientInfo":{"name":"c","version":""}}),json!({"clientInfo":{"name":"c","version":"1","title":4}}),json!({"clientInfo":{"name":"c","version":"1"},"capabilities":{"experimentalApi":null}}),json!({"clientInfo":{"name":"c","version":"1"},"capabilities":{"optOutNotificationMethods":[1]}})] {assert_eq!(parse_initialize_params(&params),None);}
    assert!(parse_initialize_params(&json!({"clientInfo":{"name":"c","version":"1"},"capabilities":null})).unwrap()["capabilities"].is_null());
}

#[test]
fn initialize_response_preserves_user_agent_and_maps_platform() {
    let mut connection = InitializedConnection::default();
    assert!(connection.initialize_response("/tmp/home", "linux").is_err());
    let params = parse_initialize_params(&json!({"clientInfo":{"name":"client","version":"1"}})).unwrap();
    connection.initialize(&params, "2", "Linux", "test", "x64");
    for (platform, family, os) in [("linux", "unix", "linux"), ("darwin", "unix", "macos"), ("win32", "windows", "windows"), ("freebsd", "unix", "linux")] {
        let response = connection.initialize_response("/tmp/home", platform).unwrap();
        assert_eq!(response, json!({"userAgent":"client/2 (Linux test; x64) senpi_app_server","codexHome":"/tmp/home","platformFamily":family,"platformOs":os}));
    }
}

#[test]
fn notification_delivery_obeys_initialization_catalog_opt_out_and_experimental_gate() {
    let mut connection = InitializedConnection::default();
    assert!(!connection.can_deliver_notification("turn/started"));
    let params = parse_initialize_params(&json!({"clientInfo":{"name":"c","version":"1"},"capabilities":{"optOutNotificationMethods":["turn/completed"]}})).unwrap();
    connection.initialize(&params, "1", "Linux", "test", "x64");
    assert!(connection.can_deliver_notification("turn/started"));
    assert!(connection.can_deliver_notification("account/providerAccounts/updated"));
    assert!(!connection.can_deliver_notification("turn/completed"));
    assert!(!connection.can_deliver_notification("thread/realtime/started"));
    assert!(!connection.can_deliver_notification("unknown"));
    let mut experimental = InitializedConnection::default();
    let params = parse_initialize_params(&json!({"clientInfo":{"name":"c","version":"1"},"capabilities":{"experimentalApi":true}})).unwrap();
    experimental.initialize(&params, "1", "Linux", "test", "x64");
    assert!(experimental.can_deliver_notification("thread/realtime/started"));
}
