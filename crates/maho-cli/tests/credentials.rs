use std::sync::Arc;

use maho_ai::auth::types::{ApiKeyCredential, Credential, CredentialStore, CredentialType, OAuthCredential};
use maho_cli::cli::credentials::AuthStorageCredentialStore;
use maho_core::auth_storage::AuthStorage;

fn store() -> (tempfile::TempDir, Arc<AuthStorageCredentialStore>) {
    let dir = tempfile::tempdir().expect("isolated auth dir");
    let path = dir.path().join("auth.json");
    let store = AuthStorageCredentialStore::create(&path.to_string_lossy());
    (dir, Arc::new(store))
}

#[tokio::test]
async fn missing_entry_reads_none() {
    let (_dir, store) = store();
    assert_eq!(store.read("anthropic", None).await.unwrap(), None);
}

#[tokio::test]
async fn api_key_round_trips_through_auth_json() {
    let (_dir, store) = store();
    let written = store
        .modify(
            "anthropic",
            Box::new(|_| Box::pin(async { Ok(Some(Credential::ApiKey(ApiKeyCredential { key: Some("k".into()), env: None }))) })),
            None,
        )
        .await
        .unwrap();
    assert_eq!(written.as_ref().and_then(Credential::as_api_key).and_then(|c| c.key.clone()), Some("k".into()));
    assert_eq!(store.read("anthropic", None).await.unwrap(), written);
}

#[tokio::test]
async fn oauth_round_trips_with_extra_fields() {
    let (_dir, store) = store();
    let credential = Credential::OAuth(OAuthCredential::new("access", "refresh", 42.0).with_extra("enterpriseUrl", serde_json::json!("https://example")));
    store.modify("github-copilot", Box::new(move |_| Box::pin(async move { Ok(Some(credential)) })), None).await.unwrap();
    let read = store.read("github-copilot", None).await.unwrap().expect("stored");
    let oauth = read.as_oauth().expect("oauth");
    assert_eq!(oauth.access, "access");
    assert_eq!(oauth.refresh, "refresh");
    assert_eq!(oauth.expires, 42.0);
    assert_eq!(oauth.get_extra_str("enterpriseUrl"), Some("https://example"));
}

#[tokio::test]
async fn modify_returning_none_leaves_the_entry_unchanged() {
    let (_dir, store) = store();
    store
        .modify("p", Box::new(|_| Box::pin(async { Ok(Some(Credential::ApiKey(ApiKeyCredential { key: Some("first".into()), env: None }))) })), None)
        .await
        .unwrap();
    let result = store
        .modify("p", Box::new(|current| Box::pin(async move { Ok::<_, anyhow::Error>(None).map(|_: Option<Credential>| current) })), None)
        .await
        .unwrap();
    assert_eq!(result.and_then(|c| c.as_api_key().and_then(|c| c.key.clone())), Some("first".into()));
    assert_eq!(store.read("p", None).await.unwrap().and_then(|c| c.as_api_key().and_then(|c| c.key.clone())), Some("first".into()));
}

#[tokio::test]
async fn delete_removes_the_entry() {
    let (_dir, store) = store();
    store
        .modify("p", Box::new(|_| Box::pin(async { Ok(Some(Credential::ApiKey(ApiKeyCredential { key: Some("k".into()), env: None }))) })), None)
        .await
        .unwrap();
    store.delete("p", None).await.unwrap();
    assert_eq!(store.read("p", None).await.unwrap(), None);
}

#[tokio::test]
async fn list_reports_metadata_without_secrets() {
    let (_dir, store) = store();
    store
        .modify("anthropic", Box::new(|_| Box::pin(async { Ok(Some(Credential::ApiKey(ApiKeyCredential { key: Some("secret".into()), env: None }))) })), None)
        .await
        .unwrap();
    let listed = store.list(None).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].provider_id, "anthropic");
    assert_eq!(listed[0].credential_type, CredentialType::ApiKey);
}

#[tokio::test]
async fn stored_bytes_use_the_auth_json_shape() {
    let dir = tempfile::tempdir().expect("isolated auth dir");
    let path = dir.path().join("auth.json");
    let store = AuthStorageCredentialStore::create(&path.to_string_lossy());
    store
        .modify("anthropic", Box::new(|_| Box::pin(async { Ok(Some(Credential::ApiKey(ApiKeyCredential { key: Some("k".into()), env: None }))) })), None)
        .await
        .unwrap();
    let storage = AuthStorage::create(&path.to_string_lossy());
    let stored = storage.get("anthropic").expect("stored credential");
    assert_eq!(stored.get("type").and_then(serde_json::Value::as_str), Some("api_key"));
    assert_eq!(stored.get("key").and_then(serde_json::Value::as_str), Some("k"));
}
