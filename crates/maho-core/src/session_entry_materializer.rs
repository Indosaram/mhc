//! Port of senpi packages/coding-agent/src/core/session-entry-materializer.ts.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::session_resident_store::ResidentStringStore;

/// Materializes resident strings for every entry, repairing an entry whose resident data is gone
/// by re-materializing it from the persisted history.
pub fn materialize_session_entries(
    entries: &[Value],
    resident_store: &ResidentStringStore,
    load_history_entries: impl Fn() -> Vec<Value>,
    on_materialized: impl Fn(&Value),
) -> Vec<Value> {
    let missing_entry_ids: RefCell<BTreeSet<String>> = RefCell::new(BTreeSet::new());
    let materialized: Vec<Value> = entries
        .iter()
        .map(|entry| {
            let entry_id = entry.get("id").and_then(Value::as_str).unwrap_or_default().to_owned();
            resident_store.materialize_with(entry, Some(&|_missing| {
                missing_entry_ids.borrow_mut().insert(entry_id.clone());
                None
            }))
        })
        .collect();

    if missing_entry_ids.borrow().is_empty() {
        for entry in &materialized {
            on_materialized(entry);
        }
        return materialized;
    }

    let missing = missing_entry_ids.into_inner();
    let persisted_by_id: BTreeMap<String, Value> = load_history_entries()
        .into_iter()
        .filter(|entry| entry.get("type").and_then(Value::as_str) != Some("session"))
        .filter_map(|entry| {
            let id = entry.get("id").and_then(Value::as_str).map(str::to_owned);
            id.map(|id| (id, entry))
        })
        .collect();

    let repaired: Vec<Value> = materialized
        .into_iter()
        .map(|entry| {
            let id = entry.get("id").and_then(Value::as_str).unwrap_or_default().to_owned();
            if !missing.contains(&id) {
                return entry;
            }
            match persisted_by_id.get(&id) {
                Some(persisted) => resident_store.materialize(persisted),
                None => entry,
            }
        })
        .collect();
    for entry in &repaired {
        on_materialized(entry);
    }
    repaired
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_resident_store::{ResidentStringStore, ResidentStringStoreOptions, RESIDENT_STRING_PREFIX};
    use serde_json::json;

    fn store() -> ResidentStringStore {
        ResidentStringStore::new(ResidentStringStoreOptions::default())
    }

    #[test]
    fn every_entry_is_reported_when_nothing_is_missing() {
        let store = store();
        let entries = vec![json!({ "id": "a", "type": "message", "text": "plain" })];
        let seen: RefCell<Vec<String>> = RefCell::new(Vec::new());
        let result = materialize_session_entries(&entries, &store, Vec::new, |entry| {
            seen.borrow_mut().push(entry.get("id").and_then(Value::as_str).unwrap_or_default().to_owned());
        });
        assert_eq!(result.len(), 1);
        assert_eq!(seen.into_inner(), vec!["a".to_owned()]);
    }

    #[test]
    fn a_missing_resident_entry_is_repaired_from_history() {
        let store = store();
        let missing_ref = format!("{RESIDENT_STRING_PREFIX}gone");
        let entries = vec![json!({ "id": "a", "type": "message", "text": missing_ref })];
        let history = vec![
            json!({ "type": "session" }),
            json!({ "id": "a", "type": "message", "text": "recovered" }),
        ];
        let result = materialize_session_entries(&entries, &store, || history.clone(), |_| {});
        assert_eq!(result[0]["text"], json!("recovered"));
    }

    #[test]
    fn an_unrecoverable_missing_entry_is_left_as_is() {
        let store = store();
        let missing_ref = format!("{RESIDENT_STRING_PREFIX}gone");
        let entries = vec![json!({ "id": "a", "type": "message", "text": missing_ref })];
        let result = materialize_session_entries(&entries, &store, Vec::new, |_| {});
        assert!(result[0]["text"].as_str().unwrap_or_default().contains(RESIDENT_STRING_PREFIX));
    }
}
