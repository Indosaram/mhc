//! Lifecycle bookkeeping-only history projection.
use serde_json::Value;
/// Child lifecycle reports, the only entries `LifecycleChildren` reads. The memoized state is
/// cloned on every refresh, so it keeps no other bookkeeping (session-lifecycle records grow by one
/// per turn and are read once, at load, from `entries_of_types`).
pub static LIFECYCLE_HISTORY: crate::session_history_reduce::HistoryReducer<Vec<Value>> =
    crate::session_history_reduce::HistoryReducer {
        initial: Vec::new,
        add: |state, entry| {
            if entry["type"] == "custom" && entry["customType"] == "child-lifecycle" {
                state.push(entry.clone());
            }
        },
    };

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunEndState {
    pub run_id: Option<String>,
    pub ended: bool,
}

/// The latest run a lifecycle record opened, and whether the transcript holds that run's end
/// (`senpi#7016`).
pub static LAST_RUN_END: crate::session_history_reduce::HistoryReducer<RunEndState> =
    crate::session_history_reduce::HistoryReducer {
        initial: RunEndState::default,
        add: |state, entry| match entry.get("type").and_then(Value::as_str) {
            Some("custom")
                if entry.get("customType").and_then(Value::as_str) == Some("session-lifecycle") =>
            {
                if let Some(data) = entry.get("data").and_then(Value::as_object)
                    && data.get("runActive").and_then(Value::as_bool) == Some(true)
                    && let Some(run_id) = data.get("runId").and_then(Value::as_str)
                    && state.run_id.as_deref() != Some(run_id)
                {
                    state.run_id = Some(run_id.to_owned());
                    state.ended = false;
                }
            }
            Some("message") => {
                let role = entry.pointer("/message/role").and_then(Value::as_str);
                let stop_reason = entry.pointer("/message/stopReason").and_then(Value::as_str);
                state.ended = role == Some("assistant") && stop_reason != Some("toolUse");
            }
            Some("custom_message") => {
                state.ended = false;
            }
            _ => {}
        },
    };
pub fn lifecycle_history(entries: &[Value]) -> Vec<Value> {
    entries
        .iter()
        .filter(|e| {
            e["type"] == "custom"
                && [
                    "session-lifecycle",
                    "work-custody",
                    "child-lifecycle",
                    "interrupted-run",
                ]
                .contains(&e["customType"].as_str().unwrap_or(""))
        })
        .cloned()
        .collect()
}
