//! Port of cursor-cli-oauth/accounts.ts; mutations retain provider-specific fields.
use maho_ai::auth::types::{Credential, CredentialStore, OAuthCredential};
use maho_ai::utils::abort::AbortSignal;
use serde::{Deserialize, Serialize};
use std::future::Future;

pub const SENTINEL_TOKEN: &str = "cursor-cli-oauth-managed";
pub const SENTINEL_EXPIRES: f64 = 4_102_444_800_000.0;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CursorCliAccountSlot {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub access: String,
    pub refresh: String,
    pub expires: f64,
    pub source: AccountSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_until: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_reason: Option<BlockReason>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AccountSource { Login, Import }

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BlockReason { RateLimit, AuthError }

pub fn empty_credential() -> OAuthCredential {
    OAuthCredential::new(SENTINEL_TOKEN, SENTINEL_TOKEN, SENTINEL_EXPIRES)
        .with_extra("accounts", serde_json::json!([]))
}

pub fn list_accounts(credential: &OAuthCredential) -> anyhow::Result<Vec<CursorCliAccountSlot>> {
    match credential.extra.get("accounts") {
        Some(value) => Ok(serde_json::from_value(value.clone())?),
        None => Ok(Vec::new()),
    }
}

pub fn assert_valid_account_name(name: &str) -> anyhow::Result<()> {
    let mut bytes = name.bytes();
    if !(1..=64).contains(&name.len())
        || !bytes.next().is_some_and(|b| b.is_ascii_alphanumeric())
        || !bytes.all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        anyhow::bail!("Invalid account name '{name}': use letters, digits, '-' or '_', starting with a letter or digit");
    }
    Ok(())
}

fn sentinel(mut credential: OAuthCredential) -> OAuthCredential {
    credential.access = SENTINEL_TOKEN.into();
    credential.refresh = SENTINEL_TOKEN.into();
    credential.expires = SENTINEL_EXPIRES;
    credential
}

pub fn add_account(credential: &OAuthCredential, slot: CursorCliAccountSlot) -> anyhow::Result<OAuthCredential> {
    assert_valid_account_name(&slot.name)?;
    let mut accounts = list_accounts(credential)?;
    if accounts.iter().any(|existing| existing.name == slot.name) {
        anyhow::bail!("Account '{}' already exists", slot.name);
    }
    accounts.push(slot);
    Ok(sentinel(credential.clone()).with_extra("accounts", serde_json::to_value(accounts)?))
}

pub fn remove_account(credential: &OAuthCredential, name: &str) -> anyhow::Result<OAuthCredential> {
    let accounts: Vec<_> = list_accounts(credential)?.into_iter().filter(|slot| slot.name != name).collect();
    let mut next = sentinel(credential.clone()).with_extra("accounts", serde_json::to_value(accounts)?);
    if credential.get_extra_str("pinned") == Some(name) { next.extra.remove("pinned"); }
    Ok(next)
}

pub fn pin_account(credential: &OAuthCredential, name: &str) -> anyhow::Result<OAuthCredential> {
    assert_valid_account_name(name)?;
    Ok(sentinel(credential.clone()).with_extra("pinned", serde_json::json!(name)))
}

pub fn assert_sentinel_invariant(credential: &OAuthCredential) -> anyhow::Result<()> {
    if credential.access != SENTINEL_TOKEN || credential.refresh != SENTINEL_TOKEN
        || (credential.expires - SENTINEL_EXPIRES).abs() > f64::EPSILON {
        anyhow::bail!("top-level OAuth fields must remain sentinel values");
    }
    Ok(())
}

/// Refresh inside the store's serialized mutation, never reading the native cursor entry.
pub async fn refresh_slot<F, Fut>(store: &dyn CredentialStore, provider_id: &str, slot_name: &str,
    refresher: F, signal: AbortSignal, now: f64) -> anyhow::Result<Option<Credential>>
where F: FnOnce(String, AbortSignal) -> Fut + Send + 'static,
      Fut: Future<Output = anyhow::Result<OAuthCredential>> + Send + 'static {
    let slot_name = slot_name.to_owned();
    store.modify(provider_id, Box::new(move |current| Box::pin(async move {
        let Some(Credential::OAuth(credential)) = current else { return Ok(None); };
        assert_sentinel_invariant(&credential)?;
        let mut accounts = list_accounts(&credential)?;
        let Some(slot) = accounts.iter().find(|slot| slot.name == slot_name) else {
            return Ok(Some(Credential::OAuth(credential)));
        };
        if now < slot.expires { return Ok(Some(Credential::OAuth(credential))); }
        let refreshed = refresher(slot.refresh.clone(), signal).await?;
        for slot in &mut accounts {
            if slot.name == slot_name {
                slot.access.clone_from(&refreshed.access);
                slot.refresh.clone_from(&refreshed.refresh);
                slot.expires = refreshed.expires;
            }
        }
        Ok(Some(Credential::OAuth(sentinel(credential).with_extra("accounts", serde_json::to_value(accounts)?))))
    })), None).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ai::auth::credential_store::InMemoryCredentialStore;
    use maho_ai::utils::abort::AbortController;
    fn slot(name: &str) -> CursorCliAccountSlot {
        CursorCliAccountSlot { name: name.into(), display_name: None, access: "slot-access".into(),
            refresh: "slot-refresh".into(), expires: SENTINEL_EXPIRES, source: AccountSource::Login,
            blocked_until: None, block_reason: None }
    }
    #[test]
    fn adds_lists_pins_removes() {
        let credential = add_account(&add_account(&empty_credential(), slot("default")).unwrap(), slot("work_2")).unwrap();
        assert_eq!(list_accounts(&credential).unwrap().iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["default", "work_2"]);
        let pinned = pin_account(&credential, "work_2").unwrap();
        assert_eq!(pinned.get_extra_str("pinned"), Some("work_2"));
        let removed = remove_account(&pinned, "work_2").unwrap();
        assert_eq!(list_accounts(&removed).unwrap(), [slot("default")]);
        assert!(removed.get_extra("pinned").is_none());
        assert_sentinel_invariant(&removed).unwrap();
    }
    #[test]
    fn rejects_duplicate() {
        let credential = add_account(&empty_credential(), slot("default")).unwrap();
        assert!(add_account(&credential, slot("default")).is_err());
    }
    #[test]
    fn rejects_invalid_names() {
        for name in ["", "-leading", "_leading", "contains space", "dot.name", &"a".repeat(65)] {
            assert!(assert_valid_account_name(name).is_err());
        }
    }
    #[test]
    fn accepts_complete_name_contract() {
        assert_valid_account_name("A0_-z").unwrap();
        assert_valid_account_name(&"a".repeat(64)).unwrap();
    }
    #[test]
    fn enforces_sentinel() {
        let mut credential = add_account(&empty_credential(), slot("default")).unwrap();
        assert_sentinel_invariant(&credential).unwrap();
        credential.access = "slot-access".into();
        assert!(assert_sentinel_invariant(&credential).is_err());
    }
    async fn stored(credential: Credential) -> InMemoryCredentialStore {
        let store = InMemoryCredentialStore::new();
        store.modify("cursor-cli-oauth", Box::new(move |_| Box::pin(async { Ok(Some(credential)) })), None).await.unwrap();
        store
    }
    #[tokio::test]
    async fn refreshes_expired_and_preserves_other_slot() {
        let mut expired = slot("default"); expired.expires = 0.0;
        let credential = add_account(&add_account(&empty_credential(), expired).unwrap(), slot("other")).unwrap();
        let store = stored(Credential::OAuth(credential)).await;
        let result = refresh_slot(&store, "cursor-cli-oauth", "default", |token, _| async move {
            assert_eq!(token, "slot-refresh"); Ok(OAuthCredential::new("new-access", "new-refresh", 60000.0))
        }, AbortController::new().signal(), 1000.0).await.unwrap().unwrap().into_oauth().unwrap();
        let accounts = list_accounts(&result).unwrap();
        assert_eq!(accounts[0].access, "new-access"); assert_eq!(accounts[0].refresh, "new-refresh");
        assert_eq!(accounts[1], slot("other")); assert_sentinel_invariant(&result).unwrap();
    }
    #[tokio::test]
    async fn does_not_refresh_unexpired_slot() {
        let store = stored(Credential::OAuth(add_account(&empty_credential(), slot("default")).unwrap())).await;
        refresh_slot(&store, "cursor-cli-oauth", "default", |_, _| async { panic!("must not refresh") }, AbortController::new().signal(), 1000.0).await.unwrap();
    }
    #[tokio::test]
    async fn ignores_non_oauth() {
        let credential: Credential = serde_json::from_value(serde_json::json!({"type":"api_key","key":"native-key"})).unwrap();
        let store = stored(credential.clone()).await;
        let result = refresh_slot(&store, "cursor-cli-oauth", "default", |_, _| async { panic!("must not refresh") }, AbortController::new().signal(), 1000.0).await.unwrap();
        assert_eq!(result, Some(credential));
    }
    #[tokio::test]
    async fn uses_current_slot_list() {
        let store = stored(Credential::OAuth(add_account(&empty_credential(), slot("replacement")).unwrap())).await;
        let result = refresh_slot(&store, "cursor-cli-oauth", "default", |_, _| async { panic!("must not refresh") }, AbortController::new().signal(), 1000.0).await.unwrap();
        assert_eq!(list_accounts(result.unwrap().as_oauth().unwrap()).unwrap(), [slot("replacement")]);
    }
    #[tokio::test]
    async fn leaves_native_entry_untouched() {
        let mut expired = slot("default"); expired.expires = 0.0;
        let store = stored(Credential::OAuth(add_account(&empty_credential(), expired).unwrap())).await;
        let native = Credential::OAuth(OAuthCredential::new("native-access", "native-refresh", 10000.0));
        let copy = native.clone();
        store.modify("cursor", Box::new(move |_| Box::pin(async { Ok(Some(copy)) })), None).await.unwrap();
        refresh_slot(&store, "cursor-cli-oauth", "default", |_, _| async { Ok(OAuthCredential::new("new", "new", 60000.0)) }, AbortController::new().signal(), 1000.0).await.unwrap();
        assert_eq!(store.read("cursor", None).await.unwrap(), Some(native));
    }
}
