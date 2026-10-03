//! Port of senpi packages/coding-agent/src/core/credential-accounts.ts.
//!
//! deviation: the anthropic-subscription extension owns the account-changed event; this port returns
//! the provider id from every mutator instead of emitting it, and todo 22-29 wires that event to the
//! extension host. SENTINEL_OAUTH_FIELDS is the same constant that extension declares.

use serde_json::{json, Map, Value};

use crate::auth_storage::AuthStorage;
use crate::credential_pool::env_slots::discover_env_slots;
use crate::credential_pool::slots::CredentialSlot;
use crate::credential_pool::state_store::{CredentialMaterial, CredentialSlotRepository, SlotHealth, slot_health};

pub const SENTINEL_OAUTH_FIELDS_ACCESS: &str = "claude-sdk-oauth-managed";
pub const SENTINEL_OAUTH_FIELDS_EXPIRES: i64 = 4_102_444_800_000;
pub const DISPLAY_NAME_MAX_COLUMNS: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialAccountSource {
    Login,
    Import,
    Env,
}

impl CredentialAccountSource {
    pub fn as_str(self) -> &'static str {
        match self {
            CredentialAccountSource::Login => "login",
            CredentialAccountSource::Import => "import",
            CredentialAccountSource::Env => "env",
        }
    }
}

/// Account metadata safe to surface: names and health only, never key material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialAccountSummary {
    pub name: String,
    pub display_name: Option<String>,
    pub source: CredentialAccountSource,
    pub blocked: bool,
    pub pinned: bool,
}

pub fn assert_valid_account_name(name: &str) -> Result<(), String> {
    let bytes = name.as_bytes();
    let valid = !bytes.is_empty()
        && bytes.len() <= 64
        && bytes[0].is_ascii_alphanumeric()
        && bytes.iter().all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_' || *byte == b'-');
    if valid {
        Ok(())
    } else {
        Err(format!("Invalid account name '{name}': use letters, digits, '-' or '_', starting with a letter or digit"))
    }
}

fn is_blank_display_character(character: char) -> bool {
    character.is_whitespace() || character.is_control()
}

/// A display name must be 1-32 terminal columns of visible text without control characters.
pub fn account_display_name(value: Option<&str>) -> Option<String> {
    let value = value?;
    if value.chars().any(|character| character.is_control()) {
        return None;
    }
    let normalized: String = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() || normalized.chars().all(is_blank_display_character) {
        return None;
    }
    let columns: usize = normalized.chars().map(|character| if character as u32 > 0x1100 { 2 } else { 1 }).sum();
    if columns > DISPLAY_NAME_MAX_COLUMNS { None } else { Some(normalized) }
}

fn slot_string(slot: &Value, key: &str) -> Option<String> {
    slot.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn slot_source(slot: &Value) -> Option<CredentialAccountSource> {
    match slot.get("source").and_then(Value::as_str) {
        Some("login") => Some(CredentialAccountSource::Login),
        Some("import") => Some(CredentialAccountSource::Import),
        Some("env") => Some(CredentialAccountSource::Env),
        _ => None,
    }
}

/// Block state has two authoritative sources: the slot's own fields inside auth.json and the pool
/// sidecar. Reading only one would silently downgrade a real block to available.
pub fn slot_blocked(slot: &Value, sidecar: Option<&crate::credential_pool::state_store::CredentialSlotState>, now: u64) -> bool {
    if slot_health(sidecar, now) == SlotHealth::Blocked {
        return true;
    }
    match slot.get("blockReason").and_then(Value::as_str) {
        Some("auth_error") | Some("account_disabled") => true,
        _ => slot.get("blockedUntil").and_then(Value::as_u64).map(|until| until > now).unwrap_or(false),
    }
}

/// The slots a stored credential carries: its accounts array, or the flat fields as one slot.
pub fn stored_slots(credential: &Value) -> Vec<Value> {
    if let Some(accounts) = credential.get("accounts").and_then(Value::as_array)
        && !accounts.is_empty() {
            return accounts.iter().filter(|slot| slot.get("name").and_then(Value::as_str).is_some_and(|name| !name.is_empty())).cloned().collect();
        }
    let pooled = CredentialSlot {
        name: crate::credential_pool::slots::DEFAULT_SLOT_NAME.to_owned(),
        key: credential.get("key").and_then(Value::as_str).map(str::to_owned),
        access: credential.get("access").and_then(Value::as_str).map(str::to_owned),
        refresh: credential.get("refresh").and_then(Value::as_str).map(str::to_owned),
        expires: credential.get("expires").and_then(Value::as_f64),
        ..CredentialSlot::default()
    };
    if pooled.key.is_none() && pooled.access.is_none() {
        return Vec::new();
    }
    vec![serde_json::to_value(&pooled).unwrap_or(Value::Null)]
}

fn material_of(slot: &Value) -> CredentialMaterial {
    CredentialMaterial {
        key: slot_string(slot, "key"),
        access: slot_string(slot, "access"),
        refresh: slot_string(slot, "refresh"),
    }
}

fn is_anthropic_subscription(provider: &str) -> bool {
    maho_ai::legacy_provider_ids::normalize_provider_id(provider) == "anthropic-subscription"
}

/// Lists a provider's credential accounts. Stored slots own the listing when a credential exists;
/// env slots are listed only when nothing is stored, mirroring resolution precedence exactly.
pub async fn summarize_credential_accounts(
    provider: &str,
    stored: Option<&Value>,
    env: &(dyn Fn(&str) -> Option<String> + Sync),
    repository: &CredentialSlotRepository,
    now: u64,
) -> Result<Vec<CredentialAccountSummary>, String> {
    let pinned = stored.and_then(|credential| credential.get("pinned")).and_then(Value::as_str).map(str::to_owned);
    let mut summaries = Vec::new();

    if let Some(credential) = stored {
        let state = repository.list_slots(provider, "stored").await?;
        let accounts = if is_anthropic_subscription(provider) && credential.get("accounts").map(Value::is_array) != Some(true) {
            Vec::new()
        } else {
            stored_slots(credential)
        };
        for slot in accounts {
            let name = slot_string(&slot, "name").unwrap_or_default();
            let display_name = account_display_name(slot_string(&slot, "displayName").as_deref());
            let persisted = state.get(&name);
            let revision = repository.stored_credential_revision(provider, &name, &material_of(&slot)).await?;
            // A block belongs to the material that earned it; a re-login starts clean.
            let applicable = persisted.filter(|state| state.credential_revision.as_deref() == Some(revision.as_str()));
            summaries.push(CredentialAccountSummary {
                name: name.clone(),
                display_name,
                source: slot_source(&slot).unwrap_or(CredentialAccountSource::Login),
                blocked: slot_blocked(&slot, applicable, now),
                pinned: pinned.as_deref() == Some(name.as_str()),
            });
        }
        if !is_anthropic_subscription(provider) {
            return Ok(summaries);
        }
    }

    let state = repository.list_slots(provider, "env").await?;
    for slot in discover_env_slots(provider, env) {
        let persisted = state.get(&slot.name);
        let revision = repository.env_credential_revision(&slot.env_var_name, &slot.key).await?;
        let applicable = persisted.filter(|state| state.credential_revision.as_deref() == Some(revision.as_str()));
        summaries.push(CredentialAccountSummary {
            name: slot.name.clone(),
            display_name: None,
            source: CredentialAccountSource::Env,
            blocked: slot_health(applicable, now) == SlotHealth::Blocked,
            pinned: pinned.as_deref() == Some(slot.name.as_str()),
        });
    }
    Ok(summaries)
}

pub async fn get_credential_accounts(
    storage: &AuthStorage,
    provider: &str,
    env: &(dyn Fn(&str) -> Option<String> + Sync),
    repository: &CredentialSlotRepository,
    now: u64,
) -> Result<Vec<CredentialAccountSummary>, String> {
    summarize_credential_accounts(provider, storage.get(provider).as_ref(), env, repository, now).await
}

/// Atomically rename/clear stored metadata without changing identity, health or environment state.
pub async fn rename_credential_account(
    storage: &AuthStorage,
    provider: &str,
    name: &str,
    display_name: Option<&str>,
) -> Result<String, String> {
    let Some(current) = storage.get(provider) else {
        return Err(format!("No stored credential for provider: {provider}"));
    };
    if current.get("type").and_then(Value::as_str) == Some("oauth")
        && current.get("accounts").map(Value::is_array) != Some(true)
        && current.get("access").and_then(Value::as_str) == current.get("refresh").and_then(Value::as_str)
        && current.get("access").and_then(Value::as_str).map(|access| access.ends_with("-managed")).unwrap_or(false)
    {
        return Err(format!("Stored provider account not found: {name}"));
    }
    let renamed = rename_slot_display_name(&current, name, display_name)?;
    storage.set(provider, Some(renamed))?;
    Ok(provider.to_owned())
}

pub fn rename_slot_display_name(credential: &Value, name: &str, display_name: Option<&str>) -> Result<Value, String> {
    assert_valid_account_name(name)?;
    let mut credential = credential.clone();
    let accounts = credential.get("accounts").and_then(Value::as_array).cloned().unwrap_or_else(|| stored_slots(&credential));
    let Some(target) = accounts.iter().find(|slot| slot.get("name").and_then(Value::as_str) == Some(name)) else {
        return Err(format!("Stored provider account not found: {name}"));
    };
    if target.get("source").and_then(Value::as_str) == Some("env") {
        return Err(format!("Environment provider account cannot be renamed: {name}"));
    }
    let normalized = match display_name {
        None => None,
        Some(value) => match account_display_name(Some(value)) {
            Some(normalized) => Some(normalized),
            None => {
                return Err(format!(
                    "Display name must be 1-{DISPLAY_NAME_MAX_COLUMNS} terminal columns of visible text without control or formatting characters."
                ));
            }
        },
    };
    if let Some(normalized) = &normalized {
        let key = normalized.to_lowercase();
        if accounts.iter().any(|slot| {
            slot.get("name").and_then(Value::as_str) != Some(name)
                && account_display_name(slot.get("displayName").and_then(Value::as_str)).map(|value| value.to_lowercase()) == Some(key.clone())
        }) {
            return Err("Display name is already used by another account for this provider.".to_owned());
        }
    }
    let mut updated_accounts = Vec::with_capacity(accounts.len());
    for mut slot in accounts {
        if slot.get("name").and_then(Value::as_str) == Some(name) {
            match &normalized {
                Some(value) => {
                    slot["displayName"] = Value::from(value.clone());
                }
                None => {
                    if let Some(object) = slot.as_object_mut() {
                        object.shift_remove("displayName");
                    }
                }
            }
        }
        updated_accounts.push(slot);
    }
    credential["accounts"] = Value::Array(updated_accounts);
    Ok(credential)
}

/// Pins one slot, or clears the pin when the name is absent.
pub async fn pin_credential_account(
    storage: &AuthStorage,
    provider: &str,
    name: Option<&str>,
    env: &(dyn Fn(&str) -> Option<String> + Sync),
    repository: &CredentialSlotRepository,
    now: u64,
) -> Result<String, String> {
    pin_credential_account_guarded(storage, provider, name, env, repository, now, &|| Ok(())).await
}

pub(crate) async fn pin_credential_account_guarded(
    storage: &AuthStorage, provider: &str, name: Option<&str>,
    env: &(dyn Fn(&str) -> Option<String> + Sync), repository: &CredentialSlotRepository,
    now: u64, admit: &(dyn Fn() -> Result<(), String> + Send + Sync),
) -> Result<String, String> {
    if let Some(name) = name {
        assert_valid_account_name(name)?;
        let accounts = get_credential_accounts(storage, provider, env, repository, now).await?;
        if !accounts.iter().any(|account| account.name == name) {
            return Err(format!("Provider account not found: {name}"));
        }
    }
    admit()?;
    match storage.get(provider) {
        None => {
            if !is_anthropic_subscription(provider) || name.is_none() {
                return Err(format!("No stored credential for provider: {provider}"));
            }
            storage.set(provider, Some(json!({
                "type": "oauth",
                "access": SENTINEL_OAUTH_FIELDS_ACCESS,
                "refresh": SENTINEL_OAUTH_FIELDS_ACCESS,
                "expires": SENTINEL_OAUTH_FIELDS_EXPIRES,
                "accounts": [],
            })))?;
        }
        Some(current) => {
            let mut updated = current.clone();
            match name {
                None => {
                    if current.get("pinned").and_then(Value::as_str).is_none() {
                        return Ok(provider.to_owned());
                    }
                    if let Some(object) = updated.as_object_mut() {
                        object.shift_remove("pinned");
                    }
                }
                Some(name) => {
                    updated["pinned"] = Value::from(name);
                }
            }
            storage.set(provider, Some(updated))?;
        }
    }
    Ok(provider.to_owned())
}

/// Removes one stored slot. Env-backed accounts are refused: the environment still defines them.
pub async fn remove_credential_account(
    storage: &AuthStorage,
    provider: &str,
    name: &str,
    env: &(dyn Fn(&str) -> Option<String> + Sync),
    repository: &CredentialSlotRepository,
    now: u64,
) -> Result<String, String> {
    remove_credential_account_guarded(storage, provider, name, env, repository, now, &|| Ok(())).await
}

pub(crate) async fn remove_credential_account_guarded(
    storage: &AuthStorage, provider: &str, name: &str,
    env: &(dyn Fn(&str) -> Option<String> + Sync), repository: &CredentialSlotRepository,
    now: u64, admit: &(dyn Fn() -> Result<(), String> + Send + Sync),
) -> Result<String, String> {
    let accounts = get_credential_accounts(storage, provider, env, repository, now).await?;
    let Some(account) = accounts.iter().find(|account| account.name == name) else {
        return Err(format!("Provider account not found: {name}"));
    };
    if account.source == CredentialAccountSource::Env {
        return Err(format!("Environment provider account cannot be removed: {name}"));
    }
    admit()?;
    let Some(current) = storage.get(provider) else {
        return Err(format!("No stored credential for provider: {provider}"));
    };
    match remove_slot(&current, name) {
        None => storage.delete(provider)?,
        Some(remaining) => storage.set(provider, Some(remaining))?,
    }
    repository.mutate_slot_state(provider, "stored", name, |_| None).await?;
    Ok(provider.to_owned())
}

pub fn remove_slot(credential: &Value, name: &str) -> Option<Value> {
    let existing = stored_slots(credential);
    let accounts: Vec<Value> = existing.iter().filter(|slot| slot.get("name").and_then(Value::as_str) != Some(name)).cloned().collect();
    if accounts.len() == existing.len() {
        return Some(credential.clone());
    }
    if accounts.is_empty() {
        return None;
    }
    let mut next = credential.clone();
    next["accounts"] = Value::Array(accounts);
    if next.get("pinned").and_then(Value::as_str) == Some(name)
        && let Some(object) = next.as_object_mut() {
            object.shift_remove("pinned");
        }
    Some(next)
}

pub fn credential_to_map(credential: &Value) -> Map<String, Value> {
    credential.as_object().cloned().unwrap_or_default()
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth_storage::AuthStorage;
    use crate::credential_pool::state_store::{BlockReason, CredentialSlotState};
    use serde_json::json;

    const NOW_FAR_FUTURE: u64 = 4_102_444_800_000;

    fn env_none() -> impl Fn(&str) -> Option<String> {
        |_: &str| None
    }

    fn repository(tmp: &tempfile::TempDir) -> CredentialSlotRepository {
        CredentialSlotRepository::new(&tmp.path().join("credential-pool-state.json").to_string_lossy())
    }

    fn storage(tmp: &tempfile::TempDir) -> AuthStorage {
        AuthStorage::create(&tmp.path().join("auth.json").to_string_lossy())
    }

    fn seed_pool(storage: &mut AuthStorage, provider: &str) {
        storage
            .set(
                provider,
                Some(json!({
                    "type": "api_key",
                    "key": "key-default",
                    "accounts": [
                        { "name": "default", "key": "key-default", "source": "login" },
                        { "name": "work", "key": "key-work", "source": "login" },
                    ],
                })),
            )
            .expect("seed");
    }

    #[tokio::test]
    async fn retired_admission_after_account_resolution_does_not_mutate_storage() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut store = storage(&tmp);
        seed_pool(&mut store, "fixture");
        let before = store.get("fixture");
        let repo = repository(&tmp);
        let reject = || Err("retired fixture".to_owned());
        let pinned = pin_credential_account_guarded(&store, "fixture", Some("work"), &env_none(), &repo,
            NOW_FAR_FUTURE, &reject).await;
        let removed = remove_credential_account_guarded(&store, "fixture", "work", &env_none(), &repo,
            NOW_FAR_FUTURE, &reject).await;
        assert_eq!(pinned, Err("retired fixture".into()));
        assert_eq!(removed, Err("retired fixture".into()));
        assert_eq!(store.get("fixture"), before);
    }

    #[tokio::test]
    async fn lists_accounts_for_any_provider_not_just_the_claude_lane() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut store = storage(&tmp);
        let repo = repository(&tmp);
        seed_pool(&mut store, "openai");
        let accounts = get_credential_accounts(&store, "openai", &env_none(), &repo, 0).await.expect("accounts");
        assert_eq!(accounts.iter().map(|account| account.name.as_str()).collect::<Vec<_>>(), vec!["default", "work"]);
        assert!(accounts.iter().all(|account| account.source == CredentialAccountSource::Login));
    }

    #[tokio::test]
    async fn account_summaries_carry_no_credential_material() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut store = storage(&tmp);
        let repo = repository(&tmp);
        seed_pool(&mut store, "openai");
        let accounts = get_credential_accounts(&store, "openai", &env_none(), &repo, 0).await.expect("accounts");
        let rendered = format!("{accounts:?}");
        assert!(!rendered.contains("key-default"));
        assert!(!rendered.contains("key-work"));
    }

    #[tokio::test]
    async fn sidecar_health_surfaces_as_blocked_without_auth_json_carrying_block_state() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut store = storage(&tmp);
        let repo = repository(&tmp);
        seed_pool(&mut store, "openai");
        let revision = repo
            .stored_credential_revision(
                "openai",
                "work",
                &CredentialMaterial { key: Some("key-work".to_owned()), ..CredentialMaterial::default() },
            )
            .await
            .expect("revision");
        repo.mutate_slot_state("openai", "stored", "work", |_| {
            Some(CredentialSlotState {
                state_version: 0,
                blocked_until: Some(NOW_FAR_FUTURE),
                block_reason: Some(BlockReason::RateLimit),
                failure_count: Some(1),
                last_success_at: None,
                credential_revision: Some(revision),
                lease: None,
            })
        })
        .await
        .expect("mutate");

        let accounts = get_credential_accounts(&store, "openai", &env_none(), &repo, 1_000).await.expect("accounts");
        assert!(!accounts[0].blocked);
        assert!(accounts[1].blocked);
        let stored = store.get("openai").expect("credential");
        assert!(stored["accounts"][1].get("blockedUntil").is_none());
    }

    #[tokio::test]
    async fn pinning_marks_exactly_one_account_and_unpinning_clears_it() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut store = storage(&tmp);
        let repo = repository(&tmp);
        seed_pool(&mut store, "openai");
        pin_credential_account(&store, "openai", Some("work"), &env_none(), &repo, 0).await.expect("pin");
        let accounts = get_credential_accounts(&store, "openai", &env_none(), &repo, 0).await.expect("accounts");
        assert_eq!(accounts.iter().filter(|account| account.pinned).count(), 1);
        assert!(accounts[1].pinned);
        pin_credential_account(&store, "openai", None, &env_none(), &repo, 0).await.expect("unpin");
        let accounts = get_credential_accounts(&store, "openai", &env_none(), &repo, 0).await.expect("accounts");
        assert_eq!(accounts.iter().filter(|account| account.pinned).count(), 0);
    }

    #[tokio::test]
    async fn pinning_an_unknown_account_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut store = storage(&tmp);
        let repo = repository(&tmp);
        seed_pool(&mut store, "openai");
        let error = pin_credential_account(&store, "openai", Some("nope"), &env_none(), &repo, 0).await.expect_err("error");
        assert_eq!(error, "Provider account not found: nope");
        let error = pin_credential_account(&store, "openai", Some("bad name"), &env_none(), &repo, 0).await.expect_err("error");
        assert!(error.starts_with("Invalid account name 'bad name'"));
    }

    #[tokio::test]
    async fn removing_a_stored_account_keeps_its_sibling_and_drops_its_sidecar_health() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut store = storage(&tmp);
        let repo = repository(&tmp);
        seed_pool(&mut store, "openai");
        repo.mutate_slot_state("openai", "stored", "work", |_| {
            Some(CredentialSlotState {
                state_version: 0,
                blocked_until: Some(NOW_FAR_FUTURE),
                block_reason: Some(BlockReason::AuthError),
                failure_count: Some(1),
                last_success_at: None,
                credential_revision: None,
                lease: None,
            })
        })
        .await
        .expect("mutate");
        remove_credential_account(&store, "openai", "work", &env_none(), &repo, 0).await.expect("remove");
        let accounts = get_credential_accounts(&store, "openai", &env_none(), &repo, 0).await.expect("accounts");
        assert_eq!(accounts.iter().map(|account| account.name.as_str()).collect::<Vec<_>>(), vec!["default"]);
        assert!(repo.list_slots("openai", "stored").await.expect("slots").is_empty());
    }

    #[tokio::test]
    async fn removing_the_final_stored_account_deletes_the_provider_credential() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = storage(&tmp);
        let repo = repository(&tmp);
        store
            .set(
                "openai",
                Some(json!({ "type": "api_key", "key": "only", "accounts": [{ "name": "only", "key": "only" }] })),
            )
            .expect("seed");
        remove_credential_account(&store, "openai", "only", &env_none(), &repo, 0).await.expect("remove");
        assert!(store.get("openai").is_none());
        let persisted = std::fs::read_to_string(tmp.path().join("auth.json")).expect("read");
        assert!(!persisted.contains("openai"));
    }

    #[tokio::test]
    async fn env_backed_accounts_are_listed_when_nothing_is_stored_and_refuse_removal() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = storage(&tmp);
        let repo = repository(&tmp);
        let env = |name: &str| (name == "ANTHROPIC_API_KEY").then(|| "env-key".to_owned());
        let accounts = get_credential_accounts(&store, "anthropic", &env, &repo, 0).await.expect("accounts");
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].source, CredentialAccountSource::Env);
        let error = remove_credential_account(&store, "anthropic", "env", &env, &repo, 0).await.expect_err("error");
        assert_eq!(error, "Environment provider account cannot be removed: env");
    }

    #[tokio::test]
    async fn a_stored_credential_hides_env_slots_matching_resolution_precedence() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut store = storage(&tmp);
        let repo = repository(&tmp);
        let env = |name: &str| (name == "ANTHROPIC_API_KEY").then(|| "env-key".to_owned());
        seed_pool(&mut store, "anthropic");
        let accounts = get_credential_accounts(&store, "anthropic", &env, &repo, 0).await.expect("accounts");
        assert_eq!(accounts.iter().map(|account| account.name.as_str()).collect::<Vec<_>>(), vec!["default", "work"]);
        assert!(accounts.iter().all(|account| account.source != CredentialAccountSource::Env));
    }

    #[tokio::test]
    async fn a_legacy_provider_id_is_read_through_its_canonical_key() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = storage(&tmp);
        let repo = repository(&tmp);
        store
            .set(
                "anthropic-subscription",
                Some(json!({ "type": "oauth", "access": "a", "refresh": "r", "expires": 1, "accounts": [{ "name": "work", "access": "a", "refresh": "r", "expires": 1 }] })),
            )
            .expect("seed");
        let accounts = get_credential_accounts(&store, "claude-sdk-oauth", &env_none(), &repo, 0).await.expect("accounts");
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].name, "work");
    }

    #[test]
    fn renames_a_slot_display_name_and_validates_it() {
        let credential = json!({ "type": "api_key", "key": "k", "accounts": [{ "name": "work", "key": "k" }] });
        let renamed = rename_slot_display_name(&credential, "work", Some("Work Account")).expect("rename");
        assert_eq!(renamed["accounts"][0]["displayName"], "Work Account");
        let cleared = rename_slot_display_name(&renamed, "work", None).expect("clear");
        assert!(cleared["accounts"][0].get("displayName").is_none());
        assert_eq!(
            rename_slot_display_name(&credential, "missing", Some("x")).expect_err("error"),
            "Stored provider account not found: missing"
        );
        assert!(rename_slot_display_name(&credential, "work", Some("\u{7}beep")).expect_err("error").starts_with("Display name must be"));
        assert!(account_display_name(Some("  ")).is_none());
        assert!(account_display_name(Some(&"w".repeat(40))).is_none());
        assert_eq!(account_display_name(Some(" Work  Account ")).as_deref(), Some("Work Account"));
    }

    #[tokio::test]
    async fn renaming_refuses_a_managed_sentinel_without_accounts() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = storage(&tmp);
        store
            .set(
                "anthropic-subscription",
                Some(json!({
                    "type": "oauth",
                    "access": SENTINEL_OAUTH_FIELDS_ACCESS,
                    "refresh": SENTINEL_OAUTH_FIELDS_ACCESS,
                    "expires": SENTINEL_OAUTH_FIELDS_EXPIRES,
                })),
            )
            .expect("seed");
        let error = rename_credential_account(&store, "anthropic-subscription", "login-1", Some("x"))
            .await
            .expect_err("error");
        assert_eq!(error, "Stored provider account not found: login-1");
    }

    #[tokio::test]
    async fn pinning_without_a_stored_credential_only_works_for_the_subscription_lane() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = storage(&tmp);
        let repo = repository(&tmp);
        let error = pin_credential_account(&store, "openai", Some("work"), &env_none(), &repo, 0).await.expect_err("error");
        assert_eq!(error, "Provider account not found: work");
        pin_credential_account(&store, "anthropic-subscription", Some("work"), &env_none(), &repo, 0)
            .await
            .expect_err("error");
    }
}
