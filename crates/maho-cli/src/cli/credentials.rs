//! CLI-owned `CredentialStore` over `auth.json`.
//!
//! senpi's credential store is the app's auth file (`auth.json`); maho-core ports the file as
//! `AuthStorage` but publishes no `CredentialStore` implementation over it, and `maho-ai` ships only
//! `InMemoryCredentialStore`. The CLI owns the app-level auth file, so the adapter lives here rather
//! than in either owner crate.
//!
//! Semantics mirror `maho_ai::auth::credential_store::InMemoryCredentialStore` exactly: one entry per
//! provider, `modify` as the only write path, per-provider serialization, abort-aware reads, and
//! `read` resolving `None` for a missing entry. `Credential` already serializes to auth.json's
//! `{"type": "api_key" | "oauth", ...}` shape (`#[serde(tag = "type")]`), so the mapping is a plain
//! serde round-trip and the stored bytes keep the file format `AuthStorage` reads.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use maho_ai::auth::types::{AuthOperationOptions, Credential, CredentialInfo, CredentialStore, CredentialType};
use maho_ai::utils::abort::{operation_signal, race_with_abort_signal};
use maho_core::auth_storage::{AuthStorage, CredentialKind};
use tokio::sync::Mutex as AsyncMutex;

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// `AuthStorage`-backed credential store. `auth.json` is the single source of truth.
pub struct AuthStorageCredentialStore {
    storage: Arc<AsyncMutex<AuthStorage>>,
    locks: std::sync::Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
}

impl AuthStorageCredentialStore {
    pub fn new(storage: AuthStorage) -> Self {
        Self { storage: Arc::new(AsyncMutex::new(storage)), locks: std::sync::Mutex::new(HashMap::new()) }
    }

    /// senpi's default store: `<agent_dir>/auth.json`.
    pub fn create_default() -> Self {
        Self::new(AuthStorage::create_default())
    }

    pub fn create(auth_path: &str) -> Self {
        Self::new(AuthStorage::create(auth_path))
    }

    /// Shared handle for callers that hold the store as `Arc<dyn CredentialStore>`.
    pub fn into_shared(self) -> Arc<dyn CredentialStore> {
        Arc::new(self)
    }

    fn lock_for(&self, provider_id: &str) -> Arc<AsyncMutex<()>> {
        let mut locks = self.locks.lock().unwrap_or_else(|poison| poison.into_inner());
        locks.entry(provider_id.to_owned()).or_insert_with(|| Arc::new(AsyncMutex::new(()))).clone()
    }

    async fn enqueue<'a, T: Send + 'a>(
        &'a self,
        provider_id: &str,
        task: impl Future<Output = T> + Send + 'a,
        options: Option<AuthOperationOptions>,
    ) -> anyhow::Result<T> {
        let signal = operation_signal(options.and_then(|options| options.signal));
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

    /// The stored credential as the file holds it, converted to the typed form.
    fn decode(value: serde_json::Value) -> anyhow::Result<Credential> {
        serde_json::from_value(value).map_err(|error| anyhow::anyhow!("stored credential is not a valid auth.json entry: {error}"))
    }

    /// The typed credential as `auth.json` stores it.
    fn encode(credential: &Credential) -> anyhow::Result<serde_json::Value> {
        serde_json::to_value(credential).map_err(|error| anyhow::anyhow!("credential cannot be written to auth.json: {error}"))
    }

    fn check_abort(options: &Option<AuthOperationOptions>) -> anyhow::Result<()> {
        if let Some(signal) = options.as_ref().and_then(|options| options.signal.as_ref())
            && let Err(reason) = signal.throw_if_aborted()
        {
            anyhow::bail!("{reason}");
        }
        Ok(())
    }
}

fn credential_type(kind: CredentialKind) -> CredentialType {
    match kind {
        CredentialKind::ApiKey => CredentialType::ApiKey,
        CredentialKind::Oauth => CredentialType::OAuth,
    }
}

#[async_trait]
impl CredentialStore for AuthStorageCredentialStore {
    async fn read(&self, provider_id: &str, options: Option<AuthOperationOptions>) -> anyhow::Result<Option<Credential>> {
        Self::check_abort(&options)?;
        let storage = self.storage.lock().await;
        match storage.get(provider_id) {
            None => Ok(None),
            Some(value) => Self::decode(value).map(Some),
        }
    }

    async fn list(&self, options: Option<AuthOperationOptions>) -> anyhow::Result<Vec<CredentialInfo>> {
        Self::check_abort(&options)?;
        let storage = self.storage.lock().await;
        Ok(storage
            .list()
            .into_iter()
            .map(|(provider_id, kind)| CredentialInfo { provider_id, credential_type: credential_type(kind) })
            .collect())
    }

    async fn modify(
        &self,
        provider_id: &str,
        f: Box<dyn FnOnce(Option<Credential>) -> BoxFuture<'static, anyhow::Result<Option<Credential>>> + Send>,
        options: Option<AuthOperationOptions>,
    ) -> anyhow::Result<Option<Credential>> {
        let signal_options = options.clone();
        let storage = Arc::clone(&self.storage);
        let provider_id_owned = provider_id.to_owned();
        let task = async move {
            let current = {
                let guard = storage.lock().await;
                match guard.get(&provider_id_owned) {
                    None => None,
                    Some(value) => Some(Self::decode(value)?),
                }
            };
            let next = f(current.clone()).await?;
            if let Some(next) = &next {
                let encoded = Self::encode(next)?;
                let mut guard = storage.lock().await;
                guard.set(&provider_id_owned, Some(encoded)).map_err(|error| anyhow::anyhow!("{error}"))?;
            }
            Ok(next.or(current))
        };
        self.enqueue(provider_id, task, signal_options).await?
    }

    async fn delete(&self, provider_id: &str, options: Option<AuthOperationOptions>) -> anyhow::Result<()> {
        let storage = Arc::clone(&self.storage);
        let provider_id_owned = provider_id.to_owned();
        let task = async move {
            let mut guard = storage.lock().await;
            guard.delete(&provider_id_owned).map_err(|error| anyhow::anyhow!("{error}"))?;
            Ok::<(), anyhow::Error>(())
        };
        self.enqueue(provider_id, task, options).await?
    }
}
