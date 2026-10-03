use maho_server::app_server::protocol::{collaboration_mode::*,fuzzy_search::*};
use serde_json::json;
#[test]
fn optional_nonnull_facade_fields_reject_null_but_allow_omission() {
    use maho_server::app_server::protocol::{base::{InitializeCapabilities,UserInput},catalogs::{SkillInterface,SkillToolDependency,SkillsListParams},config::ConfigReadParams};
    fn reject_null<T:serde::de::DeserializeOwned>(base:serde_json::Value,keys:&[&str]) {
        assert!(serde_json::from_value::<T>(base.clone()).is_ok());
        for key in keys {let mut value=base.clone();value[*key]=serde_json::Value::Null;assert!(serde_json::from_value::<T>(value).is_err(),"null {key}");}
    }
    reject_null::<InitializeCapabilities>(json!({"experimentalApi":false,"requestAttestation":false}),&["mcpServerOpenaiFormElicitation"]);
    reject_null::<UserInput>(json!({"type":"text","text":"hello"}),&["text_elements"]);
    reject_null::<UserInput>(json!({"type":"image","url":"image"}),&["detail"]);
    reject_null::<SkillInterface>(json!({}),&["displayName","shortDescription","iconSmall","iconLarge","brandColor","defaultPrompt"]);
    reject_null::<SkillToolDependency>(json!({"type":"tool","value":"native"}),&["description","transport","command","url"]);
    reject_null::<SkillsListParams>(json!({}),&["cwds","forceReload"]);
    reject_null::<ConfigReadParams>(json!({}),&["includeLayers"]);
    let nullable:ConfigReadParams=serde_json::from_value(json!({"cwd":null})).unwrap();
    assert_eq!(nullable.cwd,Some(None));
}

#[test]
fn remaining_facade_nullable_keys_reject_omission_and_roundtrip_null() {
    use maho_server::app_server::protocol::{base::GitInfo,turn::Turn,account::RateLimitWindow,models::ModelUpgradeInfo,terminal::TurnError};
    fn check<T:serde::de::DeserializeOwned+serde::Serialize>(value:serde_json::Value,keys:&[&str]) {
        let decoded:T=serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(),value);
        for key in keys {
            let mut missing=value.clone();missing.as_object_mut().unwrap().remove(*key);
            assert!(serde_json::from_value::<T>(missing).is_err(),"missing {key}");
        }
    }
    check::<GitInfo>(json!({"sha":null,"branch":null,"originUrl":null}),&["sha","branch","originUrl"]);
    check::<Turn>(json!({"id":"turn","items":[],"itemsView":"full","status":"completed","error":null,"startedAt":null,"completedAt":null,"durationMs":null}),&["error","startedAt","completedAt","durationMs"]);
    check::<RateLimitWindow>(json!({"usedPercent":0.0,"windowDurationMins":null,"resetsAt":null}),&["windowDurationMins","resetsAt"]);
    check::<ModelUpgradeInfo>(json!({"model":"native","upgradeCopy":null,"modelLink":null,"migrationMarkdown":null}),&["upgradeCopy","modelLink","migrationMarkdown"]);
    check::<TurnError>(json!({"message":"failure","codexErrorInfo":null,"additionalDetails":null}),&["codexErrorInfo","additionalDetails"]);
    check::<CollaborationModeSettings>(json!({"model":"native","reasoning_effort":null,"developer_instructions":null}),&["reasoning_effort","developer_instructions"]);
    check::<FuzzyFileSearchParams>(json!({"query":"q","roots":[],"cancellationToken":null}),&["cancellationToken"]);
}

#[test]
fn facade_nullable_fields_require_keys() {
    use maho_server::app_server::protocol::{base::ClientInfo,config::Config};
    assert!(serde_json::from_value::<ClientInfo>(json!({"name":"client","version":"1"})).is_err());
    assert!(serde_json::from_value::<ClientInfo>(json!({"name":"client","version":"1","title":null})).is_ok());
    let value=json!({"model":null,"model_provider":null,"approval_policy":null,"sandbox_mode":null,"model_reasoning_effort":null});
    assert!(serde_json::from_value::<Config>(value.clone()).is_ok());
    for key in value.as_object().unwrap().keys() {
        let mut missing=value.clone();missing.as_object_mut().unwrap().remove(key);
        assert!(serde_json::from_value::<Config>(missing).is_err());
    }
}

#[test]
fn facade_numeric_ids_and_optional_json_preserve_wire_values() {
    use maho_server::app_server::protocol::{base::RequestId,catalogs::Tool};
    for value in [json!(1),json!(1.5)] {
        let id:RequestId=serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(id).unwrap(),value);
    }
    let value=json!({"name":"tool","inputSchema":{},"outputSchema":null,"annotations":null,"_meta":null});
    let tool:Tool=serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(tool).unwrap(),value);
}

#[test]
fn catalog_nullable_fields_reject_missing_keys_and_keep_null() {
    use maho_server::app_server::protocol::catalogs::{McpServerInfo,RemoteControlClient};
    let info=json!({"name":"server","version":"1","title":null,"description":null,"icons":null,"websiteUrl":null});
    let decoded:McpServerInfo=serde_json::from_value(info.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(),info);
    for key in ["title","description","icons","websiteUrl"] {
        let mut missing=info.clone();missing.as_object_mut().unwrap().remove(key);
        assert!(serde_json::from_value::<McpServerInfo>(missing).is_err());
    }
    let client=json!({"clientId":"client","displayName":null,"deviceType":null,"platform":null,"osVersion":null,"deviceModel":null,"appVersion":null,"lastSeenAt":null});
    let decoded:RemoteControlClient=serde_json::from_value(client.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(),client);
    for key in ["displayName","deviceType","platform","osVersion","deviceModel","appVersion","lastSeenAt"] {
        let mut missing=client.clone();missing.as_object_mut().unwrap().remove(key);
        assert!(serde_json::from_value::<RemoteControlClient>(missing).is_err());
    }
}

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

#[test]
fn facade_method_sets_reject_unknown_and_malformed_typed_notifications() {
    use maho_server::app_server::protocol::{requests::ServerRequest,notifications::ServerNotification,thread_parity::ThreadMetadataUpdateParams};
    assert!(serde_json::from_value::<ServerRequest>(json!({"method":"unknown","id":1})).is_err());
    assert!(serde_json::from_value::<ServerNotification>(json!({"method":"unknown"})).is_err());
    assert!(serde_json::from_value::<ServerNotification>(json!({"method":"thread/unarchived","params":{}})).is_err());
    let notification:ServerNotification=serde_json::from_value(json!({"method":"thread/started","params":{"thread":{}}})).unwrap();
    assert_eq!(serde_json::to_value(notification).unwrap(),json!({"method":"thread/started","params":{"thread":{}}}));
    let metadata:ThreadMetadataUpdateParams=serde_json::from_value(json!({"threadId":"thread","gitInfo":null})).unwrap();
    assert_eq!(serde_json::to_value(metadata).unwrap(),json!({"threadId":"thread","gitInfo":null}));
}
