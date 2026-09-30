//! Port of senpi packages/ai/src/auth/credential-store.ts.

use crate::auth::types::{AuthOperationOptions, Credential, CredentialInfo, CredentialStore};
use crate::utils::abort::{operation_signal, race_with_abort_signal};
use async_trait::async_trait;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;
use tokio::sync::Mutex as AsyncMutex;

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Default)]
pub struct InMemoryCredentialStore {
    credentials: Mutex<HashMap<String, Credential>>,
    locks: Mutex<HashMap<String, std::sync::Arc<AsyncMutex<()>>>>,
}

impl InMemoryCredentialStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock_for(&self, provider_id: &str) -> std::sync::Arc<AsyncMutex<()>> {
        let mut locks = self.locks.lock().unwrap_or_else(|p| p.into_inner());
        locks.entry(provider_id.to_string()).or_insert_with(|| std::sync::Arc::new(AsyncMutex::new(()))).clone()
    }

    async fn enqueue<'a, T: Send + 'a>(
        &'a self,
        provider_id: &str,
        task: impl Future<Output = T> + Send + 'a,
        options: Option<AuthOperationOptions>,
    ) -> anyhow::Result<T> {
        let signal = operation_signal(options.and_then(|o| o.signal));
        let lock = self.lock_for(provider_id);
        let guard = lock.lock().await;
        if let Err(reason) = signal.throw_if_aborted() {
            drop(guard);
            anyhow::bail!("{reason}");
        }
        let result = race_with_abort_signal(task, &signal).await;
        drop(guard);
        result.map_err(|reason| anyhow::anyhow!("{reason}"))
    }
}

#[async_trait]
impl CredentialStore for InMemoryCredentialStore {
    async fn read(
        &self,
        provider_id: &str,
        options: Option<AuthOperationOptions>,
    ) -> anyhow::Result<Option<Credential>> {
        if let Some(opts) = &options
            && let Some(signal) = &opts.signal {
                signal.throw_if_aborted().map_err(|reason| anyhow::anyhow!("{reason}"))?;
            }
        let credentials = self.credentials.lock().unwrap_or_else(|p| p.into_inner());
        Ok(credentials.get(provider_id).cloned())
    }

    async fn list(&self, options: Option<AuthOperationOptions>) -> anyhow::Result<Vec<CredentialInfo>> {
        if let Some(opts) = &options
            && let Some(signal) = &opts.signal {
                signal.throw_if_aborted().map_err(|reason| anyhow::anyhow!("{reason}"))?;
            }
        let credentials = self.credentials.lock().unwrap_or_else(|p| p.into_inner());
        Ok(credentials
            .iter()
            .map(|(provider_id, credential)| CredentialInfo {
                provider_id: provider_id.clone(),
                credential_type: credential.credential_type(),
            })
            .collect())
    }

    async fn modify(
        &self,
        provider_id: &str,
        f: Box<dyn FnOnce(Option<Credential>) -> BoxFuture<'static, anyhow::Result<Option<Credential>>> + Send>,
        options: Option<AuthOperationOptions>,
    ) -> anyhow::Result<Option<Credential>> {
        let signal_opts = options.clone();
        let current = {
            let credentials = self.credentials.lock().unwrap_or_else(|p| p.into_inner());
            credentials.get(provider_id).cloned()
        };
        let credentials_lock = &self.credentials;
        let provider_id_owned = provider_id.to_string();
        let task = async move {
            let next = f(current.clone()).await?;
            if let Some(next) = &next {
                let mut credentials = credentials_lock.lock().unwrap_or_else(|p| p.into_inner());
                credentials.insert(provider_id_owned.clone(), next.clone());
            }
            Ok(next.or(current))
        };
        self.enqueue(provider_id, task, signal_opts).await?
    }

    async fn delete(&self, provider_id: &str, options: Option<AuthOperationOptions>) -> anyhow::Result<()> {
        let provider_id_owned = provider_id.to_string();
        let credentials_lock = &self.credentials;
        let task = async move {
            let mut credentials = credentials_lock.lock().unwrap_or_else(|p| p.into_inner());
            credentials.remove(&provider_id_owned);
            Ok::<(), anyhow::Error>(())
        };
        self.enqueue(provider_id, task, options).await?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::types::{ApiKeyCredential, OAuthCredential};

    fn box_fut<F>(f: F) -> BoxFuture<'static, anyhow::Result<Option<Credential>>>
    where
        F: Future<Output = anyhow::Result<Option<Credential>>> + Send + 'static,
    {
        Box::pin(f)
    }

    #[tokio::test]
    async fn read_resolves_none_for_missing_entry() {
        let store = InMemoryCredentialStore::new();
        assert_eq!(store.read("anthropic", None).await.unwrap(), None);
    }

    #[tokio::test]
    async fn modify_writes_and_read_reflects_it() {
        let store = InMemoryCredentialStore::new();
        let written = store
            .modify(
                "anthropic",
                Box::new(|_current| box_fut(async { Ok(Some(Credential::ApiKey(ApiKeyCredential { key: Some("k".into()), env: None }))) })),
                None,
            )
            .await
            .unwrap();
        assert_eq!(written.as_ref().and_then(Credential::as_api_key).and_then(|c| c.key.clone()), Some("k".into()));
        let read_back = store.read("anthropic", None).await.unwrap();
        assert_eq!(read_back, written);
    }

    #[tokio::test]
    async fn modify_returning_none_leaves_entry_unchanged() {
        let store = InMemoryCredentialStore::new();
        store
            .modify("p", Box::new(|_| box_fut(async { Ok(Some(Credential::OAuth(OAuthCredential::new("a", "r", 1.0)))) })), None)
            .await
            .unwrap();
        let result = store.modify("p", Box::new(|current| box_fut(async move { Ok::<Option<Credential>, anyhow::Error>(None).map(|_| current) })), None).await.unwrap();
        assert_eq!(result.and_then(|c| c.as_oauth().cloned()).map(|c| c.access), Some("a".into()));
    }

    #[tokio::test]
    async fn delete_removes_entry() {
        let store = InMemoryCredentialStore::new();
        store.modify("p", Box::new(|_| box_fut(async { Ok(Some(Credential::OAuth(OAuthCredential::new("a", "r", 1.0)))) })), None).await.unwrap();
        store.delete("p", None).await.unwrap();
        assert_eq!(store.read("p", None).await.unwrap(), None);
    }

    #[tokio::test]
    async fn list_reports_metadata_without_secrets() {
        let store = InMemoryCredentialStore::new();
        store.modify("p1", Box::new(|_| box_fut(async { Ok(Some(Credential::ApiKey(ApiKeyCredential { key: Some("secret".into()), env: None }))) })), None).await.unwrap();
        let listed = store.list(None).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].provider_id, "p1");
        assert_eq!(listed[0].credential_type, crate::auth::types::CredentialType::ApiKey);
    }

    #[tokio::test]
    async fn writes_are_serialized_per_provider() {
        let store = std::sync::Arc::new(InMemoryCredentialStore::new());
        let order = std::sync::Arc::new(Mutex::new(Vec::<u8>::new()));
        let (o1, o2) = (order.clone(), order.clone());
        let s1 = store.clone();
        let h1 = tokio::spawn(async move {
            s1.modify(
                "p",
                Box::new(move |_| box_fut(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    o1.lock().unwrap_or_else(|p| p.into_inner()).push(1);
                    Ok(Some(Credential::ApiKey(ApiKeyCredential { key: Some("1".into()), env: None })))
                })),
                None,
            )
            .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let s2 = store.clone();
        let h2 = tokio::spawn(async move {
            s2.modify(
                "p",
                Box::new(move |_| box_fut(async move {
                    o2.lock().unwrap_or_else(|p| p.into_inner()).push(2);
                    Ok(Some(Credential::ApiKey(ApiKeyCredential { key: Some("2".into()), env: None })))
                })),
                None,
            )
            .await
        });
        h1.await.unwrap().unwrap();
        h2.await.unwrap().unwrap();
        assert_eq!(*order.lock().unwrap_or_else(|p| p.into_inner()), vec![1, 2]);
    }
}
