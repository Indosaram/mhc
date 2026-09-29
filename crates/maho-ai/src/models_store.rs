//! Port of senpi packages/ai/src/models-store.ts.

use crate::types::{BoxFuture, Model};
use crate::utils::abort::{AbortReason, AbortSignal};
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelsStoreEntry {
    pub models: Vec<Model>,
    /// Unix timestamp from the remote catalog's Last-Modified header.
    pub last_modified: Option<i64>,
    /// Unix timestamp of the last completed remote check.
    pub checked_at: Option<i64>,
    /// Opaque ETag validator, stored verbatim (quotes included) and echoed back as If-None-Match.
    pub etag: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ModelsStoreOperationOptions {
    pub signal: Option<AbortSignal>,
}

fn throw_if_aborted(options: Option<&ModelsStoreOperationOptions>) -> Result<(), AbortReason> {
    options.and_then(|o| o.signal.as_ref()).map_or(Ok(()), AbortSignal::throw_if_aborted)
}

/// Persistent model catalogs keyed by provider ID.
pub trait ModelsStore: Send + Sync {
    fn read<'a>(&'a self, provider_id: &'a str, options: Option<&'a ModelsStoreOperationOptions>)
    -> BoxFuture<'a, Result<Option<ModelsStoreEntry>, AbortReason>>;
    fn write<'a>(
        &'a self,
        provider_id: &'a str,
        entry: &'a ModelsStoreEntry,
        options: Option<&'a ModelsStoreOperationOptions>,
    ) -> BoxFuture<'a, Result<(), AbortReason>>;
    fn delete<'a>(&'a self, provider_id: &'a str, options: Option<&'a ModelsStoreOperationOptions>)
    -> BoxFuture<'a, Result<(), AbortReason>>;
}

#[derive(Debug, Default)]
pub struct InMemoryModelsStore {
    entries: Mutex<HashMap<String, ModelsStoreEntry>>,
}

impl InMemoryModelsStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn entries(&self) -> std::sync::MutexGuard<'_, HashMap<String, ModelsStoreEntry>> {
        self.entries.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl ModelsStore for InMemoryModelsStore {
    fn read<'a>(&'a self, provider_id: &'a str, options: Option<&'a ModelsStoreOperationOptions>)
    -> BoxFuture<'a, Result<Option<ModelsStoreEntry>, AbortReason>> {
        Box::pin(async move {
            throw_if_aborted(options)?;
            Ok(self.entries().get(provider_id).cloned())
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
            self.entries().insert(provider_id.to_owned(), entry.clone());
            Ok(())
        })
    }

    fn delete<'a>(&'a self, provider_id: &'a str, options: Option<&'a ModelsStoreOperationOptions>)
    -> BoxFuture<'a, Result<(), AbortReason>> {
        Box::pin(async move {
            throw_if_aborted(options)?;
            self.entries().remove(provider_id);
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::abort::AbortController;

    #[tokio::test]
    async fn round_trips_clones_and_honors_abort() {
        let store = InMemoryModelsStore::new();
        let entry = ModelsStoreEntry { etag: Some("\"v1\"".into()), ..ModelsStoreEntry::default() };
        store.write("p", &entry, None).await.expect("write");
        assert_eq!(store.read("p", None).await.expect("read"), Some(entry));
        store.delete("p", None).await.expect("delete");
        assert_eq!(store.read("p", None).await.expect("read"), None);
        let controller = AbortController::new();
        controller.abort(None);
        let options = ModelsStoreOperationOptions { signal: Some(controller.signal()) };
        assert!(store.read("p", Some(&options)).await.is_err());
    }
}
