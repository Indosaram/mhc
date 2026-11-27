//! senpi `modes/rpc/compact-lane-transitions.ts`: maps canonical acceptance units to
//! `seeded_block` / `seeded_message` compact transitions (S03b-2b).

use maho_core::session_recovery_identity::RecoveryCarrier;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone)]
pub struct CanonicalUnit {
    pub record: Value,
    pub carrier: RecoveryCarrier,
    pub ordinal: i64,
    pub first_ordinal: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum LaneTransition {
    SeededBlock {
        message_id: String,
        epoch: String,
        ordinal: i64,
        first_ordinal: i64,
        event: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_metadata: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        correlation: Option<Map<String, Value>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resolved_tool_name: Option<String>,
    },
    SeededMessage {
        message_id: String,
        epoch: String,
        ordinal: i64,
        first_ordinal: i64,
        record: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        correlation: Option<Map<String, Value>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        invalidated_blocks: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rewritten_from: Option<Value>,
    },
}

const CORRELATION: [&str; 3] = ["clientMessageId", "clientTurnId", "clientMessages"];

pub fn lane_transition(unit: &CanonicalUnit) -> Option<LaneTransition> {
    let record = &unit.record;
    let kind = record.get("type").and_then(Value::as_str)?;
    let mut corr_map = Map::new();
    for key in CORRELATION {
        if let Some(val) = record.get(key) {
            corr_map.insert(key.to_owned(), val.clone());
        }
    }
    let correlation = (!corr_map.is_empty()).then_some(corr_map);
    let invalidated_blocks = record.get("invalidatedBlocks").cloned();
    let rewritten_from = record.get("rewrittenFrom").cloned();

    match kind {
        "message_block_state" | "message_block_metadata" | "message_tool_arguments_reset" => {
            Some(LaneTransition::SeededBlock {
                message_id: unit.carrier.message_id.clone(),
                epoch: unit.carrier.epoch.clone(),
                ordinal: unit.ordinal,
                first_ordinal: unit.first_ordinal,
                event: record.clone(),
                usage: None,
                message_metadata: None,
                correlation: None,
                resolved_tool_name: None,
            })
        }
        "message_start" | "message_end" => Some(LaneTransition::SeededMessage {
            message_id: unit.carrier.message_id.clone(),
            epoch: unit.carrier.epoch.clone(),
            ordinal: unit.ordinal,
            first_ordinal: unit.first_ordinal,
            record: record.clone(),
            correlation,
            invalidated_blocks,
            rewritten_from,
        }),
        "message_update" => {
            let event = record.get("assistantMessageEvent")?.clone();
            if event.get("contentIndex").is_none() {
                return Some(LaneTransition::SeededMessage {
                    message_id: unit.carrier.message_id.clone(),
                    epoch: unit.carrier.epoch.clone(),
                    ordinal: unit.ordinal,
                    first_ordinal: unit.first_ordinal,
                    record: record.clone(),
                    correlation,
                    invalidated_blocks,
                    rewritten_from,
                });
            }
            Some(LaneTransition::SeededBlock {
                message_id: unit.carrier.message_id.clone(),
                epoch: unit.carrier.epoch.clone(),
                ordinal: unit.ordinal,
                first_ordinal: unit.first_ordinal,
                event,
                usage: record.get("usage").cloned(),
                message_metadata: record.get("messageMetadata").cloned(),
                correlation,
                resolved_tool_name: record
                    .get("resolvedToolName")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn unit(record: Value) -> CanonicalUnit {
        CanonicalUnit {
            record,
            carrier: RecoveryCarrier {
                message_id: "m1".into(),
                epoch: "e1".into(),
                durable_entry_id: None,
            },
            ordinal: 4,
            first_ordinal: 1,
        }
    }

    #[test]
    fn maps_block_controls_and_indexed_updates_to_seeded_block() {
        let ctrl = lane_transition(&unit(
            json!({"type":"message_block_state","contentIndex":0}),
        ))
        .expect("ctrl");
        assert!(matches!(ctrl, LaneTransition::SeededBlock { .. }));

        let delta = lane_transition(&unit(json!({
            "type": "message_update",
            "usage": {"total": 10},
            "resolvedToolName": "bash",
            "assistantMessageEvent": {"type":"toolcall_delta","contentIndex":0,"delta":"x"}
        })))
        .expect("delta");
        match delta {
            LaneTransition::SeededBlock {
                resolved_tool_name,
                usage,
                ..
            } => {
                assert_eq!(resolved_tool_name.as_deref(), Some("bash"));
                assert!(usage.is_some());
            }
            _ => panic!("expected SeededBlock"),
        }
    }

    #[test]
    fn maps_message_start_and_non_indexed_updates_to_seeded_message() {
        let start = lane_transition(&unit(json!({
            "type": "message_start",
            "clientMessageId": "cm-1",
            "invalidatedBlocks": [0],
            "rewrittenFrom": 2,
        })))
        .expect("start");
        match start {
            LaneTransition::SeededMessage {
                correlation,
                invalidated_blocks,
                rewritten_from,
                ..
            } => {
                assert_eq!(correlation.expect("corr")["clientMessageId"], "cm-1");
                assert_eq!(invalidated_blocks, Some(json!([0])));
                assert_eq!(rewritten_from, Some(json!(2)));
            }
            _ => panic!("expected SeededMessage"),
        }
    }
}
