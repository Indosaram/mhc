//! Port of senpi `packages/coding-agent/src/modes/rpc/state-file-store.ts`.
//!
//! A durable STATE store in one owner-only JSON file (senpi#6767), using locked read-check-write
//! and atomic fsynced rename.

use crate::ownership_safe_lock::{LockError, LockRetries, acquire_ownership_safe_lock};
use maho_agent::durable::{
    CommitResult, DurableStoreError, DurableStoreErrorCode, StoreDocument, StoreRecord,
    StoreSnapshot, StoreValue, StoreWrite,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, thiserror::Error)]
#[error("the state file {path} is unreadable: {what}")]
pub struct StateFileCorruptError {
    pub path: String,
    pub what: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PersistedState {
    epoch: u64,
    #[serde(default)]
    records: Vec<StoreRecord>,
    documents: Vec<(String, StoreValue)>,
}

fn read_state(path: &Path) -> Result<PersistedState, String> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PersistedState::default());
        }
        Err(err) => return Err(err.to_string()),
    };
    serde_json::from_str(&text).map_err(|err| {
        StateFileCorruptError {
            path: path.display().to_string(),
            what: err.to_string(),
        }
        .to_string()
    })
}

fn write_state(path: &Path, state: &PersistedState) -> Result<(), String> {
    let pid = std::process::id();
    let nonce: u64 = rand_nonce();
    let temp = PathBuf::from(format!("{}.{pid}.{nonce:012x}.tmp", path.display()));
    #[cfg(unix)]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt;
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
            .map_err(|e| e.to_string())?
    };
    #[cfg(not(unix))]
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|e| e.to_string())?;
    let encoded = serde_json::to_vec(state).map_err(|e| e.to_string())?;
    file.write_all(&encoded).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);
    fs::rename(&temp, path).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    if let Some(parent) = path.parent()
        && let Ok(dir) = fs::File::open(parent)
    {
        let _ = dir.sync_all();
    }
    Ok(())
}

fn rand_nonce() -> u64 {
    let mut bytes = [0u8; 8];
    let _ = getrandom::fill(&mut bytes);
    u64::from_le_bytes(bytes)
}

fn store_io_err(msg: String) -> DurableStoreError {
    DurableStoreError::Other(msg)
}

fn lock_err(err: LockError) -> DurableStoreError {
    store_io_err(err.to_string())
}

fn require_owner(state: &PersistedState, epoch: u64) -> Result<(), DurableStoreError> {
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

/// Durable STATE store backed by an owner-only JSON file and an `OwnershipSafeLock`.
pub struct StateFileStore {
    path: PathBuf,
    lock_path: PathBuf,
}

impl StateFileStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, std::io::Error> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                let mut builder = fs::DirBuilder::new();
                builder.recursive(true).mode(0o700);
                builder.create(parent)?;
            }
            #[cfg(not(unix))]
            {
                fs::create_dir_all(parent)?;
            }
        }
        let lock_path = PathBuf::from(format!("{}.lock", path.display()));
        Ok(Self { path, lock_path })
    }

    pub fn read_epoch_sync(&self) -> Result<u64, DurableStoreError> {
        let state = read_state(&self.path).map_err(store_io_err)?;
        Ok(state.epoch)
    }

    pub fn read_document_sync(&self, key: &str) -> Result<Option<StoreValue>, DurableStoreError> {
        let state = read_state(&self.path).map_err(store_io_err)?;
        Ok(state
            .documents
            .into_iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v))
    }

    pub fn list_documents_sync(
        &self,
        prefix: Option<&str>,
    ) -> Result<BTreeMap<String, StoreValue>, DurableStoreError> {
        let state = read_state(&self.path).map_err(store_io_err)?;
        Ok(state
            .documents
            .into_iter()
            .filter(|(k, _)| prefix.is_none_or(|p| k.starts_with(p)))
            .collect())
    }

    pub async fn claim(&self, expected: u64, next: u64) -> Result<(), DurableStoreError> {
        let mut lock = acquire_ownership_safe_lock(&self.lock_path, LockRetries::default())
            .await
            .map_err(lock_err)?;
        let res = {
            let mut state = read_state(&self.path).map_err(store_io_err)?;
            if next <= state.epoch {
                Err(DurableStoreError::new(
                    DurableStoreErrorCode::StaleEpoch,
                    format!("epoch {next} is not above current {}", state.epoch),
                ))
            } else if expected != state.epoch {
                Err(DurableStoreError::new(
                    DurableStoreErrorCode::StaleEpoch,
                    format!("expected epoch {expected}, found {}", state.epoch),
                ))
            } else {
                state.epoch = next;
                write_state(&self.path, &state).map_err(store_io_err)
            }
        };
        let _ = lock.release();
        res
    }

    pub async fn commit(
        &self,
        epoch: u64,
        writes: &[StoreWrite],
    ) -> Result<CommitResult, DurableStoreError> {
        let mut lock = acquire_ownership_safe_lock(&self.lock_path, LockRetries::default())
            .await
            .map_err(lock_err)?;
        let res = {
            let mut state = read_state(&self.path).map_err(store_io_err)?;
            let check = require_owner(&state, epoch);
            if let Err(err) = check {
                Err(err)
            } else {
                let mut docs: BTreeMap<String, StoreValue> = state.documents.into_iter().collect();
                let mut ids: HashSet<String> = state.records.iter().map(|r| r.id.clone()).collect();
                let mut seqs = Vec::new();
                let mut dup_err = None;
                for write in writes {
                    match write {
                        StoreWrite::Append { id, kind, data } => {
                            if !ids.insert(id.clone()) {
                                dup_err = Some(DurableStoreError::new(
                                    DurableStoreErrorCode::DuplicateRecordId,
                                    format!("record {id} exists"),
                                ));
                                break;
                            }
                            let seq = (state.records.len() as u64) + 1;
                            state.records.push(StoreRecord {
                                seq,
                                id: id.clone(),
                                kind: kind.clone(),
                                data: data.clone(),
                            });
                            seqs.push(seq);
                        }
                        StoreWrite::Put { key, data } => {
                            docs.insert(key.clone(), data.clone());
                        }
                        StoreWrite::Delete { key } => {
                            docs.remove(key);
                        }
                    }
                }
                if let Some(err) = dup_err {
                    Err(err)
                } else {
                    state.documents = docs.into_iter().collect();
                    let head = state.records.len() as u64;
                    write_state(&self.path, &state).map_err(store_io_err)?;
                    Ok(CommitResult { seqs, head })
                }
            }
        };
        let _ = lock.release();
        res
    }

    pub async fn import_snapshot(
        &self,
        epoch: u64,
        snapshot: StoreSnapshot,
    ) -> Result<(), DurableStoreError> {
        let mut lock = acquire_ownership_safe_lock(&self.lock_path, LockRetries::default())
            .await
            .map_err(lock_err)?;
        let res = {
            let mut state = read_state(&self.path).map_err(store_io_err)?;
            if let Err(err) = require_owner(&state, epoch) {
                Err(err)
            } else if epoch <= snapshot.epoch {
                Err(DurableStoreError::new(
                    DurableStoreErrorCode::InvalidClaim,
                    format!(
                        "epoch {epoch} is not above the source epoch {}",
                        snapshot.epoch
                    ),
                ))
            } else {
                state.records = snapshot.records;
                state.documents = snapshot
                    .documents
                    .into_iter()
                    .map(|StoreDocument { key, data }| (key, data))
                    .collect();
                write_state(&self.path, &state).map_err(store_io_err)
            }
        };
        let _ = lock.release();
        res
    }

    pub async fn claim_next_epoch(&self) -> Result<u64, DurableStoreError> {
        loop {
            let current = self.read_epoch_sync()?;
            let next = current + 1;
            match self.claim(current, next).await {
                Ok(()) => return Ok(next),
                Err(err) if err.code() == Some(DurableStoreErrorCode::StaleEpoch) => continue,
                Err(err) => return Err(err),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn claims_epoch_and_commits_documents_atomically() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state/wake.json");
        let store = StateFileStore::open(&path).expect("open");
        let epoch = store.claim_next_epoch().await.expect("claim");
        assert_eq!(epoch, 1);
        store
            .commit(
                epoch,
                &[StoreWrite::Put {
                    key: "k1".to_owned(),
                    data: json!({"ok": true}),
                }],
            )
            .await
            .expect("commit");
        assert_eq!(
            store.read_document_sync("k1").expect("read"),
            Some(json!({"ok": true}))
        );
        let stale = store.claim(0, 2).await.expect_err("stale");
        assert_eq!(stale.code(), Some(DurableStoreErrorCode::StaleEpoch));
    }
}
