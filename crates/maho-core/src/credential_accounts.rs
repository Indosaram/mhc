//! Port of senpi packages/coding-agent/src/core/credential-accounts.ts.
//!
//! deviation: the anthropic-subscription extension owns the account-changed event; this port returns
//! the provider id from every mutator instead of emitting it, and todo 22-29 wires that event to the
//! extension host. SENTINEL_OAUTH_FIELDS is the same constant that extension declares.

use serde_json::{Map, Value, json};

use crate::auth_storage::AuthStorage;
use crate::credential_pool::env_slots::discover_env_slots;
use crate::credential_pool::slots::CredentialSlot;
use crate::credential_pool::state_store::{
    CredentialMaterial, CredentialSlotRepository, SlotHealth, slot_health,
};

pub const SENTINEL_OAUTH_FIELDS_ACCESS: &str = "claude-sdk-oauth-managed";
pub const SENTINEL_OAUTH_FIELDS_EXPIRES: i64 = 4_102_444_800_000;
pub const DISPLAY_NAME_MAX_COLUMNS: usize = 32;
pub const CHATGPT_SUBSCRIPTION: &str = "chatgpt-subscription";

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

/// Why a blocked account sits out (senpi#4327): `AuthError` means the key was rejected and is skipped until its
/// re-probe time, `RateLimit` is a cooldown, `Disabled` lasts until the credential is replaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialAccountBlockedReason {
    AuthError,
    RateLimit,
    Disabled,
}

impl CredentialAccountBlockedReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AuthError => "auth_error",
            Self::RateLimit => "rate_limit",
            Self::Disabled => "disabled",
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
    /// Present only while `blocked`; absent when the stored block names no reason.
    pub blocked_reason: Option<CredentialAccountBlockedReason>,
    /// Epoch ms when the block lifts (the re-probe time for `AuthError`); absent for `Disabled`.
    pub blocked_until: Option<u64>,
    pub pinned: bool,
    /// Usage limits binding one model (family) while the account serves the rest (senpi#2555).
    pub blocked_models: Vec<CredentialAccountBlockedModel>,
    /// A slot of the other kind than the stored credential: kept, never used, cannot be pinned (senpi#3486).
    pub unused: bool,
    pub route: Option<ChatGptAccountRoute>,
    pub plan_usage_enabled: Option<bool>,
    pub signed_out: bool,
    /// True only for the account that actually served, never merely because it is pinned.
    pub active: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatGptAccountRoute {
    Siwc,
    Legacy,
}

impl ChatGptAccountRoute {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Siwc => "siwc",
            Self::Legacy => "legacy",
        }
    }
}

pub fn account_can_serve(account: &CredentialAccountSummary, model_id: &str, now: i64) -> bool {
    use crate::credential_pool::model_scope::{ModelBlock, active_model_block_until};
    if account.blocked || account.signed_out {
        return false;
    }
    let blocks = account
        .blocked_models
        .iter()
        .map(|block| {
            (
                block.model.clone(),
                ModelBlock {
                    blocked_until: block.until,
                },
            )
        })
        .collect();
    active_model_block_until(Some(&blocks), Some(model_id), now).is_none()
}

/// One live model-scoped block: the model (family) key and the epoch ms when it lifts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialAccountBlockedModel {
    pub model: String,
    pub until: i64,
}

/// senpi `storedModelBlocks` + `blockedModels`: the slot's live `modelBlocks` entries
/// (`{model: {blockedUntil}}`), malformed entries skipped, expired ones dropped. The pool sidecar
/// carries no model blocks in this port, so the stored slot is the only source.
fn blocked_models(slot: &Value, now: u64) -> Vec<CredentialAccountBlockedModel> {
    let Some(blocks) = slot.get("modelBlocks").and_then(Value::as_object) else {
        return Vec::new();
    };
    blocks
        .iter()
        .filter_map(|(model, block)| {
            // senpi `numberField`: any finite number; anthropic-subscription writes floats.
            let until = block
                .get("blockedUntil")
                .and_then(Value::as_f64)
                .filter(|until| until.is_finite())?;
            (until > now as f64).then(|| CredentialAccountBlockedModel {
                model: model.clone(),
                until: until as i64,
            })
        })
        .collect()
}

pub fn assert_valid_account_name(name: &str) -> Result<(), String> {
    let bytes = name.as_bytes();
    let valid = !bytes.is_empty()
        && bytes.len() <= 64
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_' || *byte == b'-');
    if valid {
        Ok(())
    } else {
        Err(format!(
            "Invalid account name '{name}': use letters, digits, '-' or '_', starting with a letter or digit"
        ))
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
    let columns: usize = normalized
        .chars()
        .map(|character| if character as u32 > 0x1100 { 2 } else { 1 })
        .sum();
    if columns > DISPLAY_NAME_MAX_COLUMNS {
        None
    } else {
        Some(normalized)
    }
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

/// senpi `SlotBlock`: `blocked` with its reason and, unless disabled, the time it lifts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SlotBlock {
    pub blocked: bool,
    pub blocked_reason: Option<CredentialAccountBlockedReason>,
    pub blocked_until: Option<u64>,
}

fn account_blocked_reason(reason: Option<&str>) -> Option<CredentialAccountBlockedReason> {
    match reason {
        Some("auth_error") => Some(CredentialAccountBlockedReason::AuthError),
        Some("rate_limit") => Some(CredentialAccountBlockedReason::RateLimit),
        Some("account_disabled") => Some(CredentialAccountBlockedReason::Disabled),
        _ => None,
    }
}

/// A live block with its reason and, unless disabled, the time it lifts.
fn live_block(reason: Option<&str>, blocked_until: Option<u64>, now: u64) -> SlotBlock {
    let blocked_reason = account_blocked_reason(reason);
    if blocked_reason == Some(CredentialAccountBlockedReason::Disabled) {
        return SlotBlock {
            blocked: true,
            blocked_reason,
            blocked_until: None,
        };
    }
    match blocked_until {
        Some(until) if until > now => SlotBlock {
            blocked: true,
            blocked_reason,
            blocked_until: Some(until),
        },
        _ => SlotBlock::default(),
    }
}

fn block_reason_str(reason: crate::credential_pool::state_store::BlockReason) -> &'static str {
    use crate::credential_pool::state_store::BlockReason;
    match reason {
        BlockReason::AuthError => "auth_error",
        BlockReason::RateLimit => "rate_limit",
        BlockReason::AccountDisabled => "account_disabled",
    }
}

fn sidecar_block(
    sidecar: Option<&crate::credential_pool::state_store::CredentialSlotState>,
    now: u64,
) -> SlotBlock {
    if slot_health(sidecar, now) != SlotHealth::Blocked {
        return SlotBlock::default();
    }
    live_block(
        sidecar
            .and_then(|state| state.block_reason)
            .map(block_reason_str),
        sidecar.and_then(|state| state.blocked_until),
        now,
    )
}

/// Block state has two authoritative sources: the slot's own fields inside auth.json and the pool
/// sidecar. Reading only one would silently downgrade a real block to available (senpi#4327 merge).
pub fn slot_blocked(
    slot: &Value,
    sidecar: Option<&crate::credential_pool::state_store::CredentialSlotState>,
    now: u64,
) -> SlotBlock {
    let from_sidecar = sidecar_block(sidecar, now);
    let from_slot = live_block(
        slot.get("blockReason").and_then(Value::as_str),
        slot.get("blockedUntil").and_then(Value::as_u64),
        now,
    );
    // A disabled account outranks a timed block from the other source: it never lifts on its own.
    if from_slot.blocked_reason == Some(CredentialAccountBlockedReason::Disabled) {
        return from_slot;
    }
    if from_sidecar.blocked_reason == Some(CredentialAccountBlockedReason::Disabled)
        || !from_slot.blocked
    {
        return from_sidecar;
    }
    if !from_sidecar.blocked {
        return from_slot;
    }
    // Both sources hold a timed block: the account is usable only once the later one lifts, and a rejected
    // key stays the reason so a client still skips it.
    let later = if from_slot.blocked_until.unwrap_or(0) > from_sidecar.blocked_until.unwrap_or(0) {
        from_slot
    } else {
        from_sidecar
    };
    let rejected = [from_sidecar, from_slot]
        .iter()
        .any(|block| block.blocked_reason == Some(CredentialAccountBlockedReason::AuthError));
    if rejected {
        SlotBlock {
            blocked_reason: Some(CredentialAccountBlockedReason::AuthError),
            ..later
        }
    } else {
        later
    }
}

/// The slots a stored credential carries: its accounts array, or the flat fields as one slot.
pub fn stored_slots(credential: &Value) -> Vec<Value> {
    if let Some(accounts) = credential.get("accounts").and_then(Value::as_array)
        && !accounts.is_empty()
    {
        return accounts
            .iter()
            .filter(|slot| {
                slot.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| !name.is_empty())
            })
            .cloned()
            .collect();
    }
    let pooled = CredentialSlot {
        name: crate::credential_pool::slots::DEFAULT_SLOT_NAME.to_owned(),
        key: credential
            .get("key")
            .and_then(Value::as_str)
            .map(str::to_owned),
        access: credential
            .get("access")
            .and_then(Value::as_str)
            .map(str::to_owned),
        refresh: credential
            .get("refresh")
            .and_then(Value::as_str)
            .map(str::to_owned),
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

fn is_chatgpt_subscription(provider: &str) -> bool {
    maho_ai::legacy_provider_ids::normalize_provider_id(provider) == CHATGPT_SUBSCRIPTION
}

/// Resolve an immutable slot id, not its display name or position.
pub fn selected_oauth_account(
    storage: &AuthStorage,
    provider: &str,
    name: &str,
) -> Result<maho_ai::auth::types::OAuthCredential, String> {
    use maho_ai::auth::pool::slots::{PooledCredential, project_slot};
    assert_valid_account_name(name)?;
    let pool = storage
        .get(provider)
        .and_then(|value| serde_json::from_value::<PooledCredential>(value).ok());
    project_slot(pool.as_ref(), name)
        .and_then(maho_ai::auth::types::Credential::into_oauth)
        .ok_or_else(|| format!("OAuth account not found: {name}"))
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
    let pinned = stored
        .and_then(|credential| credential.get("pinned"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let mut summaries = Vec::new();

    if let Some(credential) = stored {
        let chatgpt_oauth = is_chatgpt_subscription(provider) && credential["type"] == "oauth";
        let has_pooled_slots = credential["accounts"]
            .as_array()
            .is_some_and(|accounts| !accounts.is_empty());
        let state = repository.list_slots(provider, "stored").await?;
        let accounts = if is_anthropic_subscription(provider)
            && credential.get("accounts").map(Value::is_array) != Some(true)
        {
            Vec::new()
        } else {
            stored_slots(credential)
        };
        let usable: std::collections::HashSet<String> = match serde_json::from_value::<
            maho_ai::auth::pool::slots::PooledCredential,
        >(credential.clone())
        {
            Ok(pooled) => maho_ai::auth::pool::kinds::slots_of_own_kind(&pooled)
                .into_iter()
                .map(|slot| slot.name)
                .collect(),
            Err(_) => stored_slots(credential)
                .iter()
                .filter_map(|slot| slot_string(slot, "name"))
                .collect(),
        };
        for slot in accounts {
            let name = slot_string(&slot, "name").unwrap_or_default();
            let display_name = account_display_name(slot_string(&slot, "displayName").as_deref());
            let persisted = state.get(&name);
            let revision = repository
                .stored_credential_revision(provider, &name, &material_of(&slot))
                .await?;
            // A block belongs to the material that earned it; a re-login starts clean.
            let applicable = persisted
                .filter(|state| state.credential_revision.as_deref() == Some(revision.as_str()));
            let block = if chatgpt_oauth {
                // The generic rotation lane overlays sidecar health, not auth.json blocks.
                sidecar_block(applicable, now)
            } else {
                slot_blocked(&slot, applicable, now)
            };
            let registration = if has_pooled_slots { &slot } else { credential };
            let route = chatgpt_oauth.then(|| {
                if registration["route"] == "siwc" {
                    ChatGptAccountRoute::Siwc
                } else {
                    ChatGptAccountRoute::Legacy
                }
            });
            summaries.push(CredentialAccountSummary {
                name: name.clone(),
                display_name,
                source: slot_source(&slot).unwrap_or(CredentialAccountSource::Login),
                blocked: block.blocked,
                blocked_reason: block.blocked_reason,
                blocked_until: block.blocked_until,
                pinned: pinned.as_deref() == Some(name.as_str()),
                blocked_models: blocked_models(registration, now),
                unused: !usable.contains(&name),
                route,
                plan_usage_enabled: (route == Some(ChatGptAccountRoute::Siwc))
                    .then(|| registration["planUsageEnabled"].as_bool())
                    .flatten(),
                signed_out: chatgpt_oauth && registration["signedOut"] == true,
                active: false,
            });
        }
        if !is_anthropic_subscription(provider) {
            return Ok(summaries);
        }
    }

    let state = repository.list_slots(provider, "env").await?;
    for slot in discover_env_slots(provider, env) {
        let persisted = state.get(&slot.name);
        let revision = repository
            .env_credential_revision(&slot.env_var_name, &slot.key)
            .await?;
        let applicable = persisted
            .filter(|state| state.credential_revision.as_deref() == Some(revision.as_str()));
        let block = sidecar_block(applicable, now);
        summaries.push(CredentialAccountSummary {
            name: slot.name.clone(),
            display_name: None,
            source: CredentialAccountSource::Env,
            blocked: block.blocked,
            blocked_reason: block.blocked_reason,
            blocked_until: block.blocked_until,
            pinned: pinned.as_deref() == Some(slot.name.as_str()),
            blocked_models: Vec::new(),
            unused: false,
            route: None,
            plan_usage_enabled: None,
            signed_out: false,
            active: false,
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
    get_credential_accounts_with_active(storage, provider, env, repository, now, None).await
}

pub async fn get_credential_accounts_with_active(
    storage: &AuthStorage,
    provider: &str,
    env: &(dyn Fn(&str) -> Option<String> + Sync),
    repository: &CredentialSlotRepository,
    now: u64,
    active_name: Option<&str>,
) -> Result<Vec<CredentialAccountSummary>, String> {
    let stored = storage.get(provider);
    let mut accounts =
        summarize_credential_accounts(provider, stored.as_ref(), env, repository, now).await?;
    if is_chatgpt_subscription(provider) {
        if let Some(order) = stored.as_ref().and_then(|value| value["order"].as_array()) {
            accounts.sort_by_key(|account| {
                order
                    .iter()
                    .position(|name| name == account.name.as_str())
                    .unwrap_or(usize::MAX)
            });
        }
        for account in &mut accounts {
            account.active = Some(account.name.as_str()) == active_name
                && !account.blocked
                && !account.signed_out
                && !account.unused;
        }
    }
    Ok(accounts)
}

pub async fn reset_credential_account(
    storage: &AuthStorage,
    provider: &str,
    name: &str,
    env: &(dyn Fn(&str) -> Option<String> + Sync),
    repository: &CredentialSlotRepository,
    now: u64,
) -> Result<String, String> {
    reset_credential_account_guarded(storage, provider, name, env, repository, now, &|| Ok(()))
        .await
}

pub(crate) async fn reset_credential_account_guarded(
    storage: &AuthStorage,
    provider: &str,
    name: &str,
    env: &(dyn Fn(&str) -> Option<String> + Sync),
    repository: &CredentialSlotRepository,
    now: u64,
    admit: &(dyn Fn() -> Result<(), String> + Send + Sync),
) -> Result<String, String> {
    let accounts = get_credential_accounts(storage, provider, env, repository, now).await?;
    let account = accounts
        .iter()
        .find(|account| account.name == name)
        .ok_or_else(|| format!("Provider account not found: {name}"))?;
    admit()?;
    let lane = if account.source == CredentialAccountSource::Env {
        "env"
    } else {
        "stored"
    };
    repository
        .mutate_slot_state(provider, lane, name, |_| None)
        .await?;
    if storage.get(provider).is_some() {
        storage
            .modify_credential(provider, |current| async move {
                let Some(mut credential) = current else {
                    return Ok(None);
                };
                if let Some(object) = credential.as_object_mut() {
                    if let Some(slots) = object.get_mut("accounts").and_then(Value::as_array_mut) {
                        for slot in slots
                            .iter_mut()
                            .filter(|slot| slot.get("name").and_then(Value::as_str) == Some(name))
                        {
                            if let Some(slot) = slot.as_object_mut() {
                                slot.shift_remove("blockReason");
                                slot.shift_remove("blockedUntil");
                                slot.shift_remove("modelBlocks");
                            }
                        }
                    }
                    if let Some(state) = object.get_mut("slotState").and_then(Value::as_object_mut)
                    {
                        state.shift_remove(name);
                    }
                }
                Ok(Some(credential))
            })
            .await?;
    }
    Ok(provider.to_owned())
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
        && current.get("access").and_then(Value::as_str)
            == current.get("refresh").and_then(Value::as_str)
        && current
            .get("access")
            .and_then(Value::as_str)
            .map(|access| access.ends_with("-managed"))
            .unwrap_or(false)
    {
        return Err(format!("Stored provider account not found: {name}"));
    }
    let renamed = rename_slot_display_name(&current, name, display_name)?;
    storage.set(provider, Some(renamed))?;
    Ok(provider.to_owned())
}

pub fn rename_slot_display_name(
    credential: &Value,
    name: &str,
    display_name: Option<&str>,
) -> Result<Value, String> {
    assert_valid_account_name(name)?;
    let mut credential = credential.clone();
    let accounts = credential
        .get("accounts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| stored_slots(&credential));
    let Some(target) = accounts
        .iter()
        .find(|slot| slot.get("name").and_then(Value::as_str) == Some(name))
    else {
        return Err(format!("Stored provider account not found: {name}"));
    };
    if target.get("source").and_then(Value::as_str) == Some("env") {
        return Err(format!(
            "Environment provider account cannot be renamed: {name}"
        ));
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
                && account_display_name(slot.get("displayName").and_then(Value::as_str))
                    .map(|value| value.to_lowercase())
                    == Some(key.clone())
        }) {
            return Err(
                "Display name is already used by another account for this provider.".to_owned(),
            );
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
    storage: &AuthStorage,
    provider: &str,
    name: Option<&str>,
    env: &(dyn Fn(&str) -> Option<String> + Sync),
    repository: &CredentialSlotRepository,
    now: u64,
    admit: &(dyn Fn() -> Result<(), String> + Send + Sync),
) -> Result<String, String> {
    if let Some(name) = name {
        assert_valid_account_name(name)?;
        let accounts = get_credential_accounts(storage, provider, env, repository, now).await?;
        let Some(account) = accounts.iter().find(|account| account.name == name) else {
            return Err(format!("Provider account not found: {name}"));
        };
        if account.unused {
            return Err(format!(
                "Account {name} holds a login of the other kind and is never used, so it cannot be pinned. To switch {provider} to it, save it again with /login."
            ));
        }
    }
    admit()?;
    match storage.get(provider) {
        None => {
            if !is_anthropic_subscription(provider) || name.is_none() {
                return Err(format!("No stored credential for provider: {provider}"));
            }
            storage.set(
                provider,
                Some(json!({
                    "type": "oauth",
                    "access": SENTINEL_OAUTH_FIELDS_ACCESS,
                    "refresh": SENTINEL_OAUTH_FIELDS_ACCESS,
                    "expires": SENTINEL_OAUTH_FIELDS_EXPIRES,
                    "accounts": [],
                })),
            )?;
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
    remove_credential_account_guarded(storage, provider, name, env, repository, now, &|| Ok(()))
        .await
}

pub(crate) async fn remove_credential_account_guarded(
    storage: &AuthStorage,
    provider: &str,
    name: &str,
    env: &(dyn Fn(&str) -> Option<String> + Sync),
    repository: &CredentialSlotRepository,
    now: u64,
    admit: &(dyn Fn() -> Result<(), String> + Send + Sync),
) -> Result<String, String> {
    let accounts = get_credential_accounts(storage, provider, env, repository, now).await?;
    let Some(account) = accounts.iter().find(|account| account.name == name) else {
        return Err(format!("Provider account not found: {name}"));
    };
    if account.source == CredentialAccountSource::Env {
        return Err(format!(
            "Environment provider account cannot be removed: {name}"
        ));
    }
    admit()?;
    let Some(current) = storage.get(provider) else {
        return Err(format!("No stored credential for provider: {provider}"));
    };
    let remaining = if is_chatgpt_subscription(provider) {
        use maho_ai::auth::pool::slots::{PooledCredential, remove_slot};
        let pool = serde_json::from_value::<PooledCredential>(current)
            .map_err(|_| "Invalid stored ChatGPT credential".to_owned())?;
        remove_slot(Some(pool), name)
            .map(|mut remaining| {
                if let Some(order) = remaining
                    .extra
                    .get_mut("order")
                    .and_then(Value::as_array_mut)
                {
                    order.retain(|entry| entry != name);
                    if order.is_empty() {
                        remaining.extra.remove("order");
                    }
                }
                serde_json::to_value(remaining.to_stored_credential())
                    .map_err(|_| "Could not serialize remaining ChatGPT accounts".to_owned())
            })
            .transpose()?
    } else {
        remove_slot(&current, name)
    };
    match remaining {
        None => storage.delete(provider)?,
        Some(remaining) => storage.set(provider, Some(remaining))?,
    }
    repository
        .mutate_slot_state(provider, "stored", name, |_| None)
        .await?;
    Ok(provider.to_owned())
}

pub fn remove_slot(credential: &Value, name: &str) -> Option<Value> {
    let existing = stored_slots(credential);
    let accounts: Vec<Value> = existing
        .iter()
        .filter(|slot| slot.get("name").and_then(Value::as_str) != Some(name))
        .cloned()
        .collect();
    if accounts.len() == existing.len() {
        return Some(credential.clone());
    }
    if accounts.is_empty() {
        return None;
    }
    let mut next = credential.clone();
    next["accounts"] = Value::Array(accounts);
    if next.get("pinned").and_then(Value::as_str) == Some(name)
        && let Some(object) = next.as_object_mut()
    {
        object.shift_remove("pinned");
    }
    Some(next)
}

pub fn credential_to_map(credential: &Value) -> Map<String, Value> {
    credential.as_object().cloned().unwrap_or_default()
}
#[cfg(test)]
mod block_tests {
    use super::*;
    use serde_json::json;

    /// senpi test/suite/regressions/4327-account-block-reason.test.ts: slot and sidecar blocks merge.
    #[test]
    fn slot_and_sidecar_blocks_merge_with_reason_and_expiry() {
        let sidecar = |reason: &str, until: u64| {
            serde_json::from_value::<crate::credential_pool::state_store::CredentialSlotState>(
                json!({"stateVersion": 1, "blockReason": reason, "blockedUntil": until}),
            )
            .expect("state")
        };
        let disabled = slot_blocked(
            &json!({"blockReason": "account_disabled"}),
            Some(&sidecar("rate_limit", 500)),
            100,
        );
        assert_eq!(
            disabled.blocked_reason,
            Some(CredentialAccountBlockedReason::Disabled)
        );
        assert_eq!(disabled.blocked_until, None);
        let merged = slot_blocked(
            &json!({"blockReason": "auth_error", "blockedUntil": 200}),
            Some(&sidecar("rate_limit", 500)),
            100,
        );
        assert_eq!(
            merged,
            SlotBlock {
                blocked: true,
                blocked_reason: Some(CredentialAccountBlockedReason::AuthError),
                blocked_until: Some(500)
            }
        );
        let expired = slot_blocked(
            &json!({"blockReason": "rate_limit", "blockedUntil": 50}),
            None,
            100,
        );
        assert_eq!(expired, SlotBlock::default());
    }
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
        CredentialSlotRepository::new(
            &tmp.path()
                .join("credential-pool-state.json")
                .to_string_lossy(),
        )
    }

    fn storage(tmp: &tempfile::TempDir) -> AuthStorage {
        AuthStorage::create(&tmp.path().join("auth.json").to_string_lossy())
    }

    fn chatgpt_pool() -> Value {
        json!({
            "type": "oauth", "expires": NOW_FAR_FUTURE, "accountId": "account-one",
            "access": "fixture-one", "refresh": "fixture-refresh-one",
            "route": "siwc", "issuer": "https://auth.openai.com", "subject": "s1", "clientId": "c",
            "idToken": "fixture-id-one", "scopes": ["openid"], "planUsageEnabled": true,
            "accounts": [
                {
                    "name": "login", "source": "login", "access": "fixture-one",
                    "refresh": "fixture-refresh-one", "expires": NOW_FAR_FUTURE, "route": "siwc",
                    "issuer": "https://auth.openai.com", "subject": "s1", "clientId": "c",
                    "email": "one@example.invalid", "idToken": "fixture-id-one",
                    "scopes": ["openid"], "planUsageEnabled": true,
                    "modelBlocks": {"sonnet": {"blockedUntil": 200}}
                },
                {
                    "name": "login-2", "source": "login", "access": "fixture-two",
                    "refresh": "fixture-refresh-two", "expires": NOW_FAR_FUTURE,
                    "email": "two@example.invalid"
                }
            ]
        })
    }

    fn write_chatgpt_pool(tmp: &tempfile::TempDir, pool: Value) {
        std::fs::write(
            tmp.path().join("auth.json"),
            json!({ (CHATGPT_SUBSCRIPTION): pool }).to_string(),
        )
        .expect("write isolated auth fixture");
    }

    #[tokio::test]
    async fn chatgpt_lists_mixed_routes_and_distinguishes_pin_from_active() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut pool = chatgpt_pool();
        let flat = pool.as_object_mut().expect("fixture object");
        flat.remove("access");
        flat.remove("refresh");
        write_chatgpt_pool(&tmp, pool);
        let store = storage(&tmp);
        let repo = repository(&tmp);
        pin_credential_account(
            &store,
            CHATGPT_SUBSCRIPTION,
            Some("login-2"),
            &env_none(),
            &repo,
            100,
        )
        .await
        .expect("pin");
        let accounts = get_credential_accounts_with_active(
            &store,
            CHATGPT_SUBSCRIPTION,
            &env_none(),
            &repo,
            100,
            Some("login"),
        )
        .await
        .expect("list");
        assert_eq!(accounts.len(), 2);
        assert_eq!(accounts[0].name, "login");
        assert_eq!(accounts[0].route, Some(ChatGptAccountRoute::Siwc));
        assert_eq!(accounts[0].plan_usage_enabled, Some(true));
        assert!(accounts[0].active && !accounts[0].pinned);
        assert_eq!(accounts[1].name, "login-2");
        assert_eq!(accounts[1].route, Some(ChatGptAccountRoute::Legacy));
        assert_eq!(accounts[1].plan_usage_enabled, None);
        assert!(!accounts[1].active && accounts[1].pinned);
        assert!(!account_can_serve(&accounts[0], "claude-sonnet-4", 100));
        assert!(account_can_serve(&accounts[0], "claude-opus-4", 100));
        assert!(account_can_serve(&accounts[0], "claude-sonnet-4", 200));
        let summary = format!("{accounts:?}");
        assert!(!summary.contains("fixture-") && !summary.contains("@example.invalid"));
    }

    #[tokio::test]
    async fn chatgpt_remove_reprojects_the_survivor_and_clears_pin_and_order() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_chatgpt_pool(&tmp, chatgpt_pool());
        let store = storage(&tmp);
        let repo = repository(&tmp);
        let mut pool = store.get(CHATGPT_SUBSCRIPTION).expect("seeded pool");
        pool["pinned"] = json!("login");
        pool["order"] = json!(["login", "login-2"]);
        store.set(CHATGPT_SUBSCRIPTION, Some(pool)).expect("seed");
        remove_credential_account(
            &store,
            CHATGPT_SUBSCRIPTION,
            "login",
            &env_none(),
            &repo,
            100,
        )
        .await
        .expect("remove account");
        let remaining = storage(&tmp).get(CHATGPT_SUBSCRIPTION).expect("survivor");
        assert!(remaining["access"] == "fixture-two");
        assert!(remaining.get("idToken").is_none());
        assert!(remaining.get("pinned").is_none());
        assert_eq!(remaining["order"], json!(["login-2"]));
        let slots = stored_slots(&remaining);
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0]["name"], "login-2");
    }

    struct ChatGptFixtureOAuth {
        storage: std::sync::Arc<AuthStorage>,
        logins: std::sync::atomic::AtomicUsize,
        revocations: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl maho_ai::auth::types::OAuthAuth for ChatGptFixtureOAuth {
        fn name(&self) -> &str {
            "Offline ChatGPT fixture"
        }

        async fn login(
            &self,
            _: &maho_ai::auth::types::ProviderAuthInteraction,
        ) -> anyhow::Result<maho_ai::auth::types::OAuthCredential> {
            use maho_ai::auth::types::OAuthCredential;
            let login = self
                .logins
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(OAuthCredential::new(
                format!("fixture-three-{login}"),
                format!("fixture-refresh-three-{login}"),
                NOW_FAR_FUTURE as f64,
            )
            .with_extra("route", json!("siwc"))
            .with_extra("issuer", json!("https://auth.openai.com"))
            .with_extra("subject", json!("s3"))
            .with_extra("clientId", json!("c")))
        }

        async fn refresh(
            &self,
            _: &maho_ai::auth::types::OAuthCredential,
            _: &maho_ai::utils::abort::AbortSignal,
        ) -> anyhow::Result<maho_ai::auth::types::OAuthCredential> {
            anyhow::bail!("fixture is not expired")
        }

        fn has_revoke(&self) -> bool {
            true
        }

        async fn revoke(
            &self,
            credential: &maho_ai::auth::types::OAuthCredential,
            options: Option<maho_ai::auth::types::LogoutOptions>,
        ) -> anyhow::Result<maho_ai::auth::types::OAuthRevocationResult> {
            assert!(credential.refresh == "fixture-refresh-one");
            assert_eq!(
                options.and_then(|options| options.slot_id).as_deref(),
                Some("login")
            );
            let stored = self
                .storage
                .get(CHATGPT_SUBSCRIPTION)
                .expect("registration retained");
            assert_eq!(stored["accounts"][0]["signedOut"], true);
            assert!(stored["accounts"][0]["refresh"] == "");
            assert!(stored["accounts"][0].get("idToken").is_none());
            assert!(stored["accounts"][1]["refresh"] == "fixture-refresh-two");
            self.revocations
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(maho_ai::auth::types::OAuthRevocationResult::RemoteRevocationUnconfirmed)
        }

        async fn to_auth(
            &self,
            _: &maho_ai::auth::types::OAuthCredential,
        ) -> anyhow::Result<maho_ai::auth::types::ModelAuth> {
            Ok(Default::default())
        }
    }

    struct ChatGptFixtureInteraction;

    #[async_trait::async_trait]
    impl maho_ai::auth::types::AuthInteraction for ChatGptFixtureInteraction {
        fn signal(&self) -> Option<maho_ai::utils::abort::AbortSignal> {
            None
        }

        async fn prompt(&self, _: maho_ai::auth::types::AuthPrompt) -> anyhow::Result<String> {
            anyhow::bail!("fixture does not prompt")
        }

        fn notify(&self, _: maho_ai::auth::types::AuthEvent) {}
    }

    fn chatgpt_fixture_runtime(
        tmp: &tempfile::TempDir,
    ) -> (
        crate::model_runtime::ModelRuntime,
        std::sync::Arc<ChatGptFixtureOAuth>,
    ) {
        use crate::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
        use maho_ai::providers::faux::{RegisterFauxProviderOptions, faux_provider};
        write_chatgpt_pool(tmp, chatgpt_pool());
        let provider = faux_provider(RegisterFauxProviderOptions {
            api: Some("faux".into()),
            provider: Some(CHATGPT_SUBSCRIPTION.into()),
            ..Default::default()
        })
        .expect("offline provider");
        let mut runtime = ModelRuntime::create_sync(CreateModelRuntimeOptions {
            auth_path: Some(tmp.path().join("auth.json")),
            agent_dir: Some(tmp.path().to_owned()),
            providers: Some(vec![provider.provider]),
            allow_model_network: Some(false),
            refresh_on_create: Some(false),
            ..Default::default()
        })
        .expect("offline runtime");
        let fake = std::sync::Arc::new(ChatGptFixtureOAuth {
            storage: runtime.credentials.clone(),
            logins: Default::default(),
            revocations: Default::default(),
        });
        runtime
            .register_provider(
                CHATGPT_SUBSCRIPTION,
                crate::provider_composer::ProviderConfigInput {
                    oauth: Some(fake.clone()),
                    ..Default::default()
                },
            )
            .expect("register fake OAuth provider");
        (runtime, fake)
    }

    #[tokio::test]
    async fn chatgpt_signout_clears_only_the_selected_session_before_remote_revoke() {
        use maho_ai::auth::types::{LogoutOptions, OAuthRevocationResult};
        let tmp = tempfile::tempdir().expect("tempdir");
        let (runtime, fake) = chatgpt_fixture_runtime(&tmp);
        let result = runtime
            .logout_with_revocation(
                CHATGPT_SUBSCRIPTION,
                LogoutOptions {
                    slot_id: Some("login".into()),
                    ..Default::default()
                },
            )
            .await
            .expect("sign out fixture account");
        assert_eq!(
            result,
            Some(OAuthRevocationResult::RemoteRevocationUnconfirmed)
        );
        assert_eq!(
            fake.revocations.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        let accounts = get_credential_accounts_with_active(
            &storage(&tmp),
            CHATGPT_SUBSCRIPTION,
            &env_none(),
            &repository(&tmp),
            0,
            Some("login"),
        )
        .await
        .expect("list after sign-out");
        assert_eq!(accounts.len(), 2);
        assert!(accounts[0].signed_out && !accounts[0].active);
        assert!(!account_can_serve(&accounts[0], "gpt-5", 0));
        assert!(!accounts[1].signed_out);
    }

    #[tokio::test]
    async fn chatgpt_oauth_login_appends_then_replaces_the_same_siwc_identity() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (runtime, _) = chatgpt_fixture_runtime(&tmp);
        for login in 0..2 {
            runtime
                .login(
                    CHATGPT_SUBSCRIPTION,
                    maho_ai::auth::types::AuthType::OAuth,
                    std::sync::Arc::new(ChatGptFixtureInteraction),
                )
                .await
                .expect("fake login");
            let pool = storage(&tmp).get(CHATGPT_SUBSCRIPTION).expect("stored");
            let names: Vec<_> = pool["accounts"]
                .as_array()
                .expect("account slots")
                .iter()
                .filter_map(|slot| slot["name"].as_str())
                .collect();
            assert_eq!(names, ["login", "login-2", "login-3"]);
            assert_eq!(pool["accounts"][2]["subject"], "s3");
            assert!(pool["accounts"][2]["access"] == format!("fixture-three-{login}"));
        }
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
        let pinned = pin_credential_account_guarded(
            &store,
            "fixture",
            Some("work"),
            &env_none(),
            &repo,
            NOW_FAR_FUTURE,
            &reject,
        )
        .await;
        let removed = remove_credential_account_guarded(
            &store,
            "fixture",
            "work",
            &env_none(),
            &repo,
            NOW_FAR_FUTURE,
            &reject,
        )
        .await;
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
        let accounts = get_credential_accounts(&store, "openai", &env_none(), &repo, 0)
            .await
            .expect("accounts");
        assert_eq!(
            accounts
                .iter()
                .map(|account| account.name.as_str())
                .collect::<Vec<_>>(),
            vec!["default", "work"]
        );
        assert!(
            accounts
                .iter()
                .all(|account| account.source == CredentialAccountSource::Login)
        );
    }

    #[tokio::test]
    async fn summary_lists_live_model_blocks_from_the_stored_slot() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = storage(&tmp);
        let repo = repository(&tmp);
        store.set("fixture", Some(json!({"type":"api_key","key":"base","accounts":[
            {"name":"work","key":"work-key","modelBlocks":{"fable":{"blockedUntil":500},"opus":{"blockedUntil":50},"bad":{}}},
            {"name":"other","key":"other-key"}
        ]}))).expect("seed");
        let accounts = get_credential_accounts(&store, "fixture", &env_none(), &repo, 100)
            .await
            .expect("accounts");
        assert_eq!(
            accounts[0].blocked_models,
            vec![CredentialAccountBlockedModel {
                model: "fable".into(),
                until: 500
            }]
        );
        assert!(!accounts[0].blocked);
        assert!(accounts[1].blocked_models.is_empty());
    }

    #[tokio::test]
    async fn summary_reads_float_model_block_expiries_as_anthropic_subscription_writes_them() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = storage(&tmp);
        let repo = repository(&tmp);
        store.set("fixture", Some(json!({"type":"api_key","key":"base","accounts":[
            {"name":"work","key":"work-key","modelBlocks":{"opus":{"blockedUntil":500.0},"fable":{"blockedUntil":50.0}}}
        ]}))).expect("seed");
        let accounts = get_credential_accounts(&store, "fixture", &env_none(), &repo, 100)
            .await
            .expect("accounts");
        assert_eq!(
            accounts[0].blocked_models,
            vec![CredentialAccountBlockedModel {
                model: "opus".into(),
                until: 500
            }]
        );
    }

    #[tokio::test]
    async fn reset_preserves_siblings_and_removes_both_health_sources() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = storage(&tmp);
        let repo = repository(&tmp);
        store.set("fixture", Some(json!({"type":"api_key","key":"base","accounts":[
            {"name":"work","key":"work-key","blockReason":"rate_limit","blockedUntil":99,"modelBlocks":{"m":1},"keep":true},
            {"name":"other","key":"other-key","blockReason":"auth_error"}
        ],"slotState":{"work":{"blocked":true},"other":{"blocked":true}}}))).expect("seed");
        repo.mutate_slot_state("fixture", "stored", "work", |_| {
            Some(CredentialSlotState {
                state_version: 0,
                blocked_until: Some(NOW_FAR_FUTURE),
                block_reason: Some(BlockReason::RateLimit),
                failure_count: Some(1),
                last_success_at: None,
                credential_revision: None,
                lease: None,
            })
        })
        .await
        .expect("sidecar");
        reset_credential_account_guarded(&store, "fixture", "work", &env_none(), &repo, 0, &|| {
            Ok(())
        })
        .await
        .expect("reset");
        let value = store.get("fixture").expect("credential");
        assert_eq!(
            value["accounts"][0],
            json!({"name":"work","key":"work-key","keep":true})
        );
        assert_eq!(value["accounts"][1]["blockReason"], "auth_error");
        assert_eq!(value["slotState"], json!({"other":{"blocked":true}}));
        assert!(
            !repo
                .list_slots("fixture", "stored")
                .await
                .expect("sidecar")
                .contains_key("work")
        );
        assert!(
            reset_credential_account_guarded(
                &store,
                "fixture",
                "missing",
                &env_none(),
                &repo,
                0,
                &|| Ok(())
            )
            .await
            .is_err()
        );
        assert_eq!(store.get("fixture"), Some(value));
    }

    #[tokio::test]
    async fn environment_reset_clears_sidecar_and_matching_stored_metadata() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = storage(&tmp);
        let repo = repository(&tmp);
        let env = |key: &str| (key == "ANTHROPIC_API_KEY").then(|| "fixture-env".to_owned());
        store.set("anthropic", Some(json!({"type":"api_key","key":"stored",
            "accounts":[{"name":"env","key":"stored","source":"env","blockReason":"rate_limit","blockedUntil":9,"modelBlocks":{"m":1}}],
            "slotState":{"env":{"blocked":true},"other":{"keep":true}}}))).expect("seed");
        repo.mutate_slot_state("anthropic", "env", "env", |_| {
            Some(CredentialSlotState {
                state_version: 0,
                blocked_until: Some(NOW_FAR_FUTURE),
                block_reason: Some(BlockReason::RateLimit),
                failure_count: Some(1),
                last_success_at: None,
                credential_revision: None,
                lease: None,
            })
        })
        .await
        .expect("sidecar");
        reset_credential_account(&store, "anthropic", "env", &env, &repo, 0)
            .await
            .expect("reset");
        let value = store.get("anthropic").expect("credential");
        assert_eq!(
            value["accounts"][0],
            json!({"name":"env","key":"stored","source":"env"})
        );
        assert_eq!(value["slotState"], json!({"other":{"keep":true}}));
        assert!(
            !repo
                .list_slots("anthropic", "env")
                .await
                .expect("sidecar")
                .contains_key("env")
        );
    }

    #[tokio::test]
    async fn account_summaries_carry_no_credential_material() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut store = storage(&tmp);
        let repo = repository(&tmp);
        seed_pool(&mut store, "openai");
        let accounts = get_credential_accounts(&store, "openai", &env_none(), &repo, 0)
            .await
            .expect("accounts");
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
                &CredentialMaterial {
                    key: Some("key-work".to_owned()),
                    ..CredentialMaterial::default()
                },
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

        let accounts = get_credential_accounts(&store, "openai", &env_none(), &repo, 1_000)
            .await
            .expect("accounts");
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
        pin_credential_account(&store, "openai", Some("work"), &env_none(), &repo, 0)
            .await
            .expect("pin");
        let accounts = get_credential_accounts(&store, "openai", &env_none(), &repo, 0)
            .await
            .expect("accounts");
        assert_eq!(accounts.iter().filter(|account| account.pinned).count(), 1);
        assert!(accounts[1].pinned);
        pin_credential_account(&store, "openai", None, &env_none(), &repo, 0)
            .await
            .expect("unpin");
        let accounts = get_credential_accounts(&store, "openai", &env_none(), &repo, 0)
            .await
            .expect("accounts");
        assert_eq!(accounts.iter().filter(|account| account.pinned).count(), 0);
    }

    #[tokio::test]
    async fn pinning_an_unknown_account_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut store = storage(&tmp);
        let repo = repository(&tmp);
        seed_pool(&mut store, "openai");
        let error = pin_credential_account(&store, "openai", Some("nope"), &env_none(), &repo, 0)
            .await
            .expect_err("error");
        assert_eq!(error, "Provider account not found: nope");
        let error =
            pin_credential_account(&store, "openai", Some("bad name"), &env_none(), &repo, 0)
                .await
                .expect_err("error");
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
        remove_credential_account(&store, "openai", "work", &env_none(), &repo, 0)
            .await
            .expect("remove");
        let accounts = get_credential_accounts(&store, "openai", &env_none(), &repo, 0)
            .await
            .expect("accounts");
        assert_eq!(
            accounts
                .iter()
                .map(|account| account.name.as_str())
                .collect::<Vec<_>>(),
            vec!["default"]
        );
        assert!(
            repo.list_slots("openai", "stored")
                .await
                .expect("slots")
                .is_empty()
        );
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
        remove_credential_account(&store, "openai", "only", &env_none(), &repo, 0)
            .await
            .expect("remove");
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
        let accounts = get_credential_accounts(&store, "anthropic", &env, &repo, 0)
            .await
            .expect("accounts");
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].source, CredentialAccountSource::Env);
        let error = remove_credential_account(&store, "anthropic", "env", &env, &repo, 0)
            .await
            .expect_err("error");
        assert_eq!(error, "Environment provider account cannot be removed: env");
    }

    #[tokio::test]
    async fn a_stored_credential_hides_env_slots_matching_resolution_precedence() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut store = storage(&tmp);
        let repo = repository(&tmp);
        let env = |name: &str| (name == "ANTHROPIC_API_KEY").then(|| "env-key".to_owned());
        seed_pool(&mut store, "anthropic");
        let accounts = get_credential_accounts(&store, "anthropic", &env, &repo, 0)
            .await
            .expect("accounts");
        assert_eq!(
            accounts
                .iter()
                .map(|account| account.name.as_str())
                .collect::<Vec<_>>(),
            vec!["default", "work"]
        );
        assert!(
            accounts
                .iter()
                .all(|account| account.source != CredentialAccountSource::Env)
        );
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
        let accounts = get_credential_accounts(&store, "claude-sdk-oauth", &env_none(), &repo, 0)
            .await
            .expect("accounts");
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].name, "work");
    }

    #[test]
    fn renames_a_slot_display_name_and_validates_it() {
        let credential =
            json!({ "type": "api_key", "key": "k", "accounts": [{ "name": "work", "key": "k" }] });
        let renamed =
            rename_slot_display_name(&credential, "work", Some("Work Account")).expect("rename");
        assert_eq!(renamed["accounts"][0]["displayName"], "Work Account");
        let cleared = rename_slot_display_name(&renamed, "work", None).expect("clear");
        assert!(cleared["accounts"][0].get("displayName").is_none());
        assert_eq!(
            rename_slot_display_name(&credential, "missing", Some("x")).expect_err("error"),
            "Stored provider account not found: missing"
        );
        assert!(
            rename_slot_display_name(&credential, "work", Some("\u{7}beep"))
                .expect_err("error")
                .starts_with("Display name must be")
        );
        assert!(account_display_name(Some("  ")).is_none());
        assert!(account_display_name(Some(&"w".repeat(40))).is_none());
        assert_eq!(
            account_display_name(Some(" Work  Account ")).as_deref(),
            Some("Work Account")
        );
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
        let error =
            rename_credential_account(&store, "anthropic-subscription", "login-1", Some("x"))
                .await
                .expect_err("error");
        assert_eq!(error, "Stored provider account not found: login-1");
    }

    #[tokio::test]
    async fn pinning_without_a_stored_credential_only_works_for_the_subscription_lane() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = storage(&tmp);
        let repo = repository(&tmp);
        let error = pin_credential_account(&store, "openai", Some("work"), &env_none(), &repo, 0)
            .await
            .expect_err("error");
        assert_eq!(error, "Provider account not found: work");
        pin_credential_account(
            &store,
            "anthropic-subscription",
            Some("work"),
            &env_none(),
            &repo,
            0,
        )
        .await
        .expect_err("error");
    }
}
