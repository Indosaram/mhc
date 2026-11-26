use maho_ai::auth::types::{AuthOperationOptions, Credential, CredentialInfo, CredentialStore};
use maho_core::auth_storage::AuthStorage;
use serde_json::Value;
use std::{future::Future, path::Path, pin::Pin};

pub(crate) struct DiskStore(AuthStorage);

type Mutation = Box<
    dyn FnOnce(
            Option<Credential>,
        ) -> Pin<Box<dyn Future<Output = anyhow::Result<Option<Credential>>> + Send>>
        + Send,
>;

impl DiskStore {
    pub(crate) fn new(path: &Path) -> Self {
        Self(AuthStorage::create(path.to_str().expect("auth path")))
    }
}

async fn mutate_value(current: Option<Value>, update: Mutation) -> Result<Option<Value>, String> {
    let current = current
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| error.to_string())?;
    update(current)
        .await
        .map_err(|error| error.to_string())?
        .map(serde_json::to_value)
        .transpose()
        .map_err(|error| error.to_string())
}

#[async_trait::async_trait]
impl CredentialStore for DiskStore {
    async fn read(
        &self,
        provider: &str,
        _: Option<AuthOperationOptions>,
    ) -> anyhow::Result<Option<Credential>> {
        self.0
            .get(provider)
            .map(serde_json::from_value)
            .transpose()
            .map_err(Into::into)
    }

    async fn modify(
        &self,
        provider: &str,
        update: Mutation,
        _: Option<AuthOperationOptions>,
    ) -> anyhow::Result<Option<Credential>> {
        let stored = self
            .0
            .modify_credential(provider, |current| mutate_value(current, update))
            .await
            .map_err(anyhow::Error::msg)?;
        stored
            .map(serde_json::from_value)
            .transpose()
            .map_err(Into::into)
    }

    async fn modify_or_delete(
        &self,
        provider: &str,
        update: Mutation,
        _: Option<AuthOperationOptions>,
    ) -> anyhow::Result<Option<Credential>> {
        let stored = self
            .0
            .modify_or_delete_credential(provider, |current| mutate_value(current, update))
            .await
            .map_err(anyhow::Error::msg)?;
        stored
            .map(serde_json::from_value)
            .transpose()
            .map_err(Into::into)
    }

    async fn delete(&self, provider: &str, _: Option<AuthOperationOptions>) -> anyhow::Result<()> {
        self.0.delete(provider).map_err(anyhow::Error::msg)
    }

    async fn list(&self, _: Option<AuthOperationOptions>) -> anyhow::Result<Vec<CredentialInfo>> {
        anyhow::bail!("fixture reads only the named provider")
    }
}
