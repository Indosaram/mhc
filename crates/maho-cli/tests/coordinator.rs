use maho_cli::experimental::coordinator::*;
#[test]
fn coordinator_messages_keep_machine_field_names() {
    let value = serde_json::json!({"type":"server_registered", "serverConnectionId":"id", "peers":["peer"]});
    let message: CoordinatorMessage = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(message).unwrap(), value);
    assert!(serde_json::from_value::<CoordinatorMessage>(serde_json::json!({"type":"invalid"})).is_err());
}
#[test]
fn routing_drops_unknown_targets_and_limits_broadcast_to_server() {
    let peers = vec!["peer".to_owned()];
    assert_eq!(routed_messages("peer", &serde_json::json!({"type":"send", "to":"server", "payload":1}), &peers, true).unwrap().len(), 1);
    assert!(routed_messages("peer", &serde_json::json!({"type":"send", "to":"missing"}), &peers, true).unwrap().is_empty());
    assert!(routed_messages("peer", &serde_json::json!({"type":"broadcast"}), &peers, true).is_err());
    assert_eq!(routed_messages("server", &serde_json::json!({"type":"broadcast", "payload":2}), &peers, true).unwrap().len(), 1);
}
