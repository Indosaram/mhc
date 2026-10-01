//! Port of senpi `packages/coding-agent/src/core/compaction/warm-anchor.ts`.

use serde_json::Value;

/// `WarmAnchorSnapshot`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WarmAnchorSnapshot {
    pub first_kept_entry_id: String,
    pub prefix_entry_ids: Vec<String>,
    pub latest_compaction_entry_id: Option<String>,
}

fn entry_id(entry: &Value) -> Option<String> {
    entry.get("id").and_then(Value::as_str).map(str::to_string)
}

/// `latestCompactionEntryId`.
pub fn latest_compaction_entry_id(entries: &[Value]) -> Option<String> {
    for entry in entries.iter().rev() {
        if entry.get("type").and_then(Value::as_str) == Some("compaction") {
            return entry_id(entry);
        }
    }
    None
}

/// `createWarmAnchorSnapshot`.
pub fn create_warm_anchor_snapshot(first_kept_entry_id: &str, branch_entries: &[Value]) -> Option<WarmAnchorSnapshot> {
    let anchor_index = branch_entries
        .iter()
        .position(|entry| entry_id(entry).as_deref() == Some(first_kept_entry_id))?;
    Some(WarmAnchorSnapshot {
        first_kept_entry_id: first_kept_entry_id.to_string(),
        prefix_entry_ids: branch_entries[..anchor_index].iter().filter_map(entry_id).collect(),
        latest_compaction_entry_id: latest_compaction_entry_id(branch_entries),
    })
}

/// `isWarmSummaryAnchorValid`.
pub fn is_warm_summary_anchor_valid(snapshot: &WarmAnchorSnapshot, current_branch_entries: &[Value]) -> bool {
    let Some(anchor_index) = current_branch_entries
        .iter()
        .position(|entry| entry_id(entry).as_deref() == Some(snapshot.first_kept_entry_id.as_str()))
    else {
        return false;
    };
    if latest_compaction_entry_id(current_branch_entries) != snapshot.latest_compaction_entry_id {
        return false;
    }
    if anchor_index != snapshot.prefix_entry_ids.len() {
        return false;
    }
    for (index, entry) in current_branch_entries.iter().enumerate().take(anchor_index) {
        if entry_id(entry).as_deref() != snapshot.prefix_entry_ids.get(index).map(String::as_str) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(id: &str, entry_type: &str) -> Value {
        json!({ "id": id, "type": entry_type })
    }

    fn branch() -> Vec<Value> {
        vec![
            entry("a", "message"),
            entry("b", "message"),
            entry("c", "message"),
            entry("d", "message"),
        ]
    }

    #[test]
    fn the_snapshot_records_the_prefix_and_the_latest_compaction() {
        let entries = vec![entry("a", "message"), entry("c1", "compaction"), entry("b", "message")];
        let snapshot = create_warm_anchor_snapshot("b", &entries).expect("snapshot");
        assert_eq!(snapshot.first_kept_entry_id, "b");
        assert_eq!(snapshot.prefix_entry_ids, vec!["a".to_string(), "c1".to_string()]);
        assert_eq!(snapshot.latest_compaction_entry_id.as_deref(), Some("c1"));
    }

    #[test]
    fn an_unknown_anchor_has_no_snapshot() {
        assert!(create_warm_anchor_snapshot("missing", &branch()).is_none());
    }

    #[test]
    fn a_grown_tail_after_the_anchor_keeps_the_snapshot_valid() {
        let mut entries = branch();
        let snapshot = create_warm_anchor_snapshot("c", &entries).expect("snapshot");
        entries.push(entry("e", "message"));
        assert!(is_warm_summary_anchor_valid(&snapshot, &entries));
    }

    #[test]
    fn a_moved_anchor_or_changed_prefix_invalidates_the_snapshot() {
        let entries = branch();
        let snapshot = create_warm_anchor_snapshot("c", &entries).expect("snapshot");
        assert!(!is_warm_summary_anchor_valid(&snapshot, &entries[..2]));
        let mut replaced = entries.clone();
        replaced[0] = entry("z", "message");
        assert!(!is_warm_summary_anchor_valid(&snapshot, &replaced));
    }

    #[test]
    fn a_new_compaction_boundary_invalidates_the_snapshot() {
        let entries = branch();
        let snapshot = create_warm_anchor_snapshot("c", &entries).expect("snapshot");
        let mut with_boundary = entries.clone();
        with_boundary.push(entry("c2", "compaction"));
        assert!(!is_warm_summary_anchor_valid(&snapshot, &with_boundary));
    }

    #[test]
    fn the_latest_compaction_id_scans_backwards() {
        let entries = vec![entry("c1", "compaction"), entry("m", "message"), entry("c2", "compaction")];
        assert_eq!(latest_compaction_entry_id(&entries).as_deref(), Some("c2"));
        assert_eq!(latest_compaction_entry_id(&branch()), None);
    }
}
