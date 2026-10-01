//! Port of senpi packages/agent/src/harness/session/commit.ts.

use serde::{Deserialize, Serialize};

use super::session::{SessionError, SessionErrorKind};
use super::types::{Entry, EntryKind, NewEntry, NewUsageRow, Write};
use super::values::{ListWrite, ValueWrite};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WriteKind {
    #[serde(rename = "entry")]
    Entry,
    #[serde(rename = "usage")]
    Usage,
    #[serde(rename = "value")]
    Value,
    #[serde(rename = "list")]
    List,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommittedEntryWrite {
    pub kind: WriteKind,
    pub id: String,
    pub parent_id: Option<String>,
    pub seq: i64,
    pub timestamp: i64,
    #[serde(flatten)]
    pub body: EntryKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommittedUsageWrite {
    pub kind: WriteKind,
    pub id: String,
    pub seq: i64,
    pub usage: maho_ai::types::Usage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<String>,
    pub adjustment: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommittedValueSetWrite {
    pub kind: WriteKind,
    pub op: ValueOp,
    pub seq: i64,
    pub namespace: String,
    pub key: String,
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommittedValueDeleteWrite {
    pub kind: WriteKind,
    pub op: ValueOp,
    pub seq: i64,
    pub namespace: String,
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommittedListAppendWrite {
    pub kind: WriteKind,
    pub op: ListOp,
    pub seq: i64,
    pub namespace: String,
    pub key: String,
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommittedListDeleteWrite {
    pub kind: WriteKind,
    pub op: ListOp,
    pub seq: i64,
    pub namespace: String,
    pub key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ValueOp {
    Set,
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ListOp {
    Append,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CommittedValueWrite {
    Set(CommittedValueSetWrite),
    Delete(CommittedValueDeleteWrite),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CommittedListWrite {
    Append(CommittedListAppendWrite),
    Delete(CommittedListDeleteWrite),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CommittedWrite {
    Entry(CommittedEntryWrite),
    Usage(CommittedUsageWrite),
    Value(CommittedValueWrite),
    List(CommittedListWrite),
}

impl CommittedWrite {
    pub fn seq(&self) -> i64 {
        match self {
            CommittedWrite::Entry(write) => write.seq,
            CommittedWrite::Usage(write) => write.seq,
            CommittedWrite::Value(CommittedValueWrite::Set(write)) => write.seq,
            CommittedWrite::Value(CommittedValueWrite::Delete(write)) => write.seq,
            CommittedWrite::List(CommittedListWrite::Append(write)) => write.seq,
            CommittedWrite::List(CommittedListWrite::Delete(write)) => write.seq,
        }
    }

    pub fn kind(&self) -> WriteKind {
        match self {
            CommittedWrite::Entry(_) => WriteKind::Entry,
            CommittedWrite::Usage(_) => WriteKind::Usage,
            CommittedWrite::Value(_) => WriteKind::Value,
            CommittedWrite::List(_) => WriteKind::List,
        }
    }

    pub fn entry_id(&self) -> Option<&str> {
        match self {
            CommittedWrite::Entry(write) => Some(&write.id),
            CommittedWrite::Usage(write) => Some(&write.id),
            _ => None,
        }
    }
}

impl CommittedValueWrite {
    pub fn seq(&self) -> i64 {
        match self {
            CommittedValueWrite::Set(write) => write.seq,
            CommittedValueWrite::Delete(write) => write.seq,
        }
    }

    pub fn namespace(&self) -> &str {
        match self {
            CommittedValueWrite::Set(write) => &write.namespace,
            CommittedValueWrite::Delete(write) => &write.namespace,
        }
    }

    pub fn key(&self) -> &str {
        match self {
            CommittedValueWrite::Set(write) => &write.key,
            CommittedValueWrite::Delete(write) => &write.key,
        }
    }

    pub fn value(&self) -> Option<&serde_json::Value> {
        match self {
            CommittedValueWrite::Set(write) => Some(&write.value),
            CommittedValueWrite::Delete(_) => None,
        }
    }

    pub fn with_value(self, value: serde_json::Value) -> Self {
        match self {
            CommittedValueWrite::Set(mut write) => {
                write.value = value;
                CommittedValueWrite::Set(write)
            }
            CommittedValueWrite::Delete(write) => CommittedValueWrite::Delete(write),
        }
    }
}

impl CommittedListWrite {
    pub fn seq(&self) -> i64 {
        match self {
            CommittedListWrite::Append(write) => write.seq,
            CommittedListWrite::Delete(write) => write.seq,
        }
    }

    pub fn namespace(&self) -> &str {
        match self {
            CommittedListWrite::Append(write) => &write.namespace,
            CommittedListWrite::Delete(write) => &write.namespace,
        }
    }

    pub fn key(&self) -> &str {
        match self {
            CommittedListWrite::Append(write) => &write.key,
            CommittedListWrite::Delete(write) => &write.key,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitResultBase {
    pub first_seq: i64,
    pub seqs: Vec<i64>,
    pub timestamp: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreparedCommit {
    pub writes: Vec<CommittedWrite>,
    pub result: CommitResultBase,
}

/// Validation callbacks supplied by the storage backend that owns the durable index.
pub struct CommitValidationState<'a> {
    pub has_entry_or_usage_id: &'a dyn Fn(&str) -> bool,
    pub has_entry_id: &'a dyn Fn(&str) -> bool,
}

pub fn insert_entry(entry: NewEntry) -> Write {
    Write::Entry(super::types::EntryWrite { entry })
}

pub fn insert_usage(row: NewUsageRow) -> Write {
    Write::Usage(super::types::UsageWrite { row })
}

pub fn commit_write(write: Write, seq: i64, timestamp: i64) -> CommittedWrite {
    match write {
        Write::Entry(entry_write) => {
            let entry = entry_write.entry;
            CommittedWrite::Entry(CommittedEntryWrite {
                kind: WriteKind::Entry,
                id: entry.id,
                parent_id: entry.parent_id,
                seq,
                timestamp,
                body: entry.kind,
            })
        }
        Write::Usage(usage_write) => {
            let row = usage_write.row;
            CommittedWrite::Usage(CommittedUsageWrite {
                kind: WriteKind::Usage,
                id: row.id,
                seq,
                usage: row.usage,
                entry_id: row.entry_id,
                adjustment: row.adjustment,
                details: row.details,
            })
        }
        Write::Value(value_write) => CommittedWrite::Value(match value_write {
            ValueWrite::Set(write) => CommittedValueWrite::Set(CommittedValueSetWrite {
                kind: WriteKind::Value,
                op: ValueOp::Set,
                seq,
                namespace: write.namespace,
                key: write.key,
                value: write.value,
            }),
            ValueWrite::Delete(write) => CommittedValueWrite::Delete(CommittedValueDeleteWrite {
                kind: WriteKind::Value,
                op: ValueOp::Delete,
                seq,
                namespace: write.namespace,
                key: write.key,
            }),
        }),
        Write::List(list_write) => CommittedWrite::List(match list_write {
            ListWrite::Append(write) => CommittedListWrite::Append(CommittedListAppendWrite {
                kind: WriteKind::List,
                op: ListOp::Append,
                seq,
                namespace: write.namespace,
                key: write.key,
                value: write.value,
            }),
            ListWrite::Delete(write) => CommittedListWrite::Delete(CommittedListDeleteWrite {
                kind: WriteKind::List,
                op: ListOp::Delete,
                seq,
                namespace: write.namespace,
                key: write.key,
            }),
        }),
    }
}

pub fn materialize_committed_entry(entry: NewEntry, seq: i64, timestamp: i64) -> Entry {
    Entry {
        id: entry.id,
        parent_id: entry.parent_id,
        seq,
        timestamp,
        kind: entry.kind,
    }
}

pub fn prepare_storage_commit(writes: Vec<Write>, first_seq: i64, timestamp: i64) -> PreparedCommit {
    let committed_writes: Vec<CommittedWrite> = writes
        .into_iter()
        .enumerate()
        .map(|(index, write)| commit_write(write, first_seq + index as i64, timestamp))
        .collect();
    PreparedCommit {
        result: CommitResultBase {
            first_seq,
            seqs: committed_writes.iter().map(CommittedWrite::seq).collect(),
            timestamp,
        },
        writes: committed_writes,
    }
}

pub fn validate_committed_writes(
    writes: &[CommittedWrite],
    first_seq: i64,
    state: &CommitValidationState<'_>,
) -> Result<(), SessionError> {
    let mut previous_seq = first_seq - 1;
    let mut transaction_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut transaction_entry_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for write in writes {
        if write.seq() <= previous_seq {
            return Err(SessionError::new(
                SessionErrorKind::NonMonotonicSequence,
                format!("Non-monotonic storage sequence: {}", write.seq()),
            ));
        }
        previous_seq = write.seq();
        let id = match write {
            CommittedWrite::Entry(entry) => entry.id.clone(),
            CommittedWrite::Usage(row) => row.id.clone(),
            _ => continue,
        };
        if (state.has_entry_or_usage_id)(&id) || transaction_ids.contains(&id) {
            return Err(SessionError::new(
                SessionErrorKind::DuplicateId,
                format!("Duplicate entry or usage id: {id}"),
            ));
        }
        if let CommittedWrite::Entry(entry) = write
            && let Some(parent_id) = &entry.parent_id
                && !(state.has_entry_id)(parent_id) && !transaction_entry_ids.contains(parent_id) {
                    return Err(SessionError::new(
                        SessionErrorKind::MissingParent,
                        format!("Missing parent entry: {parent_id}"),
                    ));
                }
        transaction_ids.insert(id);
        if let CommittedWrite::Entry(entry) = write {
            transaction_entry_ids.insert(entry.id.clone());
        }
    }
    Ok(())
}
