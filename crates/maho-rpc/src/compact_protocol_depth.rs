//! senpi `modes/rpc/compact-protocol-depth.ts`: the one protocol depth limit of the compact
//! ingest boundary (senpi#4272 RULING-59 C).

use crate::compact_content_store::{CompactWriteError, CompactWriteReason};
use crate::compact_read_lease::CompactError;
use serde_json::Value;

/// Maximum container nesting accepted at the compact ingest boundary (`[[1]]` is 2 deep).
pub const COMPACT_PROTOCOL_MAX_DEPTH: usize = 1_024;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("compact write refused ({})", .reason.as_str())]
pub struct ProtocolDepthError {
    pub reason: CompactWriteReason,
    pub limit: Option<usize>,
}

impl ProtocolDepthError {
    pub const fn new(reason: CompactWriteReason, limit: Option<usize>) -> Self {
        Self { reason, limit }
    }
}

impl From<ProtocolDepthError> for CompactWriteError {
    fn from(err: ProtocolDepthError) -> Self {
        Self::new(err.reason)
    }
}

impl From<ProtocolDepthError> for CompactError {
    fn from(err: ProtocolDepthError) -> Self {
        Self::Write(err.into())
    }
}

enum Children<'a> {
    Array(std::slice::Iter<'a, Value>),
    Object(serde_json::map::Values<'a>),
}

impl<'a> Children<'a> {
    fn next(&mut self) -> Option<&'a Value> {
        match self {
            Self::Array(iter) => iter.next(),
            Self::Object(iter) => iter.next(),
        }
    }
}

fn push_container<'a>(
    child: &'a Value,
    stack: &mut Vec<Children<'a>>,
) -> Result<(), ProtocolDepthError> {
    let children = match child {
        Value::Array(items) => Children::Array(items.iter()),
        Value::Object(map) => Children::Object(map.values()),
        _ => return Ok(()),
    };
    if stack.len() == COMPACT_PROTOCOL_MAX_DEPTH {
        return Err(ProtocolDepthError::new(
            CompactWriteReason::UnsupportedValue,
            Some(COMPACT_PROTOCOL_MAX_DEPTH),
        ));
    }
    stack.push(children);
    Ok(())
}

/// Refuses `value` when it nests more than `COMPACT_PROTOCOL_MAX_DEPTH` containers (`[[1]]` is 2 deep),
/// walking it with an explicit stack bounded by that limit, so the check itself never recurses.
pub fn assert_protocol_depth(value: &Value) -> Result<(), ProtocolDepthError> {
    let mut stack: Vec<Children<'_>> = Vec::new();
    push_container(value, &mut stack)?;
    while let Some(top) = stack.last_mut() {
        if let Some(child) = top.next() {
            push_container(child, &mut stack)?;
        } else {
            stack.pop();
        }
    }
    Ok(())
}

pub fn protocol_clone(record: &Value) -> Result<Value, ProtocolDepthError> {
    assert_protocol_depth(record)?;
    Ok(record.clone())
}

/// The limit a depth refusal carries; `None` for every other failure, which ends the epoch's compact state.
pub fn protocol_depth_limit_of(cause: &ProtocolDepthError) -> Option<usize> {
    cause.limit
}

/// A worker's `canonical_unavailable` frame as the host lane's cause: its depth refusal is rebuilt typed.
pub fn canonical_loss(reason: &str, limit: Option<usize>) -> Result<String, ProtocolDepthError> {
    match limit {
        None => Ok(reason.to_owned()),
        Some(limit) => Err(ProtocolDepthError::new(
            CompactWriteReason::UnsupportedValue,
            Some(limit),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn nested_array(depth: usize) -> Value {
        let mut v = json!(1);
        for _ in 0..depth {
            v = Value::Array(vec![v]);
        }
        v
    }

    #[test]
    fn allows_up_to_max_depth_and_refuses_above() {
        assert!(assert_protocol_depth(&json!(1)).is_ok());
        assert!(assert_protocol_depth(&nested_array(COMPACT_PROTOCOL_MAX_DEPTH)).is_ok());
        let err = assert_protocol_depth(&nested_array(COMPACT_PROTOCOL_MAX_DEPTH + 1))
            .expect_err("over max depth");
        assert_eq!(err.reason, CompactWriteReason::UnsupportedValue);
        assert_eq!(
            protocol_depth_limit_of(&err),
            Some(COMPACT_PROTOCOL_MAX_DEPTH)
        );
    }

    #[test]
    fn protocol_clone_enforces_depth_limit() {
        let deep = nested_array(COMPACT_PROTOCOL_MAX_DEPTH + 1);
        assert!(protocol_clone(&deep).is_err());
        let ok = json!({"a": [1, {"b": true}]});
        assert_eq!(protocol_clone(&ok).expect("clone"), ok);
    }

    #[test]
    fn canonical_loss_rebuilds_typed_depth_refusal() {
        assert_eq!(canonical_loss("boom", None).expect("plain"), "boom");
        let err = canonical_loss("unsupported_value", Some(1024)).expect_err("typed");
        assert_eq!(protocol_depth_limit_of(&err), Some(1024));
    }
}
