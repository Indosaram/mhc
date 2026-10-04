//! Wire form of a harness event.
//!
//! Pinned senpi publishes `HarnessEvent` objects over the mini RPC and the presentation folds them
//! with `reduceLaneSnapshot`. The Rust `maho_agent::harness::events::HarnessEvent` carries the same
//! data as a Rust enum and, as of this lane, has no `Serialize`/`to_value`, so the mini consumer
//! completes the missing serialization here (contract S2 in
//! `.omo/evidence/residual-source/task-9-contracts.md`). Field names and the `type` discriminant
//! follow the pinned `runtime/reducer.ts` read set; this module is deleted when maho-agent exposes
//! `impl From<&HarnessEvent> for serde_json::Value`.

use maho_agent::harness::events::{
    HandlerErrorPayload, HarnessEvent, HarnessEventPayload, LaneQueuedItem, NavigationEndPayload,
    RunEndPayload,
};
use serde_json::{json, Map, Value};

fn value_of<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

fn insert_optional(object: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    if let Some(value) = value {
        if !value.is_null() {
            object.insert(key.to_owned(), value);
        }
    }
}

fn run_end(payload: &RunEndPayload) -> Value {
    let mut object = Map::new();
    object.insert("runId".into(), json!(payload.run_id));
    insert_optional(&mut object, "fromTipId", Some(json!(payload.from_tip_id)));
    insert_optional(&mut object, "tipId", Some(json!(payload.tip_id)));
    object.insert("endedAt".into(), json!(payload.ended_at));
    object.insert("status".into(), json!(payload.status));
    insert_optional(&mut object, "error", payload.error.as_ref().map(value_of));
    Value::Object(object)
}

fn navigation_end(payload: &NavigationEndPayload) -> Value {
    let mut object = Map::new();
    object.insert("runId".into(), json!(payload.run_id));
    insert_optional(&mut object, "fromTipId", Some(json!(payload.from_tip_id)));
    insert_optional(&mut object, "tipId", Some(json!(payload.tip_id)));
    object.insert("endedAt".into(), json!(payload.ended_at));
    object.insert("status".into(), json!(payload.status));
    insert_optional(&mut object, "error", payload.error.as_ref().map(value_of));
    Value::Object(object)
}

fn handler_error(payload: &HandlerErrorPayload) -> Value {
    let mut object = Map::new();
    object.insert("error".into(), json!(payload.error));
    insert_optional(&mut object, "stack", Some(json!(payload.stack)));
    object.insert("kind".into(), json!(payload.kind));
    insert_optional(&mut object, "hook", Some(json!(payload.hook)));
    insert_optional(&mut object, "event", Some(json!(payload.event)));
    Value::Object(object)
}

fn queued_item(item: &LaneQueuedItem) -> Value {
    let mut object = Map::new();
    object.insert("entryId".into(), json!(item.entry_id));
    object.insert("kind".into(), json!(item.kind));
    object.insert("type".into(), json!(item.item_type));
    insert_optional(&mut object, "message", item.message.as_ref().map(value_of));
    insert_optional(&mut object, "customType", Some(json!(item.custom_type)));
    insert_optional(&mut object, "data", item.data.clone());
    Value::Object(object)
}

pub fn harness_event_value(event: &HarnessEvent) -> Value {
    let mut object = Map::new();
    object.insert("type".into(), json!(event.payload.event_type()));
    insert_optional(&mut object, "lane", Some(json!(event.lane)));
    insert_optional(&mut object, "recovery", Some(json!(event.recovery)));
    match &event.payload {
        HarnessEventPayload::RunStart { run_id, started_at } => {
            object.insert("runId".into(), json!(run_id));
            object.insert("startedAt".into(), json!(started_at));
        }
        HarnessEventPayload::RunResume { run_id } => {
            object.insert("runId".into(), json!(run_id));
        }
        HarnessEventPayload::RunSuspend { run_id, deferred, poll } => {
            object.insert("runId".into(), json!(run_id));
            object.insert("deferred".into(), value_of(deferred));
            object.insert("poll".into(), json!(poll));
        }
        HarnessEventPayload::OperationAbort { operation_id, steer, follow_up } => {
            object.insert("operationId".into(), json!(operation_id));
            object.insert("steer".into(), value_of(steer));
            object.insert("followUp".into(), value_of(follow_up));
        }
        HarnessEventPayload::RunEnd(payload) => {
            if let Value::Object(fields) = run_end(payload) {
                object.extend(fields);
            }
        }
        HarnessEventPayload::Fault { code, message } => {
            object.insert("code".into(), json!(code));
            object.insert("message".into(), json!(message));
        }
        HarnessEventPayload::HandlerError(payload) => {
            if let Value::Object(fields) = handler_error(payload) {
                object.extend(fields);
            }
        }
        HarnessEventPayload::TurnStart { run_id, turn_id } => {
            object.insert("runId".into(), json!(run_id));
            object.insert("turnId".into(), json!(turn_id));
        }
        HarnessEventPayload::TurnEnd { run_id, turn_id, message, tool_results } => {
            object.insert("runId".into(), json!(run_id));
            object.insert("turnId".into(), json!(turn_id));
            object.insert("message".into(), value_of(message.as_ref()));
            object.insert("toolResults".into(), value_of(tool_results));
        }
        HarnessEventPayload::RetryScheduled { run_id, step, attempt, max_attempts, delay_ms, not_before, error_message } => {
            object.insert("runId".into(), json!(run_id));
            object.insert("step".into(), json!(step));
            object.insert("attempt".into(), json!(attempt));
            object.insert("maxAttempts".into(), json!(max_attempts));
            object.insert("delayMs".into(), json!(delay_ms));
            object.insert("notBefore".into(), json!(not_before));
            object.insert("errorMessage".into(), json!(error_message));
        }
        HarnessEventPayload::RetryStart { run_id, step, attempt } => {
            object.insert("runId".into(), json!(run_id));
            object.insert("step".into(), json!(step));
            object.insert("attempt".into(), json!(attempt));
        }
        HarnessEventPayload::RetryEnd { run_id, step, attempt, success, final_error } => {
            object.insert("runId".into(), json!(run_id));
            object.insert("step".into(), json!(step));
            object.insert("attempt".into(), json!(attempt));
            object.insert("success".into(), json!(success));
            insert_optional(&mut object, "finalError", Some(json!(final_error)));
        }
        HarnessEventPayload::MessageStart { run_id, message } => {
            insert_optional(&mut object, "runId", Some(json!(run_id)));
            object.insert("message".into(), value_of(message));
        }
        HarnessEventPayload::MessageUpdate { run_id, message, event: update, frame } => {
            object.insert("runId".into(), json!(run_id));
            object.insert("message".into(), value_of(message));
            object.insert("event".into(), value_of(update.as_ref()));
            insert_optional(&mut object, "frame", frame.as_ref().map(value_of));
        }
        HarnessEventPayload::MessageEnd { run_id, message, entry_id } => {
            insert_optional(&mut object, "runId", Some(json!(run_id)));
            object.insert("message".into(), value_of(message));
            insert_optional(&mut object, "entryId", Some(json!(entry_id)));
        }
        HarnessEventPayload::ToolStart { run_id, turn_id, tool_call_id, tool_name }
        | HarnessEventPayload::ToolUpdate { run_id, turn_id, tool_call_id, tool_name } => {
            object.insert("runId".into(), json!(run_id));
            object.insert("turnId".into(), json!(turn_id));
            object.insert("toolCallId".into(), json!(tool_call_id));
            object.insert("toolName".into(), json!(tool_name));
        }
        HarnessEventPayload::ToolEnd { run_id, turn_id, tool_call_id, tool_name, is_error, terminate } => {
            object.insert("runId".into(), json!(run_id));
            object.insert("turnId".into(), json!(turn_id));
            object.insert("toolCallId".into(), json!(tool_call_id));
            object.insert("toolName".into(), json!(tool_name));
            object.insert("isError".into(), json!(is_error));
            object.insert("terminate".into(), json!(terminate));
        }
        HarnessEventPayload::EntryAdded { entry } => {
            object.insert("entry".into(), value_of(entry.as_ref()));
        }
        HarnessEventPayload::QueueUpdate { queues } => {
            object.insert("queues".into(), Value::Array(queues.iter().map(queued_item).collect()));
        }
        HarnessEventPayload::ValueUpdate { value, name, target_id, label } => {
            object.insert("value".into(), json!(value));
            insert_optional(&mut object, "name", Some(json!(name)));
            insert_optional(&mut object, "targetId", Some(json!(target_id)));
            insert_optional(&mut object, "label", Some(json!(label)));
        }
        HarnessEventPayload::ConfigUpdate { property, value, previous } => {
            object.insert("property".into(), json!(property));
            object.insert("value".into(), value.clone());
            object.insert("previous".into(), previous.clone());
        }
        HarnessEventPayload::CompactionStart { run_id, reason, started_at } => {
            object.insert("runId".into(), json!(run_id));
            object.insert("reason".into(), json!(reason));
            object.insert("startedAt".into(), json!(started_at));
        }
        HarnessEventPayload::CompactionEnd { run_id, reason, ended_at, status, entry_id } => {
            object.insert("runId".into(), json!(run_id));
            object.insert("reason".into(), json!(reason));
            object.insert("endedAt".into(), json!(ended_at));
            object.insert("status".into(), json!(status));
            insert_optional(&mut object, "entryId", Some(json!(entry_id)));
        }
        HarnessEventPayload::NavigationStart { run_id, target_id, started_at } => {
            object.insert("runId".into(), json!(run_id));
            insert_optional(&mut object, "targetId", Some(json!(target_id)));
            object.insert("startedAt".into(), json!(started_at));
        }
        HarnessEventPayload::NavigationEnd(payload) => {
            if let Value::Object(fields) = navigation_end(payload) {
                object.extend(fields);
            }
        }
        HarnessEventPayload::LaneCreated { at } => {
            insert_optional(&mut object, "at", Some(json!(at)));
        }
        HarnessEventPayload::Usage { lane, row, totals } => {
            object.insert("lane".into(), json!(lane));
            object.insert("row".into(), value_of(row));
            object.insert("totals".into(), value_of(totals));
        }
    }
    Value::Object(object)
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_agent::harness::events::HarnessEvent;

    #[test]
    fn run_start_carries_type_lane_and_camelcase_fields() {
        let event = HarnessEvent::new(
            HarnessEventPayload::RunStart { run_id: "op".into(), started_at: 7 },
            Some("main".into()),
        );
        assert_eq!(
            harness_event_value(&event),
            json!({"type":"run_start","lane":"main","runId":"op","startedAt":7})
        );
    }

    #[test]
    fn run_end_reduces_to_the_pinned_record_shape() {
        let event = HarnessEvent::new(
            HarnessEventPayload::RunEnd(RunEndPayload {
                run_id: "op".into(),
                from_tip_id: Some("before".into()),
                tip_id: Some("after".into()),
                ended_at: 9,
                status: "completed".into(),
                error: None,
            }),
            Some("main".into()),
        );
        assert_eq!(
            harness_event_value(&event),
            json!({"type":"run_end","lane":"main","runId":"op","fromTipId":"before","tipId":"after","endedAt":9,"status":"completed"})
        );
    }

    #[test]
    fn queue_update_serializes_lane_queued_items() {
        let event = HarnessEvent::new(
            HarnessEventPayload::QueueUpdate {
                queues: vec![LaneQueuedItem {
                    entry_id: "entry".into(),
                    kind: "steer".into(),
                    item_type: "message".into(),
                    message: None,
                    custom_type: None,
                    data: None,
                }],
            },
            Some("main".into()),
        );
        assert_eq!(
            harness_event_value(&event),
            json!({"type":"queue_update","lane":"main","queues":[{"entryId":"entry","kind":"steer","type":"message"}]})
        );
    }
}
