//! Source-backed storage contracts for the account facade; no real credentials or process env.
use maho_core::{auth_storage::AuthStorage, credential_accounts::*, credential_pool::state_store::CredentialSlotRepository};
use serde_json::json;

fn fixture() -> (tempfile::TempDir, AuthStorage, CredentialSlotRepository) {
    let directory = tempfile::tempdir().expect("isolated account directory");
    let storage = AuthStorage::create(&directory.path().join("auth.json").to_string_lossy());
    storage.set("synthetic", Some(json!({"type":"api_key", "key":"synthetic-secret-a", "accounts":[
        {"name":"first", "key":"synthetic-secret-a", "source":"login"},
        {"name":"second", "key":"synthetic-secret-b", "source":"import", "blockReason":"auth_error"}
    ]}))).expect("persist synthetic credential pool");
    let repository = CredentialSlotRepository::new(&directory.path().join("credential-pool-state.json").to_string_lossy());
    (directory, storage, repository)
}

#[tokio::test]
async fn summaries_expose_identity_and_health_without_material() {
    let (_directory, storage, repository) = fixture();
    let accounts = get_credential_accounts(&storage, "synthetic", &|_| None, &repository, 0).await.unwrap();
    assert_eq!(accounts.len(), 2);
    assert_eq!(accounts[0].source, CredentialAccountSource::Login);
    assert_eq!(accounts[1].source, CredentialAccountSource::Import);
    assert!(accounts[1].blocked);
    assert!(!format!("{accounts:?}").contains("synthetic-secret"));
}

#[tokio::test]
async fn pin_and_unpin_survive_reopening_storage() {
    let (directory, mut storage, repository) = fixture();
    pin_credential_account(&mut storage, "synthetic", Some("second"), &|_| None, &repository, 0).await.unwrap();
    let mut reopened = AuthStorage::create(&directory.path().join("auth.json").to_string_lossy());
    let accounts = get_credential_accounts(&reopened, "synthetic", &|_| None, &repository, 0).await.unwrap();
    assert!(accounts[1].pinned);
    pin_credential_account(&mut reopened, "synthetic", None, &|_| None, &repository, 0).await.unwrap();
    let reopened = AuthStorage::create(&directory.path().join("auth.json").to_string_lossy());
    assert!(get_credential_accounts(&reopened, "synthetic", &|_| None, &repository, 0).await.unwrap().iter().all(|account| !account.pinned));
}

#[tokio::test]
async fn rename_and_clear_preserve_identity_and_health() {
    let (directory, mut storage, repository) = fixture();
    rename_credential_account(&mut storage, "synthetic", "second", Some("Work Account")).await.unwrap();
    let mut reopened = AuthStorage::create(&directory.path().join("auth.json").to_string_lossy());
    let accounts = get_credential_accounts(&reopened, "synthetic", &|_| None, &repository, 0).await.unwrap();
    assert_eq!(accounts[1].display_name.as_deref(), Some("Work Account"));
    assert_eq!(accounts[1].name, "second");
    assert!(accounts[1].blocked);
    rename_credential_account(&mut reopened, "synthetic", "second", None).await.unwrap();
    let reopened = AuthStorage::create(&directory.path().join("auth.json").to_string_lossy());
    assert!(get_credential_accounts(&reopened, "synthetic", &|_| None, &repository, 0).await.unwrap()[1].display_name.is_none());
}

#[tokio::test]
async fn removal_persists_and_environment_removal_is_refused() {
    let (directory, mut storage, repository) = fixture();
    remove_credential_account(&mut storage, "synthetic", "second", &|_| None, &repository, 0).await.unwrap();
    let mut reopened = AuthStorage::create(&directory.path().join("auth.json").to_string_lossy());
    let accounts = get_credential_accounts(&reopened, "synthetic", &|_| None, &repository, 0).await.unwrap();
    assert_eq!(accounts.iter().map(|account| account.name.as_str()).collect::<Vec<_>>(), ["first"]);
    let environment = |name: &str| (name == "ANTHROPIC_API_KEY").then(|| "synthetic-env-secret".to_owned());
    let error = remove_credential_account(&mut reopened, "anthropic", "env", &environment, &repository, 0).await.unwrap_err();
    assert_eq!(error, "Environment provider account cannot be removed: env");
}
