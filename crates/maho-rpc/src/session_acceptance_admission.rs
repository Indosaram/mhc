//! senpi `modes/rpc/session-acceptance-admission.ts`: sequenced record classification,
//! pre-commit call-fold admission (RULING-13 Q9), and outer client correlation transitions (C1.1).

use crate::client_message_events::correlated_record;
use crate::compact_current_components::COMPONENT_RECORDS;
use crate::compact_lane_current::{CALL_RECORDS, CallFold, CompactLaneCurrent, is_call_record};
use serde_json::Value;

/// Core sequenced record types (besides `COMPONENT_RECORDS` and `CALL_RECORDS`).
pub const SEQUENCED_CORE: [&str; 13] = [
    "agent_start",
    "agent_end",
    "turn_start",
    "turn_end",
    "message_start",
    "message_update",
    "message_end",
    "tool_execution_start",
    "tool_execution_update",
    "tool_execution_end",
    "entry_appended",
    "session_abort",
    "transcript_write_failed",
];

/// Call-only records (from `compact-call-model.ts` `CALL_ONLY_RECORDS`).
pub const CALL_ONLY_RECORDS: [&str; 2] = ["hook_execution_start", "hook_execution_end"];

/// Recovery kinds take a sequence; responses and every other record keep FIFO order with none.
pub fn is_sequenced(record_type: &str) -> bool {
    SEQUENCED_CORE.contains(&record_type)
        || COMPONENT_RECORDS.contains(&record_type)
        || CALL_RECORDS.contains(&record_type)
}

/// True when `record_type` only updates the calls component and takes no sequence if ignored.
pub fn is_call_only_record(record_type: &str) -> bool {
    CALL_ONLY_RECORDS.contains(&record_type)
}

/// Where the lane stands, or where an admitted record lands: its sequence and the turn it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanePlace {
    pub sequence: u64,
    pub turn_id: Option<String>,
}

/// An admitted record's place and what its call fold produced, if it is a call record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedRecord {
    pub sequence: u64,
    pub turn_id: Option<String>,
    pub call: Option<CallFold>,
}

/// Outer client correlation fields extracted from an admitted record.
#[derive(Debug, Clone, PartialEq)]
pub struct CorrelationValues {
    pub client_message_id: Option<Value>,
    pub client_turn_id: Option<Value>,
    pub client_messages: Option<Value>,
}

/// The outer client correlation standing in the lane, and the sequence its transition took (C1.1).
#[derive(Debug, Clone, PartialEq)]
pub struct AcceptedCorrelation {
    pub sequence: u64,
    pub client_message_id: Option<Value>,
    pub client_turn_id: Option<Value>,
    pub client_messages: Option<Value>,
}

pub fn field<'a>(record: &'a Value, key: &str) -> Option<&'a Value> {
    record.as_object().and_then(|map| map.get(key))
}

fn turn_scoped(record_type: &str, record: &Value) -> bool {
    if record_type == "turn_end" || record_type.starts_with("tool_execution_") {
        return true;
    }
    if !record_type.starts_with("message_") {
        return false;
    }
    let role = field(record, "message")
        .and_then(|msg| field(msg, "role"))
        .and_then(Value::as_str);
    matches!(role, Some("assistant" | "toolResult"))
}

/// The calls component folds first, at the sequence and turn this record would take: an ignored
/// call-only record changes no component, takes no sequence, mints no turn, commits no correlation
/// and publishes no root (RULING-13 Q9), so the lane commits nothing before it decides.
/// Returns `None` when the record is ignored. `folds = false` indicates the record was refused at
/// the protocol depth limit and folds no call.
pub fn admit_record(
    current: &mut CompactLaneCurrent,
    record: &Value,
    record_type: &str,
    at: &LanePlace,
    folds: bool,
    call_key: Option<&str>,
) -> Option<AdmittedRecord> {
    let mints_turn =
        record_type == "turn_start" || (at.turn_id.is_none() && turn_scoped(record_type, record));
    let turn_id = if mints_turn {
        Some(uuid::Uuid::new_v4().to_string())
    } else {
        at.turn_id.clone()
    };
    let next = at.sequence + 1;
    let call = if folds && is_call_record(record_type) {
        current.fold_call(record, next, turn_id.as_deref(), call_key)
    } else {
        None
    };
    if let Some(ref fold) = call
        && !fold.changed
        && is_call_only_record(record_type)
    {
        return None;
    }
    Some(AdmittedRecord {
        sequence: next,
        turn_id,
        call,
    })
}

fn same_client_messages(current: Option<&Value>, next: Option<&Value>) -> bool {
    match (current, next) {
        (None, None) => true,
        (Some(Value::Null), Some(Value::Null)) => true,
        (Some(Value::Array(curr_arr)), Some(Value::Array(next_arr))) => {
            if curr_arr.len() != next_arr.len() {
                return false;
            }
            curr_arr.iter().zip(next_arr.iter()).all(|(a, b)| {
                field(a, "clientMessageId") == field(b, "clientMessageId")
                    && field(a, "clientTurnId") == field(b, "clientTurnId")
            })
        }
        (Some(a), Some(b)) if !a.is_object() && !a.is_array() => a == b,
        _ => false,
    }
}

/// A change of the outer client correlation is its own transition, sequenced just before its
/// record (C1.1).
pub fn correlation_change(
    standing: Option<&AcceptedCorrelation>,
    turn_id: Option<&str>,
    record: &Value,
) -> Option<CorrelationValues> {
    // Within a turn, any identity-less correlated record other than turn_start keeps the standing
    // correlation and gains no client identity of its own; only a new turn boundary ends it.
    let map = record.as_object()?;
    let identified = map.contains_key("clientMessageId") || map.contains_key("clientTurnId");
    if !identified && (standing.is_none() || !correlated_record(record)) {
        return None;
    }
    if !identified
        && turn_id.is_some()
        && field(record, "type").and_then(Value::as_str) != Some("turn_start")
    {
        return None;
    }
    let next = CorrelationValues {
        client_message_id: map.get("clientMessageId").cloned(),
        client_turn_id: map.get("clientTurnId").cloned(),
        client_messages: map.get("clientMessages").cloned(),
    };
    if let Some(current) = standing
        && current.client_message_id == next.client_message_id
        && current.client_turn_id == next.client_turn_id
        && same_client_messages(
            current.client_messages.as_ref(),
            next.client_messages.as_ref(),
        )
    {
        return None;
    }
    Some(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sequenced_includes_core_components_and_calls() {
        assert!(is_sequenced("agent_start"));
        assert!(is_sequenced("message_end"));
        assert!(is_sequenced("question_updated"));
        assert!(is_sequenced("hook_execution_start"));
        assert!(!is_sequenced("response"));
        assert!(is_call_only_record("hook_execution_start"));
        assert!(!is_call_only_record("tool_execution_start"));
    }

    #[test]
    fn admit_mints_turn_for_turn_start_and_turn_scoped_records() {
        let mut current = CompactLaneCurrent::new();
        let place = LanePlace {
            sequence: 5,
            turn_id: None,
        };
        let assistant_msg = json!({"type": "message_start", "message": {"role": "assistant"}});
        let admitted = admit_record(
            &mut current,
            &assistant_msg,
            "message_start",
            &place,
            true,
            None,
        )
        .expect("admitted");
        assert_eq!(admitted.sequence, 6);
        assert!(admitted.turn_id.is_some());

        let user_msg = json!({"type": "message_start", "message": {"role": "user"}});
        let admitted_user =
            admit_record(&mut current, &user_msg, "message_start", &place, true, None)
                .expect("admitted");
        assert_eq!(admitted_user.sequence, 6);
        assert!(admitted_user.turn_id.is_none());
    }

    #[test]
    fn correlation_change_tracks_standing_and_turn_boundaries() {
        let r1 = json!({"type": "message_start", "clientMessageId": "m1", "clientTurnId": "t1"});
        let c1 = correlation_change(None, None, &r1).expect("initial correlation");
        assert_eq!(c1.client_message_id, Some(json!("m1")));

        let standing = AcceptedCorrelation {
            sequence: 1,
            client_message_id: c1.client_message_id,
            client_turn_id: c1.client_turn_id,
            client_messages: c1.client_messages,
        };

        // Same correlation suppresses emission.
        assert!(correlation_change(Some(&standing), Some("turn-1"), &r1).is_none());

        // Mid-turn identity-less correlated record keeps standing correlation.
        let mid_turn = json!({"type": "message_update"});
        assert!(correlation_change(Some(&standing), Some("turn-1"), &mid_turn).is_none());

        // New turn_start without correlation clears standing correlation.
        let new_turn = json!({"type": "turn_start"});
        let cleared = correlation_change(Some(&standing), Some("turn-1"), &new_turn)
            .expect("turn boundary clears correlation");
        assert_eq!(cleared.client_message_id, None);
    }
}
