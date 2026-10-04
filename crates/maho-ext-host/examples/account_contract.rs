use maho_core::{auth_storage::AuthStorage, credential_accounts, credential_pool::state_store::CredentialSlotRepository};
use maho_ext_api::{CredentialAccountSource, CredentialAccountSummary, ExtensionFailure, ExtensionFuture, Model, ModelRegistry};
use serde_json::json;
use std::sync::Arc;

struct SyntheticRegistry { storage: Arc<tokio::sync::Mutex<AuthStorage>>, repository: Arc<CredentialSlotRepository> }
impl ModelRegistry for SyntheticRegistry {
    fn get_all(&self) -> Vec<Model> { Vec::new() }
    fn get_available(&self) -> Vec<Model> { Vec::new() }
    fn find(&self, _: &str, _: &str) -> Option<Model> { None }
    fn has_configured_auth(&self, _: &Model) -> bool { false }
    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<String>> { Box::pin(async { Ok(None) }) }
    fn get_credential_accounts<'a>(&'a self, provider: &'a str) -> ExtensionFuture<'a, Vec<CredentialAccountSummary>> {
        let storage = self.storage.clone();
        let repository = self.repository.clone();
        let provider = provider.to_owned();
        Box::pin(async move {
            let accounts = tokio::task::spawn_blocking(move || {
                let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|error| error.to_string())?;
                runtime.block_on(async {
                    let storage = storage.lock().await;
                    credential_accounts::get_credential_accounts(&storage, &provider, &|_| None, &repository, 0).await
                })
            }).await.map_err(|error| ExtensionFailure::new(error.to_string()))??;
            Ok(accounts.into_iter().map(|account| CredentialAccountSummary {
                name: account.name, display_name: account.display_name, blocked: account.blocked, pinned: account.pinned,
                source: match account.source {
                    credential_accounts::CredentialAccountSource::Login => CredentialAccountSource::Login,
                    credential_accounts::CredentialAccountSource::Import => CredentialAccountSource::Import,
                    credential_accounts::CredentialAccountSource::Env => CredentialAccountSource::Env,
                },
            }).collect())
        })
    }
    fn pin_credential_account<'a>(&'a self, provider: &'a str, name: Option<&'a str>) -> ExtensionFuture<'a, ()> {
        let storage = self.storage.clone();
        let repository = self.repository.clone();
        let provider = provider.to_owned();
        let name = name.map(str::to_owned);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|error| error.to_string())?;
                runtime.block_on(async {
                    let storage = storage.lock().await;
                    credential_accounts::pin_credential_account(&storage, &provider, name.as_deref(), &|_| None, &repository, 0).await
                })
            }).await.map_err(|error| ExtensionFailure::new(error.to_string()))??;
            Ok(())
        })
    }
    fn remove_credential_account<'a>(&'a self, provider: &'a str, name: &'a str) -> ExtensionFuture<'a, ()> {
        let storage = self.storage.clone();
        let repository = self.repository.clone();
        let provider = provider.to_owned();
        let name = name.to_owned();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|error| error.to_string())?;
                runtime.block_on(async {
                    let storage = storage.lock().await;
                    credential_accounts::remove_credential_account(&storage, &provider, &name, &|_| None, &repository, 0).await
                })
            }).await.map_err(|error| ExtensionFailure::new(error.to_string()))??;
            Ok(())
        })
    }
    fn rename_credential_account<'a>(&'a self, provider: &'a str, name: &'a str, display_name: Option<&'a str>) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            let storage = self.storage.lock().await;
            credential_accounts::rename_credential_account(&storage, provider, name, display_name).await?;
            Ok(())
        })
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let auth_path = directory.path().join("auth.json");
    let storage = AuthStorage::create(&auth_path.to_string_lossy());
    storage.set("synthetic", Some(json!({"type":"api_key", "key":"synthetic-secret", "accounts":[
        {"name":"first", "key":"synthetic-secret", "source":"login"},
        {"name":"second", "key":"synthetic-secret", "source":"import"}
    ]}))).map_err(ExtensionFailure::from)?;
    let registry = SyntheticRegistry { storage: Arc::new(tokio::sync::Mutex::new(storage)),
        repository: Arc::new(CredentialSlotRepository::new(&directory.path().join("credential-pool-state.json").to_string_lossy())) };
    let facade: &dyn ModelRegistry = &registry;
    let summaries = facade.get_credential_accounts("synthetic").await?;
    assert_eq!(summaries.len(), 2);
    assert!(!format!("{summaries:?}").contains("synthetic-secret"));
    facade.pin_credential_account("synthetic", Some("second")).await?;
    assert!(facade.get_credential_accounts("synthetic").await?[1].pinned);
    facade.pin_credential_account("synthetic", None).await?;
    assert!(facade.get_credential_accounts("synthetic").await?.iter().all(|account| !account.pinned));
    facade.rename_credential_account("synthetic", "second", Some("Work Account")).await?;
    assert_eq!(facade.get_credential_accounts("synthetic").await?[1].display_name.as_deref(), Some("Work Account"));
    facade.rename_credential_account("synthetic", "second", None).await?;
    assert!(facade.get_credential_accounts("synthetic").await?[1].display_name.is_none());
    let error = facade.remove_credential_account("synthetic", "missing").await.expect_err("missing account");
    assert_eq!(error.message, "Provider account not found: missing");
    facade.remove_credential_account("synthetic", "second").await?;
    let reopened = AuthStorage::create(&auth_path.to_string_lossy());
    let persisted = credential_accounts::get_credential_accounts(&reopened, "synthetic", &|_| None, &registry.repository, 0).await.map_err(ExtensionFailure::from)?;
    assert_eq!(persisted.len(), 1);
    assert_eq!(persisted[0].name, "first");
    println!("account_contract: pass=true list pin unpin rename clear-name remove; persisted=true secret_output=false");
    drop(registry);
    directory.close()?;
    println!("cleanup: synthetic auth and sidecar directory removed; no provider, credentials, PTY or child process used");
    Ok(())
}
