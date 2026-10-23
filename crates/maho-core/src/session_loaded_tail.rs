//! Port of senpi packages/coding-agent/src/core/session-loaded-tail.ts (senpi#3519, #2537).
//!
//! The part of a loaded session file the resident mirror keeps: the latest compaction on the
//! leaf's branch and everything from that compaction's first kept entry on. Entries are session
//! file JSON values; the header is the `"session"` entry.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct LoadedTail {
    /// The header, the non-message entries of the leaf's path before the kept window, and the window itself.
    pub retained: Vec<Value>,
    /// Ids of the entries the mirror leaves out; a new entry must never reuse one.
    pub trimmed_ids: Vec<String>,
    /// How many entries at the end of `retained` are the file's own tail, in order (senpi#2537).
    pub window_length: usize,
}

fn kind(entry: &Value) -> &str {
    entry
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

fn id_of(entry: &Value) -> Option<&str> {
    entry.get("id").and_then(Value::as_str)
}

fn parent_of(entry: &Value) -> Option<&str> {
    entry.get("parentId").and_then(Value::as_str)
}

fn with_parent(entry: &Value, parent_id: Option<&str>) -> Value {
    let mut copy = entry.clone();
    copy["parentId"] = parent_id.map_or(Value::Null, Value::from);
    copy
}

/// Returns the retained tail of `entries` as loaded from a session file, or `None` when the whole
/// file has to stay resident: no compaction on the leaf's branch, duplicated ids, a first kept
/// entry that is not on the leaf's branch, or a window entry whose parent is outside it.
pub fn plan_loaded_tail(entries: &[Value]) -> Option<LoadedTail> {
    let mut by_id: HashMap<&str, &Value> = HashMap::new();
    let mut order: Vec<&str> = Vec::new();
    let mut leaf: Option<&Value> = None;
    for entry in entries {
        if kind(entry) == "session" {
            continue;
        }
        let id = id_of(entry)?;
        if by_id.insert(id, entry).is_some() {
            return None;
        }
        order.push(id);
        leaf = Some(entry);
    }
    let path = leaf_path(leaf, &by_id);
    let compaction = path.iter().find(|entry| kind(entry) == "compaction")?;
    let path_ids: HashSet<&str> = path.iter().filter_map(|entry| id_of(entry)).collect();
    let first_kept_id = compaction.get("firstKeptEntryId").and_then(Value::as_str)?;
    if !path_ids.contains(first_kept_id) {
        return None;
    }
    let first_kept_index = entries
        .iter()
        .position(|entry| kind(entry) != "session" && id_of(entry) == Some(first_kept_id))?;
    let boundary_parent = parent_of(&entries[first_kept_index]);

    let window: Vec<&Value> = entries[first_kept_index..]
        .iter()
        .filter(|entry| kind(entry) != "session")
        .collect();
    let window_ids: HashSet<&str> = window.iter().filter_map(|entry| id_of(entry)).collect();
    for entry in &window {
        if let Some(parent) = parent_of(entry)
            && Some(parent) != boundary_parent
            && !window_ids.contains(parent)
        {
            return None;
        }
    }

    let header = entries.iter().find(|entry| kind(entry) == "session");
    let prefix: Vec<&Value> = entries[..first_kept_index]
        .iter()
        .filter(|entry| kind(entry) != "session" && keeps_before_window(entry, &path_ids))
        .collect();
    let mut retained: Vec<Value> = header.into_iter().cloned().collect();
    let mut parent_id: Option<&str> = None;
    for entry in &prefix {
        retained.push(with_parent(entry, parent_id));
        parent_id = id_of(entry);
    }
    for entry in &window {
        retained.push(if parent_of(entry) == boundary_parent {
            with_parent(entry, parent_id)
        } else {
            (*entry).clone()
        });
    }

    let retained_ids: HashSet<&str> = prefix
        .iter()
        .chain(window.iter())
        .filter_map(|entry| id_of(entry))
        .collect();
    let trimmed_ids: Vec<String> = order
        .into_iter()
        .filter(|id| !retained_ids.contains(id))
        .map(str::to_owned)
        .collect();
    (!trimmed_ids.is_empty()).then_some(LoadedTail {
        retained,
        trimmed_ids,
        window_length: window.len(),
    })
}

/// Before the window, the mirror keeps the leaf path's non-message entries and every label and
/// session name, which describe the whole session rather than one branch.
fn keeps_before_window(entry: &Value, path_ids: &HashSet<&str>) -> bool {
    if matches!(kind(entry), "label" | "session_info") {
        return true;
    }
    kind(entry) != "message" && id_of(entry).is_some_and(|id| path_ids.contains(id))
}

fn leaf_path<'a>(leaf: Option<&'a Value>, by_id: &HashMap<&str, &'a Value>) -> Vec<&'a Value> {
    let mut path = Vec::new();
    let mut visited: HashSet<&str> = HashSet::new();
    let mut current = leaf;
    while let Some(entry) = current {
        let Some(id) = id_of(entry) else { break };
        if !visited.insert(id) {
            break;
        }
        path.push(entry);
        current = parent_of(entry).and_then(|parent| by_id.get(parent).copied());
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn message(id: &str, parent: Option<&str>) -> Value {
        json!({ "type": "message", "id": id, "parentId": parent, "message": { "role": "user" } })
    }

    #[test]
    fn a_file_without_a_compaction_stays_resident() {
        let entries = vec![
            json!({ "type": "session", "id": "s" }),
            message("a", None),
            message("b", Some("a")),
        ];
        assert_eq!(plan_loaded_tail(&entries), None);
    }

    #[test]
    fn the_window_length_counts_the_file_tail_at_the_end_of_retained() {
        let compaction =
            json!({ "type": "compaction", "id": "c", "parentId": "c2", "firstKeptEntryId": "b" });
        let entries = vec![
            json!({ "type": "session", "id": "s" }),
            message("a", None),
            message("b", Some("a")),
            message("c2", Some("b")),
            compaction,
            message("d", Some("c")),
        ];
        let tail = plan_loaded_tail(&entries).expect("tail");
        assert_eq!(tail.trimmed_ids, vec!["a".to_owned()]);
        assert_eq!(tail.window_length, 4);
        assert_eq!(tail.retained.len(), 5, "header plus the window");
        let ids: Vec<&str> = tail.retained[tail.retained.len() - tail.window_length..]
            .iter()
            .filter_map(id_of)
            .collect();
        assert_eq!(ids, vec!["b", "c2", "c", "d"]);
    }
}
