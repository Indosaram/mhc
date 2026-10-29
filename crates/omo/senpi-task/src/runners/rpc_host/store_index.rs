//! Append-only task store admission (`rpc-host/store-index.ts`).
use super::durable_json::{read_text_if_present, write_text_durably};
use crate::store::{StoreError, with_task_record_lock_options};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};
pub const TASK_STORE_INDEX_VERSION: u8 = 1;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreIndexEntry {
    pub first_seen: String,
    pub last_seen: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskStoreIndex {
    pub version: u8,
    pub stores: BTreeMap<String, StoreIndexEntry>,
}
pub fn task_store_index_path(agent_dir: &Path) -> PathBuf {
    agent_dir.join("rpc/task-stores.json")
}
pub fn parse_store_index(text: Option<&str>) -> Result<TaskStoreIndex, StoreError> {
    let index = match text {
        None => TaskStoreIndex {
            version: 1,
            stores: BTreeMap::new(),
        },
        Some(text) => serde_json::from_str::<TaskStoreIndex>(text)
            .map_err(|error| StoreError::Parse(error.to_string()))?,
    };
    if index.version != 1 {
        return Err(StoreError::Parse(
            "the store index is not a version 1 index".into(),
        ));
    }
    Ok(index)
}
fn normalized(path: &Path) -> std::io::Result<PathBuf> {
    Ok(std::path::absolute(path)?
        .components()
        .fold(PathBuf::new(), |mut result, component| {
            match component {
                std::path::Component::ParentDir => {
                    result.pop();
                }
                std::path::Component::CurDir => {}
                other => result.push(other.as_os_str()),
            }
            result
        }))
}
pub fn read_task_store_index(path: &Path) -> Result<TaskStoreIndex, StoreError> {
    parse_store_index(read_text_if_present(path)?.as_deref())
}
static ADMISSIONS: OnceLock<Mutex<BTreeMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
pub async fn register_store_index(
    index_path: PathBuf,
    store_dir: PathBuf,
    now: i64,
) -> Result<(), StoreError> {
    let key = normalized(&index_path)?;
    let lock = ADMISSIONS
        .get_or_init(Default::default)
        .lock()
        .expect("hosted child state lock poisoned")
        .entry(key.clone())
        .or_default()
        .clone();
    tokio::task::spawn_blocking(move || {
        let _guard = lock.lock().expect("hosted child state lock poisoned");
        admit_to_store_index(&key, &store_dir, now)
    })
    .await
    .map_err(|error| StoreError::Parse(error.to_string()))?
}
pub fn admit_to_store_index(index: &Path, store: &Path, now: i64) -> Result<(), StoreError> {
    if let Some(parent) = index.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let store = normalized(store)?.to_string_lossy().into_owned();
    with_task_record_lock_options(
        index,
        || {
            let mut current = read_task_store_index(index)?;
            if current.stores.contains_key(&store) {
                return Ok(());
            }
            let timestamp = chrono::DateTime::from_timestamp_millis(now)
                .ok_or_else(|| StoreError::Parse("invalid timestamp".into()))?
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            current.stores.insert(
                store.clone(),
                StoreIndexEntry {
                    first_seen: timestamp.clone(),
                    last_seen: timestamp,
                },
            );
            write_text_durably(
                index,
                &format!(
                    "{}\n",
                    serde_json::to_string_pretty(&current)
                        .map_err(|error| StoreError::Parse(error.to_string()))?
                ),
            )?;
            if !read_task_store_index(index)?.stores.contains_key(&store) {
                return Err(StoreError::Parse(
                    "the written index does not read back the store".into(),
                ));
            }
            Ok(())
        },
        std::time::Duration::from_millis(super::durable_json::DURABLE_JSON_LOCK_WAIT_MS),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn concurrent_admission_retains_both_stores_and_known_entry_is_not_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let index = task_store_index_path(dir.path());
        let a = register_store_index(index.clone(), dir.path().join("a"), 0);
        let b = register_store_index(index.clone(), dir.path().join("b"), 1);
        let (a, b) = tokio::join!(a, b);
        a.unwrap();
        b.unwrap();
        let before = std::fs::read(&index).unwrap();
        register_store_index(index.clone(), dir.path().join("a"), 2)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&index).unwrap(), before);
        assert_eq!(read_task_store_index(&index).unwrap().stores.len(), 2);
    }
    #[test]
    fn corrupt_index_is_not_empty() {
        assert!(parse_store_index(Some("broken")).is_err());
        assert!(parse_store_index(Some("{\"version\":2,\"stores\":{}}")).is_err());
    }
}
