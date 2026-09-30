//! Port of senpi packages/ai/src/auth/pool/slots.ts.

use crate::auth::types::{ApiKeyCredential, Credential, OAuthCredential};
use crate::legacy_provider_ids::LEGACY_PROVIDER_IDS;
use crate::types::ProviderEnv;
use fancy_regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;
use unicode_segmentation::UnicodeSegmentation;

pub const DEFAULT_SLOT_NAME: &str = "default";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CredentialSlotSource {
    Login,
    Import,
    Env,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CredentialSlot {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none", rename = "displayName")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<CredentialSlotSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<ProviderEnv>,
}

/// `Credential` plus the pool bookkeeping (`accounts`, `pinned`). Field order mirrors the TS
/// intersection type so JSON round-trips keep senpi's key set.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PooledCredential {
    #[serde(rename = "type")]
    pub credential_type: PooledCredentialType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<ProviderEnv>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accounts: Option<Vec<CredentialSlot>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PooledCredentialType {
    #[default]
    #[serde(rename = "api_key")]
    ApiKey,
    #[serde(rename = "oauth")]
    OAuth,
}

impl From<Credential> for PooledCredential {
    fn from(credential: Credential) -> Self {
        match credential {
            Credential::ApiKey(c) => {
                Self { credential_type: PooledCredentialType::ApiKey, key: c.key, env: c.env, ..Default::default() }
            }
            Credential::OAuth(c) => Self {
                credential_type: PooledCredentialType::OAuth,
                access: Some(c.access),
                refresh: Some(c.refresh),
                expires: Some(c.expires),
                env: env_from_extra(&c.extra),
                accounts: accounts_from_extra(&c.extra),
                pinned: pinned_from_extra(&c.extra),
                ..Default::default()
            },
        }
    }
}

fn env_from_extra(extra: &serde_json::Map<String, serde_json::Value>) -> Option<ProviderEnv> {
    let value = extra.get("env")?.as_object()?;
    let mut env = ProviderEnv::new();
    for (k, v) in value {
        env.insert(k.clone(), v.as_str()?.to_owned());
    }
    Some(env)
}

/// senpi stores the pooled object itself, so a credential read back from the store still carries
/// its `accounts`/`pinned` bookkeeping. `OAuthCredential`'s flattened extra keeps those keys, and
/// this reconstructs them; without it a slot-scoped resolution could never see a stored pool.
fn accounts_from_extra(extra: &serde_json::Map<String, serde_json::Value>) -> Option<Vec<CredentialSlot>> {
    serde_json::from_value(extra.get("accounts")?.clone()).ok()
}

fn pinned_from_extra(extra: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    extra.get("pinned").and_then(serde_json::Value::as_str).map(str::to_owned)
}

impl PooledCredential {
    pub fn to_flat_credential(&self) -> Option<Credential> {
        match self.credential_type {
            PooledCredentialType::ApiKey => {
                Some(Credential::ApiKey(ApiKeyCredential { key: self.key.clone(), env: self.env.clone() }))
            }
            PooledCredentialType::OAuth => {
                let mut cred = OAuthCredential::new(self.access.clone()?, self.refresh.clone()?, self.expires?);
                if let Some(env) = &self.env {
                    cred = cred.with_extra("env", serde_json::to_value(env).unwrap_or_default());
                }
                Some(Credential::OAuth(cred))
            }
        }
    }

    /// The credential shape senpi persists: the pooled object itself, with `accounts`/`pinned`
    /// riding alongside the flat fields so a store round-trip keeps the pool. Unlike
    /// [`Self::to_flat_credential`] this never drops sibling slots.
    pub fn to_stored_credential(&self) -> Credential {
        match self.credential_type {
            PooledCredentialType::ApiKey => {
                Credential::ApiKey(ApiKeyCredential { key: self.key.clone(), env: self.env.clone() })
            }
            PooledCredentialType::OAuth => {
                let mut cred = OAuthCredential::new(
                    self.access.clone().unwrap_or_default(),
                    self.refresh.clone().unwrap_or_default(),
                    self.expires.unwrap_or(0.0),
                );
                if let Some(env) = &self.env {
                    cred = cred.with_extra("env", serde_json::to_value(env).unwrap_or_default());
                }
                if let Some(accounts) = &self.accounts {
                    cred = cred.with_extra("accounts", serde_json::to_value(accounts).unwrap_or_default());
                }
                if let Some(pinned) = &self.pinned {
                    cred = cred.with_extra("pinned", serde_json::to_value(pinned).unwrap_or_default());
                }
                Credential::OAuth(cred)
            }
        }
    }
}

static SLOT_NAME_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-zA-Z0-9][a-zA-Z0-9_-]{0,63}$").expect("valid regex"));

pub fn assert_valid_slot_name(name: &str) -> Result<(), String> {
    if SLOT_NAME_PATTERN.is_match(name).unwrap_or(false) {
        Ok(())
    } else {
        Err(format!("Invalid account name '{name}': use letters, digits, '-' or '_', starting with a letter or digit"))
    }
}

pub const DISPLAY_NAME_MAX_COLUMNS: usize = 32;

fn is_mark(c: char) -> bool {
    matches!(
        unicode_general_category::get_general_category(c),
        unicode_general_category::GeneralCategory::NonspacingMark
            | unicode_general_category::GeneralCategory::SpacingMark
            | unicode_general_category::GeneralCategory::EnclosingMark
    )
}

fn is_invisible(base: char) -> bool {
    matches!(base, '\u{115f}' | '\u{1160}' | '\u{17b4}' | '\u{17b5}' | '\u{2800}' | '\u{3164}' | '\u{ffa0}')
        || is_mark(base)
}

fn is_wide(base: char) -> bool {
    let cp = base as u32;
    matches!(cp,
        0x1100..=0x115f | 0x2329 | 0x232a | 0x2e80..=0x303e | 0x3041..=0x33ff | 0x3400..=0x4dbf
        | 0x4e00..=0x9fff | 0xa000..=0xa4cf | 0xa960..=0xa97f | 0xac00..=0xd7a3 | 0xf900..=0xfaff
        | 0xfe10..=0xfe19 | 0xfe30..=0xfe6f | 0xff00..=0xff60 | 0xffe0..=0xffe6
        | 0x1f300..=0x1faff | 0x20000..=0x3fffd
    )
}

fn is_blank_render(base: char) -> bool {
    is_invisible(base) || base.is_whitespace()
}

pub fn display_name_columns(value: &str) -> usize {
    let mut columns = 0usize;
    for segment in value.graphemes(true) {
        let Some(base) = segment.chars().next() else { continue };
        if is_invisible(base) {
            continue;
        }
        columns += if is_wide(base) || segment.contains('\u{fe0f}') { 2 } else { 1 };
    }
    columns
}

fn normalize_display_name(value: &str) -> String {
    let nfc: String = unicode_normalization::UnicodeNormalization::nfc(value).collect();
    let trimmed = nfc.trim();
    let mut result = String::with_capacity(trimmed.len());
    let mut last_was_space = false;
    for ch in trimmed.chars() {
        if ch.is_whitespace() {
            if !last_was_space {
                result.push(' ');
            }
            last_was_space = true;
        } else {
            result.push(ch);
            last_was_space = false;
        }
    }
    result
}

fn cyrillic_lookalike(ch: char) -> char {
    match ch {
        '\u{0430}' => 'a',
        '\u{0432}' => 'b',
        '\u{0435}' => 'e',
        '\u{043a}' => 'k',
        '\u{043c}' => 'm',
        '\u{043d}' => 'h',
        '\u{043e}' => 'o',
        '\u{0440}' => 'p',
        '\u{0441}' => 'c',
        '\u{0442}' => 't',
        '\u{0443}' => 'y',
        '\u{0445}' => 'x',
        '\u{0455}' => 's',
        '\u{0456}' => 'i',
        '\u{0458}' => 'j',
        other => other,
    }
}

fn display_name_key(value: &str) -> String {
    let normalized = normalize_display_name(value);
    let nfkc: String = unicode_normalization::UnicodeNormalization::nfkc(normalized.as_str()).collect();
    nfkc.to_lowercase().chars().filter(|c| !is_invisible(*c)).map(cyrillic_lookalike).collect()
}

const UNSAFE_DISPLAY_CATEGORIES: &[unicode_general_category::GeneralCategory] = &[];

fn has_unsafe_display_characters(value: &str) -> bool {
    let _ = UNSAFE_DISPLAY_CATEGORIES;
    value.chars().any(|c| {
        matches!(
            unicode_general_category::get_general_category(c),
            unicode_general_category::GeneralCategory::Control
                | unicode_general_category::GeneralCategory::Format
                | unicode_general_category::GeneralCategory::LineSeparator
                | unicode_general_category::GeneralCategory::ParagraphSeparator
        )
    })
}

pub fn account_display_name(value: Option<&str>) -> Option<String> {
    let value = value?;
    if has_unsafe_display_characters(value) {
        return None;
    }
    let normalized = normalize_display_name(value);
    let mut chars = normalized.chars();
    let first = chars.next()?;
    if is_mark(first) {
        return None;
    }
    if normalized.chars().all(is_blank_render) {
        return None;
    }
    if display_name_columns(&normalized) <= DISPLAY_NAME_MAX_COLUMNS { Some(normalized) } else { None }
}

pub fn account_label(name: &str, display_name: Option<&str>) -> String {
    match account_display_name(display_name) {
        Some(display) => format!("{display} ({name})"),
        None => name.to_string(),
    }
}

pub fn rename_slot_display_name(
    credential: &PooledCredential,
    name: &str,
    value: Option<&str>,
) -> Result<PooledCredential, String> {
    assert_valid_slot_name(name)?;
    let accounts = credential.accounts.clone().unwrap_or_else(|| list_slots(Some(credential)));
    let Some(target) = accounts.iter().find(|slot| slot.name == name) else {
        return Err(format!("Stored provider account not found: {name}"));
    };
    if target.source == Some(CredentialSlotSource::Env) {
        return Err(format!("Environment provider account cannot be renamed: {name}"));
    }
    let display_name = match value {
        None => None,
        Some(value) => account_display_name(Some(value)),
    };
    if value.is_some() && display_name.is_none() {
        return Err(format!(
            "Display name must be 1-{DISPLAY_NAME_MAX_COLUMNS} terminal columns of visible text without control or formatting characters."
        ));
    }
    if let Some(display_name) = &display_name {
        let key = display_name_key(display_name);
        let duplicate = accounts.iter().any(|slot| {
            slot.name != name
                && display_name_key(account_display_name(slot.display_name.as_deref()).unwrap_or_default().as_str())
                    == key
        });
        if duplicate {
            return Err("Display name is already used by another account for this provider.".to_string());
        }
    }
    let mut next = credential.clone();
    next.accounts = Some(
        accounts
            .into_iter()
            .map(|mut slot| {
                if slot.name == name {
                    slot.display_name = display_name.clone();
                }
                slot
            })
            .collect(),
    );
    Ok(next)
}

fn stored_slots(credential: &PooledCredential) -> Vec<CredentialSlot> {
    credential.accounts.clone().unwrap_or_default()
}

fn credential_slot_env(credential: &Credential) -> Option<ProviderEnv> {
    match credential {
        Credential::ApiKey(c) => c.env.clone(),
        Credential::OAuth(c) => env_from_extra(&c.extra),
    }
}

fn slot_from_flat_credential_named(credential: &Credential, name: &str) -> CredentialSlot {
    match credential {
        Credential::OAuth(c) => CredentialSlot {
            name: name.to_string(),
            source: Some(CredentialSlotSource::Login),
            access: Some(c.access.clone()),
            refresh: Some(c.refresh.clone()),
            expires: Some(c.expires),
            env: credential_slot_env(credential),
            ..Default::default()
        },
        Credential::ApiKey(c) => CredentialSlot {
            name: name.to_string(),
            source: Some(CredentialSlotSource::Login),
            key: c.key.clone(),
            env: credential_slot_env(credential),
            ..Default::default()
        },
    }
}

fn slot_from_flat_credential(credential: &PooledCredential) -> Option<CredentialSlot> {
    let flat = credential.to_flat_credential()?;
    Some(slot_from_flat_credential_named(&flat, DEFAULT_SLOT_NAME))
}

pub fn list_slots(credential: Option<&PooledCredential>) -> Vec<CredentialSlot> {
    let Some(credential) = credential else { return vec![] };
    let slots = stored_slots(credential);
    if !slots.is_empty() {
        return slots;
    }
    slot_from_flat_credential(credential).into_iter().collect()
}

pub fn find_slot(credential: Option<&PooledCredential>, name: &str) -> Option<CredentialSlot> {
    list_slots(credential).into_iter().find(|slot| slot.name == name)
}

pub fn upsert_slot(credential: Option<PooledCredential>, slot: CredentialSlot) -> Result<PooledCredential, String> {
    assert_valid_slot_name(&slot.name)?;
    let base = credential.unwrap_or_else(|| {
        if slot.access.is_some() || slot.refresh.is_some() {
            PooledCredential {
                credential_type: PooledCredentialType::OAuth,
                access: Some(slot.access.clone().unwrap_or_default()),
                refresh: Some(slot.refresh.clone().unwrap_or_default()),
                expires: Some(slot.expires.unwrap_or(0.0)),
                ..Default::default()
            }
        } else {
            PooledCredential { credential_type: PooledCredentialType::ApiKey, key: slot.key.clone(), ..Default::default() }
        }
    });
    let existing = list_slots(Some(&base));
    let index = existing.iter().position(|candidate| candidate.name == slot.name);
    let accounts = match index {
        Some(_) => existing
            .into_iter()
            .map(|candidate| if candidate.name == slot.name { merge_slot(&candidate, &slot) } else { candidate })
            .collect(),
        None => {
            let mut v = existing;
            v.push(slot);
            v
        }
    };
    Ok(PooledCredential { accounts: Some(accounts), ..base })
}

fn merge_slot(base: &CredentialSlot, over: &CredentialSlot) -> CredentialSlot {
    CredentialSlot {
        name: over.name.clone(),
        display_name: over.display_name.clone().or_else(|| base.display_name.clone()),
        source: over.source.or(base.source),
        key: over.key.clone().or_else(|| base.key.clone()),
        access: over.access.clone().or_else(|| base.access.clone()),
        refresh: over.refresh.clone().or_else(|| base.refresh.clone()),
        expires: over.expires.or(base.expires),
        env: over.env.clone().or_else(|| base.env.clone()),
    }
}

fn slot_mirrors_flat(credential: &PooledCredential, slot: &CredentialSlot) -> bool {
    match credential.credential_type {
        PooledCredentialType::OAuth => slot.access == credential.access || slot.refresh == credential.refresh,
        PooledCredentialType::ApiKey => slot.key == credential.key,
    }
}

fn project_flat_fields(credential: &PooledCredential, slot: &CredentialSlot) -> PooledCredential {
    match credential.credential_type {
        PooledCredentialType::OAuth => {
            if slot.access.is_none() || slot.refresh.is_none() || slot.expires.is_none() {
                return credential.clone();
            }
            PooledCredential {
                access: slot.access.clone(),
                refresh: slot.refresh.clone(),
                expires: slot.expires,
                env: slot.env.clone().or_else(|| credential.env.clone()),
                ..credential.clone()
            }
        }
        PooledCredentialType::ApiKey => {
            PooledCredential { key: slot.key.clone(), env: slot.env.clone().or_else(|| credential.env.clone()), ..credential.clone() }
        }
    }
}

pub fn remove_slot(credential: Option<PooledCredential>, name: &str) -> Option<PooledCredential> {
    let credential = credential?;
    let existing = list_slots(Some(&credential));
    let removed = existing.iter().find(|slot| slot.name == name).cloned();
    let accounts: Vec<CredentialSlot> = existing.into_iter().filter(|slot| slot.name != name).collect();
    if accounts.is_empty() {
        return None;
    }
    let reprojected = match &removed {
        Some(removed) if slot_mirrors_flat(&credential, removed) => project_flat_fields(&credential, &accounts[0]),
        _ => credential,
    };
    let mut next = PooledCredential { accounts: Some(accounts), ..reprojected };
    if next.pinned.as_deref() == Some(name) {
        next.pinned = None;
    }
    Some(next)
}

pub fn pin_slot(credential: PooledCredential, name: &str) -> Result<PooledCredential, String> {
    assert_valid_slot_name(name)?;
    Ok(PooledCredential { pinned: Some(name.to_string()), ..credential })
}

pub fn project_slot(credential: Option<&PooledCredential>, name: &str) -> Option<Credential> {
    let credential = credential?;
    let slot = find_slot(Some(credential), name)?;
    let env = slot.env.clone().or_else(|| credential.env.clone());
    match credential.credential_type {
        PooledCredentialType::OAuth => {
            let (access, refresh, expires) = (slot.access?, slot.refresh?, slot.expires?);
            let mut cred = OAuthCredential::new(access, refresh, expires);
            if let Some(env) = env {
                cred = cred.with_extra("env", serde_json::to_value(env).unwrap_or_default());
            }
            Some(Credential::OAuth(cred))
        }
        PooledCredentialType::ApiKey => Some(Credential::ApiKey(ApiKeyCredential { key: slot.key, env })),
    }
}

pub fn next_login_slot_name(credential: &PooledCredential) -> Result<String, String> {
    let taken: std::collections::HashSet<String> = list_slots(Some(credential)).into_iter().map(|s| s.name).collect();
    for index in 2..1000 {
        let candidate = format!("login-{index}");
        if !taken.contains(&candidate) {
            return Ok(candidate);
        }
    }
    Err("Credential pool is full".into())
}

fn provided_slots(credential: &PooledCredential) -> Option<Vec<CredentialSlot>> {
    let accounts = credential.accounts.clone()?;
    if accounts.is_empty() { None } else { Some(accounts) }
}

fn merge_provided_pool(
    current: &PooledCredential,
    existing: &[CredentialSlot],
    provided: &[CredentialSlot],
) -> PooledCredential {
    let known: std::collections::HashSet<&str> = existing.iter().map(|s| s.name.as_str()).collect();
    let added: Vec<CredentialSlot> = provided.iter().filter(|slot| !known.contains(slot.name.as_str())).cloned().collect();
    if added.is_empty() {
        current.clone()
    } else {
        let mut accounts = existing.to_vec();
        accounts.extend(added);
        PooledCredential { accounts: Some(accounts), ..current.clone() }
    }
}

pub enum LoginSlotOrigin {
    Generated,
    Provider,
}

pub fn append_login_slot(
    current: Option<PooledCredential>,
    flat: PooledCredential,
    mut on_allocated: impl FnMut(&str, LoginSlotOrigin),
) -> Result<PooledCredential, String> {
    let provided = provided_slots(&flat);
    if let Some(provided) = &provided {
        let previous: std::collections::HashSet<String> = match &current {
            Some(current) if current.accounts.is_some() => {
                current.accounts.clone().unwrap_or_default().into_iter().map(|s| s.name).collect()
            }
            _ => list_slots(current.as_ref()).into_iter().map(|s| s.name).collect(),
        };
        let added: Vec<&CredentialSlot> = provided.iter().filter(|slot| !previous.contains(&slot.name)).collect();
        if added.len() == 1
            && SLOT_NAME_PATTERN.is_match(&added[0].name).unwrap_or(false)
            && added[0].source != Some(CredentialSlotSource::Env)
        {
            on_allocated(&added[0].name, LoginSlotOrigin::Provider);
        }
    }
    let Some(current) = current else {
        if provided.is_none() {
            on_allocated(DEFAULT_SLOT_NAME, LoginSlotOrigin::Generated);
        }
        return Ok(flat);
    };
    let stored_accounts = current.accounts.clone();
    if let Some(provided) = provided {
        return Ok(match stored_accounts {
            Some(stored) => merge_provided_pool(&current, &stored, &provided),
            None => flat,
        });
    }
    let name = next_login_slot_name(&current)?;
    let flat_credential = flat.to_flat_credential().ok_or("cannot derive flat credential")?;
    let next = upsert_slot(Some(current), slot_from_flat_credential_named(&flat_credential, &name))?;
    on_allocated(&name, LoginSlotOrigin::Generated);
    Ok(next)
}

pub fn managed_sentinel_material(provider_id: &str) -> String {
    format!("{provider_id}-managed")
}

pub fn managed_sentinel_materials(provider_id: &str) -> Vec<String> {
    let mut materials = vec![managed_sentinel_material(provider_id)];
    if let Some((legacy_id, _)) = LEGACY_PROVIDER_IDS.iter().find(|(_, canonical)| *canonical == provider_id) {
        let legacy_material = managed_sentinel_material(legacy_id);
        if !materials.contains(&legacy_material) {
            materials.push(legacy_material);
        }
    }
    materials
}

pub fn is_managed_sentinel_slot(provider_id: &str, slot: &CredentialSlot) -> bool {
    let materials = managed_sentinel_materials(provider_id);
    let access = slot.access.as_deref().unwrap_or("");
    materials.iter().any(|m| m == access) && slot.refresh.as_deref() == Some(access)
}

pub fn repair_managed_sentinel_slots(provider_id: &str, credential: &PooledCredential) -> Option<PooledCredential> {
    let accounts = credential.accounts.clone()?;
    let kept: Vec<CredentialSlot> =
        accounts.iter().filter(|slot| !is_managed_sentinel_slot(provider_id, slot)).cloned().collect();
    if kept.len() == accounts.len() {
        return None;
    }
    let mut repaired = PooledCredential { accounts: Some(kept.clone()), ..credential.clone() };
    if let Some(pinned) = &repaired.pinned
        && !kept.iter().any(|slot| &slot.name == pinned) {
            repaired.pinned = None;
        }
    Some(repaired)
}

pub fn merge_refreshed_slot(current: &PooledCredential, name: &str, refreshed: &Credential) -> PooledCredential {
    let Credential::OAuth(refreshed_oauth) = refreshed else { return current.clone() };
    if current.credential_type != PooledCredentialType::OAuth {
        return current.clone();
    }
    let Some(accounts) = &current.accounts else { return merge_refreshed(current, refreshed) };
    if accounts.is_empty() {
        return merge_refreshed(current, refreshed);
    }
    let Some(target) = accounts.iter().find(|slot| slot.name == name).cloned() else { return current.clone() };
    let mirrors_flat = target.access == current.access || target.refresh == current.refresh;
    let rotated_accounts: Vec<CredentialSlot> = accounts
        .iter()
        .map(|slot| {
            if slot.name == target.name {
                CredentialSlot {
                    access: Some(refreshed_oauth.access.clone()),
                    refresh: Some(refreshed_oauth.refresh.clone()),
                    expires: Some(refreshed_oauth.expires),
                    ..slot.clone()
                }
            } else {
                slot.clone()
            }
        })
        .collect();
    if mirrors_flat {
        PooledCredential {
            access: Some(refreshed_oauth.access.clone()),
            refresh: Some(refreshed_oauth.refresh.clone()),
            expires: Some(refreshed_oauth.expires),
            accounts: Some(rotated_accounts),
            ..current.clone()
        }
    } else {
        PooledCredential { accounts: Some(rotated_accounts), ..current.clone() }
    }
}

pub fn merge_refreshed(current: &PooledCredential, refreshed: &Credential) -> PooledCredential {
    let Some(accounts) = &current.accounts else { return refreshed.clone().into() };
    if accounts.is_empty() {
        return refreshed.clone().into();
    }
    let Credential::OAuth(refreshed_oauth) = refreshed else { return refreshed.clone().into() };
    if current.credential_type != PooledCredentialType::OAuth {
        return refreshed.clone().into();
    }
    let target = accounts.iter().find(|slot| slot.access == current.access || slot.refresh == current.refresh).cloned();
    let rotated_accounts: Vec<CredentialSlot> = match &target {
        Some(target) => accounts
            .iter()
            .map(|slot| {
                if slot.name == target.name {
                    CredentialSlot {
                        access: Some(refreshed_oauth.access.clone()),
                        refresh: Some(refreshed_oauth.refresh.clone()),
                        expires: Some(refreshed_oauth.expires),
                        ..slot.clone()
                    }
                } else {
                    slot.clone()
                }
            })
            .collect(),
        None => accounts.clone(),
    };
    PooledCredential {
        access: Some(refreshed_oauth.access.clone()),
        refresh: Some(refreshed_oauth.refresh.clone()),
        expires: Some(refreshed_oauth.expires),
        accounts: Some(rotated_accounts),
        ..current.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oauth_pool(access: &str, refresh: &str, accounts: Vec<CredentialSlot>) -> PooledCredential {
        PooledCredential {
            credential_type: PooledCredentialType::OAuth,
            access: Some(access.into()),
            refresh: Some(refresh.into()),
            expires: Some(1.0),
            accounts: Some(accounts),
            ..Default::default()
        }
    }

    fn slot(name: &str, access: &str, refresh: &str) -> CredentialSlot {
        CredentialSlot { name: name.into(), access: Some(access.into()), refresh: Some(refresh.into()), expires: Some(1.0), ..Default::default() }
    }

    #[test]
    fn list_slots_falls_back_to_flat_credential_as_default() {
        let credential = PooledCredential {
            credential_type: PooledCredentialType::ApiKey,
            key: Some("k".into()),
            ..Default::default()
        };
        let slots = list_slots(Some(&credential));
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0].name, DEFAULT_SLOT_NAME);
        assert_eq!(slots[0].key, Some("k".into()));
    }

    #[test]
    fn upsert_slot_appends_or_replaces() {
        let credential = oauth_pool("a1", "r1", vec![slot("default", "a1", "r1")]);
        let updated = upsert_slot(Some(credential), slot("secondary", "a2", "r2")).unwrap();
        assert_eq!(updated.accounts.unwrap().len(), 2);
    }

    #[test]
    fn remove_slot_drops_pool_when_empty_and_clears_pin() {
        let credential = PooledCredential { pinned: Some("only".into()), ..oauth_pool("a1", "r1", vec![slot("only", "a1", "r1")]) };
        assert_eq!(remove_slot(Some(credential), "only"), None);
    }

    #[test]
    fn remove_slot_reprojects_flat_fields_when_removed_slot_mirrored_flat() {
        let credential = oauth_pool("a1", "r1", vec![slot("default", "a1", "r1"), slot("secondary", "a2", "r2")]);
        let next = remove_slot(Some(credential), "default").unwrap();
        assert_eq!(next.access, Some("a2".into()));
        assert_eq!(next.refresh, Some("r2".into()));
    }

    #[test]
    fn project_slot_returns_flat_credential_for_named_slot() {
        let credential = oauth_pool("a1", "r1", vec![slot("default", "a1", "r1"), slot("secondary", "a2", "r2")]);
        let projected = project_slot(Some(&credential), "secondary").unwrap();
        assert_eq!(projected.as_oauth().unwrap().access, "a2");
    }

    #[test]
    fn managed_sentinel_material_and_detection() {
        assert_eq!(managed_sentinel_material("anthropic"), "anthropic-managed");
        let sentinel_slot = slot("default", "anthropic-managed", "anthropic-managed");
        assert!(is_managed_sentinel_slot("anthropic", &sentinel_slot));
        assert!(!is_managed_sentinel_slot("anthropic", &slot("default", "real", "real")));
    }

    #[test]
    fn repair_managed_sentinel_slots_drops_poisoned_entries() {
        let sentinel = managed_sentinel_material("anthropic-subscription");
        let credential = oauth_pool(
            &sentinel,
            &sentinel,
            vec![slot("default", "real-access", "real-refresh"), slot("login-2", &sentinel, &sentinel)],
        );
        let repaired = repair_managed_sentinel_slots("anthropic-subscription", &credential).unwrap();
        assert_eq!(repaired.accounts.clone().unwrap().len(), 1);
        assert_eq!(repaired.accounts.unwrap()[0].name, "default");

        let clean = remove_slot(Some(credential), "login-2").unwrap();
        assert!(repair_managed_sentinel_slots("anthropic-subscription", &clean).is_none());
        let flat = PooledCredential {
            credential_type: PooledCredentialType::OAuth,
            access: Some("a".into()),
            refresh: Some("r".into()),
            expires: Some(1.0),
            ..Default::default()
        };
        assert!(repair_managed_sentinel_slots("anthropic-subscription", &flat).is_none());
    }

    #[test]
    fn repair_clears_a_pin_that_pointed_at_a_dropped_slot() {
        let sentinel = managed_sentinel_material("anthropic-subscription");
        let mut credential = oauth_pool(
            &sentinel,
            &sentinel,
            vec![slot("default", "real-access", "real-refresh"), slot("login-2", &sentinel, &sentinel)],
        );
        credential.pinned = Some("login-2".into());
        let repaired = repair_managed_sentinel_slots("anthropic-subscription", &credential).unwrap();
        assert!(repaired.pinned.is_none());

        credential.pinned = Some("default".into());
        let repaired = repair_managed_sentinel_slots("anthropic-subscription", &credential).unwrap();
        assert_eq!(repaired.pinned.as_deref(), Some("default"));
    }

    #[test]
    fn merge_refreshed_rotates_matching_slot_and_flat_fields() {
        let current = oauth_pool("old-a", "old-r", vec![slot("default", "old-a", "old-r")]);
        let refreshed = Credential::OAuth(OAuthCredential::new("new-a", "new-r", 2.0));
        let merged = merge_refreshed(&current, &refreshed);
        assert_eq!(merged.access, Some("new-a".into()));
        assert_eq!(merged.accounts.unwrap()[0].access, Some("new-a".into()));
    }

    #[test]
    fn merge_refreshed_slot_only_rotates_named_slot() {
        let current = oauth_pool("a1", "r1", vec![slot("default", "a1", "r1"), slot("secondary", "a2", "r2")]);
        let refreshed = Credential::OAuth(OAuthCredential::new("new-a2", "new-r2", 2.0));
        let merged = merge_refreshed_slot(&current, "secondary", &refreshed);
        assert_eq!(merged.access, Some("a1".into()), "flat fields untouched when non-mirroring slot rotates");
        let accounts = merged.accounts.unwrap();
        assert_eq!(accounts[1].access, Some("new-a2".into()));
        assert_eq!(accounts[0].access, Some("a1".into()));
    }

    #[test]
    fn account_display_name_enforces_column_budget_and_rejects_control_chars() {
        assert_eq!(account_display_name(Some("Work")), Some("Work".into()));
        assert_eq!(account_display_name(Some("\u{0007}bad")), None);
        assert_eq!(account_display_name(Some(&"x".repeat(40))), None);
    }

    #[test]
    fn account_label_combines_display_name_and_name() {
        assert_eq!(account_label("acct1", Some("Work")), "Work (acct1)");
        assert_eq!(account_label("acct1", None), "acct1");
    }

    #[test]
    fn assert_valid_slot_name_rejects_bad_characters() {
        assert!(assert_valid_slot_name("valid-name_1").is_ok());
        assert!(assert_valid_slot_name("-bad").is_err());
        assert!(assert_valid_slot_name("has space").is_err());
    }

    #[test]
    fn append_login_slot_generates_default_for_first_login() {
        let mut allocated = None;
        let flat = PooledCredential { credential_type: PooledCredentialType::ApiKey, key: Some("k".into()), ..Default::default() };
        let result = append_login_slot(None, flat, |name, _origin| allocated = Some(name.to_string())).unwrap();
        assert_eq!(allocated, Some(DEFAULT_SLOT_NAME.to_string()));
        assert_eq!(result.key, Some("k".into()));
    }

    #[test]
    fn append_login_slot_allocates_incrementing_name_for_second_login() {
        let current = PooledCredential { credential_type: PooledCredentialType::ApiKey, key: Some("k1".into()), ..Default::default() };
        let flat = PooledCredential { credential_type: PooledCredentialType::ApiKey, key: Some("k2".into()), ..Default::default() };
        let mut allocated = None;
        let result = append_login_slot(Some(current), flat, |name, _| allocated = Some(name.to_string())).unwrap();
        assert_eq!(allocated, Some("login-2".to_string()));
        assert_eq!(result.accounts.unwrap().len(), 2);
    }

    fn region_env(region: &str) -> ProviderEnv {
        let mut env = ProviderEnv::new();
        env.insert("KIMI_CODE_REGION".into(), region.into());
        env
    }

    fn oauth_flat(access: &str, env: Option<ProviderEnv>) -> PooledCredential {
        PooledCredential {
            credential_type: PooledCredentialType::OAuth,
            access: Some(access.into()),
            refresh: Some(format!("{access}-refresh")),
            expires: Some(1.0),
            env,
            ..Default::default()
        }
    }

    fn login(current: Option<PooledCredential>, incoming: PooledCredential) -> PooledCredential {
        append_login_slot(current, incoming, |_, _| {}).unwrap()
    }

    fn projected_env(credential: &Credential) -> Option<ProviderEnv> {
        env_from_extra(&credential.as_oauth().expect("oauth").extra)
    }

    #[test]
    fn a_stored_pooled_oauth_credential_keeps_its_slots_and_pin() {
        let stored = serde_json::json!({
            "type": "oauth",
            "access": "expired-access",
            "refresh": "r1",
            "expires": 1,
            "accounts": [
                {"name": "default", "access": "expired-access", "refresh": "r1", "expires": 1, "source": "login"},
                {"name": "work", "access": "work-access", "refresh": "r2", "expires": 4_102_444_800_000.0, "source": "login"}
            ],
            "pinned": "work"
        });
        let credential: Credential = serde_json::from_value(stored).expect("senpi's pooled shape parses");
        let pooled: PooledCredential = credential.into();

        assert_eq!(
            list_slots(Some(&pooled)).iter().map(|slot| slot.name.clone()).collect::<Vec<_>>(),
            vec!["default", "work"]
        );
        assert_eq!(pooled.pinned.as_deref(), Some("work"));
        let projected = project_slot(Some(&pooled), "work").expect("work slot projects");
        assert_eq!(projected.as_oauth().expect("oauth").access, "work-access");
        assert_eq!(projected.as_oauth().expect("oauth").refresh, "r2");
    }

    #[test]
    fn a_second_login_in_another_region_does_not_inherit_the_first_accounts_region() {
        let mainland = region_env("mainland-cn");
        let global = region_env("global");
        let pooled = login(None, oauth_flat("cn-access", Some(mainland.clone())));
        let pooled = login(Some(pooled), oauth_flat("intl-access", Some(global.clone())));

        let slots = list_slots(Some(&pooled));
        assert_eq!(slots.iter().map(|slot| slot.env.clone()).collect::<Vec<_>>(), vec![Some(mainland), Some(global)]);

        let default = project_slot(Some(&pooled), "default").expect("default slot");
        assert_eq!(default.as_oauth().expect("oauth").access, "cn-access");
        assert_eq!(projected_env(&default), Some(region_env("mainland-cn")));

        let second = project_slot(Some(&pooled), "login-2").expect("login-2 slot");
        assert_eq!(second.as_oauth().expect("oauth").access, "intl-access");
        assert_eq!(projected_env(&second), Some(region_env("global")));
    }

    #[test]
    fn a_slot_without_its_own_env_still_reads_the_flat_credentials_env() {
        let mainland = region_env("mainland-cn");
        let pooled = login(None, oauth_flat("cn-access", Some(mainland.clone())));
        let pooled = login(Some(pooled), oauth_flat("second-access", None));

        let second = project_slot(Some(&pooled), "login-2").expect("login-2 slot");
        assert_eq!(second.as_oauth().expect("oauth").access, "second-access");
        assert_eq!(projected_env(&second), Some(mainland));
    }

    #[test]
    fn removing_the_projected_account_re_projects_the_survivors_env_onto_the_flat_fields() {
        let pooled = login(None, oauth_flat("cn-access", Some(region_env("mainland-cn"))));
        let pooled = login(Some(pooled), oauth_flat("intl-access", Some(region_env("global"))));

        let survivor = remove_slot(Some(pooled), "default").expect("a survivor remains");
        assert_eq!(survivor.access, Some("intl-access".into()));
        assert_eq!(survivor.env, Some(region_env("global")));
    }

    // --- credential-pool-mutations.test.ts ---

    fn api_key_pool() -> PooledCredential {
        PooledCredential {
            credential_type: PooledCredentialType::ApiKey,
            key: Some("primary-key".into()),
            accounts: Some(vec![
                CredentialSlot {
                    name: "default".into(),
                    key: Some("primary-key".into()),
                    source: Some(CredentialSlotSource::Login),
                    ..Default::default()
                },
                CredentialSlot {
                    name: "work".into(),
                    key: Some("work-key".into()),
                    source: Some(CredentialSlotSource::Login),
                    ..Default::default()
                },
            ]),
            pinned: Some("work".into()),
            ..Default::default()
        }
    }

    fn slot_names(credential: Option<&PooledCredential>) -> Vec<String> {
        list_slots(credential).into_iter().map(|slot| slot.name).collect()
    }

    fn flat_oauth(access: &str, refresh: &str, expires: f64) -> PooledCredential {
        PooledCredential {
            credential_type: PooledCredentialType::OAuth,
            access: Some(access.into()),
            refresh: Some(refresh.into()),
            expires: Some(expires),
            ..Default::default()
        }
    }

    fn login_slot(name: &str, access: &str, refresh: &str, expires: f64) -> CredentialSlot {
        CredentialSlot {
            name: name.into(),
            source: Some(CredentialSlotSource::Login),
            access: Some(access.into()),
            refresh: Some(refresh.into()),
            expires: Some(expires),
            ..Default::default()
        }
    }

    fn sentinel_pool() -> PooledCredential {
        PooledCredential {
            credential_type: PooledCredentialType::OAuth,
            access: Some("claude-sdk-oauth-managed".into()),
            refresh: Some("claude-sdk-oauth-managed".into()),
            expires: Some(4_102_444_800_000.0),
            accounts: Some(vec![login_slot("default", "real-a", "refresh-a", 999.0)]),
            ..Default::default()
        }
    }

    #[test]
    fn a_flat_oauth_credential_reads_as_a_one_slot_pool_carrying_its_tokens() {
        let flat = flat_oauth("a", "r", 123.0);
        let slots = list_slots(Some(&flat));
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0].name, DEFAULT_SLOT_NAME);
        assert_eq!(slots[0].source, Some(CredentialSlotSource::Login));
        assert_eq!(slots[0].access, Some("a".into()));
        assert_eq!(slots[0].refresh, Some("r".into()));
        assert_eq!(slots[0].expires, Some(123.0));
        assert_eq!(slots[0].key, None);
    }

    #[test]
    fn upsert_slot_preserves_the_pin() {
        let next = upsert_slot(
            Some(api_key_pool()),
            CredentialSlot { name: "default".into(), key: Some("rotated".into()), source: Some(CredentialSlotSource::Login), ..Default::default() },
        )
        .unwrap();
        assert_eq!(next.pinned.as_deref(), Some("work"));
    }

    #[test]
    fn upsert_slot_leaves_the_flat_credential_usable_by_a_build_predating_pools() {
        let flat = PooledCredential {
            credential_type: PooledCredentialType::ApiKey,
            key: Some("legacy-key".into()),
            ..Default::default()
        };
        let next = upsert_slot(
            Some(flat),
            CredentialSlot { name: "second".into(), key: Some("s".into()), source: Some(CredentialSlotSource::Login), ..Default::default() },
        )
        .unwrap();
        assert_eq!(next.credential_type, PooledCredentialType::ApiKey);
        assert_eq!(next.key, Some("legacy-key".into()));
        assert_eq!(slot_names(Some(&next)), vec!["default", "second"]);
    }

    #[test]
    fn append_login_slot_promotes_a_legacy_flat_credential_before_adding_a_login() {
        let current = flat_oauth("first-access", "first-refresh", 1.0);
        let incoming = flat_oauth("second-access", "second-refresh", 2.0);

        let next = append_login_slot(Some(current), incoming, |_, _| {}).unwrap();

        assert_eq!(next.credential_type, PooledCredentialType::OAuth);
        assert_eq!(next.access, Some("first-access".into()));
        assert_eq!(next.refresh, Some("first-refresh".into()));
        assert_eq!(next.expires, Some(1.0));
        assert_eq!(slot_names(Some(&next)), vec!["default", "login-2"]);
        let login_two = list_slots(Some(&next)).into_iter().find(|slot| slot.name == "login-2").expect("login-2");
        assert_eq!(login_two.access, Some("second-access".into()));
        assert_eq!(login_two.refresh, Some("second-refresh".into()));
        assert_eq!(login_two.expires, Some(2.0));
    }

    #[test]
    fn remove_slot_deletes_only_the_named_slot() {
        assert_eq!(slot_names(remove_slot(Some(api_key_pool()), "default").as_ref()), vec!["work"]);
    }

    #[test]
    fn remove_slot_leaves_the_flat_projection_alone_when_a_non_projected_slot_is_removed() {
        let promoted = append_login_slot(
            Some(flat_oauth("legacy-access", "legacy-refresh", 1.0)),
            flat_oauth("second-access", "second-refresh", 2.0),
            |_, _| {},
        )
        .unwrap();

        let next = remove_slot(Some(promoted), "login-2").unwrap();

        assert_eq!(slot_names(Some(&next)), vec!["default"]);
        assert_eq!(next.access, Some("legacy-access".into()));
        assert_eq!(next.refresh, Some("legacy-refresh".into()));
        assert_eq!(next.expires, Some(1.0));
    }

    #[test]
    fn remove_slot_re_projects_an_api_key_survivor_when_the_projected_slot_is_removed() {
        let promoted = append_login_slot(
            Some(PooledCredential { credential_type: PooledCredentialType::ApiKey, key: Some("legacy-key".into()), ..Default::default() }),
            PooledCredential { credential_type: PooledCredentialType::ApiKey, key: Some("second-key".into()), ..Default::default() },
            |_, _| {},
        )
        .unwrap();

        let next = remove_slot(Some(promoted), "default").unwrap();

        assert_eq!(slot_names(Some(&next)), vec!["login-2"]);
        assert_eq!(next.key, Some("second-key".into()));
    }

    #[test]
    fn append_login_slot_returns_a_provider_owned_pooled_login_result_unchanged() {
        let current = sentinel_pool();
        let mut accounts = list_slots(Some(&current));
        accounts.push(login_slot("account-2", "real-b", "refresh-b", 999.0));
        let provider_login_result = PooledCredential { accounts: Some(accounts), ..current.clone() };

        let next = append_login_slot(Some(current), provider_login_result.clone(), |_, _| {}).unwrap();

        assert_eq!(next, provider_login_result);
        assert_eq!(slot_names(Some(&next)), vec!["default", "account-2"]);
        assert_eq!(list_slots(Some(&next)).pop().unwrap().access, Some("real-b".into()));
    }

    #[test]
    fn a_provider_owned_pool_never_rewinds_a_sibling_the_browser_round_trip_left_behind() {
        // The provider builds its pool from a snapshot read BEFORE the interactive login; the value
        // under the credential lock at commit time may have moved on since.
        let snapshot = sentinel_pool();
        let rotated = login_slot("default", "rotated-access", "rotated-refresh", 4_102_444_800_000.0);
        let authoritative =
            PooledCredential { pinned: Some("default".into()), accounts: Some(vec![rotated.clone()]), ..snapshot.clone() };
        let mut provided = list_slots(Some(&snapshot));
        provided.push(login_slot("account-2", "real-b", "refresh-b", 999.0));
        let provider_login_result = PooledCredential { accounts: Some(provided), ..snapshot.clone() };

        let next = append_login_slot(Some(authoritative.clone()), provider_login_result, |_, _| {}).unwrap();

        assert_eq!(slot_names(Some(&next)), vec!["default", "account-2"]);
        let stored = next.accounts.as_ref().unwrap().iter().find(|slot| slot.name == "default").unwrap();
        assert_eq!(stored, authoritative.accounts.as_ref().unwrap().first().unwrap());
    }

    #[test]
    fn a_provider_owned_pool_onto_a_flat_current_keeps_the_whole_write_shape() {
        let flat = flat_oauth("legacy-a", "legacy-r", 999.0);
        let provider_login_result =
            PooledCredential { accounts: Some(vec![login_slot("default", "fresh-a", "fresh-r", 999.0)]), ..flat_oauth("legacy-a", "legacy-r", 999.0) };

        let next = append_login_slot(Some(flat), provider_login_result.clone(), |_, _| {}).unwrap();

        assert_eq!(next, provider_login_result);
    }

    #[test]
    fn append_login_slot_still_appends_an_unnamed_flat_oauth_credential_as_login_2() {
        let next = append_login_slot(Some(sentinel_pool()), flat_oauth("fresh-access", "fresh-refresh", 999.0), |_, _| {}).unwrap();

        assert_eq!(slot_names(Some(&next)), vec!["default", "login-2"]);
        let last = list_slots(Some(&next)).pop().unwrap();
        assert_eq!(last.access, Some("fresh-access".into()));
        assert_eq!(last.refresh, Some("fresh-refresh".into()));
    }

    #[test]
    fn append_login_slot_still_appends_an_unnamed_flat_api_key_credential() {
        let next = append_login_slot(
            Some(api_key_pool()),
            PooledCredential { credential_type: PooledCredentialType::ApiKey, key: Some("third-key".into()), ..Default::default() },
            |_, _| {},
        )
        .unwrap();

        assert_eq!(slot_names(Some(&next)), vec!["default", "work", "login-2"]);
        assert_eq!(list_slots(Some(&next)).pop().unwrap().key, Some("third-key".into()));
    }

    #[test]
    fn append_login_slot_without_a_current_credential_writes_the_flat_credential_as_is() {
        let flat = PooledCredential { credential_type: PooledCredentialType::ApiKey, key: Some("only-key".into()), ..Default::default() };
        assert_eq!(append_login_slot(None, flat.clone(), |_, _| {}).unwrap(), flat);
    }

    // --- account-display-names-unicode.test.ts ---

    const HANGUL_FILLER: char = '\u{3164}';
    const BRAILLE_BLANK: char = '\u{2800}';
    const NFD_CAFE: &str = "Cafe\u{0301}";
    const NFC_CAFE: &str = "Caf\u{00e9}";

    fn display_pool(display_names: &[Option<&str>]) -> PooledCredential {
        PooledCredential {
            credential_type: PooledCredentialType::OAuth,
            access: Some("fake-a".into()),
            refresh: Some("fake-r".into()),
            expires: Some(1.0),
            accounts: Some(
                display_names
                    .iter()
                    .enumerate()
                    .map(|(index, display_name)| CredentialSlot {
                        name: format!("slot-{}", index + 1),
                        display_name: display_name.map(str::to_string),
                        access: Some("fake-a".into()),
                        refresh: Some("fake-r".into()),
                        expires: Some(1.0),
                        source: Some(CredentialSlotSource::Login),
                        ..Default::default()
                    })
                    .collect(),
            ),
            ..Default::default()
        }
    }

    #[test]
    fn normalizes_to_nfc_and_collapses_internal_whitespace_runs_in_the_stored_form() {
        assert_eq!(account_display_name(Some(&format!("  {NFD_CAFE} "))), Some(NFC_CAFE.to_string()));
        assert_eq!(account_display_name(Some("Work  account")), Some("Work account".into()));
    }

    #[test]
    fn measures_the_limit_in_terminal_columns_not_utf16_code_units() {
        assert_eq!(account_display_name(Some(&"\u{4e2d}".repeat(20))), None);
        assert_eq!(account_display_name(Some(&"\u{4e2d}".repeat(16))), Some("\u{4e2d}".repeat(16)));
        let emoji = "\u{1f389}".repeat(16);
        assert_eq!(account_display_name(Some(&emoji)), Some(emoji.clone()));
        assert_eq!(account_display_name(Some(&"\u{1f389}".repeat(17))), None);
        assert_eq!(display_name_columns("\u{4e2d}\u{4e2d}"), 4);
        assert_eq!(display_name_columns("aa"), 2);
    }

    #[test]
    fn rejects_names_that_render_as_blank_or_begin_with_a_combining_mark() {
        assert_eq!(account_display_name(Some(&HANGUL_FILLER.to_string())), None);
        assert_eq!(account_display_name(Some(&HANGUL_FILLER.to_string().repeat(3))), None);
        assert_eq!(account_display_name(Some(&BRAILLE_BLANK.to_string().repeat(3))), None);
        assert_eq!(account_display_name(Some(" \t ")), None);
        assert_eq!(account_display_name(Some("\u{0301}abc")), None);
        let with_filler = format!("Work{HANGUL_FILLER}");
        assert_eq!(account_display_name(Some(&with_filler)), Some(with_filler));
    }

    #[test]
    fn omits_hand_written_metadata_that_fails_validation_from_labels() {
        assert_eq!(account_label("default", Some(&HANGUL_FILLER.to_string())), "default");
        assert_eq!(account_label("default", Some(&format!(" {NFD_CAFE} "))), format!("{NFC_CAFE} (default)"));
    }

    #[test]
    fn rejects_whitespace_nfc_nfd_fullwidth_invisible_and_homoglyph_duplicates() {
        let cases: [(&str, &str); 5] = [
            ("Work account", "Work  account"),
            (NFC_CAFE, NFD_CAFE),
            ("Work", "\u{ff37}ork"),
            ("Work", "Work\u{3164}"),
            ("Bork", "\u{0412}ork"),
        ];
        for (existing, duplicate) in cases {
            let credential =
                rename_slot_display_name(&display_pool(&[None, Some(existing)]), "slot-2", Some(existing)).unwrap();
            let error = rename_slot_display_name(&credential, "slot-1", Some(duplicate)).unwrap_err();
            assert!(error.contains("already used"), "{existing} vs {duplicate}: {error}");
        }
    }

    #[test]
    fn keeps_distinct_renderings_distinct() {
        let credential = rename_slot_display_name(&display_pool(&[None, None]), "slot-1", Some("Work account")).unwrap();
        assert!(rename_slot_display_name(&credential, "slot-2", Some("Workaccount")).is_ok());
        assert!(rename_slot_display_name(&credential, "slot-2", Some("W\u{00f6}rk")).is_ok());
    }

    #[test]
    fn pins_the_rejection_message_for_invalid_labels() {
        let error = rename_slot_display_name(&display_pool(&[None]), "slot-1", Some(&"\u{4e2d}".repeat(40))).unwrap_err();
        assert!(error.contains("1-32 terminal columns"), "{error}");
        let error = rename_slot_display_name(&display_pool(&[None]), "slot-1", Some("")).unwrap_err();
        assert!(error.contains("1-32 terminal columns"), "{error}");
        let error = rename_slot_display_name(&display_pool(&[None]), "missing", Some("Valid")).unwrap_err();
        assert!(error.contains("not found"), "{error}");
    }

    // --- anthropic-subscription-rename.test.ts ---

    #[test]
    fn normalizes_the_legacy_claude_sdk_oauth_id_to_anthropic_subscription() {
        assert_eq!(crate::legacy_provider_ids::normalize_provider_id("claude-sdk-oauth"), "anthropic-subscription");
        assert_eq!(crate::legacy_provider_ids::normalize_provider_id("anthropic-subscription"), "anthropic-subscription");
    }

    #[test]
    fn exposes_a_widened_managed_sentinel_materials_helper() {
        assert_eq!(
            managed_sentinel_materials("anthropic-subscription"),
            vec!["anthropic-subscription-managed".to_string(), "claude-sdk-oauth-managed".to_string()]
        );
    }

    #[test]
    fn matches_both_the_canonical_and_the_legacy_managed_sentinel_material_under_the_new_id() {
        for material in ["claude-sdk-oauth-managed", "anthropic-subscription-managed"] {
            let slot = CredentialSlot {
                name: "login-1".into(),
                access: Some(material.into()),
                refresh: Some(material.into()),
                ..Default::default()
            };
            assert!(is_managed_sentinel_slot("anthropic-subscription", &slot), "{material} must be recognized");
        }
    }

    #[test]
    fn does_not_widen_the_sentinel_match_across_unrelated_providers() {
        let foreign = CredentialSlot {
            name: "login-1".into(),
            access: Some("claude-sdk-oauth-managed".into()),
            refresh: Some("claude-sdk-oauth-managed".into()),
            ..Default::default()
        };
        assert!(!is_managed_sentinel_slot("other-provider", &foreign));
        let half = CredentialSlot {
            name: "login-1".into(),
            access: Some("real-access".into()),
            refresh: Some("claude-sdk-oauth-managed".into()),
            ..Default::default()
        };
        assert!(!is_managed_sentinel_slot("anthropic-subscription", &half));
    }

    #[test]
    fn keeps_the_wire_api_id_frozen_on_the_prompt_cache_ttl_table() {
        let models = crate::models_generated::get_builtin_provider_models("anthropic").expect("anthropic catalog");
        let mut model = (**models.first().expect("at least one anthropic model")).clone();
        model.api = "claude-sdk-oauth".into();
        assert_eq!(crate::utils::prompt_cache_ttl::resolve_prompt_cache_ttl_seconds(&model, None), Some(300));
    }
}

/// `credential-pool-mutations.test.ts` drives ordinary resolution after removing a promoted
/// account and `credential-pool-write-paths.test.ts` drives a slot-scoped refresh; both need the
/// resolve choke point, so they live in their own module to keep the pool algebra tests free of
/// the auth-resolution imports.
#[cfg(test)]
mod pool_resolution_tests {
    use super::*;
    use crate::auth::credential_store::InMemoryCredentialStore;
    use crate::auth::resolve::resolve_provider_auth;
    use crate::auth::types::{
        AuthContext, Credential, CredentialStore, ModelAuth, OAuthAuth, OAuthCredential, ProviderAuth, ProviderAuthInteraction,
    };
    use crate::utils::abort::{AbortController, AbortSignal};
    use async_trait::async_trait;
    use std::sync::Arc;

    const FUTURE: f64 = 4_102_444_800_000.0;

    fn flat_oauth(access: &str, refresh: &str, expires: f64) -> PooledCredential {
        PooledCredential {
            credential_type: PooledCredentialType::OAuth,
            access: Some(access.into()),
            refresh: Some(refresh.into()),
            expires: Some(expires),
            ..Default::default()
        }
    }

    fn login_slot(name: &str, access: &str, refresh: &str, expires: f64) -> CredentialSlot {
        CredentialSlot {
            name: name.into(),
            source: Some(CredentialSlotSource::Login),
            access: Some(access.into()),
            refresh: Some(refresh.into()),
            expires: Some(expires),
            ..Default::default()
        }
    }

    struct EmptyCtx;

    #[async_trait]
    impl AuthContext for EmptyCtx {
        async fn env(&self, _name: &str) -> Option<String> {
            None
        }
        async fn file_exists(&self, _path: &str) -> bool {
            false
        }
    }

    struct PassthroughOAuth;

    #[async_trait]
    impl OAuthAuth for PassthroughOAuth {
        fn name(&self) -> &str {
            "passthrough"
        }
        async fn login(&self, _interaction: &ProviderAuthInteraction) -> anyhow::Result<OAuthCredential> {
            unimplemented!()
        }
        async fn refresh(&self, credential: &OAuthCredential, _signal: &AbortSignal) -> anyhow::Result<OAuthCredential> {
            Ok(credential.clone())
        }
        async fn to_auth(&self, credential: &OAuthCredential) -> anyhow::Result<ModelAuth> {
            Ok(ModelAuth { api_key: Some(credential.access.clone()), ..Default::default() })
        }
    }

    /// senpi stores the pooled object itself; this round-trips a pool through the stored
    /// credential shape exactly as the app's auth.json writer does.
    fn stored_pool(pooled: &PooledCredential) -> Credential {
        pooled.to_stored_credential()
    }

    async fn seeded_store(provider_id: &str, pooled: &PooledCredential) -> Arc<dyn CredentialStore> {
        let store = Arc::new(InMemoryCredentialStore::new());
        let stored = stored_pool(pooled);        store
            .modify(provider_id, Box::new(move |_| Box::pin(async move { Ok(Some(stored)) })), None)
            .await
            .unwrap();
        store
    }

    async fn resolve(provider_id: &str, provider_auth: &ProviderAuth, store: &Arc<dyn CredentialStore>, slot_name: Option<&str>) -> Option<crate::auth::types::AuthResult> {
        let ctx = EmptyCtx;
        resolve_provider_auth(
            provider_id,
            provider_auth,
            store,
            &ctx,
            slot_name,
            None,
            &AbortController::new().signal(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn removing_the_promoted_default_makes_login_2_the_credential_ordinary_requests_use() {
        let promoted = append_login_slot(
            Some(flat_oauth("legacy-access", "legacy-refresh", FUTURE)),
            flat_oauth("second-access", "second-refresh", FUTURE),
            |_, _| {},
        )
        .unwrap();
        let remaining = remove_slot(Some(promoted), "default").expect("remove_slot dropped a pool that still had a slot");
        let store = seeded_store("removeoauth", &remaining).await;

        let provider_auth = ProviderAuth { api_key: None, oauth: Some(Arc::new(PassthroughOAuth)) };
        let resolved = resolve("removeoauth", &provider_auth, &store, None).await;

        assert_eq!(resolved.expect("resolved").auth.api_key, Some("second-access".into()));
    }

    #[tokio::test]
    async fn removing_login_2_keeps_the_promoted_default_the_credential_ordinary_requests_use() {
        let promoted = append_login_slot(
            Some(flat_oauth("legacy-access", "legacy-refresh", FUTURE)),
            flat_oauth("second-access", "second-refresh", FUTURE),
            |_, _| {},
        )
        .unwrap();
        let remaining = remove_slot(Some(promoted), "login-2").expect("remove_slot dropped a pool that still had a slot");
        let store = seeded_store("removeoauth", &remaining).await;

        let provider_auth = ProviderAuth { api_key: None, oauth: Some(Arc::new(PassthroughOAuth)) };
        let resolved = resolve("removeoauth", &provider_auth, &store, None).await;

        assert_eq!(resolved.expect("resolved").auth.api_key, Some("legacy-access".into()));
    }

    struct RotatingOAuth;

    #[async_trait]
    impl OAuthAuth for RotatingOAuth {
        fn name(&self) -> &str {
            "rotating"
        }
        async fn login(&self, _interaction: &ProviderAuthInteraction) -> anyhow::Result<OAuthCredential> {
            unimplemented!()
        }
        async fn refresh(&self, credential: &OAuthCredential, _signal: &AbortSignal) -> anyhow::Result<OAuthCredential> {
            Ok(OAuthCredential::new(
                format!("refreshed-{}", credential.refresh),
                format!("{}-next", credential.refresh),
                FUTURE,
            ))
        }
        async fn to_auth(&self, credential: &OAuthCredential) -> anyhow::Result<ModelAuth> {
            Ok(ModelAuth { api_key: Some(credential.access.clone()), ..Default::default() })
        }
    }

    #[tokio::test]
    async fn resolve_provider_auth_refreshes_the_matching_slot_and_leaves_siblings_byte_identical() {
        let pooled = PooledCredential {
            credential_type: PooledCredentialType::OAuth,
            access: Some("expired-access".into()),
            refresh: Some("r1".into()),
            expires: Some(1.0),
            accounts: Some(vec![
                login_slot("default", "expired-access", "r1", 1.0),
                login_slot("work", "work-access", "r2", FUTURE),
            ]),
            pinned: Some("work".into()),
            ..Default::default()
        };
        let store = seeded_store("pooloauth", &pooled).await;

        let provider_auth = ProviderAuth { api_key: None, oauth: Some(Arc::new(RotatingOAuth)) };
        let resolved = resolve("pooloauth", &provider_auth, &store, Some("default")).await;

        assert_eq!(resolved.expect("resolved").auth.api_key, Some("refreshed-r1".into()));

        let stored = store.read("pooloauth", None).await.unwrap().expect("stored credential");
        let pooled: PooledCredential = stored.into();
        let slots = list_slots(Some(&pooled));
        let work = slots.iter().find(|slot| slot.name == "work").expect("work slot");
        assert_eq!(work.access, Some("work-access".into()));
        assert_eq!(work.refresh, Some("r2".into()));
        assert_eq!(work.expires, Some(FUTURE));
        assert_eq!(pooled.pinned.as_deref(), Some("work"));
        let default = slots.iter().find(|slot| slot.name == "default").expect("default slot");
        assert_eq!(default.access, Some("refreshed-r1".into()));
        assert_eq!(default.refresh, Some("r1-next".into()));
    }
}
