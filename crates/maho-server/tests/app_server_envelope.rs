use maho_server::app_server::envelope::{
    ClassifiedIncoming as Incoming, classify_incoming, populate_outbound_notification,
};
use serde_json::json;
#[test]
fn preserves_request_params_and_drops_unrelated_fields() {
    assert_eq!(
        classify_incoming(json!({"id":1.5,"method":"test","params":null,"extra":true})),
        Incoming::Request(json!({"id":1.5,"method":"test","params":null}))
    );
}
#[test]
fn result_precedes_error_and_accepts_null_response_id() {
    assert_eq!(
        classify_incoming(json!({"id":null,"result":false,"error":{"code":-1,"message":"error"}})),
        Incoming::Response(json!({"id":null,"result":false}))
    );
}
#[test]
fn parses_error_data_without_extra_fields() {
    assert_eq!(
        classify_incoming(
            json!({"id":"x","error":{"code":-1,"message":"error","data":null,"other":true}})
        ),
        Incoming::Response(json!({"id":"x","error":{"code":-1,"message":"error","data":null}}))
    );
}
#[test]
fn rejects_invalid_boundaries() {
    for value in [
        json!(null),
        json!([]),
        json!({"id":null,"method":"x"}),
        json!({"id":true,"result":1}),
        json!({"id":1,"error":{"code":"bad","message":"x"}}),
    ] {
        assert_eq!(
            classify_incoming(value.clone()),
            Incoming::ProtocolInvalid(value)
        );
    }
}
#[test]
fn strips_incoming_notification_timestamp() {
    assert_eq!(
        classify_incoming(json!({"method":"x","params":[],"emittedAtMs":2})),
        Incoming::Notification(json!({"method":"x","params":[]}))
    );
}
#[test]
fn only_populates_outbound_notifications() {
    assert_eq!(
        populate_outbound_notification(json!({"method":"x"}), 3),
        json!({"method":"x","emittedAtMs":3})
    );
    assert_eq!(
        populate_outbound_notification(json!({"method":"x","emittedAtMs":0}), 3),
        json!({"method":"x","emittedAtMs":0})
    );
    assert_eq!(
        populate_outbound_notification(json!({"method":"x","id":1}), 3),
        json!({"method":"x","id":1})
    );
}
