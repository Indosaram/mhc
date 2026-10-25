use maho_ext_api::{ExtensionApi, ExtensionContext, ExtensionFailure, SessionEntry};
pub const GOAL_STOP_PAUSED_ENTRY: &str = "goal-stop-paused";
pub const GOAL_STOP_RESUMED_ENTRY: &str = "goal-stop-resumed";
/// Reduce durable ledger entries without confusing a run fence with a session Stop.
pub fn live_hold_kinds(entries: &[SessionEntry]) -> Vec<String> {
    let mut holds = std::collections::BTreeMap::new();
    for entry in entries {
        if entry.kind != "custom" || entry.data["customType"] != "session-hold" {
            continue;
        }
        let data = &entry.data["data"];
        let key = data["key"].as_str().unwrap_or("").to_owned();
        match data["op"].as_str() {
            Some("add") => {
                if let Some(kind) = data["kind"].as_str() {
                    holds.insert(key, kind.to_owned());
                }
            }
            Some("release") => {
                if let Some(keys) = data["keys"].as_array() {
                    for key in keys.iter().filter_map(serde_json::Value::as_str) {
                        holds.remove(key);
                    }
                } else {
                    holds.remove(&key);
                }
            }
            _ => (),
        }
    }
    holds.into_values().collect()
}
pub fn stop_hold_live(ctx: &ExtensionContext) -> bool {
    live_hold_kinds(&ctx.session_manager.get_entries())
        .iter()
        .any(|kind| kind == "session_hold")
}
pub fn mark_stop_paused(pi: &ExtensionApi, goal_id: &str) -> Result<(), ExtensionFailure> {
    pi.append_entry(
        GOAL_STOP_PAUSED_ENTRY,
        Some(serde_json::json!({"goalId":goal_id})),
    )
}
pub fn mark_stop_resumed(pi: &ExtensionApi, goal_id: &str) -> Result<(), ExtensionFailure> {
    pi.append_entry(
        GOAL_STOP_RESUMED_ENTRY,
        Some(serde_json::json!({"goalId":goal_id})),
    )
}
pub fn paused_by_stop(ctx: &ExtensionContext, goal_id: &str) -> bool {
    ctx.session_manager
        .get_entries()
        .iter()
        .rev()
        .find_map(|entry| {
            if entry.kind != "custom" || entry.data["data"]["goalId"] != goal_id {
                return None;
            }
            match entry.data["customType"].as_str() {
                Some(GOAL_STOP_PAUSED_ENTRY) => Some(true),
                Some(GOAL_STOP_RESUMED_ENTRY) => Some(false),
                _ => None,
            }
        })
        .unwrap_or(false)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ledger_releases_exact_key() {
        let entry = |op: &str, key: &str, kind: &str| SessionEntry {
            id: String::new(),
            parent_id: None,
            timestamp: String::new(),
            kind: "custom".into(),
            data: serde_json::json!({"customType":"session-hold","data":{"op":op,"key":key,"kind":kind}}),
        };
        assert_eq!(
            live_hold_kinds(&[
                entry("add", "s", "session_hold"),
                entry("add", "f", "run_fence"),
                entry("release", "f", "")
            ]),
            vec!["session_hold"]
        );
    }
}
