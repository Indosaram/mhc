//! Engine state classification for cold wake (`thread/wake/engine-state.ts`).

use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineView {
    pub held: bool,
    pub hold_kinds: Vec<String>,
    pub waiting_for_person: bool,
    pub working: bool,
    pub finished: bool,
    pub human_input_revision: Option<i64>,
}

pub fn read_engine_state(state: &Value) -> EngineView {
    let empty = Map::new();
    let data = state.as_object().unwrap_or(&empty);
    let lifecycle = data
        .get("lifecycle")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let waiting_on = lifecycle
        .get("waitingOn")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let hold_kinds: Vec<String> = data
        .get("holds")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|h| {
                    h.as_object()
                        .and_then(|o| o.get("kind"))
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_owned()
                })
                .collect()
        })
        .unwrap_or_default();
    let human_waits: Vec<&str> = waiting_on
        .get("human")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|w| {
                    w.as_object()
                        .and_then(|o| o.get("kind"))
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                })
                .collect()
        })
        .unwrap_or_default();
    let pending_dialogs = data
        .get("pendingDialogs")
        .and_then(Value::as_array)
        .map(|a| !a.is_empty())
        .unwrap_or(false);
    let waiting_for_person = human_waits.iter().any(|k| *k != "stalled") || pending_dialogs;
    let held = !hold_kinds.is_empty() || data.get("paused").and_then(Value::as_bool) == Some(true);
    let lc_state = lifecycle.get("state").and_then(Value::as_str);
    let bg_len = waiting_on
        .get("background")
        .and_then(Value::as_array)
        .map(|a| a.len())
        .unwrap_or(0);
    let children = waiting_on
        .get("children")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let working = data.get("isStreaming").and_then(Value::as_bool) == Some(true)
        || matches!(
            lc_state,
            Some("running" | "waiting_background" | "waiting_children")
        )
        || bg_len > 0
        || children > 0;
    let human_input_revision = lifecycle.get("humanInputRevision").and_then(Value::as_i64);

    EngineView {
        held,
        hold_kinds,
        waiting_for_person,
        working,
        finished: !held && !waiting_for_person && !working,
        human_input_revision,
    }
}
