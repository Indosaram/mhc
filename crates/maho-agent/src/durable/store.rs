//! Port of senpi `packages/agent/src/durable/store.ts`.

use std::sync::Arc;

use maho_ai::types::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A JSON value as the store persists it.
pub type StoreValue = Value;

/// One log record. `seq` is assigned by the store: gapless from 1, in commit order. `id` is unique in the log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoreRecord {
    pub seq: u64,
    pub id: String,
    pub kind: String,
    pub data: StoreValue,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoreDocument {
    pub key: String,
    pub data: StoreValue,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum StoreWrite {
    Append {
        id: String,
        kind: String,
        data: StoreValue,
    },
    Put {
        key: String,
        data: StoreValue,
    },
    Delete {
        key: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitResult {
    /// The sequence numbers of the batch's appends, in write order.
    pub seqs: Vec<u64>,
    /// The last committed sequence number after this batch (0 for an empty log).
    pub head: u64,
}

/// Everything a store holds; the unit of claim + import between hosts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoreSnapshot {
    pub epoch: u64,
    pub head: u64,
    /// Every record, ascending by `seq`.
    pub records: Vec<StoreRecord>,
    /// Every document, ascending by `key`.
    pub documents: Vec<StoreDocument>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanOrder {
    Asc,
    Desc,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecordScan {
    pub from_seq: Option<u64>,
    pub to_seq: Option<u64>,
    pub kind: Option<String>,
    pub order: Option<ScanOrder>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StoreOperation {
    Commit,
    Claim,
    Import,
}

/// A point inside a durable write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "at", rename_all = "lowercase")]
pub enum StoreWritePoint {
    Before { op: StoreOperation },
    Write { op: StoreOperation, index: usize },
    Durable { op: StoreOperation },
}

pub type OnWritePoint = Arc<dyn Fn(StoreWritePoint) -> Result<(), DurableStoreError> + Send + Sync>;

#[derive(Clone, Default)]
pub struct DurableStoreOptions {
    pub on_write_point: Option<OnWritePoint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DurableStoreErrorCode {
    Unclaimed,
    StaleEpoch,
    InvalidClaim,
    DuplicateRecordId,
    ImportConflict,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DurableStoreError {
    #[error("{message}")]
    Store {
        code: DurableStoreErrorCode,
        message: String,
    },
    #[error("{0}")]
    Cut(String),
    #[error("{0}")]
    Other(String),
}

impl DurableStoreError {
    pub fn new(code: DurableStoreErrorCode, message: impl Into<String>) -> Self {
        Self::Store {
            code,
            message: message.into(),
        }
    }

    pub fn code(&self) -> Option<DurableStoreErrorCode> {
        match self {
            Self::Store { code, .. } => Some(*code),
            Self::Cut(_) | Self::Other(_) => None,
        }
    }

    pub fn is_cut(&self) -> bool {
        matches!(self, Self::Cut(_))
    }
}

pub type AdoptCallback<'a> = Box<dyn FnOnce(&CommitResult) + Send + 'a>;

pub trait DurableStore: Send + Sync {
    /// The current owner epoch; 0 when the store was never claimed.
    fn read_epoch(&self) -> BoxFuture<'_, Result<u64, DurableStoreError>>;
    /// The last committed sequence number; 0 for an empty log.
    fn read_head(&self) -> BoxFuture<'_, Result<u64, DurableStoreError>>;
    /// Compare-and-set the owner epoch from `expected_epoch` to `next_epoch`.
    fn claim(
        &self,
        expected_epoch: u64,
        next_epoch: u64,
    ) -> BoxFuture<'_, Result<(), DurableStoreError>>;
    /// Commit a batch atomically under the owner `epoch`.
    fn commit<'a>(
        &'a self,
        epoch: u64,
        writes: Vec<StoreWrite>,
        adopt: Box<dyn FnOnce(&CommitResult) + Send + 'a>,
    ) -> BoxFuture<'a, Result<CommitResult, DurableStoreError>>;
    /// A new ID, unique across instances, reopens and hosts.
    fn mint_id(&self) -> String;
    fn scan_records(
        &self,
        query: RecordScan,
    ) -> BoxFuture<'_, Result<Vec<StoreRecord>, DurableStoreError>>;
    fn get_document(
        &self,
        key: &str,
    ) -> BoxFuture<'_, Result<Option<StoreValue>, DurableStoreError>>;
    /// Documents whose key starts with `prefix`, ascending by key.
    fn scan_documents(
        &self,
        prefix: &str,
    ) -> BoxFuture<'_, Result<Vec<StoreDocument>, DurableStoreError>>;
    fn export_snapshot(&self) -> BoxFuture<'_, Result<StoreSnapshot, DurableStoreError>>;
    /// Import another host's snapshot under this store's current owner `epoch`.
    fn import_snapshot(
        &self,
        epoch: u64,
        snapshot: StoreSnapshot,
    ) -> BoxFuture<'_, Result<(), DurableStoreError>>;
}

/// A backend whose commit is durable when it returns.
pub trait SyncCommit: DurableStore {
    fn commit_sync(
        &self,
        epoch: u64,
        writes: Vec<StoreWrite>,
        adopt: &mut dyn FnMut(&CommitResult),
    ) -> Result<CommitResult, DurableStoreError>;
}

pub trait SyncCommitStore: DurableStore + SyncCommit {}

impl<T: DurableStore + SyncCommit> SyncCommitStore for T {}
