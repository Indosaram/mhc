//! Inline-or-reference packing of a payload (`payload-wire.ts`).

use serde_json::Value;

use crate::payload_ref::{
    AbortSignal, PayloadRefError, PayloadRefErrorCode, PayloadRefFailure, PayloadRefReader,
    PayloadRefWriter, RefAcceptance,
};
use crate::protocol::{PayloadRef, TOOL_CHILD_LIMITS};

#[derive(Debug, Clone, PartialEq)]
pub enum PackedPayload {
    Inline(Value),
    Ref(PayloadRef),
}

/// Inline at or below `REF_THRESHOLD` JSON bytes; above it, by acknowledged reference. Same rule in both directions.
/// (An `undefined` payload is already the explicit `Value::Null` the peer's guard takes, senpi#3569 review.)
pub async fn pack_payload(
    value: &Value,
    writer: &PayloadRefWriter,
) -> Result<PackedPayload, PayloadRefFailure> {
    let json = serde_json::to_vec(value)?;
    if json.len() <= TOOL_CHILD_LIMITS.ref_threshold {
        return Ok(PackedPayload::Inline(value.clone()));
    }
    Ok(PackedPayload::Ref(writer.create(&json).await?))
}

pub async fn unpack_payload(
    payload_ref: &PayloadRef,
    reader: &PayloadRefReader,
    signal: Option<&AbortSignal>,
) -> Result<Value, PayloadRefFailure> {
    if let RefAcceptance::Refused(reason) = reader.accept(payload_ref, signal).await? {
        return Err(PayloadRefError::new(
            PayloadRefErrorCode::RefRejected,
            &format!("ref {} was refused ({})", payload_ref.ref_id, reason),
            Some(reason),
        )
        .into());
    }
    let bytes = reader.consume(&payload_ref.ref_id).await?;
    Ok(serde_json::from_slice(&bytes)?)
}
