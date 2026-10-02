use maho_server::app_server::protocol::{collaboration_mode::*,fuzzy_search::*};
use serde_json::json;
#[test]
fn collaboration_wire_preserves_nullable_snake_case_settings() {
    assert_eq!(serde_json::to_value(build_senpi_collaboration_mode("model".into(),Some("off".into()))).unwrap(),json!({"mode":"default","settings":{"model":"model","reasoning_effort":"off","developer_instructions":null}}));
    assert_eq!(serde_json::to_value(build_senpi_collaboration_mode_preset("model".into())).unwrap(),json!({"name":"default","mode":null,"model":"model","reasoning_effort":null}));
}
#[test]
fn fuzzy_facade_preserves_mixed_case_fields_and_nullable_indices() {
    let result:FuzzyFileSearchResult = serde_json::from_value(json!({"root":"root","path":"path","match_type":"file","file_name":"name","score":2,"indices":null})).unwrap();
    assert_eq!(result.match_type,FuzzyFileSearchMatchType::File);assert!(result.indices.is_none());
    let request:FuzzyFileSearchParams = serde_json::from_value(json!({"query":"q","roots":[],"cancellationToken":"token"})).unwrap();
    assert_eq!(request.cancellation_token.as_deref(),Some("token"));
    assert_eq!(serde_json::to_value(FuzzyFileSearchSessionStopResponse::default()).unwrap(),json!({}));
}

#[test]
fn base_and_terminal_facades_preserve_discriminants_and_snake_case_input() {
    use maho_server::app_server::protocol::{base::*,terminal::*};
    let input:UserInput=serde_json::from_value(json!({"type":"text","text":"input","text_elements":[{"start":0}]})).unwrap();
    assert_eq!(serde_json::to_value(input).unwrap(),json!({"type":"text","text":"input","text_elements":[{"start":0}]}));
    let policy:SandboxPolicy=serde_json::from_value(json!({"type":"externalSandbox","networkAccess":"restricted"})).unwrap();
    assert_eq!(policy,SandboxPolicy::ExternalSandbox {network_access:NetworkAccess::Restricted});
    let error:CodexErrorInfo=serde_json::from_value(json!({"activeTurnNotSteerable":{"turnKind":"compact"}})).unwrap();
    assert_eq!(serde_json::to_value(error).unwrap(),json!({"activeTurnNotSteerable":{"turnKind":"compact"}}));
    assert_eq!(serde_json::to_value(TurnStatus::InProgress).unwrap(),"inProgress");
}
