//! Port of senpi packages/coding-agent/src/core/auth-storage.ts (the storage layer).
//!
//! Credentials are kept as serde_json values so an auth.json read and rewritten keeps every field
//! and key order; typed accessors expose the api_key/oauth shapes senpi validates on load.
//!
//! deviation: senpi's AuthStorage also drives OAuth login through pi-ai's AuthInteraction/OAuthAuth
//! surface, which maho-ai has not ported yet (todos 13/17). Those methods are present but return
//! an explicit unsupported error instead of silently succeeding; the file/lock/env layer below is
//! the complete storage behavior todo 16 owns.

use std::collections::HashMap;
use std::path::Path;

use serde_json::{Map, Value};

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
    if Path::new(path).exists() {
        if let Ok(metadata) = std::fs::metadata(path) {
            use std::os::unix::fs::PermissionsExt;
            let mode = metadata.permissions().mode() & 0o777;
            set_mode(&temporary, mode);
        }
    }
    if let Err(error) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error.to_string());
    }
    Ok(())
}

fn set_mode(path: &str, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
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
        if let Some(parent) = Path::new(&self.auth_path).parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                set_mode(&parent.to_string_lossy(), 0o700);
            }
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
        let _guard = acquire_lock_sync(&self.auth_path, FILE_STORAGE_SYNC_LOCK_BUDGET_MS)
            .map_err(|error: CredentialStoreBusyError| error.message())?;
        let current = std::fs::read_to_string(&self.auth_path).ok();
        let (result, next) = action(current.as_deref())?;
        if let Some(next) = next {
            write_auth_file(&self.auth_path, &next)?;
        }
        Ok(result)
    }

    pub async fn with_lock_async<T>(
        &self,
        action: impl AsyncFnOnce(Option<&str>) -> Result<(T, Option<String>), String>,
    ) -> Result<T, String> {
        self.ensure_parent_dir()?;
        self.ensure_file_exists()?;
        let _guard = acquire_lock_async(&self.auth_path, FILE_STORAGE_LOCK_RETRY_BUDGET_MS)
            .await
            .map_err(|error: CredentialStoreBusyError| error.message())?;
        let current = std::fs::read_to_string(&self.auth_path).ok();
        let (result, next) = action(current.as_deref()).await?;
        if let Some(next) = next {
            write_auth_file(&self.auth_path, &next)?;
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
pub const OAUTH_UNSUPPORTED_ERROR: &str = "OAuth login is not available in this build (maho-ai auth surface pending)";

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
    data: AuthStorageData,
    runtime_overrides: HashMap<String, String>,
    errors: Vec<String>,
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
            data: AuthStorageData::new(),
            runtime_overrides: HashMap::new(),
            errors: Vec::new(),
        };
        storage.reload();
        storage
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
            data: AuthStorageData::new(),
            runtime_overrides: HashMap::new(),
            errors: Vec::new(),
        };
        storage.reload();
        storage
    }

    pub fn storage_path(&self) -> Option<&str> {
        self.auth_path.as_deref()
    }

    pub fn errors(&self) -> &[String] {
        &self.errors
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
                self.data = data;
            }
            Err(error) => self.errors.push(error),
        }
    }

    pub fn set_runtime_api_key(&mut self, provider: &str, api_key: &str) {
        self.runtime_overrides.insert(provider.to_owned(), api_key.to_owned());
    }

    pub fn remove_runtime_api_key(&mut self, provider: &str) {
        self.runtime_overrides.remove(provider);
    }

    pub fn get(&self, provider: &str) -> Option<Value> {
        read_by_provider_id(&self.data, provider)
    }

    pub fn get_provider_env(&self, provider: &str) -> Option<HashMap<String, String>> {
        credential_env(&self.get(provider)?)
    }

    /// The API key for a provider: runtime override, then stored credential, then the environment.
    pub async fn get_api_key(&self, provider: &str) -> Option<String> {
        if let Some(runtime) = self.runtime_overrides.get(provider) {
            return Some(runtime.clone());
        }
        if let Some(credential) = self.get(provider) {
            if let Some(key) = credential_key(&credential) {
                if !is_command_config_value(key) {
                    return Some(key.to_owned());
                }
                let env = credential_env(&credential);
                if let Some(resolved) = resolve_config_value(key, env.as_ref()).await {
                    return Some(resolved);
                }
            }
        }
        maho_ai::env_api_keys::get_env_api_key(provider, None)
    }

    pub fn list(&self) -> Vec<(String, CredentialKind)> {
        self.data
            .iter()
            .filter_map(|(provider_id, credential)| credential_kind(credential).map(|kind| (provider_id.clone(), kind)))
            .collect()
    }

    /// Stores a credential for a provider; a None value deletes the entry.
    pub fn set(&mut self, provider_id: &str, credential: Option<Value>) -> Result<(), String> {
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
                self.data = data;
                Ok(())
            }
            Err(error) => {
                self.errors.push(error.clone());
                Err(error)
            }
        }
    }

    pub fn delete(&mut self, provider_id: &str) -> Result<(), String> {
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
        self.data.insert(provider_id.to_owned(), credential);
    }

    /// Records the file revision the in-memory snapshot was read from.
    pub fn revision(&self) -> Option<String> {
        self.auth_path.as_deref().and_then(get_file_content_revision)
    }

    pub fn oauth_login(&self) -> Result<(), String> {
        Err(OAUTH_UNSUPPORTED_ERROR.to_owned())
    }

    pub fn oauth_logout(&self) -> Result<(), String> {
        Err(OAUTH_UNSUPPORTED_ERROR.to_owned())
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
        assert_eq!(credential_key(&read_by_provider_id(&data, "anthropic-subscription").expect("credential")).as_deref(), Some("legacy"));
        assert!(read_by_provider_id(&data, "missing").is_none());
        data.insert("anthropic-subscription".to_owned(), api_key("canonical"));
        assert_eq!(credential_key(&read_by_provider_id(&data, "anthropic-subscription").expect("credential")).as_deref(), Some("canonical"));
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
        assert_eq!(credential_key(&resolved["chatgpt-subscription"]).as_deref(), Some("canonical"));
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
        let mut storage = AuthStorage::create(&path);
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
    async fn a_command_config_key_is_resolved_at_read_time() {
        let mut storage = AuthStorage::in_memory(AuthStorageData::new());
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
    fn oauth_login_reports_the_pending_auth_surface_instead_of_succeeding_silently() {
        let storage = AuthStorage::in_memory(AuthStorageData::new());
        assert_eq!(storage.oauth_login().expect_err("error"), OAUTH_UNSUPPORTED_ERROR);
        assert_eq!(storage.oauth_logout().expect_err("error"), OAUTH_UNSUPPORTED_ERROR);
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
