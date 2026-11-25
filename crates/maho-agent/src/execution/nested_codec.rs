//! Port of senpi `packages/agent/src/execution/nested-codec.ts`.

use crate::durable::store::StoreValue;

use super::nested_types::NestedRecord;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("corrupt nested admission record")]
pub struct NestedRecordCorruptError;

fn valid_string_list(value: Option<&StoreValue>) -> bool {
    value
        .and_then(StoreValue::as_array)
        .is_some_and(|arr| arr.iter().all(|e| e.as_str().is_some()))
}

fn valid_parent(value: Option<&StoreValue>) -> bool {
    let Some(obj) = value.and_then(StoreValue::as_object) else {
        return false;
    };
    let strings = ["executionId", "executor", "scope"]
        .iter()
        .all(|k| obj.get(*k).and_then(StoreValue::as_str).is_some());
    let numbers = ["generation", "admittedAt", "leaseUntil", "budget"]
        .iter()
        .all(|k| obj.get(*k).and_then(StoreValue::as_i64).is_some());
    let caps = obj.get("capabilities").and_then(StoreValue::as_object);
    strings
        && numbers
        && caps.is_some_and(|c| {
            valid_string_list(c.get("preparation")) && valid_string_list(c.get("body"))
        })
}

fn valid_child(value: &StoreValue) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    obj.get("executionId")
        .and_then(StoreValue::as_str)
        .is_some()
        && obj.get("target").and_then(StoreValue::as_str).is_some()
        && obj.get("cost").and_then(StoreValue::as_u64).is_some()
        && obj.get("origin").and_then(StoreValue::as_str) == Some("tool")
        && matches!(
            obj.get("activityId"),
            Some(StoreValue::Null) | Some(StoreValue::String(_))
        )
}

fn valid_activity(value: &StoreValue) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    let strings = ["activityId", "parent", "executor", "scope"]
        .iter()
        .all(|k| obj.get(*k).and_then(StoreValue::as_str).is_some());
    let numbers = ["budget", "expiresAt", "debited"]
        .iter()
        .all(|k| obj.get(*k).and_then(StoreValue::as_i64).is_some());
    strings
        && numbers
        && valid_string_list(obj.get("targets"))
        && obj.get("ended").and_then(StoreValue::as_bool).is_some()
}

/// Decodes the brain's nested record; anything else is a corrupt store, never a guess.
pub fn decode_nested_record(value: &StoreValue) -> Result<NestedRecord, NestedRecordCorruptError> {
    let Some(obj) = value.as_object() else {
        return Err(NestedRecordCorruptError);
    };
    let phase_ok = matches!(
        obj.get("phase").and_then(StoreValue::as_str),
        Some("preparation" | "body" | "settled")
    );
    let cancels_ok = obj
        .get("cancels")
        .and_then(StoreValue::as_object)
        .is_some_and(|m| {
            m.values()
                .all(|v| matches!(v.as_str(), Some("queued" | "delivered")))
        });
    let children_ok = obj
        .get("children")
        .and_then(StoreValue::as_object)
        .is_some_and(|m| m.values().all(valid_child));
    let activities_ok = obj
        .get("activities")
        .and_then(StoreValue::as_object)
        .is_some_and(|m| m.values().all(valid_activity));

    if valid_parent(obj.get("parent"))
        && phase_ok
        && obj.get("cancelled").and_then(StoreValue::as_bool).is_some()
        && obj.get("expiresAt").and_then(StoreValue::as_i64).is_some()
        && obj.get("debited").and_then(StoreValue::as_u64).is_some()
        && children_ok
        && activities_ok
        && cancels_ok
    {
        serde_json::from_value(value.clone()).map_err(|_| NestedRecordCorruptError)
    } else {
        Err(NestedRecordCorruptError)
    }
}
