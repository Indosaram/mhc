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
