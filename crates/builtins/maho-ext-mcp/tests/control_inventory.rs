use maho_ext_mcp::{control_inventory::*,service_types::*};
use serde_json::json;
#[test]
fn wire_auth_status_and_inventory_fields_match_the_rpc_contract() {
    assert_eq!(serde_json::to_value(McpWireAuthStatus::OAuth).unwrap(),json!("oAuth"));
    assert_eq!(serde_json::to_value(McpWireAuthStatus::NotLoggedIn).unwrap(),json!("notLoggedIn"));
    let changed=McpControlInventoryChanged {session_id:"session".into(),snapshot:McpWireStatusSnapshot::default()};
    let wire=serde_json::to_value(changed).unwrap();assert_eq!(wire,json!({"sessionId":"session","snapshot":{"servers":[]}}));assert!(is_mcp_control_inventory_changed(&wire));
    assert!(!is_mcp_control_inventory_changed(&json!({"sessionId":1,"snapshot":{"servers":[]}})));
    assert!(!is_mcp_control_inventory_changed(&json!({"sessionId":"s","snapshot":{"servers":{}}})));
}
#[test]
fn transient_oauth_errors_do_not_require_reauthentication() {
    use maho_ext_mcp::{auth::{oauth::OAuthRequestError,oauth_errors::{OAuthFlowError,OAuthFailureKind}},needs_auth::is_oauth_needs_auth_error};
    assert!(!is_oauth_needs_auth_error(&OAuthRequestError::Flow(Box::new(OAuthFlowError::new(OAuthFailureKind::Transient,"network")))));
    assert!(is_oauth_needs_auth_error(&OAuthRequestError::Flow(Box::new(OAuthFlowError::new(OAuthFailureKind::InvalidGrant,"rejected")))));
}
