//! Port of senpi `packages/coding-agent/src/modes/interactive/tips/history-writer.ts`.

use std::collections::HashMap;

pub fn record_tip_shown(
    history: &HashMap<String, u64>,
    tip_id: &str,
    now: u64,
) -> HashMap<String, u64> {
    let mut next = history.clone();
    next.insert(tip_id.to_string(), now);
    next
}
