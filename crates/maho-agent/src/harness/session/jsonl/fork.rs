//! Port of senpi packages/agent/src/harness/session/jsonl/fork.ts.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use crate::harness::context::Context;
use crate::harness::session::commit::{
    CommittedEntryWrite, CommittedListAppendWrite, CommittedListWrite, CommittedValueSetWrite, CommittedValueWrite,
    CommittedWrite,
};
use crate::harness::session::fork_policy::{
    ForkCurrentStatePlan, ForkCurrentStateRow, project_fork_current_state_write, select_branch_fork,
};
use crate::harness::session::session::{SessionError, SessionErrorKind, session_invariant_error};
use crate::harness::session::types::ForkOptions;
use crate::harness::session::values::physical_key;
use crate::harness::types::{FileSystem, TextLineReader};

use super::io::{file_value, parse_jsonl_transaction, publish_jsonl, read_jsonl_header};
use super::types::{JSONL_STORAGE_VERSION, JsonlStorageHeader};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonlForkSourceMetadata {
    pub id: String,
    pub cwd: String,
    pub path: String,
}

/// Prepared fork input: format-4 file metadata or an already-normalized legacy source.
#[derive(Debug, Clone)]
pub enum JsonlForkInput {
    Open {
        metadata: JsonlForkSourceMetadata,
        next_seq: i64,
    },
    Closed {
        metadata: JsonlForkSourceMetadata,
    },
}

impl JsonlForkInput {
    fn metadata(&self) -> &JsonlForkSourceMetadata {
        match self {
            JsonlForkInput::Open { metadata, .. } | JsonlForkInput::Closed { metadata } => metadata,
        }
    }
}

fn quote(value: &str) -> String {
    serde_json::Value::String(value.to_owned()).to_string()
}

async fn read_jsonl_fork_header(
    reader: &dyn TextLineReader,
    source: &JsonlForkSourceMetadata,
    context: &Context,
) -> Result<JsonlStorageHeader, SessionError> {
    let parsed = read_jsonl_header(reader, &source.path, context).await?;
    let header = match parsed {
        super::codec::JsonlParsedSessionHeader::V4(header) => header,
        super::codec::JsonlParsedSessionHeader::V3Legacy(_) => {
            return Err(SessionError::new(
                SessionErrorKind::Io,
                format!("Invalid JSONL storage {}: expected format 4 header", source.path),
            ));
        }
    };
    if header.id != source.id || header.cwd != source.cwd {
        return Err(session_invariant_error(format!(
            "Session identity does not match header: {}",
            source.id
        )));
    }
    if header.storage_version != JSONL_STORAGE_VERSION {
        return Err(session_invariant_error(format!(
            "Session {} uses unsupported storage version {}",
            source.id, header.storage_version
        )));
    }
    Ok(header)
}

fn reaches_fork_boundary(writes: &[CommittedWrite], stop_before_seq: Option<i64>) -> Result<bool, SessionError> {
    let Some(stop_before_seq) = stop_before_seq else {
        return Ok(false);
    };
    if writes.is_empty() {
        return Ok(false);
    }
    let first = writes.first().map(CommittedWrite::seq).unwrap_or_default();
    let last = writes.last().map(CommittedWrite::seq).unwrap_or_default();
    if first >= stop_before_seq {
        return Ok(true);
    }
    if last >= stop_before_seq {
        return Err(SessionError::new(
            SessionErrorKind::Io,
            format!("JSONL transaction crosses fork sequence boundary {stop_before_seq}"),
        ));
    }
    Ok(false)
}

/// Read complete transactions after the header, never splitting a transaction at the sequence boundary.
async fn read_jsonl_fork_transactions(
    reader: &dyn TextLineReader,
    path: &str,
    stop_before_seq: Option<i64>,
    context: &Context,
) -> Result<Vec<Vec<CommittedWrite>>, SessionError> {
    let mut transactions: Vec<Vec<CommittedWrite>> = Vec::new();
    loop {
        let line = file_value(
            reader.read_line(context).await,
            &format!("Failed to read JSONL fork source {path}"),
        )?;
        let Some(line) = line else {
            break;
        };
        if !line.terminated {
            break;
        }
        let writes = parse_jsonl_transaction(&line.text)?;
        if reaches_fork_boundary(&writes, stop_before_seq)? {
            break;
        }
        transactions.push(writes);
    }
    Ok(transactions)
}

#[derive(Default)]
pub struct JsonlForkIndex {
    current_scalar_seqs: HashMap<String, i64>,
    branch_tips: BTreeMap<String, Option<String>>,
    first_surviving_list_seqs: HashMap<String, i64>,
    entry_parents: HashMap<String, Option<String>>,
    copied_entry_ids: HashSet<String>,
    lane_configs: HashSet<String>,
    lane_states: HashSet<String>,
}

impl JsonlForkIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply_entry(&mut self, id: &str, parent_id: Option<String>) {
        self.entry_parents.insert(id.to_owned(), parent_id);
    }

    pub fn apply_writes(&mut self, writes: &[CommittedWrite]) {
        for write in writes {
            match write {
                CommittedWrite::Entry(entry) => self.apply_entry(&entry.id, entry.parent_id.clone()),
                CommittedWrite::Value(value) => {
                    let key = physical_key(value.namespace(), value.key());
                    match value {
                        CommittedValueWrite::Delete(_) => {
                            self.current_scalar_seqs.remove(&key);
                        }
                        CommittedValueWrite::Set(set) => {
                            self.current_scalar_seqs.insert(key, set.seq);
                        }
                    }
                    self.apply_lane_value(value);
                }
                CommittedWrite::List(list) => {
                    let key = physical_key(list.namespace(), list.key());
                    match list {
                        CommittedListWrite::Delete(_) => {
                            self.first_surviving_list_seqs.remove(&key);
                        }
                        CommittedListWrite::Append(append) => {
                            self.first_surviving_list_seqs.entry(key).or_insert(append.seq);
                        }
                    }
                }
                CommittedWrite::Usage(_) => {}
            }
        }
    }

    fn apply_lane_value(&mut self, write: &CommittedValueWrite) {
        let present = matches!(write, CommittedValueWrite::Set(_));
        match write.namespace() {
            "pi.branch.tip" => {
                if present {
                    let value = write
                        .value()
                        .and_then(|value| serde_json::from_value::<Option<String>>(value.clone()).ok())
                        .flatten();
                    self.branch_tips.insert(write.key().to_owned(), value);
                } else {
                    self.branch_tips.remove(write.key());
                }
            }
            "pi.lane.config" => {
                if present {
                    self.lane_configs.insert(write.key().to_owned());
                } else {
                    self.lane_configs.remove(write.key());
                }
            }
            "pi.lane.state" => {
                if present {
                    self.lane_states.insert(write.key().to_owned());
                } else {
                    self.lane_states.remove(write.key());
                }
            }
            _ => {}
        }
    }

    pub fn get_branch_tip(&self, branch: &str) -> Option<Option<String>> {
        self.branch_tips.get(branch).cloned()
    }

    pub fn has_complete_lane(&self, branch: &str) -> bool {
        self.lane_configs.contains(branch) && self.lane_states.contains(branch)
    }

    pub fn get_current_scalar_seq(&self, namespace: &str, key: &str) -> Option<i64> {
        self.current_scalar_seqs.get(&physical_key(namespace, key)).copied()
    }

    pub fn is_surviving_list_element(&self, namespace: &str, key: &str, seq: i64) -> bool {
        self.first_surviving_list_seqs
            .get(&physical_key(namespace, key))
            .is_some_and(|first_seq| seq >= *first_seq)
    }

    pub fn get_parent(&self, entry_id: &str) -> Option<Option<String>> {
        self.entry_parents.get(entry_id).cloned()
    }

    pub fn select_entry(&self, _entry_id: &str) {}

    pub fn is_entry_selected(&self, entry_id: &str) -> bool {
        self.copied_entry_ids.contains(entry_id)
    }

    pub fn select(&mut self, entry_id: &str) {
        self.copied_entry_ids.insert(entry_id.to_owned());
    }
}

/// Validate the source lanes and return the fork plan.
pub fn select_jsonl_fork(index: &JsonlForkIndex, options: &ForkOptions) -> Result<ForkCurrentStatePlan, SessionError> {
    if matches!(options, ForkOptions::Tree { .. }) {
        return Ok(ForkCurrentStatePlan::Tree);
    }
    let ForkOptions::Branch { branch, .. } = options else {
        unreachable!("tree scope handled above")
    };
    let selected: std::cell::RefCell<Vec<String>> = std::cell::RefCell::new(Vec::new());
    let plan = select_branch_fork(
        options,
        &crate::harness::session::fork_policy::BranchForkSource {
            tip: index.get_branch_tip(branch),
            get_parent: &|entry_id: &str| index.get_parent(entry_id),
            select_entry: &|entry_id: &str| selected.borrow_mut().push(entry_id.to_owned()),
        },
    )?;
    if !index.has_complete_lane(branch) {
        return Err(session_invariant_error(format!(
            "Source branch {} is not a configured AgentLane",
            quote(branch)
        )));
    }
    Ok(plan)
}

/// Selected entries collected by `select_branch_fork` through the index.
pub fn select_jsonl_fork_with_index(
    index: &mut JsonlForkIndex,
    options: &ForkOptions,
) -> Result<ForkCurrentStatePlan, SessionError> {
    if matches!(options, ForkOptions::Tree { .. }) {
        return Ok(ForkCurrentStatePlan::Tree);
    }
    let ForkOptions::Branch { branch, .. } = options else {
        unreachable!("tree scope handled above")
    };
    let selected: std::cell::RefCell<Vec<String>> = std::cell::RefCell::new(Vec::new());
    let plan = select_branch_fork(
        options,
        &crate::harness::session::fork_policy::BranchForkSource {
            tip: index.get_branch_tip(branch),
            get_parent: &|entry_id: &str| index.get_parent(entry_id),
            select_entry: &|entry_id: &str| selected.borrow_mut().push(entry_id.to_owned()),
        },
    )?;
    if !index.has_complete_lane(branch) {
        return Err(session_invariant_error(format!(
            "Source branch {} is not a configured AgentLane",
            quote(branch)
        )));
    }
    for entry_id in selected.into_inner() {
        index.select(&entry_id);
    }
    Ok(plan)
}

pub type JsonlForkWrite = CommittedWrite;

pub fn project_jsonl_fork_write(
    write: &CommittedWrite,
    index: &JsonlForkIndex,
    plan: &ForkCurrentStatePlan,
    is_entry_copied: &dyn Fn(&str) -> bool,
) -> Result<Option<CommittedWrite>, SessionError> {
    match write {
        CommittedWrite::Entry(entry) => Ok(if is_entry_copied(&entry.id) {
            Some(write.clone())
        } else {
            None
        }),
        CommittedWrite::Value(value) => {
            let CommittedValueWrite::Set(set) = value else {
                return Ok(None);
            };
            if index.get_current_scalar_seq(&set.namespace, &set.key) != Some(set.seq) {
                return Ok(None);
            }
            let row = ForkCurrentStateRow::Value(CommittedValueSetWrite {
                kind: crate::harness::session::commit::WriteKind::Value,
                op: crate::harness::session::commit::ValueOp::Set,
                seq: set.seq,
                namespace: set.namespace.clone(),
                key: set.key.clone(),
                value: set.value.clone(),
            });
            Ok(project_fork_current_state_write(&row, plan, is_entry_copied)?.map(|projected| match projected {
                ForkCurrentStateRow::Value(projected) => {
                    CommittedWrite::Value(CommittedValueWrite::Set(projected))
                }
                ForkCurrentStateRow::List(projected) => {
                    CommittedWrite::List(CommittedListWrite::Append(projected))
                }
            }))
        }
        CommittedWrite::List(list) => {
            let CommittedListWrite::Append(append) = list else {
                return Ok(None);
            };
            if !index.is_surviving_list_element(&append.namespace, &append.key, append.seq) {
                return Ok(None);
            }
            let row = ForkCurrentStateRow::List(CommittedListAppendWrite {
                kind: crate::harness::session::commit::WriteKind::List,
                op: crate::harness::session::commit::ListOp::Append,
                seq: append.seq,
                namespace: append.namespace.clone(),
                key: append.key.clone(),
                value: append.value.clone(),
            });
            Ok(project_fork_current_state_write(&row, plan, is_entry_copied)?.map(|projected| match projected {
                ForkCurrentStateRow::Value(projected) => {
                    CommittedWrite::Value(CommittedValueWrite::Set(projected))
                }
                ForkCurrentStateRow::List(projected) => {
                    CommittedWrite::List(CommittedListWrite::Append(projected))
                }
            }))
        }
        CommittedWrite::Usage(_) => Ok(None),
    }
}

/// Build the index used to select branch ancestry and identify current scalar/list writes.
pub async fn index_fork_input(
    input: &JsonlForkInput,
    file_system: &Arc<dyn FileSystem>,
    context: &Context,
) -> Result<(JsonlForkIndex, i64), SessionError> {
    let mut index = JsonlForkIndex::new();
    let metadata = input.metadata();
    let reader = file_value(
        file_system.open_text_line_reader(&metadata.path, context).await,
        &format!("Failed to open JSONL fork source {}", metadata.path),
    )?;
    let result = async {
        let header = read_jsonl_fork_header(reader.as_ref(), metadata, context).await?;
        let stop_before_seq = match input {
            JsonlForkInput::Open { next_seq, .. } => Some(*next_seq),
            JsonlForkInput::Closed { .. } => None,
        };
        let mut highest_complete_seq = 0;
        for writes in read_jsonl_fork_transactions(reader.as_ref(), &metadata.path, stop_before_seq, context).await? {
            index.apply_writes(&writes);
            if let Some(last) = writes.last() {
                highest_complete_seq = last.seq();
            }
        }
        let next_seq = match input {
            JsonlForkInput::Open { next_seq, .. } => *next_seq,
            JsonlForkInput::Closed { .. } => header.next_seq.unwrap_or(1).max(highest_complete_seq + 1),
        };
        Ok::<_, SessionError>((next_seq,))
    }
    .await;
    reader.close(context).await;
    let (next_seq,) = result?;
    Ok((index, next_seq))
}

/// Yield source writes; the caller owns final projection and filtering.
async fn stream_fork_writes(
    input: &JsonlForkInput,
    file_system: &Arc<dyn FileSystem>,
    stop_before_seq: i64,
    context: &Context,
) -> Result<Vec<CommittedWrite>, SessionError> {
    let metadata = input.metadata();
    let reader = file_value(
        file_system.open_text_line_reader(&metadata.path, context).await,
        &format!("Failed to open JSONL fork source {}", metadata.path),
    )?;
    let result = async {
        read_jsonl_fork_header(reader.as_ref(), metadata, context).await?;
        let transactions = read_jsonl_fork_transactions(reader.as_ref(), &metadata.path, Some(stop_before_seq), context).await?;
        Ok::<_, SessionError>(transactions.into_iter().flatten().collect())
    }
    .await;
    reader.close(context).await;
    result
}

/// Index the source, validate the requested fork, and stream selected entries and current state
/// into an atomically published format-4 destination without modifying the source.
pub async fn run_jsonl_fork(
    input: &JsonlForkInput,
    file_system: &Arc<dyn FileSystem>,
    destination_path: &str,
    destination_header: &JsonlStorageHeader,
    fork: &ForkOptions,
    context: &Context,
) -> Result<(), SessionError> {
    let (mut index, next_seq) = index_fork_input(input, file_system, context).await?;
    let plan = select_jsonl_fork_with_index(&mut index, fork)?;
    let is_entry_copied = |entry_id: &str| -> bool {
        match plan {
            ForkCurrentStatePlan::Tree => true,
            ForkCurrentStatePlan::Branch { .. } => index.is_entry_selected(entry_id),
        }
    };
    let writes = stream_fork_writes(input, file_system, next_seq, context).await?;
    let mut selected: Vec<CommittedWrite> = Vec::new();
    for write in &writes {
        if let Some(projected) = project_jsonl_fork_write(write, &index, &plan, &is_entry_copied)? {
            selected.push(projected);
        }
    }
    let mut header = destination_header.clone();
    header.next_seq = Some(next_seq);
    publish_jsonl(
        file_system.clone(),
        destination_path.to_owned(),
        &header,
        context,
        Arc::new(move |append| {
            let selected = selected.clone();
            Box::pin(async move {
                for write in selected {
                    append(vec![write]).await?;
                }
                Ok(())
            })
        }),
    )
    .await
}

pub type JsonlForkWriteList = Vec<CommittedWrite>;

pub type JsonlEntryWrite = CommittedEntryWrite;
