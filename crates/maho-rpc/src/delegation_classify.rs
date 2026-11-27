//! Port of senpi `packages/coding-agent/src/modes/rpc/delegation-classify.ts`.
//!
//! The `classify_record` answer (`senpi#5632`): the class of one record, without running it, on
//! any connection.

use serde_json::{Map, Value};

use crate::delegation_wire::{DelegationResponse, request_id};
use crate::method_classes::{
    RPC_METHOD_CLASSES_VERSION, RpcMethodClassEntry, classify_rpc_method,
    lookup_method_class_entry, rpc_method_classes_digest_value,
};

pub const CLASSIFY_RECORD_COMMAND: &str = "classify_record";

pub fn classify_record(record: &Value) -> DelegationResponse {
    let id = request_id(record);
    let target = record
        .as_object()
        .and_then(|obj| obj.get("record"))
        .unwrap_or(&Value::Null);
    let method_class = classify_rpc_method(target);
    let target_type = target
        .as_object()
        .and_then(|obj| obj.get("type"))
        .and_then(Value::as_str);
    let entry = method_class
        .and(target_type)
        .and_then(lookup_method_class_entry);

    let mut data = Map::new();
    data.insert(
        "class".to_owned(),
        match method_class {
            Some(c) => Value::String(c.as_str().to_owned()),
            None => Value::Null,
        },
    );
    data.insert(
        "version".to_owned(),
        Value::Number(RPC_METHOD_CLASSES_VERSION.into()),
    );
    data.insert(
        "digest".to_owned(),
        Value::String(rpc_method_classes_digest_value().to_owned()),
    );
    match entry {
        None => {}
        Some(RpcMethodClassEntry::Static(_)) => {
            data.insert("static".to_owned(), Value::Bool(true));
        }
        Some(RpcMethodClassEntry::Dynamic { version, .. }) => {
            data.insert("static".to_owned(), Value::Bool(false));
            data.insert("rule".to_owned(), Value::Number(version.into()));
        }
    }
    DelegationResponse::success(id, CLASSIFY_RECORD_COMMAND, Value::Object(data))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn classifies_static_dynamic_and_unknown_records() {
        let static_resp = classify_record(&json!({
            "id": "c1",
            "type": "classify_record",
            "record": { "type": "get_state" }
        }));
        assert!(static_resp.success);
        assert_eq!(static_resp.command, CLASSIFY_RECORD_COMMAND);
        let data = static_resp.data.expect("data");
        assert_eq!(data["class"], "observe");
        assert_eq!(data["static"], true);
        assert!(data.get("rule").is_none());

        let dyn_resp = classify_record(&json!({
            "type": "classify_record",
            "record": { "type": "open_session", "env": {} }
        }));
        let dyn_data = dyn_resp.data.expect("data");
        assert_eq!(dyn_data["class"], "device_admin");
        assert_eq!(dyn_data["static"], false);
        assert_eq!(dyn_data["rule"], 2);

        let unknown = classify_record(&json!({
            "type": "classify_record",
            "record": { "type": "nonexistent" }
        }));
        let unknown_data = unknown.data.expect("data");
        assert_eq!(unknown_data["class"], Value::Null);
        assert!(unknown_data.get("static").is_none());
    }
}
