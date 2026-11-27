//! Port of senpi `packages/agent/src/durable/store-memory.ts`.

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};

use maho_ai::types::BoxFuture;
use uuid::Uuid;

use super::store::{
    CommitResult, DurableStore, DurableStoreError, DurableStoreErrorCode, DurableStoreOptions,
    OnWritePoint, RecordScan, ScanOrder, StoreDocument, StoreOperation, StoreRecord, StoreSnapshot,
    StoreValue, StoreWrite, StoreWritePoint, SyncCommit,
};

#[derive(Clone, Default)]
struct MediumState {
    epoch: u64,
    records: Vec<StoreRecord>,
    documents: BTreeMap<String, StoreValue>,
}

/// The memory backend's durable medium. It outlives every store opened on it, so a kill is dropping an instance.
#[derive(Clone, Default)]
pub struct MemoryStoreMedium {
    current: Arc<Mutex<MediumState>>,
}

pub fn create_memory_store_medium() -> MemoryStoreMedium {
    MemoryStoreMedium::default()
}

pub fn utf16_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

fn same_value(a: &StoreValue, b: &StoreValue) -> bool {
    a == b
}

fn same_record(a: &StoreRecord, b: Option<&StoreRecord>) -> bool {
    let Some(b) = b else {
        return false;
    };
    a.seq == b.seq && a.id == b.id && a.kind == b.kind && same_value(&a.data, &b.data)
}

fn require_owner(state: &MediumState, epoch: u64) -> Result<(), DurableStoreError> {
    if state.epoch == 0 {
        return Err(DurableStoreError::new(
            DurableStoreErrorCode::Unclaimed,
            "the store has no owner; claim it first",
        ));
    }
    if epoch != state.epoch {
        return Err(DurableStoreError::new(
            DurableStoreErrorCode::StaleEpoch,
            format!("epoch {epoch} is not the owner epoch {}", state.epoch),
        ));
    }
    Ok(())
}

fn imported_tail<'a>(
    state: &MediumState,
    snapshot: &'a StoreSnapshot,
) -> Result<&'a [StoreRecord], DurableStoreError> {
    let mut ids = HashSet::new();
    let well_formed = snapshot.head as usize == snapshot.records.len()
        && snapshot.records.iter().enumerate().all(|(index, record)| {
            record.seq == (index as u64) + 1 && ids.insert(record.id.as_str())
        });
    let extends_log = state.records.len() <= snapshot.records.len()
        && state
            .records
            .iter()
            .enumerate()
            .all(|(index, record)| same_record(record, snapshot.records.get(index)));
    if !well_formed || !extends_log {
        return Err(DurableStoreError::new(
            DurableStoreErrorCode::ImportConflict,
            "the snapshot's log does not extend this store's log",
        ));
    }
    Ok(&snapshot.records[state.records.len()..])
}

fn scan(records: &[StoreRecord], query: &RecordScan) -> Vec<StoreRecord> {
    let mut matching: Vec<StoreRecord> = records
        .iter()
        .filter(|record| {
            query.from_seq.is_none_or(|from| record.seq >= from)
                && query.to_seq.is_none_or(|to| record.seq <= to)
                && query.kind.as_deref().is_none_or(|kind| record.kind == kind)
        })
        .cloned()
        .collect();
    if query.order == Some(ScanOrder::Desc) {
        matching.reverse();
    }
    if let Some(limit) = query.limit {
        matching.truncate(limit);
    }
    matching
}

fn documents_of(state: &MediumState, prefix: &str) -> Vec<StoreDocument> {
    let mut docs: Vec<StoreDocument> = state
        .documents
        .iter()
        .filter(|(key, _)| key.starts_with(prefix))
        .map(|(key, data)| StoreDocument {
            key: key.clone(),
            data: data.clone(),
        })
        .collect();
    docs.sort_by(|a, b| utf16_cmp(&a.key, &b.key));
    docs
}

#[derive(Clone)]
pub struct MemoryStore {
    medium: MemoryStoreMedium,
    on_write_point: Option<OnWritePoint>,
}

impl MemoryStore {
    fn point(&self, at: StoreWritePoint) -> Result<(), DurableStoreError> {
        if let Some(hook) = &self.on_write_point {
            hook(at)?;
        }
        Ok(())
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, MediumState> {
        self.medium
            .current
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// A DurableStore over `medium`. Each operation stages its writes and publishes them in one assignment: atomic.
pub fn open_memory_store(medium: MemoryStoreMedium, options: DurableStoreOptions) -> MemoryStore {
    MemoryStore {
        medium,
        on_write_point: options.on_write_point,
    }
}

impl SyncCommit for MemoryStore {
    fn commit_sync(
        &self,
        epoch: u64,
        writes: Vec<StoreWrite>,
        adopt: &mut dyn FnMut(&CommitResult),
    ) -> Result<CommitResult, DurableStoreError> {
        self.point(StoreWritePoint::Before {
            op: StoreOperation::Commit,
        })?;
        let result = {
            let mut guard = self.lock_state();
            require_owner(&guard, epoch)?;
            let mut records = guard.records.clone();
            let mut ids: HashSet<String> = records.iter().map(|record| record.id.clone()).collect();
            let mut documents = guard.documents.clone();
            let mut seqs = Vec::new();
            for (index, write) in writes.into_iter().enumerate() {
                self.point(StoreWritePoint::Write {
                    op: StoreOperation::Commit,
                    index,
                })?;
                match write {
                    StoreWrite::Append { id, kind, data } => {
                        if !ids.insert(id.clone()) {
                            return Err(DurableStoreError::new(
                                DurableStoreErrorCode::DuplicateRecordId,
                                format!("record {id} exists"),
                            ));
                        }
                        let seq = (records.len() as u64) + 1;
                        records.push(StoreRecord {
                            seq,
                            id,
                            kind,
                            data,
                        });
                        seqs.push(seq);
                    }
                    StoreWrite::Put { key, data } => {
                        documents.insert(key, data);
                    }
                    StoreWrite::Delete { key } => {
                        documents.remove(&key);
                    }
                }
            }
            guard.records = records;
            guard.documents = documents;
            CommitResult {
                seqs,
                head: guard.records.len() as u64,
            }
        };
        self.point(StoreWritePoint::Durable {
            op: StoreOperation::Commit,
        })?;
        adopt(&result);
        Ok(result)
    }
}

impl DurableStore for MemoryStore {
    fn read_epoch(&self) -> BoxFuture<'_, Result<u64, DurableStoreError>> {
        Box::pin(async move { Ok(self.lock_state().epoch) })
    }

    fn read_head(&self) -> BoxFuture<'_, Result<u64, DurableStoreError>> {
        Box::pin(async move { Ok(self.lock_state().records.len() as u64) })
    }

    fn claim(
        &self,
        expected_epoch: u64,
        next_epoch: u64,
    ) -> BoxFuture<'_, Result<(), DurableStoreError>> {
        Box::pin(async move {
            self.point(StoreWritePoint::Before {
                op: StoreOperation::Claim,
            })?;
            {
                let mut guard = self.lock_state();
                if next_epoch != expected_epoch.saturating_add(1) {
                    return Err(DurableStoreError::new(
                        DurableStoreErrorCode::InvalidClaim,
                        format!(
                            "a claim must raise the epoch to exactly {}",
                            expected_epoch.saturating_add(1)
                        ),
                    ));
                }
                if guard.epoch != expected_epoch {
                    return Err(DurableStoreError::new(
                        DurableStoreErrorCode::StaleEpoch,
                        format!(
                            "expected epoch {expected_epoch}, the owner epoch is {}",
                            guard.epoch
                        ),
                    ));
                }
                guard.epoch = next_epoch;
            }
            self.point(StoreWritePoint::Durable {
                op: StoreOperation::Claim,
            })?;
            Ok(())
        })
    }

    fn commit<'a>(
        &'a self,
        epoch: u64,
        writes: Vec<StoreWrite>,
        mut adopt: Box<dyn FnOnce(&CommitResult) + Send + 'a>,
    ) -> BoxFuture<'a, Result<CommitResult, DurableStoreError>> {
        Box::pin(async move {
            let mut adopt_once = Some(&mut adopt);
            let mut runner = |result: &CommitResult| {
                if let Some(slot) = adopt_once.take() {
                    let cb = std::mem::replace(slot, Box::new(|_| {}));
                    cb(result);
                }
            };
            self.commit_sync(epoch, writes, &mut runner)
        })
    }

    fn mint_id(&self) -> String {
        Uuid::new_v4().to_string()
    }

    fn scan_records(
        &self,
        query: RecordScan,
    ) -> BoxFuture<'_, Result<Vec<StoreRecord>, DurableStoreError>> {
        Box::pin(async move {
            let guard = self.lock_state();
            Ok(scan(&guard.records, &query))
        })
    }

    fn get_document(
        &self,
        key: &str,
    ) -> BoxFuture<'_, Result<Option<StoreValue>, DurableStoreError>> {
        let key = key.to_owned();
        Box::pin(async move {
            let guard = self.lock_state();
            Ok(guard.documents.get(&key).cloned())
        })
    }

    fn scan_documents(
        &self,
        prefix: &str,
    ) -> BoxFuture<'_, Result<Vec<StoreDocument>, DurableStoreError>> {
        let prefix = prefix.to_owned();
        Box::pin(async move {
            let guard = self.lock_state();
            Ok(documents_of(&guard, &prefix))
        })
    }

    fn export_snapshot(&self) -> BoxFuture<'_, Result<StoreSnapshot, DurableStoreError>> {
        Box::pin(async move {
            let guard = self.lock_state();
            Ok(StoreSnapshot {
                epoch: guard.epoch,
                head: guard.records.len() as u64,
                records: guard.records.clone(),
                documents: documents_of(&guard, ""),
            })
        })
    }

    fn import_snapshot(
        &self,
        epoch: u64,
        snapshot: StoreSnapshot,
    ) -> BoxFuture<'_, Result<(), DurableStoreError>> {
        Box::pin(async move {
            self.point(StoreWritePoint::Before {
                op: StoreOperation::Import,
            })?;
            {
                let mut guard = self.lock_state();
                require_owner(&guard, epoch)?;
                if epoch <= snapshot.epoch {
                    return Err(DurableStoreError::new(
                        DurableStoreErrorCode::InvalidClaim,
                        format!(
                            "epoch {epoch} is not above the source epoch {}",
                            snapshot.epoch
                        ),
                    ));
                }
                let tail = imported_tail(&guard, &snapshot)?;
                let mut records = guard.records.clone();
                let mut documents = BTreeMap::new();
                let mut index = 0usize;
                for record in tail {
                    self.point(StoreWritePoint::Write {
                        op: StoreOperation::Import,
                        index,
                    })?;
                    index += 1;
                    records.push(record.clone());
                }
                for document in &snapshot.documents {
                    self.point(StoreWritePoint::Write {
                        op: StoreOperation::Import,
                        index,
                    })?;
                    index += 1;
                    documents.insert(document.key.clone(), document.data.clone());
                }
                guard.records = records;
                guard.documents = documents;
            }
            self.point(StoreWritePoint::Durable {
                op: StoreOperation::Import,
            })?;
            Ok(())
        })
    }
}
