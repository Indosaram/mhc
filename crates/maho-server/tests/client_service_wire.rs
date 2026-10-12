use maho_server::client::service_wire::{
    ServiceControlCall, create_service_catalogue_call, create_service_subscribe_call,
    create_service_unsubscribe_call, decode_service_control_call, parse_service_provider_update,
    parse_service_subscription_snapshot, parse_wire_service_provider_update,
    parse_wire_service_subscription_snapshot,
};
use serde_json::json;

#[test]
fn control_calls_round_trip_and_reject_malformed_shapes() {
    assert_eq!(decode_service_control_call(&create_service_catalogue_call()), Some(ServiceControlCall::Catalogue));
    assert_eq!(decode_service_control_call(&create_service_subscribe_call("s1", "svc", "keyed")), Some(ServiceControlCall::Subscribe { subscription_id: "s1".into(), service_id: "svc".into(), mode: "keyed".into() }));
    assert_eq!(decode_service_control_call(&create_service_unsubscribe_call("s1")), Some(ServiceControlCall::Unsubscribe { subscription_id: "s1".into() }));
    for call in [
        json!({"serviceId":"$chord.service","member":"subscribe","args":["s1","svc","invalid"]}),
        json!({"serviceId":"$chord.service","member":"subscribe","args":["s1","svc"]}),
        json!({"serviceId":"$chord.service","member":"catalogue","args":["extra"]}),
        json!({"serviceId":"other","member":"catalogue","args":[]}),
        json!({"serviceId":"$chord.service","member":"catalogue","args":[],"instance":{"key":"k","generation":1}}),
        json!({"serviceId":"$chord.service","member":"unsubscribe","args":[""]}),
    ] {
        assert_eq!(decode_service_control_call(&call), None, "{call}");
    }
}

#[test]
fn subscription_validators_distinguish_decoded_and_wire_ops() {
    let decoded = json!({"serviceId":"echo","mode":"singleton","instances":[{"members":[{"name":"state","kind":"state","sequence":0,"ops":[["r",{"n":1}],["s",["n"],2]]}]}]});
    assert!(parse_service_subscription_snapshot(&decoded).is_ok());
    assert!(parse_wire_service_subscription_snapshot(&decoded).is_ok());

    let wire = json!({"serviceId":"echo","mode":"singleton","instances":[{"members":[{"name":"state","kind":"state","sequence":0,"ops":[["r",{"n":1}],["#",0,["n"]],["s",0,2]]}]}]});
    assert!(parse_wire_service_subscription_snapshot(&wire).is_ok());
    assert!(parse_service_subscription_snapshot(&wire).is_err());

    let malformed = json!({"serviceId":"echo","mode":"singleton","instances":[{"members":[{"name":"state","kind":"state","sequence":0,"ops":[["s"]]}]}]});
    assert!(parse_service_subscription_snapshot(&malformed).is_err());
    assert!(parse_wire_service_subscription_snapshot(&malformed).is_err());
}

#[test]
fn provider_updates_validate_decoded_and_wire_ops() {
    let decoded = json!({"type":"state","member":"state","sequence":1,"ops":[["s",["n"],2]]});
    assert!(parse_service_provider_update(&decoded).is_ok());
    assert!(parse_wire_service_provider_update(&decoded).is_ok());

    let wire = json!({"type":"state","member":"state","sequence":1,"ops":[["#",0,["n"]],["s",0,2]]});
    assert!(parse_wire_service_provider_update(&wire).is_ok());
    assert!(parse_service_provider_update(&wire).is_err());

    assert!(parse_service_provider_update(&json!({"type":"state","member":"state","sequence":0,"ops":[]})).is_err());
    assert!(parse_wire_service_provider_update(&json!({"type":"closed","instance":{"key":"k","generation":1}})).is_ok());
    assert!(parse_service_provider_update(&json!({"type":"closed","instance":{"key":"k","generation":0}})).is_err());
    assert!(parse_wire_service_provider_update(&json!({"type":"unknown"})).is_err());
}
