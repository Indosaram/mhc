//! Port of senpi `packages/agent/src/execution/coordinator-codec.ts`.

use crate::durable::store::StoreValue;

use super::coordinator_types::BatchRecord;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid durable batch record")]
pub struct BatchRecordCorruptError;

fn valid_terminal(value: &StoreValue) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    if obj.get("hooksRan").and_then(StoreValue::as_bool).is_none()
        || obj.get("uncertain").and_then(StoreValue::as_bool).is_none()
    {
        return false;
    }
    match obj.get("reason").and_then(StoreValue::as_str) {
        Some("result" | "blocked") => obj.contains_key("value"),
        Some("cancelled" | "lease_expired" | "hand_restarted" | "uncertain") => true,
        _ => false,
    }
}

fn valid_manifest_call(value: &StoreValue) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    let strings = ["executionId", "executor", "name", "requestDigest"]
        .iter()
        .all(|k| obj.get(*k).and_then(StoreValue::as_str).is_some());
    let numbers = ["sourceIndex", "wave"]
        .iter()
        .all(|k| obj.get(*k).and_then(StoreValue::as_u64).is_some());
    strings
        && numbers
        && obj
            .get("sequential")
            .and_then(StoreValue::as_bool)
            .is_some()
        && obj.contains_key("args")
        && obj
            .get("dependsOn")
            .and_then(StoreValue::as_array)
            .is_some_and(|arr| arr.iter().all(|v| v.as_u64().is_some()))
}

fn valid_manifest(value: &StoreValue) -> Option<usize> {
    let obj = value.as_object()?;
    let strings = ["batchId", "scope", "workspace", "issuer", "manifestDigest"]
        .iter()
        .all(|k| obj.get(*k).and_then(StoreValue::as_str).is_some());
    let numbers = ["generation", "admittedAt", "leaseUntil"]
        .iter()
        .all(|k| obj.get(*k).and_then(StoreValue::as_i64).is_some());
    let calls = obj.get("calls").and_then(StoreValue::as_array)?;
    if strings && numbers && calls.iter().all(valid_manifest_call) {
        Some(calls.len())
    } else {
        None
    }
}

fn valid_prepare_reply(value: &StoreValue) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    match obj.get("kind").and_then(StoreValue::as_str) {
        Some("prepared" | "prepared_awaiting_consent") => obj
            .get("frozenDigest")
            .and_then(StoreValue::as_str)
            .is_some(),
        Some("blocked") => obj.contains_key("result"),
        Some("batch_abort") => true,
        Some("settled") => obj.get("outcome").is_some_and(valid_terminal),
        _ => false,
    }
}

fn valid_call_outcome(value: &StoreValue) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    match obj.get("kind").and_then(StoreValue::as_str) {
        Some("result") => obj.contains_key("value"),
        Some("blocked") => obj.contains_key("result"),
        Some("not_run") => matches!(
            obj.get("cause").and_then(StoreValue::as_str),
            Some("batch_abort" | "interrupted_preparation")
        ),
        Some("interrupted") => {
            obj.get("reason").and_then(StoreValue::as_str).is_some()
                && obj.get("uncertain").and_then(StoreValue::as_bool).is_some()
        }
        _ => false,
    }
}

fn valid_consent_record(value: &StoreValue) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    matches!(
        obj.get("gate").and_then(StoreValue::as_str),
        Some("raw" | "frozen")
    ) && obj.get("digest").and_then(StoreValue::as_str).is_some()
        && matches!(
            obj.get("answer").and_then(StoreValue::as_str),
            Some("approved" | "denied" | "expired")
        )
}

fn valid_record(value: &StoreValue) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    let Some(size) = obj.get("manifest").and_then(valid_manifest) else {
        return false;
    };
    let consents_ok = match obj.get("consents") {
        None | Some(StoreValue::Null) => true,
        Some(val) => val.as_array().is_some_and(|arr| {
            arr.len() == size && arr.iter().all(|e| e.is_null() || valid_consent_record(e))
        }),
    };
    let decision_ok = match obj.get("decision") {
        Some(StoreValue::Null) => true,
        Some(StoreValue::String(s)) if s == "RELEASE" || s == "ABORT" => true,
        _ => false,
    };
    let prepares_ok = obj
        .get("prepares")
        .and_then(StoreValue::as_array)
        .is_some_and(|arr| {
            arr.len() == size && arr.iter().all(|e| e.is_null() || valid_prepare_reply(e))
        });
    let released_ok = obj
        .get("released")
        .and_then(StoreValue::as_array)
        .is_some_and(|arr| arr.len() == size && arr.iter().all(|e| e.as_bool().is_some()));
    let outcomes_ok = obj
        .get("outcomes")
        .and_then(StoreValue::as_array)
        .is_some_and(|arr| {
            arr.len() == size && arr.iter().all(|e| e.is_null() || valid_call_outcome(e))
        });
    consents_ok && decision_ok && prepares_ok && released_ok && outcomes_ok
}

/// Stored JSON crosses its schema boundary once, on read. A corrupt record must never look like a missing batch.
pub fn decode_batch_record(value: &StoreValue) -> Result<BatchRecord, BatchRecordCorruptError> {
    if !valid_record(value) {
        return Err(BatchRecordCorruptError);
    }
    serde_json::from_value(value.clone()).map_err(|_| BatchRecordCorruptError)
}
