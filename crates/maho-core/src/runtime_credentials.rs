//! Port of senpi packages/coding-agent/src/core/runtime-credentials.ts.
//!
//! Async credential-store overlay for non-persistent runtime API keys. The OAuth registry hooks of
//! senpi's extension surface have no Rust counterpart (extensions are native), so those two methods
//! are N/A here.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use maho_ai::auth::types::{
    ApiKeyCredential, AuthOperationOptions, Credential, CredentialInfo, CredentialStore, CredentialType,
};

type ModifyFn = Box<
    dyn FnOnce(Option<Credential>) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<Option<Credential>>> + Send>>
        + Send,
>;

pub struct RuntimeCredentials {
    store: Arc<dyn CredentialStore>,
    overrides: Mutex<BTreeMap<String, String>>,
}

impl RuntimeCredentials {
    pub fn new(store: Arc<dyn CredentialStore>) -> Self {
        Self { store, overrides: Mutex::new(BTreeMap::new()) }
    }

    pub fn set_runtime_api_key(&self, provider_id: &str, api_key: &str) {
        self.overrides
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(provider_id.to_owned(), api_key.to_owned());
    }

    pub fn remove_runtime_api_key(&self, provider_id: &str) {
        self.overrides.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(provider_id);
    }

    pub fn has_runtime_api_key(&self, provider_id: &str) -> bool {
        self.overrides.lock().unwrap_or_else(std::sync::PoisonError::into_inner).contains_key(provider_id)
    }

    fn override_for(&self, provider_id: &str) -> Option<String> {
        self.overrides.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(provider_id).cloned()
    }
}

fn throw_if_aborted(options: &Option<AuthOperationOptions>) -> anyhow::Result<()> {
    if let Some(signal) = options.as_ref().and_then(|options| options.signal.as_ref()) {
        signal.throw_if_aborted().map_err(|reason| anyhow::anyhow!("{reason}"))?;
    }
    Ok(())
}

#[async_trait::async_trait]
impl CredentialStore for RuntimeCredentials {
    async fn read(&self, provider_id: &str, options: Option<AuthOperationOptions>) -> anyhow::Result<Option<Credential>> {
        throw_if_aborted(&options)?;
        if let Some(key) = self.override_for(provider_id) {
            return Ok(Some(Credential::ApiKey(ApiKeyCredential { key: Some(key), env: None })));
        }
        self.store.read(provider_id, options).await
    }

    async fn list(&self, options: Option<AuthOperationOptions>) -> anyhow::Result<Vec<CredentialInfo>> {
        let mut entries: BTreeMap<String, CredentialInfo> = self
            .store
            .list(options.clone())
            .await?
            .into_iter()
            .map(|entry| (entry.provider_id.clone(), entry))
            .collect();
        throw_if_aborted(&options)?;
        let overrides = self.overrides.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        for provider_id in overrides.keys() {
            entries.insert(provider_id.clone(), CredentialInfo { provider_id: provider_id.clone(), credential_type: CredentialType::ApiKey });
        }
        Ok(entries.into_values().collect())
    }

    async fn modify(
        &self,
        provider_id: &str,
        f: ModifyFn,
        options: Option<AuthOperationOptions>,
    ) -> anyhow::Result<Option<Credential>> {
        self.store.modify(provider_id, f, options).await
    }

    async fn delete(&self, provider_id: &str, options: Option<AuthOperationOptions>) -> anyhow::Result<()> {
        throw_if_aborted(&options)?;
        self.store.delete(provider_id, options).await?;
        self.overrides.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(provider_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct MemoryStore {
        entries: Mutex<BTreeMap<String, Credential>>,
    }

    #[async_trait::async_trait]
    impl CredentialStore for MemoryStore {
        async fn read(&self, provider_id: &str, _options: Option<AuthOperationOptions>) -> anyhow::Result<Option<Credential>> {
            Ok(self.entries.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(provider_id).cloned())
        }

        async fn list(&self, _options: Option<AuthOperationOptions>) -> anyhow::Result<Vec<CredentialInfo>> {
            Ok(self
                .entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .iter()
                .map(|(provider_id, credential)| CredentialInfo { provider_id: provider_id.clone(), credential_type: credential.credential_type() })
                .collect())
        }

        async fn modify(
            &self,
            provider_id: &str,
            f: ModifyFn,
            _options: Option<AuthOperationOptions>,
        ) -> anyhow::Result<Option<Credential>> {
            let current = self.entries.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(provider_id).cloned();
            let next = f(current).await?;
            let mut entries = self.entries.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            match &next {
                Some(credential) => {
                    entries.insert(provider_id.to_owned(), credential.clone());
                }
                None => {
                    entries.remove(provider_id);
                }
            }
            Ok(next)
        }

        async fn delete(&self, provider_id: &str, _options: Option<AuthOperationOptions>) -> anyhow::Result<()> {
            self.entries.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(provider_id);
            Ok(())
        }
    }

    fn store() -> Arc<dyn CredentialStore> {
        Arc::new(MemoryStore::default())
    }

    #[tokio::test]
    async fn a_runtime_key_overrides_the_underlying_store() {
        let credentials = RuntimeCredentials::new(store());
        credentials.set_runtime_api_key("p", "runtime-key");
        assert!(credentials.has_runtime_api_key("p"));
        let read = credentials.read("p", None).await.expect("read").expect("present");
        assert_eq!(read.as_api_key().and_then(|key| key.key.clone()), Some("runtime-key".to_owned()));
    }

    #[tokio::test]
    async fn removing_a_runtime_key_falls_back_to_the_store() {
        let credentials = RuntimeCredentials::new(store());
        credentials.set_runtime_api_key("p", "runtime-key");
        credentials.remove_runtime_api_key("p");
        assert!(!credentials.has_runtime_api_key("p"));
        assert!(credentials.read("p", None).await.expect("read").is_none());
    }

    #[tokio::test]
    async fn listing_includes_runtime_keys() {
        let credentials = RuntimeCredentials::new(store());
        credentials.set_runtime_api_key("p", "k");
        let list = credentials.list(None).await.expect("list");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].provider_id, "p");
        assert_eq!(list[0].credential_type, CredentialType::ApiKey);
    }

    #[tokio::test]
    async fn deleting_a_provider_removes_its_runtime_key() {
        let credentials = RuntimeCredentials::new(store());
        credentials.set_runtime_api_key("p", "k");
        credentials.delete("p", None).await.expect("delete");
        assert!(!credentials.has_runtime_api_key("p"));
    }
}
