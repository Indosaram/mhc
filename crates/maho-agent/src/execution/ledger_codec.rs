//! Port of senpi `packages/agent/src/execution/ledger-codec.ts`.

use crate::durable::store::StoreValue;

use super::ledger_types::{ExecutionEntry, ExecutionOutcome, ExecutionProgress, FrozenCall};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid durable execution ledger row")]
pub struct ExecutionLedgerCorruptError;

fn valid_frozen(value: &StoreValue) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    obj.get("name").and_then(StoreValue::as_str).is_some()
        && obj.get("digest").and_then(StoreValue::as_str).is_some()
        && obj.contains_key("args")
}

fn valid_outcome(value: &StoreValue) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    let Some(hooks_ran) = obj.get("hooksRan").and_then(StoreValue::as_bool) else {
        return false;
    };
    let Some(uncertain) = obj.get("uncertain").and_then(StoreValue::as_bool) else {
        return false;
    };
    let _ = hooks_ran;
    match obj.get("reason").and_then(StoreValue::as_str) {
        Some("result" | "blocked") => obj.contains_key("value") && !uncertain,
        Some("cancelled" | "lease_expired" | "hand_restarted" | "uncertain") => true,
        _ => false,
    }
}

fn valid_progress(value: &StoreValue) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    match obj.get("state").and_then(StoreValue::as_str) {
        Some("admitted" | "preparing" | "acked") => true,
        Some("prepared" | "prepared_awaiting_consent" | "released" | "dispatched") => {
            obj.get("call").is_some_and(valid_frozen)
        }
        Some("reconciling") => {
            obj.get("call").is_some_and(valid_frozen)
                && obj.get("until").and_then(StoreValue::as_i64).is_some()
        }
        Some("settled") => obj.get("outcome").is_some_and(valid_outcome),
        _ => false,
    }
}

fn valid_entry(value: &StoreValue) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    let has_strings = [
        "issuer",
        "commandId",
        "executor",
        "workspace",
        "scope",
        "requestDigest",
    ]
    .iter()
    .all(|k| obj.get(*k).and_then(StoreValue::as_str).is_some());
    let has_numbers = [
        "generation",
        "admittedAt",
        "leaseUntil",
        "initialLeaseUntil",
        "renewSeq",
    ]
    .iter()
    .all(|k| obj.get(*k).and_then(StoreValue::as_i64).is_some());
    let has_bools = ["reconcilable", "effectPossible", "bodyDispatched"]
        .iter()
        .all(|k| obj.get(*k).and_then(StoreValue::as_bool).is_some());
    has_strings && has_numbers && has_bools && obj.get("progress").is_some_and(valid_progress)
}

/// Stored JSON crosses its schema boundary once, on read. A corrupt row must never look like a missing execution.
pub fn decode_execution(value: &StoreValue) -> Result<ExecutionEntry, ExecutionLedgerCorruptError> {
    if !valid_entry(value) {
        return Err(ExecutionLedgerCorruptError);
    }
    let parsed: ExecutionEntry =
        serde_json::from_value(value.clone()).map_err(|_| ExecutionLedgerCorruptError)?;
    let _ = (
        size_of::<FrozenCall>(),
        size_of::<ExecutionOutcome>(),
        size_of::<ExecutionProgress>(),
    );
    Ok(parsed)
}
