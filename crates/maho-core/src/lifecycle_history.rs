//! Lifecycle bookkeeping-only history projection.
use serde_json::Value;
pub static LIFECYCLE_HISTORY: crate::session_history_reduce::HistoryReducer<Vec<Value>> =
    crate::session_history_reduce::HistoryReducer {
        initial: Vec::new,
        add: |state, entry| {
            if entry["type"] == "custom"
                && [
                    "session-lifecycle",
                    "work-custody",
                    "child-lifecycle",
                    "interrupted-run",
                ]
                .contains(&entry["customType"].as_str().unwrap_or(""))
            {
                state.push(entry.clone());
            }
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
