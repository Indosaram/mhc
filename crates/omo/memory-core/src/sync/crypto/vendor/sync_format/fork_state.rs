use super::fold::fold_at;
use serde_json::{Value, json};
use std::collections::BTreeMap;
fn merge(
    base: &BTreeMap<String, Value>,
    ends: &[BTreeMap<String, Value>],
) -> BTreeMap<String, Value> {
    let mut merged: BTreeMap<_, _> = base
        .iter()
        .filter(|(key, _)| ends.iter().all(|end| end.contains_key(*key)))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if let Some(first) = ends.first() {
        for (key, value) in first {
            if !base.contains_key(key) && ends.iter().all(|end| end.contains_key(key)) {
                merged.insert(key.clone(), value.clone());
            }
        }
    }
    merged
}
pub fn recompute_fork_state(ancestry: &[Value], evidence: &Value, depth: usize) -> Option<Value> {
    if depth > 64 {
        return None;
    }
    let common = fold_at(ancestry, depth).0;
    let mut ends = Vec::new();
    let mut epoch = 0;
    for branch in evidence["branches"].as_array()? {
        let records = branch["records"].as_array()?;
        for record in records {
            epoch = epoch.max(record["epoch"].as_u64()?);
        }
        let mut entries = ancestry.to_vec();
        entries.extend(records.iter().cloned());
        ends.push(fold_at(&entries, depth).0);
    }
    let members = merge(
        &common.members,
        &ends.iter().map(|a| a.members.clone()).collect::<Vec<_>>(),
    );
    let registry = merge(
        &common.registry,
        &ends.iter().map(|a| a.registry.clone()).collect::<Vec<_>>(),
    );
    let mut members: Vec<_> = members.values().collect();
    members.sort_by(|a, b| {
        a["id"]
            .as_str()
            .unwrap_or("")
            .encode_utf16()
            .cmp(b["id"].as_str().unwrap_or("").encode_utf16())
    });
    let mut registry: Vec<_> = registry.values().collect();
    registry.sort_by(|a, b| {
        a["credential_id"]
            .as_str()
            .unwrap_or("")
            .encode_utf16()
            .cmp(b["credential_id"].as_str().unwrap_or("").encode_utf16())
    });
    Some(json!({"members":members,"recovery_keys":registry,"epoch":epoch}))
}
