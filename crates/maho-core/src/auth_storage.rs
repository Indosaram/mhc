//! Port of senpi packages/coding-agent/src/core/auth-storage.ts (the storage layer).
//!
//! Credentials are kept as serde_json values so an auth.json read and rewritten keeps every field
//! and key order; typed accessors expose the api_key/oauth shapes senpi validates on load.
//!
//! AuthStorage also drives OAuth login and logout through maho-ai's OAuth providers and
//! dynamically registered extension OAuth providers.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use serde_json::{Map, Value};

use maho_ai::auth::types::{
    AccountLoginOrigin, AccountLoginReceipt, AuthEvent, AuthInteraction, AuthPrompt, AuthPromptKind, Credential, OAuthAuth, OAuthCredential,
    ProviderAuthInteraction,
};
use maho_ai::compat::extension_oauth_types::{
    OAuthAuthInfo, OAuthDeviceCodeInfo, OAuthLoginCallbacks, OAuthPrompt, OAuthSelectOption, OAuthSelectPrompt,
};
use maho_ai::utils::abort::{AbortController, AbortSignal};

use crate::config::get_agent_dir;
use crate::credential_pool::slots::{CredentialSlot, PooledCredential};
use crate::lockfile_policy::{FILE_STORAGE_LOCK_RETRY_BUDGET_MS, CredentialStoreBusyError, acquire_lock_async, acquire_lock_sync, FILE_STORAGE_SYNC_LOCK_BUDGET_MS};
use crate::paths::{get_file_content_revision, normalize_path};
use crate::resolve_config_value::{is_command_config_value, resolve_config_value};
use crate::text::strip_bom;

pub type AuthStorageData = Map<String, Value>;

pub const AUTH_FILE_MODE: u32 = 0o600;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialKind {
    ApiKey,
    Oauth,
}

pub fn credential_kind(credential: &Value) -> Option<CredentialKind> {
    match credential.get("type").and_then(Value::as_str) {
        Some("api_key") => Some(CredentialKind::ApiKey),
        Some("oauth") => Some(CredentialKind::Oauth),
        _ => None,
    }
}

pub fn credential_key(credential: &Value) -> Option<&str> {
    credential.get("key").and_then(Value::as_str)
}

pub fn credential_env(credential: &Value) -> Option<HashMap<String, String>> {
    let env = credential.get("env")?.as_object()?;
    let mut out = HashMap::new();
    for (key, value) in env {
        out.insert(key.clone(), value.as_str()?.to_owned());
    }
    Some(out)
}

/// senpi validates every entry on load: an unknown type or a malformed credential is an error.
fn validate_credential(provider_id: &str, credential: &Value) -> Result<(), String> {
    let Some(object) = credential.as_object() else {
        return Err(format!("Invalid auth.json credential for provider \"{provider_id}\""));
    };
    match object.get("type").and_then(Value::as_str) {
        Some("api_key") => {
            let key_ok = match object.get("key") {
                None => true,
                Some(Value::String(_)) => true,
                Some(_) => false,
            };
            let env_ok = match object.get("env") {
                None => true,
                Some(Value::Object(env)) => env.values().all(Value::is_string),
                Some(_) => false,
            };
            if key_ok && env_ok {
                Ok(())
            } else {
                Err(format!("Invalid auth.json credential for provider \"{provider_id}\""))
            }
        }
        Some("oauth") => {
            let access_ok = object.get("access").map(Value::is_string).unwrap_or(false);
            let refresh_ok = object.get("refresh").map(Value::is_string).unwrap_or(false);
            let expires_ok = object.get("expires").and_then(Value::as_f64).map(f64::is_finite).unwrap_or(false);
            if access_ok && refresh_ok && expires_ok {
                Ok(())
            } else {
                Err(format!("Invalid auth.json credential for provider \"{provider_id}\""))
            }
        }
        _ => Err(format!("Invalid auth.json credential for provider \"{provider_id}\""))
    }
}

pub fn parse_auth_json(content: &str) -> Result<AuthStorageData, String> {
    let parsed: Value = serde_json::from_str(strip_bom(content)).map_err(|error| format!("Failed to read auth.json: {error}"))?;
    let Some(object) = parsed.as_object() else {
        return Err("Invalid auth.json: expected an object".to_owned());
    };
    for (provider_id, credential) in object {
        validate_credential(provider_id, credential)?;
    }
    Ok(object.clone())
}

/// Heals pools poisoned by a build that stored a provider-owned pool's flat sentinel as a generated
/// login-N slot: such a slot resolves to sentinel material and hard-errors every request that picks it.
pub fn repair_poisoned_pool_slots(data: &AuthStorageData) -> (AuthStorageData, bool) {
    let mut repaired: Option<AuthStorageData> = None;
    for (provider_id, credential) in data {
        if !credential.is_object() {
            continue;
        }
        if let Some(healed) = repair_managed_sentinel_slots(provider_id, credential) {
            let target = repaired.get_or_insert_with(|| data.clone());
            target.insert(provider_id.clone(), healed);
        }
    }
    match repaired {
        Some(repaired) => (repaired, true),
        None => (data.clone(), false),
    }
}

/// A generated slot that carries the flat sentinel material is dropped: the flat fields already hold
/// that material, so the slot is redundant and poisonous.
fn repair_managed_sentinel_slots(provider_id: &str, credential: &Value) -> Option<Value> {
    let object = credential.as_object()?;
    let accounts = object.get("accounts")?.as_array()?;
    let flat = PooledCredential {
        credential_type: object.get("type").and_then(Value::as_str).unwrap_or_default().to_owned(),
        key: credential_key(credential).map(str::to_owned),
        access: object.get("access").and_then(Value::as_str).map(str::to_owned),
        refresh: object.get("refresh").and_then(Value::as_str).map(str::to_owned),
        expires: object.get("expires").and_then(Value::as_i64),
        accounts: None,
        pinned: object.get("pinned").and_then(Value::as_str).map(str::to_owned),
    };
    let flat_slots: Vec<CredentialSlot> = flat.list_slots();
    let sentinel = flat_slots.first().cloned().unwrap_or_default();
    let mut healed: Vec<Value> = Vec::new();
    let mut changed = false;
    for account in accounts {
        let name = account.get("name").and_then(Value::as_str).unwrap_or_default();
        let is_generated = name == "default" || name.strip_prefix("login-").map(|index| index.chars().all(|c| c.is_ascii_digit())).unwrap_or(false);
        let carries_sentinel = account.get("key").and_then(Value::as_str) == sentinel.key.as_deref()
            && account.get("access").and_then(Value::as_str) == sentinel.access.as_deref()
            && account.get("refresh").and_then(Value::as_str) == sentinel.refresh.as_deref()
            && sentinel.key.is_some();
        if is_generated && carries_sentinel {
            changed = true;
            continue;
        }
        healed.push(account.clone());
    }
    if !changed {
        return None;
    }
    let _ = provider_id;
    let mut updated = object.clone();
    updated.insert("accounts".to_owned(), Value::Array(healed));
    Some(Value::Object(updated))
}

/// A write stages a fresh 0o600 file and renames it over the store, so the credential file is never
/// briefly world-readable and never half-written.
pub fn write_auth_file(path: &str, content: &str) -> Result<(), String> {
    let temporary = format!("{path}.{}.tmp", std::process::id());
    if let Err(error) = std::fs::write(&temporary, content) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error.to_string());
    }
    set_mode(&temporary, AUTH_FILE_MODE);
    #[cfg(unix)]
    if Path::new(path).exists()
        && let Ok(metadata) = std::fs::metadata(path) {
            use std::os::unix::fs::PermissionsExt;
            let mode = metadata.permissions().mode() & 0o777;
            set_mode(&temporary, mode);
        }
    if let Err(error) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error.to_string());
    }
    Ok(())
}

fn set_mode(path: &str, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
}

pub struct FileAuthStorageBackend {
    auth_path: String,
}

impl FileAuthStorageBackend {
    pub fn new(auth_path: &str) -> Self {
        Self { auth_path: normalize_path(auth_path, &crate::paths::PathInputOptions::default()) }
    }

    pub fn auth_path(&self) -> &str {
        &self.auth_path
    }

    fn ensure_parent_dir(&self) -> Result<(), String> {
        if let Some(parent) = Path::new(&self.auth_path).parent()
            && !parent.exists() {
                std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                set_mode(&parent.to_string_lossy(), 0o700);
            }
        Ok(())
    }

    fn ensure_file_exists(&self) -> Result<(), String> {
        if !Path::new(&self.auth_path).exists() {
            write_auth_file(&self.auth_path, "{}")?;
        }
        Ok(())
    }

    pub fn with_lock<T>(&self, action: impl FnOnce(Option<&str>) -> Result<(T, Option<String>), String>) -> Result<T, String> {
        self.ensure_parent_dir()?;
        self.ensure_file_exists()?;
        let mut guard = acquire_lock_sync(&self.auth_path, FILE_STORAGE_SYNC_LOCK_BUDGET_MS)
            .map_err(|error: CredentialStoreBusyError| error.message())?;
        let current = std::fs::read_to_string(&self.auth_path).ok();
        let (result, next) = action(current.as_deref())?;
        if let Some(next) = next && !guard.broker_write(&next)? { write_auth_file(&self.auth_path, &next)?; }
        Ok(result)
    }

    pub async fn with_lock_async<T>(
        &self,
        action: impl AsyncFnOnce(Option<&str>) -> Result<(T, Option<String>), String>,
    ) -> Result<T, String> {
        self.ensure_parent_dir()?;
        self.ensure_file_exists()?;
        let mut guard = acquire_lock_async(&self.auth_path, FILE_STORAGE_LOCK_RETRY_BUDGET_MS)
            .await
            .map_err(|error: CredentialStoreBusyError| error.message())?;
        let current = std::fs::read_to_string(&self.auth_path).ok();
        let (result, next) = action(current.as_deref()).await?;
        if let Some(next) = next {
            let path = self.auth_path.clone();
            tokio::task::spawn_blocking(move || {
                if !guard.broker_write(&next)? { write_auth_file(&path, &next)?; }
                Ok::<(), String>(())
            }).await.map_err(|error| error.to_string())??;
        }
        Ok(result)
    }
}

#[derive(Default)]
pub struct InMemoryAuthStorageBackend {
    value: std::sync::Mutex<Option<String>>,
}

impl InMemoryAuthStorageBackend {
    pub fn with_lock<T>(&self, action: impl FnOnce(Option<&str>) -> Result<(T, Option<String>), String>) -> Result<T, String> {
        let mut guard = self.value.lock().expect("in-memory auth lock");
        let (result, next) = action(guard.as_deref())?;
        if let Some(next) = next {
            *guard = Some(next);
        }
        Ok(result)
    }

    pub fn content(&self) -> Option<String> {
        self.value.lock().expect("in-memory auth lock").clone()
    }
}

/// One-off synchronous read of a stored credential, without resolving configured key values.
pub fn read_stored_credential(provider_id: &str, auth_path: &str) -> Option<Value> {
    let content = std::fs::read_to_string(normalize_path(auth_path, &crate::paths::PathInputOptions::default())).ok()?;
    let data = parse_auth_json(&content).ok()?;
    read_by_provider_id(&data, provider_id)
}

/// Reads a record keyed by provider id, preferring the canonical key over legacy spellings.
pub fn read_by_provider_id(data: &AuthStorageData, provider_id: &str) -> Option<Value> {
    let canonical = maho_ai::legacy_provider_ids::normalize_provider_id(provider_id);
    if let Some(credential) = data.get(&canonical) {
        return Some(credential.clone());
    }
    for legacy in maho_ai::legacy_provider_ids::legacy_provider_ids_for(&canonical) {
        if let Some(credential) = data.get(legacy) {
            return Some(credential.clone());
        }
    }
    None
}

pub fn serialize_auth_data(data: &AuthStorageData) -> String {
    let mut serialized = serde_json::to_string_pretty(&Value::Object(data.clone())).unwrap_or_else(|_| "{}".to_owned());
    serialized.push('\n');
    serialized
}

pub const READ_ONLY_ERROR: &str = "Read-only credential storage cannot modify auth.json";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OAuthProviderInfo {
    pub id: String,
    pub name: String,
}

struct LegacyInteraction {
    callbacks: Arc<dyn OAuthLoginCallbacks>,
    signal: AbortSignal,
}

#[async_trait::async_trait]
impl AuthInteraction for LegacyInteraction {
    fn signal(&self) -> Option<AbortSignal> {
        Some(self.signal.clone())
    }

    async fn prompt(&self, prompt: AuthPrompt) -> anyhow::Result<String> {
        match prompt.kind {
            AuthPromptKind::ManualCode { message, placeholder } => {
                if let Some(fut) = self.callbacks.on_manual_code_input() {
                    Ok(fut.await)
                } else {
                    Ok(self.callbacks.on_prompt(OAuthPrompt {
                        message,
                        placeholder,
                        allow_empty: None,
                        signal: prompt.signal,
                    }).await)
                }
            }
            AuthPromptKind::Select { message, options } => {
                let select_options = options
                    .into_iter()
                    .map(|opt| OAuthSelectOption {
                        id: opt.id,
                        label: opt.label,
                        description: opt.description,
                    })
                    .collect();
                let value = self.callbacks.on_select(OAuthSelectPrompt {
                    message,
                    options: select_options,
                    signal: prompt.signal,
                }).await;
                match value {
                    Some(v) => Ok(v),
                    None => Err(anyhow::anyhow!("Login cancelled")),
                }
            }
            AuthPromptKind::Text { message, placeholder }
            | AuthPromptKind::Secret { message, placeholder } => {
                Ok(self.callbacks.on_prompt(OAuthPrompt {
                    message,
                    placeholder,
                    allow_empty: None,
                    signal: prompt.signal,
                }).await)
            }
        }
    }

    fn notify(&self, event: AuthEvent) {
        match event {
            AuthEvent::AuthUrl { url, instructions } => {
                self.callbacks.on_auth(OAuthAuthInfo { url, instructions });
            }
            AuthEvent::DeviceCode { user_code, verification_uri, interval_seconds, expires_in_seconds } => {
                self.callbacks.on_device_code(OAuthDeviceCodeInfo {
                    user_code,
                    verification_uri,
                    interval_seconds: interval_seconds.map(|v| v as u64),
                    expires_in_seconds: expires_in_seconds.map(|v| v as u64),
                });
            }
            AuthEvent::Info { message, .. } | AuthEvent::Progress { message } => {
                self.callbacks.on_progress(&message);
            }
        }
    }
}

pub struct ReadOnlyAuthStorage {
    auth_path: String,
    data: AuthStorageData,
}

impl ReadOnlyAuthStorage {
    pub fn new(auth_path: &str) -> Result<Self, String> {
        let auth_path = normalize_path(auth_path, &crate::paths::PathInputOptions::default());
        let content = std::fs::read_to_string(&auth_path).map_err(|error| format!("Failed to read auth.json: {error}"))?;
        let data = parse_auth_json(&content)?;
        let (data, _) = repair_poisoned_pool_slots(&data);
        Ok(Self { auth_path, data })
    }

    pub fn auth_path(&self) -> &str {
        &self.auth_path
    }

    pub fn data(&self) -> &AuthStorageData {
        &self.data
    }

    pub async fn read(&self, provider_id: &str) -> Result<Option<Value>, String> {
        let Some(credential) = read_by_provider_id(&self.data, provider_id) else { return Ok(None) };
        let Some(key) = credential_key(&credential) else { return Ok(Some(credential)) };
        if credential_kind(&credential) != Some(CredentialKind::ApiKey) || !is_command_config_value(key) {
            return Ok(Some(credential));
        }
        let env = credential_env(&credential);
        let resolved = resolve_config_value(key, env.as_ref()).await;
        let mut credential = credential;
        if let Some(resolved) = resolved {
            credential["key"] = Value::from(resolved);
        }
        Ok(Some(credential))
    }

    pub fn list(&self) -> Vec<(String, CredentialKind)> {
        self.data
            .iter()
            .filter_map(|(provider_id, credential)| credential_kind(credential).map(|kind| (provider_id.clone(), kind)))
            .collect()
    }

    pub fn modify(&self) -> Result<(), String> {
        Err(READ_ONLY_ERROR.to_owned())
    }

    pub fn delete(&self) -> Result<(), String> {
        Err(READ_ONLY_ERROR.to_owned())
    }
}

pub struct AuthStorage {
    storage: AuthStorageBackend,
    auth_path: Option<String>,
    data: std::sync::Mutex<AuthStorageData>,
    runtime_overrides: std::sync::Mutex<HashMap<String, String>>,
    extension_oauth_providers: std::sync::Mutex<HashMap<String, Arc<dyn OAuthAuth>>>,
    errors: std::sync::Mutex<Vec<String>>,
    pub(crate) account_mutation: tokio::sync::Mutex<()>,
}

enum AuthStorageBackend {
    File(FileAuthStorageBackend),
    InMemory(InMemoryAuthStorageBackend),
}

impl AuthStorage {
    pub fn create(auth_path: &str) -> Self {
        let normalized = normalize_path(auth_path, &crate::paths::PathInputOptions::default());
        let mut storage = Self {
            storage: AuthStorageBackend::File(FileAuthStorageBackend::new(&normalized)),
            auth_path: Some(normalized),
            data: std::sync::Mutex::new(AuthStorageData::new()),
            runtime_overrides: std::sync::Mutex::new(HashMap::new()),
            extension_oauth_providers: std::sync::Mutex::new(HashMap::new()),
            errors: std::sync::Mutex::new(Vec::new()),
            account_mutation: tokio::sync::Mutex::new(()),
        };
        storage.reload();
        storage
    }

    pub fn prepare_facts_auth_file(auth_path: &str) -> Result<(), String> {
        let backend = FileAuthStorageBackend::new(auth_path);
        backend.ensure_parent_dir()?;
        let _guard = acquire_lock_sync(&backend.auth_path, FILE_STORAGE_SYNC_LOCK_BUDGET_MS)
            .map_err(|error: CredentialStoreBusyError| error.message())?;
        backend.ensure_file_exists()
    }

    pub fn create_default() -> Self {
        Self::create(&Path::new(&get_agent_dir()).join("auth.json").to_string_lossy())
    }

    pub fn in_memory(data: AuthStorageData) -> Self {
        let backend = InMemoryAuthStorageBackend::default();
        let _ = backend.with_lock(|_| Ok(((), Some(serialize_auth_data(&data)))));
        let mut storage = Self {
            storage: AuthStorageBackend::InMemory(backend),
            auth_path: None,
            data: std::sync::Mutex::new(AuthStorageData::new()),
            runtime_overrides: std::sync::Mutex::new(HashMap::new()),
            extension_oauth_providers: std::sync::Mutex::new(HashMap::new()),
            errors: std::sync::Mutex::new(Vec::new()),
            account_mutation: tokio::sync::Mutex::new(()),
        };
        storage.reload();
        storage
    }

    pub fn storage_path(&self) -> Option<&str> {
        self.auth_path.as_deref()
    }

    pub fn errors(&self) -> Vec<String> {
        self.errors.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    /// Take every recorded load/refresh error, leaving none behind (senpi `drainErrors`).
    pub fn drain_errors(&self) -> Vec<String> {
        std::mem::take(&mut *self.errors.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
    }

    fn parse_storage_content(content: Option<&str>) -> Result<(AuthStorageData, bool, bool), String> {
        let Some(content) = content else { return Ok((AuthStorageData::new(), false, false)) };
        let parsed = parse_auth_json(content)?;
        let (repaired, repaired_flag) = repair_poisoned_pool_slots(&parsed);
        let (migrated, migrated_flag) = migrate_legacy_provider_keys(&repaired);
        Ok((migrated, repaired_flag, migrated_flag))
    }

    /// Reload credentials from storage; a written repair or migration invalidates the revision.
    pub fn reload(&mut self) {
        let result: Result<(AuthStorageData, bool, bool), String> = match &self.storage {
            AuthStorageBackend::File(backend) => backend.with_lock(|current| {
                let (data, repaired, migrated) = Self::parse_storage_content(current)?;
                if migrated {
                    backup_auth_file(self.auth_path.as_deref(), current);
                }
                if repaired || migrated {
                    Ok(((data.clone(), repaired, migrated), Some(serialize_auth_data(&data))))
                } else {
                    Ok(((data, repaired, migrated), None))
                }
            }),
            AuthStorageBackend::InMemory(backend) => backend.with_lock(|current| {
                let (data, repaired, migrated) = Self::parse_storage_content(current)?;
                Ok(((data, repaired, migrated), None))
            }),
        };
        match result {
            Ok((data, _, _)) => {
                *self.data.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = data;
            }
            Err(error) => self.errors.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(error),
        }
    }

    pub fn set_runtime_api_key(&self, provider: &str, api_key: &str) {
        self.runtime_overrides
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(provider.to_owned(), api_key.to_owned());
    }

    pub fn remove_runtime_api_key(&self, provider: &str) {
        self.runtime_overrides
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(provider);
    }

    /// Whether a non-persistent runtime API key is set for the provider. senpi's
    /// `getProviderAuthStatus` reports `source: "runtime"` for exactly this case.
    pub fn has_runtime_api_key(&self, provider: &str) -> bool {
        self.runtime_overrides
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(provider)
    }

    /// The non-persistent runtime API key for a provider, if one is set.
    pub fn runtime_api_key(&self, provider: &str) -> Option<String> {
        self.runtime_overrides
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(provider)
            .cloned()
    }

    pub fn get(&self, provider: &str) -> Option<Value> {
        read_by_provider_id(&self.data.lock().unwrap_or_else(std::sync::PoisonError::into_inner), provider)
    }

    pub fn get_provider_env(&self, provider: &str) -> Option<HashMap<String, String>> {
        credential_env(&self.get(provider)?)
    }

    /// The API key for a provider: runtime override, then stored credential, then the environment.
    pub async fn get_api_key(&self, provider: &str) -> Option<String> {
        if let Some(runtime) = self.runtime_overrides
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(provider)
            .cloned()
        {
            return Some(runtime);
        }
        if let Some(credential) = self.get(provider)
            && let Some(key) = credential_key(&credential) {
                if !is_command_config_value(key) {
                    return Some(key.to_owned());
                }
                let env = credential_env(&credential);
                if let Some(resolved) = resolve_config_value(key, env.as_ref()).await {
                    return Some(resolved);
                }
            }
        maho_ai::env_api_keys::get_env_api_key(provider, None)
    }

    /// senpi `list`: stored credential metadata, with every runtime-override provider reported as
    /// an api-key entry (replacing any stored entry for that provider in place).
    pub fn list(&self) -> Vec<(String, CredentialKind)> {
        let mut entries: Vec<(String, CredentialKind)> = self.data
            .lock().unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter_map(|(provider_id, credential)| credential_kind(credential).map(|kind| (provider_id.clone(), kind)))
            .collect();
        let mut runtime: Vec<String> = self.runtime_overrides
            .lock().unwrap_or_else(std::sync::PoisonError::into_inner)
            .keys()
            .cloned()
            .collect();
        runtime.sort();
        for provider_id in runtime {
            match entries.iter_mut().find(|(id, _)| *id == provider_id) {
                Some(entry) => entry.1 = CredentialKind::ApiKey,
                None => entries.push((provider_id, CredentialKind::ApiKey)),
            }
        }
        entries
    }

    /// Stores a credential for a provider; a None value deletes the entry.
    pub fn set(&self, provider_id: &str, credential: Option<Value>) -> Result<(), String> {
        let mut cached = self.data.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let provider_id = provider_id.to_owned();
        let credential = credential.clone();
        let outcome: Result<AuthStorageData, String> = match &self.storage {
            AuthStorageBackend::File(backend) => backend.with_lock(|current| {
                let (mut data, _, _) = Self::parse_storage_content(current)?;
                match &credential {
                    Some(value) => {
                        data.insert(provider_id.clone(), value.clone());
                    }
                    None => {
                        data.remove(&provider_id);
                    }
                }
                Ok((data.clone(), Some(serialize_auth_data(&data))))
            }),
            AuthStorageBackend::InMemory(backend) => backend.with_lock(|current| {
                let (mut data, _, _) = Self::parse_storage_content(current)?;
                match &credential {
                    Some(value) => {
                        data.insert(provider_id.clone(), value.clone());
                    }
                    None => {
                        data.remove(&provider_id);
                    }
                }
                Ok((data.clone(), Some(serialize_auth_data(&data))))
            }),
        };
        match outcome {
            Ok(data) => {
                *cached = data;
                Ok(())
            }
            Err(error) => {
                self.errors.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(error.clone());
                Err(error)
            }
        }
    }

    /// Atomically appends a completed OAuth login to the provider's credential pool.
    /// The receipt is returned only after the serialized auth.json write succeeds.
    pub async fn append_oauth_account(
        &self,
        provider_id: &str,
        credential: OAuthCredential,
        signal: Option<AbortSignal>,
    ) -> Result<Option<AccountLoginReceipt>, String> {
        self.append_login_account(provider_id, Credential::OAuth(credential), signal).await
    }

    /// senpi `Models.login`'s `credentials.modify(appendLoginSlot)`: appends a completed api-key
    /// or OAuth login to the provider's credential pool (`login-N` beside existing slots). The
    /// receipt is returned only after the serialized auth.json write succeeds.
    pub async fn append_login_account(
        &self,
        provider_id: &str,
        flat: Credential,
        signal: Option<AbortSignal>,
    ) -> Result<Option<AccountLoginReceipt>, String> {
        let _mutation = self.account_mutation.lock().await;
        if let Some(signal) = &signal {
            signal.throw_if_aborted().map_err(|reason| reason.message)?;
        }
        let mut receipt = None;
        let apply = |current: Option<&str>| -> Result<(AuthStorageData, Option<String>), String> {
            if let Some(signal) = &signal {
                signal.throw_if_aborted().map_err(|reason| reason.message)?;
            }
            let (mut data, _, _) = Self::parse_storage_content(current)?;
            let current = data.get(provider_id).cloned()
                .map(|value| serde_json::from_value::<maho_ai::auth::pool::slots::PooledCredential>(value).map_err(|error| error.to_string()))
                .transpose()?;
            let pooled = maho_ai::auth::pool::slots::PooledCredential::from(flat.clone());
            let next = maho_ai::auth::pool::slots::append_login_slot(current, pooled, |name, origin| {
                receipt = Some(AccountLoginReceipt {
                    provider_id: provider_id.to_owned(),
                    name: name.to_owned(),
                    origin: match origin {
                        maho_ai::auth::pool::slots::LoginSlotOrigin::Generated => AccountLoginOrigin::Generated,
                        maho_ai::auth::pool::slots::LoginSlotOrigin::Provider => AccountLoginOrigin::Provider,
                    },
                });
            })?;
            // An api-key `ApiKeyCredential` has no room for `accounts`/`pinned`; persist the pooled
            // object itself, as senpi does.
            let stored = match next.credential_type {
                maho_ai::auth::pool::slots::PooledCredentialType::ApiKey => serde_json::to_value(&next),
                maho_ai::auth::pool::slots::PooledCredentialType::OAuth => serde_json::to_value(next.to_stored_credential()),
            }.map_err(|error| error.to_string())?;
            data.insert(provider_id.to_owned(), stored);
            Ok((data.clone(), Some(serialize_auth_data(&data))))
        };
        let mut cached = self.data.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let result: Result<AuthStorageData, String> = match &self.storage {
            AuthStorageBackend::File(backend) => backend.with_lock(apply),
            AuthStorageBackend::InMemory(backend) => backend.with_lock(apply),
        };
        match result {
            Ok(data) => {
                *cached = data;
                Ok(receipt)
            }
            Err(error) => {
                self.errors.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(error.clone());
                Err(error)
            }
        }
    }

    /// senpi `delete`: drops the runtime override as well as the stored credential.
    pub fn delete(&self, provider_id: &str) -> Result<(), String> {
        self.remove_runtime_api_key(provider_id);
        self.set(provider_id, None)
    }

    pub fn set_many(&mut self, credentials: &[(String, Value)]) -> Result<(), String> {
        for (provider_id, credential) in credentials {
            self.set(provider_id, Some(credential.clone()))?;
        }
        Ok(())
    }

    pub fn has(&self, provider_id: &str) -> bool {
        self.get(provider_id).is_some()
    }

    pub fn set_cached(&mut self, provider_id: &str, credential: Value) {
        self.data.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(provider_id.to_owned(), credential);
    }

    /// Records the file revision the in-memory snapshot was read from.
    pub fn revision(&self) -> Option<String> {
        self.auth_path.as_deref().and_then(get_file_content_revision)
    }

    pub fn register_oauth_provider(&self, provider_id: &str, oauth: Arc<dyn OAuthAuth>) {
        self.extension_oauth_providers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(provider_id.to_string(), oauth);
    }

    pub fn unregister_oauth_provider(&self, provider_id: &str) {
        self.extension_oauth_providers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(provider_id);
    }

    pub fn get_oauth_providers(&self) -> Vec<OAuthProviderInfo> {
        let dynamic = self.extension_oauth_providers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut results: Vec<OAuthProviderInfo> = dynamic
            .iter()
            .map(|(id, oauth)| OAuthProviderInfo {
                id: id.clone(),
                name: oauth.name().to_string(),
            })
            .collect();
        // senpi `getOAuthProviders`: builtins are every builtin provider that declares OAuth, named
        // by that OAuth method (so Radius is included and names match the login flow).
        for provider in maho_ai::providers::all::builtin_providers() {
            let Some(oauth) = provider.auth().and_then(|auth| auth.oauth.as_ref()) else { continue };
            if !dynamic.contains_key(provider.id()) {
                results.push(OAuthProviderInfo {
                    id: provider.id().to_owned(),
                    name: oauth.name().to_owned(),
                });
            }
        }
        results
    }

    pub async fn login(&self, provider_id: &str, callbacks: Arc<dyn OAuthLoginCallbacks>) -> Result<(), String> {
        let dynamic = self.extension_oauth_providers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(provider_id)
            .cloned();
        let oauth = match dynamic {
            Some(flow) => flow,
            None => maho_ai::auth::oauth::load::load_builtin_oauth(provider_id)
                .await
                .map_err(|err| err.to_string())?
                .ok_or_else(|| format!("Unknown OAuth provider: {provider_id}"))?,
        };
        let signal = callbacks.signal().unwrap_or_else(|| AbortController::new().signal());
        let interaction = ProviderAuthInteraction::new(
            signal.clone(),
            Arc::new(LegacyInteraction { callbacks, signal }),
        );
        let credential = oauth.login(&interaction).await.map_err(|err| err.to_string())?;
        let val = serde_json::to_value(Credential::OAuth(credential)).map_err(|err| err.to_string())?;
        self.set(provider_id, Some(val))
    }

    pub async fn oauth_login(&self, provider_id: &str, callbacks: Arc<dyn OAuthLoginCallbacks>) -> Result<(), String> {
        self.login(provider_id, callbacks).await
    }

    pub fn logout(&self, provider: &str) -> Result<(), String> {
        self.runtime_overrides
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(provider);
        self.delete(provider)
    }

    pub fn oauth_logout(&self, provider: &str) -> Result<(), String> {
        self.logout(provider)
    }
}
/// One-shot auth.json provider-key migration for the provider rename: a credential stored under a
/// legacy provider id is rewritten under its canonical id, moved VERBATIM. Completion is derived
/// from the data, not a marker, so an older binary re-introducing a legacy key still migrates.
pub fn migrate_legacy_provider_keys(data: &AuthStorageData) -> (AuthStorageData, bool) {
    let mut next: Option<AuthStorageData> = None;
    for (legacy_id, canonical_id) in maho_ai::legacy_provider_ids::LEGACY_PROVIDER_IDS {
        let Some(legacy) = data.get(*legacy_id) else { continue };
        if !legacy.is_object() {
            continue;
        }
        let target = next.get_or_insert_with(|| data.clone());
        target.remove(*legacy_id);
        if !target.contains_key(*canonical_id) {
            target.insert((*canonical_id).to_owned(), legacy.clone());
        }
    }
    match next {
        Some(next) => (next, true),
        None => (data.clone(), false),
    }
}

/// Timestamped 0o600 copy of the pre-migration bytes, written before the migrated document replaces
/// them. A same-millisecond collision gets a numeric suffix instead of overwriting the backup.
pub fn backup_auth_file(auth_path: Option<&str>, content: Option<&str>) -> Option<String> {
    let auth_path = auth_path?;
    let content = content?;
    let stamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true).replace([':', '.'], "-");
    let mut backup_path = format!("{auth_path}.backup-{stamp}");
    let mut attempt = 1;
    while Path::new(&backup_path).exists() {
        backup_path = format!("{auth_path}.backup-{stamp}-{attempt}");
        attempt += 1;
    }
    std::fs::write(&backup_path, content).ok()?;
    set_mode(&backup_path, AUTH_FILE_MODE);
    Some(backup_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use maho_ai::auth::types::OAuthCredential;

    fn api_key(key: &str) -> Value {
        json!({ "type": "api_key", "key": key })
    }

    #[test]
    fn parses_a_valid_store_and_rejects_malformed_credentials() {
        let data = parse_auth_json("{\"anthropic\":{\"type\":\"api_key\",\"key\":\"k\"}}").expect("data");
        assert_eq!(credential_kind(&data["anthropic"]), Some(CredentialKind::ApiKey));
        assert_eq!(credential_key(&data["anthropic"]), Some("k"));

        assert_eq!(parse_auth_json("[]").expect_err("error"), "Invalid auth.json: expected an object");
        assert_eq!(
            parse_auth_json("{\"x\":{\"type\":\"nope\"}}").expect_err("error"),
            "Invalid auth.json credential for provider \"x\""
        );
        assert!(parse_auth_json("{\"x\":{\"type\":\"api_key\",\"key\":5}}").is_err());
        assert!(parse_auth_json("{\"x\":{\"type\":\"api_key\",\"env\":{\"a\":1}}}").is_err());
        assert!(parse_auth_json("{\"x\":{\"type\":\"oauth\",\"access\":\"a\"}}").is_err());
        assert!(parse_auth_json("{\"x\":{\"type\":\"oauth\",\"access\":\"a\",\"refresh\":\"r\",\"expires\":1}}").is_ok());
        assert!(parse_auth_json("not json").expect_err("error").starts_with("Failed to read auth.json: "));
    }

    #[test]
    fn reads_a_credential_under_the_canonical_then_the_legacy_key() {
        let mut data = AuthStorageData::new();
        data.insert("claude-sdk-oauth".to_owned(), api_key("legacy"));
        assert_eq!(credential_key(&read_by_provider_id(&data, "anthropic-subscription").expect("credential")), Some("legacy"));
        assert!(read_by_provider_id(&data, "missing").is_none());
        data.insert("anthropic-subscription".to_owned(), api_key("canonical"));
        assert_eq!(credential_key(&read_by_provider_id(&data, "anthropic-subscription").expect("credential")), Some("canonical"));
    }

    #[test]
    fn migrates_legacy_provider_keys_verbatim_and_lets_the_canonical_entry_win() {
        let mut data = AuthStorageData::new();
        data.insert("openai-codex".to_owned(), json!({ "type": "api_key", "key": "k", "accounts": [ { "name": "login-1" } ], "pinned": "login-1" }));
        let (migrated, flag) = migrate_legacy_provider_keys(&data);
        assert!(flag);
        assert!(!migrated.contains_key("openai-codex"));
        assert_eq!(migrated["chatgpt-subscription"]["pinned"], "login-1");
        assert_eq!(migrated["chatgpt-subscription"]["accounts"][0]["name"], "login-1");

        let (again, flag) = migrate_legacy_provider_keys(&migrated);
        assert!(!flag);
        assert_eq!(again, migrated);

        let mut conflict = AuthStorageData::new();
        conflict.insert("openai-codex".to_owned(), api_key("legacy"));
        conflict.insert("chatgpt-subscription".to_owned(), api_key("canonical"));
        let (resolved, flag) = migrate_legacy_provider_keys(&conflict);
        assert!(flag);
        assert_eq!(credential_key(&resolved["chatgpt-subscription"]), Some("canonical"));
        assert!(!resolved.contains_key("openai-codex"));

        let mut non_object = AuthStorageData::new();
        non_object.insert("openai-codex".to_owned(), Value::from("inspect me"));
        let (untouched, flag) = migrate_legacy_provider_keys(&non_object);
        assert!(!flag);
        assert!(untouched.contains_key("openai-codex"));
    }

    #[test]
    fn heals_a_poisoned_pool_slot_that_duplicates_the_flat_sentinel() {
        let data = parse_auth_json(
            "{\"anthropic\":{\"type\":\"api_key\",\"key\":\"flat\",\"accounts\":[{\"name\":\"login-1\",\"key\":\"flat\"},{\"name\":\"login-2\",\"key\":\"real\"}]}}",
        )
        .expect("data");
        let (repaired, flag) = repair_poisoned_pool_slots(&data);
        assert!(flag);
        let accounts = repaired["anthropic"]["accounts"].as_array().expect("accounts");
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0]["name"], "login-2");
        let (_again, flag) = repair_poisoned_pool_slots(&repaired);
        assert!(!flag);
    }

    #[test]
    #[cfg(unix)]
    fn writes_the_store_with_mode_600_and_preserves_a_wider_existing_mode() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("auth.json").to_string_lossy().into_owned();
        write_auth_file(&path, "{}").expect("write");
        assert_eq!(std::fs::metadata(&path).expect("metadata").permissions().mode() & 0o777, 0o600);
        set_mode(&path, 0o640);
        write_auth_file(&path, "{\"a\":1}").expect("write");
        assert_eq!(std::fs::metadata(&path).expect("metadata").permissions().mode() & 0o777, 0o640);
        let leftovers: Vec<String> = std::fs::read_dir(tmp.path())
            .expect("read_dir")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn the_file_backend_creates_the_parent_directory_and_the_store() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("agent/auth.json").to_string_lossy().into_owned();
        let backend = FileAuthStorageBackend::new(&path);
        backend.with_lock(|current| {
            assert_eq!(current, Some("{}"));
            Ok(((), None))
        })
        .expect("lock");
        assert!(Path::new(&path).exists());
    }

    #[tokio::test]
    async fn the_async_backend_writes_the_next_document() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("auth.json").to_string_lossy().into_owned();
        let backend = FileAuthStorageBackend::new(&path);
        backend
            .with_lock_async(async |_current| Ok(((), Some("{\"a\":1}".to_owned()))))
            .await
            .expect("write");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "{\"a\":1}");
    }

    #[test]
    fn read_stored_credential_is_a_one_off_read_without_resolution() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("auth.json").to_string_lossy().into_owned();
        std::fs::write(&path, "{\"anthropic\":{\"type\":\"api_key\",\"key\":\"!cmd\"}}").expect("write");
        let credential = read_stored_credential("anthropic", &path).expect("credential");
        assert_eq!(credential_key(&credential), Some("!cmd"));
        assert!(read_stored_credential("missing", &path).is_none());
        assert!(read_stored_credential("anthropic", "/definitely/not/here.json").is_none());
    }

    #[test]
    fn a_read_only_store_reads_and_refuses_mutations() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("auth.json").to_string_lossy().into_owned();
        std::fs::write(&path, "{\"anthropic\":{\"type\":\"api_key\",\"key\":\"k\"}}").expect("write");
        let store = ReadOnlyAuthStorage::new(&path).expect("store");
        assert_eq!(store.list(), vec![("anthropic".to_owned(), CredentialKind::ApiKey)]);
        assert_eq!(store.modify().expect_err("error"), READ_ONLY_ERROR);
        assert_eq!(store.delete().expect_err("error"), READ_ONLY_ERROR);
        assert!(ReadOnlyAuthStorage::new("/definitely/not/here.json").is_err());
    }

    #[tokio::test]
    async fn the_auth_storage_sets_reads_and_deletes_credentials() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("auth.json").to_string_lossy().into_owned();
        let storage = AuthStorage::create(&path);
        assert!(storage.get("anthropic").is_none());
        storage.set("anthropic", Some(api_key("stored"))).expect("set");
        assert_eq!(storage.get_api_key("anthropic").await.as_deref(), Some("stored"));
        assert_eq!(storage.list(), vec![("anthropic".to_owned(), CredentialKind::ApiKey)]);
        assert!(storage.has("anthropic"));

        storage.set_runtime_api_key("anthropic", "runtime");
        assert_eq!(storage.get_api_key("anthropic").await.as_deref(), Some("runtime"));
        storage.remove_runtime_api_key("anthropic");
        assert_eq!(storage.get_api_key("anthropic").await.as_deref(), Some("stored"));

        storage.delete("anthropic").expect("delete");
        assert!(storage.get("anthropic").is_none());
        let persisted = std::fs::read_to_string(&path).expect("read");
        assert_eq!(persisted, "{}\n");
    }

    #[tokio::test]
    async fn append_oauth_account_preserves_existing_accounts_and_pin_on_disk() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("auth.json").to_string_lossy().into_owned();
        let storage = AuthStorage::create(&path);
        storage.set("chatgpt-subscription", Some(json!({
            "type": "oauth", "access": "first-a", "refresh": "first-r", "expires": 1.0,
            "accounts": [{"name":"default","access":"first-a","refresh":"first-r","expires":1.0,"source":"login"}],
            "pinned": "default"
        }))).expect("seed");

        let receipt = storage.append_oauth_account(
            "chatgpt-subscription",
            OAuthCredential::new("second-a", "second-r", 2.0),
            None,
        ).await.expect("append").expect("generated account receipt");

        assert_eq!(receipt.provider_id, "chatgpt-subscription");
        assert_eq!(receipt.name, "login-2");
        assert_eq!(receipt.origin, AccountLoginOrigin::Generated);
        let disk: Value = serde_json::from_str(&std::fs::read_to_string(&path).expect("read persisted auth")).expect("json");
        assert_eq!(disk["chatgpt-subscription"]["pinned"], "default");
        assert_eq!(disk["chatgpt-subscription"]["accounts"].as_array().expect("accounts").len(), 2);
        assert_eq!(disk["chatgpt-subscription"]["accounts"][1]["access"], "second-a");
        assert_eq!(storage.get("chatgpt-subscription").expect("cached pool")["accounts"].as_array().expect("accounts").len(), 2);
    }

    #[tokio::test]
    async fn append_oauth_account_rejects_cancellation_without_changing_disk_or_cache() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("auth.json").to_string_lossy().into_owned();
        let initial = "{\n  \"chatgpt-subscription\": {\n    \"type\": \"oauth\",\n    \"access\": \"first-a\",\n    \"refresh\": \"first-r\",\n    \"expires\": 1.0\n  }\n}\n";
        std::fs::write(&path, initial).expect("seed auth file");
        let storage = AuthStorage::create(&path);
        let controller = AbortController::new();
        controller.abort(None);

        let error = storage.append_oauth_account(
            "chatgpt-subscription",
            OAuthCredential::new("second-a", "second-r", 2.0),
            Some(controller.signal()),
        ).await.expect_err("cancelled append");

        assert!(!error.is_empty());
        assert_eq!(std::fs::read_to_string(&path).expect("read auth file"), initial);
        assert_eq!(storage.get("chatgpt-subscription").expect("cached credential")["access"], "first-a");
    }

    #[tokio::test]
    async fn a_command_config_key_is_resolved_at_read_time() {
        let storage = AuthStorage::in_memory(AuthStorageData::new());
        storage.set("anthropic", Some(api_key("!echo resolved-key"))).expect("set");
        assert_eq!(storage.get_api_key("anthropic").await.as_deref(), Some("resolved-key"));
        crate::resolve_config_value::clear_config_value_cache();
    }

    #[tokio::test]
    async fn an_in_memory_store_never_touches_disk_and_serves_the_environment_last() {
        let mut data = AuthStorageData::new();
        data.insert("anthropic".to_owned(), api_key("stored"));
        let storage = AuthStorage::in_memory(data);
        assert!(storage.storage_path().is_none());
        assert_eq!(storage.get_api_key("anthropic").await.as_deref(), Some("stored"));
        assert!(storage.get_api_key("not-a-provider").await.is_none());
    }

    #[test]
    fn a_migration_writes_a_timestamped_backup_before_the_rewrite() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("auth.json").to_string_lossy().into_owned();
        let original = "{\n  \"openai-codex\": {\n    \"type\": \"api_key\",\n    \"key\": \"k\"\n  }\n}";
        std::fs::write(&path, original).expect("write");
        let storage = AuthStorage::create(&path);
        assert!(storage.get("chatgpt-subscription").is_some());
        let rewritten = std::fs::read_to_string(&path).expect("read");
        assert!(!rewritten.contains("openai-codex"));
        assert!(rewritten.contains("chatgpt-subscription"));
        let backups: Vec<String> = std::fs::read_dir(tmp.path())
            .expect("read_dir")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".backup-"))
            .collect();
        assert_eq!(backups.len(), 1);
        let backup = std::fs::read_to_string(tmp.path().join(&backups[0])).expect("read");
        assert_eq!(backup, original);
        assert!(storage.errors().is_empty());
    }

    #[test]
    fn a_malformed_store_is_reported_and_the_snapshot_stays_empty() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("auth.json").to_string_lossy().into_owned();
        std::fs::write(&path, "{\"anthropic\":{\"type\":\"bogus\"}}").expect("write");
        let storage = AuthStorage::create(&path);
        assert!(storage.get("anthropic").is_none());
        assert_eq!(storage.errors().len(), 1);
        assert!(storage.errors()[0].contains("Invalid auth.json credential for provider"));
    }

    #[test]
    fn drain_errors_returns_recorded_errors_once() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("auth.json").to_string_lossy().into_owned();
        std::fs::write(&path, "{\"anthropic\":{\"type\":\"bogus\"}}").expect("write");
        let storage = AuthStorage::create(&path);
        assert_eq!(storage.drain_errors().len(), 1);
        assert!(storage.drain_errors().is_empty());
        assert!(storage.errors().is_empty());
    }

    struct FauxOAuth {
        name: String,
        credential: OAuthCredential,
    }

    #[async_trait::async_trait]
    impl OAuthAuth for FauxOAuth {
        fn name(&self) -> &str {
            &self.name
        }
        async fn login(&self, _interaction: &ProviderAuthInteraction) -> anyhow::Result<OAuthCredential> {
            Ok(self.credential.clone())
        }
        async fn refresh(&self, credential: &OAuthCredential, _signal: &AbortSignal) -> anyhow::Result<OAuthCredential> {
            Ok(credential.clone())
        }
        async fn to_auth(&self, credential: &OAuthCredential) -> anyhow::Result<maho_ai::auth::types::ModelAuth> {
            Ok(maho_ai::auth::types::ModelAuth { api_key: Some(credential.access.clone()), ..Default::default() })
        }
    }

    struct CancelledOAuth;

    #[async_trait::async_trait]
    impl OAuthAuth for CancelledOAuth {
        fn name(&self) -> &str {
            "Cancelled"
        }
        async fn login(&self, interaction: &ProviderAuthInteraction) -> anyhow::Result<OAuthCredential> {
            let res = interaction.prompt(AuthPrompt {
                kind: AuthPromptKind::Select {
                    message: "Choose".into(),
                    options: vec![],
                },
                signal: None,
            }).await?;
            Ok(OAuthCredential::new(res, "r", 1000.0))
        }
        async fn refresh(&self, credential: &OAuthCredential, _signal: &AbortSignal) -> anyhow::Result<OAuthCredential> {
            Ok(credential.clone())
        }
        async fn to_auth(&self, credential: &OAuthCredential) -> anyhow::Result<maho_ai::auth::types::ModelAuth> {
            Ok(maho_ai::auth::types::ModelAuth { api_key: Some(credential.access.clone()), ..Default::default() })
        }
    }

    struct TestCallbacks {
        cancelled: bool,
    }

    impl OAuthLoginCallbacks for TestCallbacks {
        fn on_auth(&self, _info: OAuthAuthInfo) {}
        fn on_device_code(&self, _info: OAuthDeviceCodeInfo) {}
        fn on_prompt(&self, _prompt: OAuthPrompt) -> maho_ai::types::BoxFuture<'_, String> {
            Box::pin(async move { String::new() })
        }
        fn on_select(&self, _prompt: OAuthSelectPrompt) -> maho_ai::types::BoxFuture<'_, Option<String>> {
            let cancelled = self.cancelled;
            Box::pin(async move {
                if cancelled {
                    None
                } else {
                    Some("selected".into())
                }
            })
        }
    }

    #[test]
    fn extension_registered_oauth_provider_appears_in_get_oauth_providers() {
        let storage = AuthStorage::in_memory(AuthStorageData::new());
        let oauth = Arc::new(FauxOAuth {
            name: "Anthropic Subscription (Claude Pro/Max)".into(),
            credential: OAuthCredential::new("a", "r", 123.0),
        });
        storage.register_oauth_provider("anthropic-subscription", oauth);
        let providers = storage.get_oauth_providers();
        assert!(providers.iter().any(|p| p.id == "anthropic-subscription" && p.name == "Anthropic Subscription (Claude Pro/Max)"));
    }

    #[tokio::test]
    async fn login_runs_the_extension_oauth_flow_and_stores_the_credential() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("auth.json").to_string_lossy().into_owned();
        let storage = AuthStorage::create(&path);
        let credential = OAuthCredential::new("a", "r", 123.0);
        let oauth = Arc::new(FauxOAuth {
            name: "Anthropic Subscription".into(),
            credential: credential.clone(),
        });
        storage.register_oauth_provider("anthropic-subscription", oauth);
        let callbacks = Arc::new(TestCallbacks { cancelled: false });
        storage.login("anthropic-subscription", callbacks).await.expect("login");

        let stored = storage.get("anthropic-subscription").expect("stored");
        assert_eq!(stored.get("type").and_then(Value::as_str), Some("oauth"));
        assert_eq!(stored.get("access").and_then(Value::as_str), Some("a"));
        assert_eq!(stored.get("refresh").and_then(Value::as_str), Some("r"));
        assert_eq!(stored.get("expires").and_then(Value::as_f64), Some(123.0));

        // Verify disk file
        let disk_content = std::fs::read_to_string(&path).expect("read");
        let disk_json: Value = serde_json::from_str(&disk_content).expect("json");
        assert_eq!(disk_json["anthropic-subscription"]["type"], "oauth");
        assert_eq!(disk_json["anthropic-subscription"]["access"], "a");
        assert_eq!(disk_json["anthropic-subscription"]["refresh"], "r");
        assert_eq!(disk_json["anthropic-subscription"]["expires"], 123.0);

        // Logout removes it from disk
        storage.logout("anthropic-subscription").expect("logout");
        assert!(storage.get("anthropic-subscription").is_none());
        let disk_after = std::fs::read_to_string(&path).expect("read");
        let disk_after_json: Value = serde_json::from_str(&disk_after).expect("json");
        assert!(!disk_after_json.as_object().unwrap().contains_key("anthropic-subscription"));
    }

    #[test]
    fn unregister_oauth_provider_hides_the_provider_again() {
        let storage = AuthStorage::in_memory(AuthStorageData::new());
        storage.register_oauth_provider(
            "anthropic-subscription",
            Arc::new(FauxOAuth { name: "x".into(), credential: OAuthCredential::new("", "", 0.0) }),
        );
        storage.unregister_oauth_provider("anthropic-subscription");
        let ids: Vec<String> = storage.get_oauth_providers().into_iter().map(|p| p.id).collect();
        assert!(!ids.contains(&"anthropic-subscription".to_string()));
    }

    #[test]
    fn builtin_oauth_providers_come_from_provider_auth_declarations() {
        let providers = AuthStorage::in_memory(AuthStorageData::new()).get_oauth_providers();
        assert!(providers.iter().any(|p| p.id == "anthropic" && p.name == "Anthropic (Claude Pro/Max)"));
        assert!(providers.iter().any(|p| p.id == "radius"));
    }

    #[test]
    fn builtin_providers_still_enumerate_after_dynamic_registration() {
        let storage = AuthStorage::in_memory(AuthStorageData::new());
        storage.register_oauth_provider(
            "anthropic-subscription",
            Arc::new(FauxOAuth { name: "x".into(), credential: OAuthCredential::new("", "", 0.0) }),
        );
        let ids: Vec<String> = storage.get_oauth_providers().into_iter().map(|p| p.id).collect();
        assert!(ids.contains(&"anthropic".to_string()));
        assert!(ids.contains(&"anthropic-subscription".to_string()));
    }

    #[tokio::test]
    async fn logout_removes_stored_credential_and_runtime_key() {
        let storage = AuthStorage::in_memory(AuthStorageData::new());
        storage.set("anthropic", Some(api_key("stored"))).unwrap();
        storage.set_runtime_api_key("anthropic", "runtime");
        assert!(storage.has("anthropic"));
        assert!(storage.has_runtime_api_key("anthropic"));

        storage.logout("anthropic").unwrap();
        assert!(!storage.has("anthropic"));
        assert!(!storage.has_runtime_api_key("anthropic"));
    }

    #[tokio::test]
    async fn delete_clears_the_runtime_key_and_list_reports_runtime_only_providers() {
        let storage = AuthStorage::in_memory(AuthStorageData::new());
        storage.set_runtime_api_key("openai", "runtime");
        assert_eq!(storage.list(), vec![("openai".to_owned(), CredentialKind::ApiKey)]);

        storage.delete("openai").unwrap();
        assert!(!storage.has_runtime_api_key("openai"));
        assert!(storage.list().is_empty());
        assert_eq!(storage.get_api_key("openai").await.filter(|key| key == "runtime"), None);
    }

    #[tokio::test]
    async fn cancelled_login_leaves_auth_json_unchanged() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("auth.json").to_string_lossy().into_owned();
        let initial_json = "{\n  \"existing\": {\n    \"type\": \"api_key\",\n    \"key\": \"k\"\n  }\n}\n";
        std::fs::write(&path, initial_json).unwrap();

        let storage = AuthStorage::create(&path);
        storage.register_oauth_provider("cancelled-prov", Arc::new(CancelledOAuth));
        let callbacks = Arc::new(TestCallbacks { cancelled: true });

        let err = storage.login("cancelled-prov", callbacks).await.unwrap_err();
        assert!(err.contains("Login cancelled"));

        let content_after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content_after, initial_json);
        assert!(storage.get("cancelled-prov").is_none());
        assert!(storage.get("existing").is_some());
    }

    #[tokio::test]
    async fn unknown_oauth_provider_returns_error() {
        let storage = AuthStorage::in_memory(AuthStorageData::new());
        let callbacks = Arc::new(TestCallbacks { cancelled: false });
        let err = storage.login("unknown-provider-xyz", callbacks).await.unwrap_err();
        assert_eq!(err, "Unknown OAuth provider: unknown-provider-xyz");
    }

    #[test]
    fn serialized_data_round_trips_with_key_order_preserved() {
        let mut data = AuthStorageData::new();
        data.insert("z-provider".to_owned(), api_key("z"));
        data.insert("a-provider".to_owned(), api_key("a"));
        let serialized = serialize_auth_data(&data);
        assert!(serialized.starts_with("{\n  \"z-provider\""));
        assert!(serialized.ends_with('\n'));
        assert_eq!(parse_auth_json(&serialized).expect("data"), data);
    }
}
