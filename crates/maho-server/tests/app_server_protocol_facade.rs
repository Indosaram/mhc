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
    assert_eq!(start.overrides.model.as_ref().and_then(|model|model.as_deref()),Some("native"));assert_eq!(start.experimental_raw_events,Some(true));
    assert_eq!(serde_json::to_value(ThreadStartParams::default()).unwrap(),json!({}));
    let turn:TurnStartParams=serde_json::from_value(json!({"threadId":"thread","input":[],"responsesapiClientMetadata":{"source":"test"},"collaborationMode":{"mode":"plan","settings":{"model":"native","reasoning_effort":null,"developer_instructions":null}}})).unwrap();
    assert_eq!(turn.common.responsesapi_client_metadata.flatten().unwrap()["source"],"test");
    let config:ConfigReadParams=serde_json::from_value(json!({"includeLayers":true})).unwrap();assert_eq!(config.include_layers,Some(true));
    let model=maho_server::app_server::model_list::build_wire_model(&json!({"id":"native","provider":"faux","reasoning":false}),&[],None);
    let model:Model=serde_json::from_value(model).unwrap();assert_eq!(model.input_modalities,vec![InputModality::Text]);
    let turn=maho_server::app_server::turn_runtime::build_turn("turn","completed",0.0,Some(1.0),&[],None);
    let turn:Turn=serde_json::from_value(turn).unwrap();assert_eq!(turn.items_view,TurnItemsView::Full);
    assert_eq!(SENPI_COLLABORATION_MODE.settings.reasoning_effort.as_deref(),Some("off"));
}

#[test]
fn account_catalog_request_and_notification_facades_keep_typed_payloads() {
    use maho_server::app_server::protocol::{account::*,catalogs::*,requests::*,notifications::*,thread_parity::*};
    let account:Account=serde_json::from_value(json!({"type":"chatgpt","email":null,"planType":"enterprise_cbp_usage_based"})).unwrap();
    assert_eq!(account,Account::Chatgpt {email:None,plan_type:PlanType::EnterpriseCbpUsageBased});
    let request:ClientRequest=serde_json::from_value(json!({"method":"thread/goal/set","id":"request","params":{"threadId":"thread","status":"usageLimited","tokenBudget":2.5}})).unwrap();
    assert!(matches!(request,ClientRequest::ThreadGoalSet {params:ThreadGoalSetParams {status:Some(Some(ThreadGoalStatus::UsageLimited)),..},..}));
    let catalog:McpServerStatusListResponse=serde_json::from_value(json!({"data":[{"name":"server","serverInfo":null,"tools":{},"resources":[],"resourceTemplates":[],"authStatus":"oAuth"}],"nextCursor":null})).unwrap();
    assert_eq!(catalog.data[0].auth_status,McpAuthStatus::OAuth);
    let notification=serde_json::from_value::<PopulatedServerNotificationEnvelope>(json!({"method":"thread/unarchived","params":{"threadId":"thread"},"emittedAtMs":1.5})).unwrap();
    let ServerNotification::Typed(TypedServerNotification::Plan(plan))=notification.notification else {panic!("expected typed plan notification");};
    assert!(matches!(*plan,AppServerPlanNotification::ThreadUnarchived(_)));
    assert_eq!(notification.emitted_at_ms,1.5);
}

#[test]
fn nullable_protocol_fields_distinguish_absent_from_explicit_clear() {
    use maho_server::app_server::protocol::{thread::ThreadStartParams,models::ModelListParams,turn::TurnStartParams,ToolRequestUserInputParams};
    let value=json!({"model":null,"serviceTier":null});let params:ThreadStartParams=serde_json::from_value(value.clone()).unwrap();
    assert_eq!(params.overrides.model,Some(None));assert_eq!(serde_json::to_value(params).unwrap(),value);
    let params:ModelListParams=serde_json::from_value(json!({"cursor":null})).unwrap();assert_eq!(serde_json::to_value(params).unwrap(),json!({"cursor":null}));
    let params:TurnStartParams=serde_json::from_value(json!({"threadId":"thread","input":[],"outputSchema":null})).unwrap();assert_eq!(params.output_schema,Some(None));
    let params:ToolRequestUserInputParams=serde_json::from_value(json!({"threadId":"thread","turnId":"turn","itemId":"item","questions":[{"id":"q","header":"h","question":"question","isOther":true,"isSecret":false,"options":null}],"autoResolutionMs":null})).unwrap();
    assert!(params.questions[0].options.is_none());
}
