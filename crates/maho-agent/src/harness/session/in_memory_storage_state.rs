//! Port of senpi packages/agent/src/harness/session/in-memory-storage-state.ts.

use std::collections::{BTreeMap, HashMap};

use super::commit::{
    CommittedEntryWrite, CommittedListAppendWrite, CommittedListDeleteWrite, CommittedListWrite, CommittedUsageWrite,
    CommittedValueDeleteWrite, CommittedValueSetWrite, CommittedValueWrite, CommittedWrite, CommitResultBase,
    CommitValidationState, PreparedCommit, prepare_storage_commit, validate_committed_writes,
};
use super::fork_policy::{
    ForkCurrentStatePlan, ForkCurrentStateRow, select_branch_fork, project_fork_current_state_write,
};
use super::session::{SessionError, SessionErrorKind, session_invariant_error};
use super::types::{
    Entry, EntryScan, EntryStructure, ForkOptions, ScanOrder, SessionStats, StorageBranchScan, UsageRow, UsageScan,
    Write,
};
use super::values::{
    ListCursor, ListElement, ListOrder, ListReadOptions, StoredValue, Value, ValueList, branch_tip, lane_config,
    lane_state, physical_key, resolve_list_read_options, value,
};
use crate::harness::utils::usage::{add_usage, empty_usage};

struct StoredListSnapshot {
    address: ValueList,
    elements: Vec<ListElement>,
}

enum MemoryForkPlan {
    Tree,
    Branch {
        branch: String,
        destination_tip: Option<String>,
        entry_ids: std::collections::HashSet<String>,
    },
}

impl MemoryForkPlan {
    fn plan(&self) -> ForkCurrentStatePlan {
        match self {
            MemoryForkPlan::Tree => ForkCurrentStatePlan::Tree,
            MemoryForkPlan::Branch {
                branch,
                destination_tip,
                ..
            } => ForkCurrentStatePlan::Branch {
                branch: branch.clone(),
                destination_tip: destination_tip.clone(),
            },
        }
    }
}

/// Complete materialized session state for MemoryStorage and JsonlStorage.
pub struct InMemoryStorageState {
    entries: HashMap<String, Entry>,
    entries_by_seq: Vec<Entry>,
    scalar_values: HashMap<String, StoredValue>,
    list_values: HashMap<String, StoredListSnapshot>,
    usage: HashMap<String, UsageRow>,
    stats: SessionStats,
    next_seq: i64,
}

impl Default for InMemoryStorageState {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryStorageState {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            entries_by_seq: Vec::new(),
            scalar_values: HashMap::new(),
            list_values: HashMap::new(),
            usage: HashMap::new(),
            stats: SessionStats {
                message_count: 0,
                usage: empty_usage(),
            },
            next_seq: 1,
        }
    }

    pub fn prepare_commit(&self, writes: Vec<Write>, timestamp: i64) -> Result<PreparedCommit, SessionError> {
        let prepared = prepare_storage_commit(writes, self.next_seq, timestamp);
        self.validate_committed(&prepared.writes)?;
        Ok(prepared)
    }

    pub fn validate_committed(&self, writes: &[CommittedWrite]) -> Result<(), SessionError> {
        validate_committed_writes(
            writes,
            self.next_seq,
            &CommitValidationState {
                has_entry_or_usage_id: &|id: &str| {
                    self.entries.contains_key(id) || self.usage.contains_key(id)
                },
                has_entry_id: &|id: &str| self.entries.contains_key(id),
            },
        )
    }

    /// Apply writes already accepted by validate_committed() and return the post-apply totals.
    pub fn apply_validated(&mut self, writes: &[CommittedWrite]) -> SessionStats {
        for write in writes {
            match write {
                CommittedWrite::Entry(entry) => {
                    let materialized = materialize(entry);
                    self.entries.insert(materialized.id.clone(), materialized.clone());
                    self.entries_by_seq.push(materialized.clone());
                    if materialized.entry_type() == super::types::EntryType::Message {
                        self.stats.message_count += 1;
                    }
                }
                CommittedWrite::Usage(row) => {
                    let row = usage_row(row);
                    self.usage.insert(row.id.clone(), row.clone());
                    self.stats.usage = add_usage(&self.stats.usage, &row.usage);
                }
                CommittedWrite::Value(CommittedValueWrite::Delete(write)) => {
                    self.scalar_values.remove(&physical_key(&write.namespace, &write.key));
                }
                CommittedWrite::Value(CommittedValueWrite::Set(write)) => {
                    self.apply_value_set_or_list_append(&ForkCurrentStateRow::Value(write.clone()));
                }
                CommittedWrite::List(CommittedListWrite::Delete(write)) => {
                    self.list_values.remove(&physical_key(&write.namespace, &write.key));
                }
                CommittedWrite::List(CommittedListWrite::Append(write)) => {
                    self.apply_value_set_or_list_append(&ForkCurrentStateRow::List(write.clone()));
                }
            }
            self.next_seq = write.seq() + 1;
        }
        self.stats.clone()
    }

    pub fn create_fork(&self, options: &ForkOptions) -> Result<InMemoryStorageState, SessionError> {
        let plan = self.select_fork_plan(options)?;
        let is_entry_copied = |entry_id: &str| -> bool {
            match &plan {
                MemoryForkPlan::Tree => true,
                MemoryForkPlan::Branch { entry_ids, .. } => entry_ids.contains(entry_id),
            }
        };

        let mut destination = InMemoryStorageState::new();
        let mut message_count = 0;
        for entry in &self.entries_by_seq {
            if !is_entry_copied(&entry.id) {
                continue;
            }
            destination.entries.insert(entry.id.clone(), entry.clone());
            destination.entries_by_seq.push(entry.clone());
            if entry.entry_type() == super::types::EntryType::Message {
                message_count += 1;
            }
        }
        destination.stats.message_count = message_count;

        for stored in self.scalar_values.values() {
            let row = ForkCurrentStateRow::Value(CommittedValueSetWrite {
                kind: super::commit::WriteKind::Value,
                op: super::commit::ValueOp::Set,
                seq: stored.seq,
                namespace: stored.address.namespace.clone(),
                key: stored.address.key.clone(),
                value: stored.value.clone(),
            });
            if let Some(projected) = project_fork_current_state_write(&row, &plan.plan(), &is_entry_copied)? {
                destination.apply_value_set_or_list_append(&projected);
            }
        }

        for stored in self.list_values.values() {
            for element in &stored.elements {
                let row = ForkCurrentStateRow::List(CommittedListAppendWrite {
                    kind: super::commit::WriteKind::List,
                    op: super::commit::ListOp::Append,
                    seq: element.seq,
                    namespace: stored.address.namespace.clone(),
                    key: stored.address.key.clone(),
                    value: element.value.clone(),
                });
                if let Some(projected) = project_fork_current_state_write(&row, &plan.plan(), &is_entry_copied)? {
                    destination.apply_value_set_or_list_append(&projected);
                }
            }
        }
        destination.next_seq = self.next_seq;
        Ok(destination)
    }

    fn select_fork_plan(&self, options: &ForkOptions) -> Result<MemoryForkPlan, SessionError> {
        let ForkOptions::Branch { branch, .. } = options else {
            return Ok(MemoryForkPlan::Tree);
        };
        let selected: std::cell::RefCell<std::collections::HashSet<String>> =
            std::cell::RefCell::new(std::collections::HashSet::new());
        let plan = select_branch_fork(
            options,
            &super::fork_policy::BranchForkSource {
                tip: self
                    .get_value(&branch_tip(branch))
                    .map(|stored| serde_json::from_value::<Option<String>>(stored.value).unwrap_or(None)),
                get_parent: &|entry_id: &str| self.entries.get(entry_id).map(|entry| entry.parent_id.clone()),
                select_entry: &|entry_id: &str| {
                    selected.borrow_mut().insert(entry_id.to_owned());
                },
            },
        )?;
        if self.get_value(&lane_config(branch)).is_none() || self.get_value(&lane_state(branch)).is_none() {
            return Err(session_invariant_error(format!(
                "Source branch {} is not a configured AgentLane",
                serde_json::Value::String(branch.clone())
            )));
        }
        let ForkCurrentStatePlan::Branch {
            branch,
            destination_tip,
        } = plan
        else {
            return Ok(MemoryForkPlan::Tree);
        };
        Ok(MemoryForkPlan::Branch {
            branch,
            destination_tip,
            entry_ids: selected.into_inner(),
        })
    }

    fn apply_value_set_or_list_append(&mut self, write: &ForkCurrentStateRow) {
        match write {
            ForkCurrentStateRow::Value(write) => {
                let key = physical_key(&write.namespace, &write.key);
                self.scalar_values.insert(
                    key,
                    StoredValue {
                        address: value(write.namespace.clone(), write.key.clone())
                            .expect("committed value address must be valid"),
                        value: write.value.clone(),
                        seq: write.seq,
                    },
                );
            }
            ForkCurrentStateRow::List(write) => {
                let key = physical_key(&write.namespace, &write.key);
                let element = ListElement {
                    seq: write.seq,
                    value: write.value.clone(),
                };
                match self.list_values.get_mut(&key) {
                    Some(stored) => stored.elements.push(element),
                    None => {
                        self.list_values.insert(
                            key,
                            StoredListSnapshot {
                                address: super::values::list(write.namespace.clone(), write.key.clone())
                                    .expect("committed list address must be valid"),
                                elements: vec![element],
                            },
                        );
                    }
                }
            }
        }
    }

    pub fn advance_next_seq(&mut self, next_seq: i64) -> Result<(), SessionError> {
        if next_seq < 1 {
            return Err(session_invariant_error(format!(
                "Invalid storage sequence high-water mark: {next_seq}"
            )));
        }
        self.next_seq = self.next_seq.max(next_seq);
        Ok(())
    }

    pub fn get_entries(&self, ids: &[String]) -> BTreeMap<String, Entry> {
        let mut found = BTreeMap::new();
        for id in ids {
            if let Some(entry) = self.entries.get(id) {
                found.insert(id.clone(), entry.clone());
            }
        }
        found
    }

    pub fn get_value(&self, address: &Value) -> Option<StoredValue> {
        self.scalar_values
            .get(&physical_key(&address.namespace, &address.key))
            .cloned()
    }

    pub fn scan_values(&self, prefix: &Value) -> Vec<StoredValue> {
        let mut found: Vec<StoredValue> = self
            .scalar_values
            .values()
            .filter(|stored| {
                stored.address.namespace == prefix.namespace && stored.address.key.starts_with(&prefix.key)
            })
            .cloned()
            .collect();
        found.sort_by(|left, right| left.address.key.cmp(&right.address.key));
        found
    }

    pub fn read_list(&self, address: &ValueList, options: Option<ListReadOptions>) -> Result<Vec<ListElement>, SessionError> {
        let resolved = resolve_list_read_options(options)
            .map_err(|error| SessionError::new(SessionErrorKind::Invariant, error.message))?;
        let elements = self
            .list_values
            .get(&physical_key(&address.namespace, &address.key))
            .map(|stored| stored.elements.clone())
            .unwrap_or_default();
        let filtered: Vec<ListElement> = elements
            .into_iter()
            .filter(|element| match resolved.cursor {
                None => true,
                Some(ListCursor { seq }) => {
                    if resolved.order == ListOrder::Asc {
                        element.seq > seq
                    } else {
                        element.seq < seq
                    }
                }
            })
            .collect();
        let mut ordered = filtered;
        if resolved.order == ListOrder::Desc {
            ordered.reverse();
        }
        ordered.truncate(resolved.limit);
        Ok(ordered)
    }

    pub fn scan_branch(&self, query: &StorageBranchScan) -> Result<Vec<Entry>, SessionError> {
        let Some(start) = self.entries.get(&query.start) else {
            return Err(session_invariant_error(format!(
                "Unknown branch start: {}",
                query.start
            )));
        };

        let mut path: Vec<Entry> = Vec::new();
        let mut entry = Some(start.clone());
        while let Some(current) = entry {
            path.push(current.clone());
            let Some(parent_id) = current.parent_id.clone() else {
                break;
            };
            match self.entries.get(&parent_id) {
                Some(parent) => entry = Some(parent.clone()),
                None => return Err(session_invariant_error("Corrupt branch: missing parent")),
            }
        }
        if query.order == Some(super::types::BranchOrder::OldestFirst) {
            path.reverse();
        }

        let mut stopped: Vec<Entry> = Vec::new();
        for candidate in path {
            let stop = Some(candidate.id.clone()) == query.stop_at_id
                || query.stop_at_type.map(|stop| stop == candidate.entry_type()) == Some(true);
            stopped.push(candidate);
            if stop {
                break;
            }
        }
        let oldest_first = query.order == Some(super::types::BranchOrder::OldestFirst);
        let filtered: Vec<Entry> = stopped
            .into_iter()
            .filter(|candidate| query.entry_type.is_none() || query.entry_type == Some(candidate.entry_type()))
            .filter(|candidate| {
                query.custom_type.is_none() || query.custom_type.as_deref() == candidate.custom_type()
            })
            .filter(|candidate| match query.cursor {
                None => true,
                Some(cursor) => {
                    if oldest_first {
                        candidate.seq > cursor.seq
                    } else {
                        candidate.seq < cursor.seq
                    }
                }
            })
            .collect();
        Ok(match query.limit {
            None => filtered,
            Some(limit) => filtered.into_iter().take(limit).collect(),
        })
    }

    pub fn scan_branch_structure(&self, query: &StorageBranchScan) -> Result<Vec<EntryStructure>, SessionError> {
        Ok(self
            .scan_branch(query)?
            .into_iter()
            .map(|entry| {
                let entry_type = entry.entry_type();
                let custom_type = entry.custom_type().map(str::to_owned);
                EntryStructure {
                    id: entry.id,
                    parent_id: entry.parent_id,
                    seq: entry.seq,
                    timestamp: entry.timestamp,
                    entry_type,
                    custom_type,
                }
            })
            .collect())
    }

    pub fn scan_entries(&self, query: &EntryScan) -> Vec<Entry> {
        let limit = query.limit.unwrap_or(usize::MAX);
        let mut entries: Vec<Entry> = Vec::new();
        let descending = query.order == Some(ScanOrder::Desc);
        let mut index: i64 = if descending {
            self.entries_by_seq.len() as i64 - 1
        } else {
            0
        };
        while index >= 0 && (index as usize) < self.entries_by_seq.len() && entries.len() < limit {
            let entry = &self.entries_by_seq[index as usize];
            let matches = (query.entry_type.is_none() || query.entry_type == Some(entry.entry_type()))
                && (query.custom_type.is_none() || query.custom_type.as_deref() == entry.custom_type())
                && query.from_seq.map(|from| entry.seq >= from).unwrap_or(true)
                && query.to_seq.map(|to| entry.seq <= to).unwrap_or(true);
            if matches {
                entries.push(entry.clone());
            }
            index += if descending { -1 } else { 1 };
        }
        entries
    }

    pub fn scan_usage(&self, query: &UsageScan) -> Vec<UsageRow> {
        let mut rows: Vec<UsageRow> = self
            .usage
            .values()
            .filter(|row| query.from_seq.map(|from| row.seq >= from).unwrap_or(true))
            .filter(|row| query.to_seq.map(|to| row.seq <= to).unwrap_or(true))
            .cloned()
            .collect();
        rows.sort_by_key(|row| row.seq);
        if query.order == Some(ScanOrder::Desc) {
            rows.reverse();
        }
        if let Some(limit) = query.limit {
            rows.truncate(limit);
        }
        rows
    }

    pub fn get_stats(&self) -> SessionStats {
        self.stats.clone()
    }

    pub fn get_next_seq(&self) -> i64 {
        self.next_seq
    }
}

fn materialize(write: &CommittedEntryWrite) -> Entry {
    Entry {
        id: write.id.clone(),
        parent_id: write.parent_id.clone(),
        seq: write.seq,
        timestamp: write.timestamp,
        kind: write.body.clone(),
    }
}

fn usage_row(write: &CommittedUsageWrite) -> UsageRow {
    UsageRow {
        id: write.id.clone(),
        seq: write.seq,
        usage: write.usage,
        entry_id: write.entry_id.clone(),
        adjustment: write.adjustment,
        details: write.details.clone(),
    }
}

pub type CommittedValueSetWriteAlias = CommittedValueSetWrite;
pub type CommittedValueDeleteWriteAlias = CommittedValueDeleteWrite;
pub type CommittedListDeleteWriteAlias = CommittedListDeleteWrite;
pub type CommitResultBaseAlias = CommitResultBase;
