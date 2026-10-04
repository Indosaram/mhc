use serde::{Serialize,de::DeserializeOwned};
use serde_json::{Value,json};
fn roundtrip<T:Serialize+DeserializeOwned>(value:Value) {
    let decoded:T=serde_json::from_value(value.clone()).expect("valid protocol fixture");
    assert_eq!(serde_json::to_value(decoded).expect("serializable protocol value"),value);
}
#[test]
fn generated_tagged_union_maps_nullable_and_numeric_contracts_roundtrip() {
    use maho_server::app_server::protocol::generated::{self,v2};
    roundtrip::<generated::AgentMessageInputContent>(json!({"type":"encrypted_content","encrypted_content":"opaque"}));
    roundtrip::<generated::RequestId>(json!(1.5));roundtrip::<generated::RequestId>(json!("request"));
    roundtrip::<v2::ThreadGoalSetParams>(json!({"threadId":"thread","objective":null,"tokenBudget":2.5}));
    roundtrip::<v2::ThreadGoalSetParams>(json!({"threadId":"thread"}));
    roundtrip::<v2::AskForApproval>(json!({"granular":{"sandbox_approval":true,"rules":false,"skill_approval":true,"request_permissions":false,"mcp_elicitations":true}}));
    roundtrip::<v2::AskForApproval>(json!("on-request"));
    roundtrip::<v2::AppsConfig>(json!({"_default":null,"app":{"enabled":true,"approvals_reviewer":null,"destructive_enabled":null,"open_world_enabled":false,"default_tools_approval_mode":null,"default_tools_enabled":true,"tools":null}}));
    roundtrip::<v2::McpElicitationConstOption>(json!({"const":"choice","title":"Choice"}));
    roundtrip::<v2::TurnInterruptResponse>(json!({}));
    assert!(serde_json::from_value::<v2::TurnInterruptResponse>(json!({"extra":true})).is_err());
    assert!(serde_json::from_value::<generated::ImageDetail>(json!("invalid")).is_err());
}
#[test]
fn generated_request_union_preserves_method_specific_payload_and_rejects_unknown() {
    use maho_server::app_server::protocol::generated::ClientRequest;
    roundtrip::<ClientRequest>(json!({"method":"thread/goal/set","id":1,"params":{"threadId":"thread","objective":null,"tokenBudget":2.5}}));
    assert!(serde_json::from_value::<ClientRequest>(json!({"method":"unknown","id":1,"params":{}})).is_err());
    assert!(serde_json::from_value::<ClientRequest>(json!({"method":"thread/goal/set","id":1,"params":{}})).is_err());
}

#[test]
fn generated_optional_json_preserves_absence_null_and_values() {
    use maho_server::app_server::protocol::generated::{Resource,ResourceContent,ResourceTemplate,Tool,v2};
    for extra in [json!({}),json!({"annotations":null}),json!({"annotations":{"audience":["user"]}})] {
        let mut resource=json!({"name":"resource","uri":"test://resource"});
        resource.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        roundtrip::<Resource>(resource);
        let mut template=json!({"name":"resource","uriTemplate":"test://{id}"});
        template.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        roundtrip::<ResourceTemplate>(template);
    }
    roundtrip::<ResourceContent>(json!({"uri":"test://resource","text":"text","_meta":null}));
    roundtrip::<Tool>(json!({"name":"tool","inputSchema":{},"outputSchema":null,"annotations":null,"_meta":null}));
    roundtrip::<v2::McpServerToolCallParams>(json!({"threadId":"thread","server":"server","tool":"tool","arguments":null,"_meta":null}));
    roundtrip::<v2::McpServerToolCallResponse>(json!({"content":[],"structuredContent":null,"_meta":null}));
}

#[test]
fn required_nullable_fields_accept_null_but_reject_missing_keys() {
    use maho_server::app_server::protocol::generated::{ClientInfo,v2};
    roundtrip::<ClientInfo>(json!({"name":"client","title":null,"version":"1"}));
    assert!(serde_json::from_value::<ClientInfo>(json!({"name":"client","version":"1"})).is_err());
    roundtrip::<v2::GitInfo>(json!({"sha":null,"branch":null,"originUrl":null}));
    for key in ["sha","branch","originUrl"] {
        let mut value=json!({"sha":null,"branch":null,"originUrl":null});
        value.as_object_mut().unwrap().remove(key);
        assert!(serde_json::from_value::<v2::GitInfo>(value).is_err());
    }
}
