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

#[test]
fn thread_turn_model_and_config_facades_decode_runtime_wire_values() {
    use maho_server::app_server::protocol::{thread::*,turn::*,models::*,config::*};
    let start:ThreadStartParams=serde_json::from_value(json!({"model":"native","runtimeWorkspaceRoots":["/tmp"],"experimentalRawEvents":true})).unwrap();
    assert_eq!(start.overrides.model.as_deref(),Some("native"));assert_eq!(start.experimental_raw_events,Some(true));
    assert_eq!(serde_json::to_value(ThreadStartParams::default()).unwrap(),json!({}));
    let turn:TurnStartParams=serde_json::from_value(json!({"threadId":"thread","input":[],"responsesapiClientMetadata":{"source":"test"},"collaborationMode":{"mode":"plan","settings":{"model":"native","reasoning_effort":null,"developer_instructions":null}}})).unwrap();
    assert_eq!(turn.common.responsesapi_client_metadata.unwrap()["source"],"test");
    let config:ConfigReadParams=serde_json::from_value(json!({"includeLayers":true})).unwrap();assert_eq!(config.include_layers,Some(true));
    let model=maho_server::app_server::model_list::build_wire_model(&json!({"id":"native","provider":"faux","reasoning":false}),&[],None);
    let model:Model=serde_json::from_value(model).unwrap();assert_eq!(model.input_modalities,vec![InputModality::Text]);
    let turn=maho_server::app_server::turn_runtime::build_turn("turn","completed",0.0,Some(1.0),&[],None);
    let turn:Turn=serde_json::from_value(turn).unwrap();assert_eq!(turn.items_view,TurnItemsView::Full);
    assert_eq!(SENPI_COLLABORATION_MODE.settings.reasoning_effort.as_deref(),Some("off"));
}
