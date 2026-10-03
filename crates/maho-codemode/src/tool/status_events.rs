use serde_json::{Value, json};

pub const STATUS_EVENT_HISTORY_LIMIT: usize = 100;
const OMITTED_STATUS_EVENTS_OP: &str = "status-events-omitted";

fn trim_status_history(events: &mut Vec<Value>) {
    if events.len() <= STATUS_EVENT_HISTORY_LIMIT {
        return;
    }
    let first = &events[0];
    if first["op"] == OMITTED_STATUS_EVENTS_OP
        && let Some(count) = first["count"].as_f64()
    {
        let remove_count = events.len() - STATUS_EVENT_HISTORY_LIMIT;
        events.drain(1..=remove_count);
        events[0] = json!({"op": OMITTED_STATUS_EVENTS_OP, "count": count + remove_count as f64});
        return;
    }
    let remove_count = events.len() - STATUS_EVENT_HISTORY_LIMIT + 1;
    events.splice(0..remove_count, [json!({"op": OMITTED_STATUS_EVENTS_OP, "count": remove_count})]);
}

pub fn upsert_status_event(events: &mut Vec<Value>, event: Value) {
    if event["op"] == "agent"
        && let Some(id) = event["id"].as_str()
        && let Some(index) = events.iter().position(|candidate| candidate["op"] == "agent" && candidate["id"].as_str() == Some(id))
    {
        events[index] = event;
        trim_status_history(events);
        return;
    }
    events.push(event);
    trim_status_history(events);
}
