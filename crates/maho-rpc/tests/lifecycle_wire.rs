use maho_rpc::rpc_types::*;
use serde_json::json;

#[test]
fn host_lifecycle_wire_preserves_null_successor_and_optional_attribution() {
    for value in [json!({"type":"host_superseded","instanceId":"host","generation":2.0,"successor":null}),json!({"type":"host_stalled","driftMs":6000.0}),json!({"type":"host_memory_pressure","rssMb":5000.0,"sessions":2.0})] {
        let event:RpcHostLifecycleEvent=serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(event).unwrap(),value);
    }
}

#[test]
fn closed_reason_remains_open_to_future_members() {
    let value=json!({"type":"session_closed","sessionId":"route","reason":"future_reason"});
    let event:RpcSessionLifecycleEvent=serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(event).unwrap(),value);
}

#[test]
fn replacement_identity_does_not_use_the_routing_handle_field() {
    let event=RpcSessionLifecycleEvent::SessionReplaced{durable_session_id:"durable".into(),session_file:None,cwd:"/work".into(),session_name:None};
    assert_eq!(serde_json::to_value(event).unwrap(),json!({"type":"session_replaced","durableSessionId":"durable","cwd":"/work"}));
}

#[test]
fn queued_notice_correlates_without_a_response_id() {
    let value=json!({"type":"queued","for_request":"open","position":2.0,"in_flight":1.0});
    let event:RpcOpenQueuedEvent=serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(event).unwrap(),value);
}
