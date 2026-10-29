//! Task-event emission for lead mailbox deliveries.

use serde_json::{Map, Value};
use team_core::types::Message;

use crate::team::messaging::lead_poller_types::{LeadPollerDeps, TeamTaskEvent};

fn message_fields(message: &Message) -> Map<String, Value> {
    match serde_json::to_value(message) {
        Ok(Value::Object(record)) => record,
        _ => Map::new(),
    }
}

fn field(record: &Map<String, Value>, key: &str) -> Value {
    record.get(key).cloned().unwrap_or(Value::Null)
}

fn emit(deps: &LeadPollerDeps, message: &Message, event_type: &str, keys: &[(&str, &str)]) {
    let Some(task_id) = (deps.event_task_id)(message) else {
        return;
    };
    let Some(append_event) = deps.append_event.as_ref() else {
        return;
    };
    let record = message_fields(message);
    let mut payload = Map::new();
    for (payload_key, message_key) in keys {
        payload.insert((*payload_key).to_string(), field(&record, message_key));
    }
    append_event(
        &task_id,
        TeamTaskEvent {
            event_type: event_type.to_string(),
            payload: Value::Object(payload),
        },
    );
}

pub fn append_delivered_event(deps: &LeadPollerDeps, message: &Message) {
    emit(
        deps,
        message,
        "team_message_delivered",
        &[("message_id", "messageId"), ("from", "from"), ("to", "to"), ("kind", "kind")],
    );
}

pub fn append_waited_event(deps: &LeadPollerDeps, message: &Message) {
    emit(
        deps,
        message,
        "team_message_waited",
        &[("message_id", "messageId"), ("from", "from"), ("body", "body")],
    );
}
