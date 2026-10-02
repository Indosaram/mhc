use maho_cli::experimental::mini::shared::protocol::*;
#[test]
fn model_ref_wire_fields_match_upstream() {
    let model = ModelRef { provider: "provider".to_owned(), model_id: "model".to_owned() };
    assert_eq!(serde_json::to_value(model).unwrap(), serde_json::json!({"provider":"provider", "modelId":"model"}));
    assert_eq!(LANE.name, "lane"); assert_eq!(SESSIONS.name, "sessions");
}
#[test]
fn absent_account_fields_are_not_serialized() {
    let account = ProviderAccount { id: "id".to_owned(), name: "name".to_owned(), auth_type: AuthType::ApiKey, configured: false, source: None, interactive: true, method_name: None };
    let value = serde_json::to_value(&account).unwrap();
    assert_eq!(value["authType"], "api_key"); assert!(value.get("source").is_none());
    assert_eq!(serde_json::from_value::<ProviderAccount>(value).unwrap(), account);
}
