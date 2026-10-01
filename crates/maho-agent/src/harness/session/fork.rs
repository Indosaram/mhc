//! Port of senpi packages/agent/src/harness/session/fork.ts.

use std::collections::{BTreeMap, HashSet};

use super::fork_policy::{
    ForkCurrentStatePlan, ForkCurrentStateRow, select_branch_fork, project_fork_current_state_write,
};
use super::session::{SessionError, SessionErrorKind, session_invariant_error};
use super::types::{Entry, ForkOptions};
use super::values::{StoredValue, Value, branch_tip, lane_config, lane_state, value};

fn quote(value: &str) -> String {
    serde_json::Value::String(value.to_owned()).to_string()
}

pub struct ForkSourceSnapshot {
    pub entries: Vec<Entry>,
    pub scalar_values: Vec<StoredValue>,
    /// False when a backend supplied only the requested branch rather than the full tree.
    pub entries_complete: Option<bool>,
}

pub struct ForkDestinationSnapshot {
    pub entries: BTreeMap<String, Entry>,
    pub scalar_values: Vec<StoredValue>,
    pub next_seq: i64,
}

fn stored_values_in_namespace<'a>(values: &'a [StoredValue], address: &Value) -> Vec<&'a StoredValue> {
    values
        .iter()
        .filter(|stored| stored.address.namespace == address.namespace)
        .collect()
}

fn find_stored_value<'a>(values: &'a [StoredValue], address: &Value) -> Option<&'a StoredValue> {
    values
        .iter()
        .find(|stored| stored.address.namespace == address.namespace && stored.address.key == address.key)
}

/// Build the complete logical state for a forked destination session.
pub fn create_fork_snapshot(
    source: ForkSourceSnapshot,
    options: &ForkOptions,
) -> Result<ForkDestinationSnapshot, SessionError> {
    let source_entries: BTreeMap<String, Entry> = source
        .entries
        .iter()
        .map(|entry| (entry.id.clone(), entry.clone()))
        .collect();
    let source_tips = stored_values_in_namespace(&source.scalar_values, &branch_tip(""));
    validate_fork_source_snapshot(&source, &source_entries, &source_tips, options)?;

    let (entry_ids, plan) = select_fork_contents(&source_entries, &source_tips, options)?;
    let entries: BTreeMap<String, Entry> = entry_ids
        .iter()
        .filter_map(|id| source_entries.get(id).map(|entry| (id.clone(), entry.clone())))
        .collect();

    let mut scalar_values: Vec<StoredValue> = Vec::new();
    let mut next_seq = entries.values().map(|entry| entry.seq).max().unwrap_or(0).max(0) + 1;
    for stored in &source.scalar_values {
        let row = ForkCurrentStateRow::Value(super::commit::CommittedValueSetWrite {
            kind: super::commit::WriteKind::Value,
            op: super::commit::ValueOp::Set,
            seq: stored.seq,
            namespace: stored.address.namespace.clone(),
            key: stored.address.key.clone(),
            value: stored.value.clone(),
        });
        let projected = project_fork_current_state_write(&row, &plan, &|entry_id: &str| entry_ids.contains(entry_id))?;
        if let Some(ForkCurrentStateRow::Value(projected)) = projected {
            scalar_values.push(StoredValue {
                address: value(projected.namespace, projected.key).map_err(|error| {
                    SessionError::new(SessionErrorKind::Invariant, error.message)
                })?,
                value: projected.value,
                seq: next_seq,
            });
            next_seq += 1;
        }
    }

    Ok(ForkDestinationSnapshot {
        entries,
        scalar_values,
        next_seq,
    })
}

fn select_fork_contents(
    source_entries: &BTreeMap<String, Entry>,
    source_tips: &[&StoredValue],
    options: &ForkOptions,
) -> Result<(HashSet<String>, ForkCurrentStatePlan), SessionError> {
    let mut entry_ids: HashSet<String> = HashSet::new();
    let ForkOptions::Branch { branch, .. } = options else {
        for id in source_entries.keys() {
            entry_ids.insert(id.clone());
        }
        return Ok((entry_ids, ForkCurrentStatePlan::Tree));
    };

    let source_tip = source_tips.iter().find(|stored| stored.address.key == *branch);
    let tip = source_tip.map(|stored| serde_json::from_value::<Option<String>>(stored.value.clone()).unwrap_or(None));
    let selected: std::cell::RefCell<HashSet<String>> = std::cell::RefCell::new(HashSet::new());
    let plan = select_branch_fork(
        options,
        &super::fork_policy::BranchForkSource {
            tip,
            get_parent: &|entry_id: &str| source_entries.get(entry_id).map(|entry| entry.parent_id.clone()),
            select_entry: &|entry_id: &str| {
                selected.borrow_mut().insert(entry_id.to_owned());
            },
        },
    )?;
    entry_ids.extend(selected.into_inner());
    Ok((entry_ids, plan))
}

fn validate_fork_source_snapshot(
    source: &ForkSourceSnapshot,
    source_entries: &BTreeMap<String, Entry>,
    source_tips: &[&StoredValue],
    options: &ForkOptions,
) -> Result<(), SessionError> {
    let source_tip_keys: HashSet<&str> = source_tips.iter().map(|stored| stored.address.key.as_str()).collect();

    for stored in &source.scalar_values {
        if (stored.address.namespace == lane_config("").namespace
            || stored.address.namespace == lane_state("").namespace)
            && !source_tip_keys.contains(stored.address.key.as_str())
        {
            return Err(session_invariant_error(format!(
                "Source session branch {} is missing branch.tip",
                quote(&stored.address.key)
            )));
        }
    }
    for tip in source_tips {
        let configuration = find_stored_value(&source.scalar_values, &lane_config(&tip.address.key));
        let state = find_stored_value(&source.scalar_values, &lane_state(&tip.address.key));
        if configuration.is_some() != state.is_some() {
            return Err(session_invariant_error(format!(
                "Source session branch {} has incomplete lane state",
                quote(&tip.address.key)
            )));
        }
        if let ForkOptions::Branch { branch, .. } = options
            && tip.address.key == *branch && configuration.is_none() {
                return Err(session_invariant_error(format!(
                    "Source branch {} is not a configured AgentLane",
                    quote(branch)
                )));
            }
        let tip_value = serde_json::from_value::<Option<String>>(tip.value.clone()).unwrap_or(None);
        if (source.entries_complete != Some(false) || matches!(options, ForkOptions::Tree { .. }))
            && tip_value.is_some()
            && !source_entries.contains_key(tip_value.as_deref().unwrap_or_default())
        {
            return Err(session_invariant_error(format!(
                "Source session branch {} has an unknown tip",
                quote(&tip.address.key)
            )));
        }
    }
    Ok(())
}
