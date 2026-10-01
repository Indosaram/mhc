//! Port of senpi packages/coding-agent/src/core/models-store.ts.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use maho_ai::models_store::{ModelsStore, ModelsStoreEntry, ModelsStoreOperationOptions};
use maho_ai::types::BoxFuture;
use maho_ai::utils::abort::AbortReason;

use crate::auth_storage::FileAuthStorageBackend;
use crate::config::get_agent_dir;
use crate::paths::{PathInputOptions, get_file_revision, normalize_path};
use crate::text::strip_bom;

/// The in-memory coding-agent store; senpi declares its own, maho-ai's is behaviorally identical.
pub use maho_ai::models_store::InMemoryModelsStore as InMemoryCodingAgentModelsStore;

type StoredModels = BTreeMap<String, ModelsStoreEntry>;

#[derive(Default)]
struct ModelsFileReadState {
    data: StoredModels,
    revision: Option<String>,
}

/// Locked JSON-backed storage for dynamically refreshed provider catalogs.
pub struct FileModelsStore {
    storage: FileAuthStorageBackend,
    path: String,
    read_state: Arc<Mutex<ModelsFileReadState>>,
}

impl FileModelsStore {
    pub fn new(path: Option<&str>) -> Self {
        let path = normalize_path(
            path.unwrap_or(&format!("{}/models-store.json", get_agent_dir())),
            &PathInputOptions::default(),
        );
        Self { storage: FileAuthStorageBackend::new(&path), path, read_state: Arc::new(Mutex::new(ModelsFileReadState::default())) }
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    /// ModelsStoreEntry carries no serde derive in maho-ai, so the JSON mapping is explicit here.
    fn parse(content: Option<&str>) -> StoredModels {
        let Some(content) = content.filter(|content| !content.is_empty()) else { return StoredModels::new() };
        let Ok(serde_json::Value::Object(entries)) = serde_json::from_str::<serde_json::Value>(strip_bom(content)) else {
            return StoredModels::new();
        };
        entries
            .into_iter()
            .filter_map(|(provider, value)| entry_from_json(&value).map(|entry| (provider, entry)))
            .collect()
    }

    fn read_state(&self) -> std::sync::MutexGuard<'_, ModelsFileReadState> {
        self.read_state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    async fn read_latest(&self, options: Option<&ModelsStoreOperationOptions>) -> Result<StoredModels, AbortReason> {
        throw_if_aborted(options)?;
        let revision = get_file_revision(&self.path);
        {
            let state = self.read_state();
            if revision.is_some() && revision == state.revision {
                return Ok(state.data.clone());
            }
        }
        let data = self
            .storage
            .with_lock_async(async |content| {
                let parsed = Self::parse(content);
                Ok((parsed, None))
            })
            .await
            .map_err(|_| AbortReason::dom_default())?;
        let mut state = self.read_state();
        state.data = data.clone();
        state.revision = revision;
        Ok(data)
    }
}

fn entry_from_json(value: &serde_json::Value) -> Option<ModelsStoreEntry> {
    let object = value.as_object()?;
    let models = object.get("models")?.as_array()?.iter().filter_map(|model| serde_json::from_value(model.clone()).ok()).collect();
    Some(ModelsStoreEntry {
        models,
        last_modified: object.get("lastModified").and_then(serde_json::Value::as_i64),
        checked_at: object.get("checkedAt").and_then(serde_json::Value::as_i64),
        etag: object.get("etag").and_then(serde_json::Value::as_str).map(str::to_owned),
    })
}

fn entry_to_json(entry: &ModelsStoreEntry) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    object.insert("models".into(), serde_json::to_value(&entry.models).unwrap_or(serde_json::Value::Array(Vec::new())));
    if let Some(last_modified) = entry.last_modified {
        object.insert("lastModified".into(), serde_json::Value::from(last_modified));
    }
    if let Some(checked_at) = entry.checked_at {
        object.insert("checkedAt".into(), serde_json::Value::from(checked_at));
    }
    if let Some(etag) = &entry.etag {
        object.insert("etag".into(), serde_json::Value::String(etag.clone()));
    }
    serde_json::Value::Object(object)
}

fn models_to_json(models: &StoredModels) -> String {
    let object: serde_json::Map<String, serde_json::Value> =
        models.iter().map(|(provider, entry)| (provider.clone(), entry_to_json(entry))).collect();
    serde_json::to_string_pretty(&serde_json::Value::Object(object)).unwrap_or_default()
}

fn throw_if_aborted(options: Option<&ModelsStoreOperationOptions>) -> Result<(), AbortReason> {
    options.and_then(|options| options.signal.as_ref()).map_or(Ok(()), maho_ai::utils::abort::AbortSignal::throw_if_aborted)
}

impl ModelsStore for FileModelsStore {
    fn read<'a>(
        &'a self,
        provider_id: &'a str,
        options: Option<&'a ModelsStoreOperationOptions>,
    ) -> BoxFuture<'a, Result<Option<ModelsStoreEntry>, AbortReason>> {
        Box::pin(async move {
            let latest = self.read_latest(options).await?;
            throw_if_aborted(options)?;
            Ok(latest.get(provider_id).cloned())
        })
    }

    fn write<'a>(
        &'a self,
        provider_id: &'a str,
        entry: &'a ModelsStoreEntry,
        options: Option<&'a ModelsStoreOperationOptions>,
    ) -> BoxFuture<'a, Result<(), AbortReason>> {
        Box::pin(async move {
            throw_if_aborted(options)?;
            let latest = self
                .storage
                .with_lock_async(async |content| {
                    let mut current = Self::parse(content);
                    current.insert(provider_id.to_owned(), entry.clone());
                    let next = models_to_json(&current);
                    Ok((current, Some(next)))
                })
                .await
                .map_err(|_| AbortReason::dom_default())?;
            self.read_state().data = latest;
            Ok(())
        })
    }

    fn delete<'a>(
        &'a self,
        provider_id: &'a str,
        options: Option<&'a ModelsStoreOperationOptions>,
    ) -> BoxFuture<'a, Result<(), AbortReason>> {
        Box::pin(async move {
            throw_if_aborted(options)?;
            let latest = self
                .storage
                .with_lock_async(async |content| {
                    let mut current = Self::parse(content);
                    current.remove(provider_id);
                    let next = models_to_json(&current);
                    Ok((current, Some(next)))
                })
                .await
                .map_err(|_| AbortReason::dom_default())?;
            self.read_state().data = latest;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(model_id: &str) -> ModelsStoreEntry {
        let model: maho_ai::model::Model = serde_json::from_value(serde_json::json!({
            "id": model_id, "name": "M", "api": "openai-completions", "provider": "p", "baseUrl": "",
            "reasoning": false, "input": ["text"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 1000, "maxTokens": 100
        }))
        .expect("model");
        ModelsStoreEntry { models: vec![model], ..ModelsStoreEntry::default() }
    }

    #[tokio::test]
    async fn the_in_memory_store_round_trips() {
        let store = InMemoryCodingAgentModelsStore::new();
        store.write("p", &entry("a"), None).await.expect("write");
        let read = store.read("p", None).await.expect("read").expect("present");
        assert_eq!(read.models.len(), 1);
        store.delete("p", None).await.expect("delete");
        assert!(store.read("p", None).await.expect("read").is_none());
    }

    #[tokio::test]
    async fn the_file_store_persists_across_instances() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("models-store.json").to_string_lossy().into_owned();
        let store = FileModelsStore::new(Some(&path));
        store.write("p", &entry("a"), None).await.expect("write");
        let reopened = FileModelsStore::new(Some(&path));
        let read = reopened.read("p", None).await.expect("read").expect("present");
        assert_eq!(read.models.len(), 1);
    }

    #[tokio::test]
    async fn a_missing_entry_reads_as_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("models-store.json").to_string_lossy().into_owned();
        let store = FileModelsStore::new(Some(&path));
        assert!(store.read("missing", None).await.expect("read").is_none());
    }

    #[tokio::test]
    async fn delete_removes_the_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("models-store.json").to_string_lossy().into_owned();
        let store = FileModelsStore::new(Some(&path));
        store.write("p", &entry("a"), None).await.expect("write");
        store.delete("p", None).await.expect("delete");
        assert!(store.read("p", None).await.expect("read").is_none());
    }
}
