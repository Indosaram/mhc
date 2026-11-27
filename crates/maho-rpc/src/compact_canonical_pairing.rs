//! senpi `modes/rpc/compact-canonical-pairing.ts`: joins the two projections of one session
//! event into one acceptance unit (RULING-26 Q2), enforcing `COMPACT_PROTOCOL_MAX_DEPTH`.

use crate::compact_content_store::CompactWriteReason;
use crate::compact_read_lease::CompactError;
use maho_core::session_recovery_identity::RecoveryCarrier;
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};

/// The one protocol depth limit of the compact ingest boundary (senpi#4272 RULING-59 C).
pub const COMPACT_PROTOCOL_MAX_DEPTH: usize = 1_024;

const CONTROLS: [&str; 3] = [
    "message_block_state",
    "message_block_metadata",
    "message_tool_arguments_reset",
];

pub type CanonicalRecord = Value;

#[derive(Debug, Clone, PartialEq)]
pub struct CanonicalControl {
    pub record: CanonicalRecord,
    pub carrier: RecoveryCarrier,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CanonicalUnit {
    pub record: CanonicalRecord,
    pub carrier: RecoveryCarrier,
    pub ordinal: i64,
    pub first_ordinal: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CarrierKey {
    epoch: String,
    message_id: String,
}

impl CarrierKey {
    fn from_carrier(carrier: &RecoveryCarrier) -> Self {
        Self {
            epoch: carrier.epoch.clone(),
            message_id: carrier.message_id.clone(),
        }
    }
}

/// Refuses `value` when it nests more than `COMPACT_PROTOCOL_MAX_DEPTH` containers (`[[1]]` is 2
/// deep), walking it with an explicit stack bounded by that limit.
pub fn assert_protocol_depth(value: &Value) -> Result<(), CompactError> {
    enum Iter<'a> {
        Array(std::slice::Iter<'a, Value>),
        Object(serde_json::map::Values<'a>),
    }
    let mut stack: Vec<Iter<'_>> = Vec::new();
    let mut current = Some(value);
    loop {
        if let Some(val) = current.take() {
            match val {
                Value::Array(arr) => {
                    if stack.len() == COMPACT_PROTOCOL_MAX_DEPTH {
                        return Err(CompactWriteReason::UnsupportedValue.into());
                    }
                    stack.push(Iter::Array(arr.iter()));
                }
                Value::Object(obj) => {
                    if stack.len() == COMPACT_PROTOCOL_MAX_DEPTH {
                        return Err(CompactWriteReason::UnsupportedValue.into());
                    }
                    stack.push(Iter::Object(obj.values()));
                }
                _ => {}
            }
            continue;
        }
        let Some(top) = stack.last_mut() else {
            return Ok(());
        };
        let next = match top {
            Iter::Array(iter) => iter.next(),
            Iter::Object(iter) => iter.next(),
        };
        match next {
            Some(child) => current = Some(child),
            None => {
                stack.pop();
            }
        }
    }
}

pub fn protocol_clone(record: &Value) -> Result<CanonicalRecord, CompactError> {
    assert_protocol_depth(record)?;
    Ok(record.clone())
}

/// Joins the two projections of one session event into one acceptance unit (RULING-26 Q2).
#[derive(Default)]
pub struct CanonicalPairing {
    pending: HashMap<CarrierKey, VecDeque<CanonicalRecord>>,
    refusals: HashSet<CarrierKey>,
}

impl CanonicalPairing {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn structured(
        &mut self,
        record: &Value,
        carrier: Option<&RecoveryCarrier>,
    ) -> Result<Option<CanonicalControl>, CompactError> {
        let Some(carrier) = carrier else {
            return Ok(None);
        };
        let key = CarrierKey::from_carrier(carrier);
        let canonical = match protocol_clone(record) {
            Ok(canonical) => canonical,
            Err(err) => {
                if matches!(
                    err,
                    CompactError::Write(ref w) if w.reason == CompactWriteReason::UnsupportedValue
                ) {
                    self.refusals.insert(key);
                }
                return Err(err);
            }
        };
        let kind = canonical.get("type").and_then(Value::as_str).unwrap_or("");
        if CONTROLS.contains(&kind) {
            return Ok(Some(CanonicalControl {
                record: canonical,
                carrier: carrier.clone(),
            }));
        }
        self.pending.entry(key).or_default().push_back(canonical);
        Ok(None)
    }

    pub fn take(&mut self, carrier: Option<&RecoveryCarrier>) -> Option<CanonicalRecord> {
        let carrier = carrier?;
        let key = CarrierKey::from_carrier(carrier);
        let queue = self.pending.get_mut(&key)?;
        let canonical = queue.pop_front();
        if queue.is_empty() {
            self.pending.remove(&key);
        }
        canonical
    }

    /// Whether the canonical record of `legacy`'s event was refused at the protocol depth limit
    /// (and reported).
    pub fn refused(&mut self, carrier: Option<&RecoveryCarrier>) -> bool {
        let Some(carrier) = carrier else {
            return false;
        };
        self.refusals.remove(&CarrierKey::from_carrier(carrier))
    }
}

/// Carries a paired canonical record (or its depth refusal) alongside a legacy record on a frame.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CarriedCanonical {
    pub canonical: Option<CanonicalRecord>,
    pub refused: bool,
}

pub fn frame_unit(
    ordinal: Option<i64>,
    first_ordinal: Option<i64>,
    recovery: Option<RecoveryCarrier>,
    canonical: Option<CanonicalRecord>,
) -> Option<CanonicalUnit> {
    Some(CanonicalUnit {
        record: canonical?,
        carrier: recovery?,
        ordinal: ordinal?,
        first_ordinal: first_ordinal?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn nest(depth: usize) -> Value {
        let mut v = json!("leaf");
        for _ in 0..depth {
            v = json!([v]);
        }
        v
    }

    #[test]
    fn accepts_max_depth_and_refuses_beyond_protocol_limit_recording_refusal_once() {
        assert!(assert_protocol_depth(&nest(COMPACT_PROTOCOL_MAX_DEPTH)).is_ok());
        assert!(assert_protocol_depth(&nest(COMPACT_PROTOCOL_MAX_DEPTH + 1)).is_err());

        let carrier = RecoveryCarrier {
            message_id: "m1".into(),
            epoch: "e1".into(),
            durable_entry_id: None,
        };
        let mut pairing = CanonicalPairing::new();
        assert!(
            pairing
                .structured(&nest(COMPACT_PROTOCOL_MAX_DEPTH + 1), Some(&carrier))
                .is_err()
        );
        assert!(pairing.refused(Some(&carrier)));
        assert!(!pairing.refused(Some(&carrier)));

        let control = pairing
            .structured(
                &json!({"type": "message_block_state", "phase": "insert"}),
                Some(&carrier),
            )
            .expect("control")
            .expect("immediate control");
        assert_eq!(control.carrier, carrier);

        assert!(
            pairing
                .structured(&json!({"type": "message_start"}), Some(&carrier))
                .expect("start")
                .is_none()
        );
        let taken = pairing.take(Some(&carrier)).expect("taken");
        assert_eq!(taken["type"], "message_start");
        assert!(pairing.take(Some(&carrier)).is_none());
    }
}
