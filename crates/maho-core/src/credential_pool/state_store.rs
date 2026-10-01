//! Port of senpi packages/coding-agent/src/core/credential-pool/state-store.ts.
//!
//! The sidecar holds only credential health, never credential material: env slots are keyed by an
//! installation-local HMAC revision, so a rotated env value clears its own stale block.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::config::get_agent_dir;
use crate::lockfile_policy::{FILE_STORAGE_LOCK_RETRY_BUDGET_MS, CredentialStoreBusyError, acquire_lock_async};

pub const CREDENTIAL_POOL_STATE_FILENAME: &str = "credential-pool-state.json";
pub const DEFAULT_HALF_OPEN_LEASE_TTL_MS: u64 = 30_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlotLease {
    pub id: String,
    pub expires_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockReason {
    AuthError,
    RateLimit,
    AccountDisabled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialSlotState {
    pub state_version: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_until: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_reason: Option<BlockReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_success_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease: Option<SlotLease>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CredentialPoolLane {
    pub slots: BTreeMap<String, CredentialSlotState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CredentialPoolProvider {
    pub lanes: BTreeMap<String, CredentialPoolLane>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialPoolStateDocument {
    pub schema_version: u32,
    pub installation_key: String,
    pub providers: BTreeMap<String, CredentialPoolProvider>,
}

impl CredentialPoolStateDocument {
    pub fn fresh() -> Self {
        Self { schema_version: 1, installation_key: random_hex(32), providers: BTreeMap::new() }
    }
}

fn random_hex(bytes: usize) -> String {
    let mut out = String::new();
    while out.len() < bytes * 2 {
        out.push_str(&uuid::Uuid::new_v4().to_string().replace('-', ""));
    }
    out.truncate(bytes * 2);
    out
}

pub fn credential_pool_state_path(agent_dir: &str) -> String {
    Path::new(agent_dir).join(CREDENTIAL_POOL_STATE_FILENAME).to_string_lossy().into_owned()
}

fn is_hex_256(bit: &str) -> bool {
    bit.len() == 64 && bit.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_valid(document: &CredentialPoolStateDocument) -> bool {
    if document.schema_version != 1 || !is_hex_256(&document.installation_key) {
        return false;
    }
    document.providers.values().all(|provider| {
        provider.lanes.values().all(|lane| {
            lane.slots.values().all(|slot| {
                let revision_ok = slot.credential_revision.as_deref().map(is_hex_256).unwrap_or(true);
                let lease_ok = slot.lease.as_ref().map(|lease| !lease.id.is_empty() && lease.id.len() <= 128).unwrap_or(true);
                let blocked_ok = slot.blocked_until.map(|until| until > 0).unwrap_or(true);
                revision_ok && lease_ok && blocked_ok
            })
        })
    })
}

/// An unreadable or invalid document resets to a fresh one instead of failing auth resolution.
pub fn parse_document(content: &str) -> CredentialPoolStateDocument {
    match serde_json::from_str::<CredentialPoolStateDocument>(content) {
        Ok(document) if is_valid(&document) => document,
        _ => CredentialPoolStateDocument::fresh(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotHealth {
    Ready,
    Blocked,
    HalfOpen,
}

/// Deadlines are absolute, so a restart never re-enables a cooling credential early.
pub fn slot_health(state: Option<&CredentialSlotState>, now: u64) -> SlotHealth {
    let Some(state) = state else { return SlotHealth::Ready };
    if matches!(state.block_reason, Some(BlockReason::AuthError) | Some(BlockReason::AccountDisabled)) {
        return SlotHealth::Blocked;
    }
    if let Some(blocked_until) = state.blocked_until {
        if blocked_until > now {
            return SlotHealth::Blocked;
        }
        if let Some(lease) = &state.lease
            && lease.expires_at > now {
                return SlotHealth::HalfOpen;
            }
    }
    SlotHealth::Ready
}

pub fn hmac_sha256_hex(key_hex: &str, message: &str) -> String {
    let key = hex::decode(key_hex).unwrap_or_default();
    let mut normalized = [0u8; 64];
    if key.len() > 64 {
        let mut hasher = Sha256::new();
        hasher.update(&key);
        let digest = hasher.finalize();
        normalized[..digest.len()].copy_from_slice(&digest);
    } else {
        normalized[..key.len()].copy_from_slice(&key);
    }
    let mut inner_pad = [0x36u8; 64];
    let mut outer_pad = [0x5cu8; 64];
    for index in 0..64 {
        inner_pad[index] ^= normalized[index];
        outer_pad[index] ^= normalized[index];
    }
    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(message.as_bytes());
    let inner_digest = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_digest);
    hex::encode(outer.finalize())
}

pub struct CredentialSlotRepository {
    path: String,
}

impl CredentialSlotRepository {
    pub fn new(path: &str) -> Self {
        Self { path: path.to_owned() }
    }

    pub fn default_path() -> Self {
        Self::new(&credential_pool_state_path(&get_agent_dir()))
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    fn ensure_file(&self) -> Result<(), String> {
        if Path::new(&self.path).exists() {
            return Ok(());
        }
        if let Some(parent) = Path::new(&self.path).parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        write_document(&self.path, &CredentialPoolStateDocument::fresh())
    }

    async fn with_document<T>(
        &self,
        mutate: impl FnOnce(CredentialPoolStateDocument) -> (T, Option<CredentialPoolStateDocument>),
    ) -> Result<T, String> {
        self.ensure_file()?;
        let _guard = acquire_lock_async(&self.path, FILE_STORAGE_LOCK_RETRY_BUDGET_MS)
            .await
            .map_err(|error: CredentialStoreBusyError| error.message())?;
        let content = std::fs::read_to_string(&self.path).unwrap_or_default();
        let document = parse_document(&content);
        let (result, next) = mutate(document);
        if let Some(next) = next {
            write_document(&self.path, &next)?;
        }
        Ok(result)
    }

    pub async fn installation_key(&self) -> Result<String, String> {
        self.with_document(|document| {
            let key = document.installation_key.clone();
            (key, Some(document))
        })
        .await
    }

    /// HMAC over the installation key: the revision is useless outside this installation.
    pub async fn env_credential_revision(&self, env_var_name: &str, env_value: &str) -> Result<String, String> {
        let key = self.installation_key().await?;
        Ok(hmac_sha256_hex(&key, &format!("{env_var_name}\u{0}{env_value}")))
    }

    pub async fn stored_credential_revision(
        &self,
        provider_id: &str,
        slot_name: &str,
        material: &CredentialMaterial,
    ) -> Result<String, String> {
        let key = self.installation_key().await?;
        Ok(hmac_sha256_hex(
            &key,
            &format!(
                "{provider_id}\u{0}{slot_name}\u{0}{}\u{0}{}\u{0}{}",
                material.key.as_deref().unwrap_or_default(),
                material.access.as_deref().unwrap_or_default(),
                material.refresh.as_deref().unwrap_or_default()
            ),
        ))
    }

    pub async fn list_slots(&self, provider_id: &str, lane_id: &str) -> Result<BTreeMap<String, CredentialSlotState>, String> {
        self.with_document(|document| {
            let slots = document
                .providers
                .get(provider_id)
                .and_then(|provider| provider.lanes.get(lane_id))
                .map(|lane| lane.slots.clone())
                .unwrap_or_default();
            (slots, None)
        })
        .await
    }

    /// Atomic read-modify-write for one slot's health; a None result removes the slot state entirely.
    pub async fn mutate_slot_state(
        &self,
        provider_id: &str,
        lane_id: &str,
        slot_id: &str,
        mutate: impl FnOnce(Option<&CredentialSlotState>) -> Option<CredentialSlotState>,
    ) -> Result<Option<CredentialSlotState>, String> {
        self.with_document(|mut document| {
            let provider = document.providers.entry(provider_id.to_owned()).or_default();
            let lane = provider.lanes.entry(lane_id.to_owned()).or_default();
            let current = lane.slots.get(slot_id).cloned();
            let mutated = mutate(current.as_ref());
            let result = match mutated {
                None => {
                    lane.slots.remove(slot_id);
                    None
                }
                Some(mutated) => {
                    let mut next = mutated;
                    next.state_version = current.as_ref().map(|state| state.state_version).unwrap_or(0) + 1;
                    lane.slots.insert(slot_id.to_owned(), next.clone());
                    Some(next)
                }
            };
            (result, Some(document))
        })
        .await
    }
}

fn write_document(path: &str, document: &CredentialPoolStateDocument) -> Result<(), String> {
    let serialized = serialize_document(document);
    std::fs::write(path, serialized).map_err(|error| error.to_string())?;
    set_file_mode_600(path);
    Ok(())
}

fn set_file_mode_600(path: &str) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CredentialMaterial {
    pub key: Option<String>,
    pub access: Option<String>,
    pub refresh: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HalfOpenLease {
    pub lease_id: String,
    pub expires_at: u64,
}

/// Transitions an expired cooldown atomically to half_open: exactly one caller wins the probe lease.
pub async fn acquire_half_open_lease(
    repository: &CredentialSlotRepository,
    provider_id: &str,
    lane_id: &str,
    slot_id: &str,
    now: u64,
    lease_ttl_ms: u64,
) -> Result<Option<HalfOpenLease>, String> {
    let mut lease: Option<HalfOpenLease> = None;
    repository
        .mutate_slot_state(provider_id, lane_id, slot_id, |current| {
            let current = current?;
            let expired = current.blocked_until.map(|until| until <= now).unwrap_or(false);
            let lease_live = current.lease.as_ref().map(|lease| lease.expires_at > now).unwrap_or(false);
            if !expired || lease_live || current.block_reason == Some(BlockReason::AuthError) {
                return Some(current.clone());
            }
            let next_lease = HalfOpenLease {
                lease_id: uuid::Uuid::new_v4().to_string(),
                expires_at: now + lease_ttl_ms,
            };
            let mut updated = current.clone();
            updated.lease = Some(SlotLease { id: next_lease.lease_id.clone(), expires_at: next_lease.expires_at });
            lease = Some(next_lease);
            Some(updated)
        })
        .await?;
    Ok(lease)
}

/// Serializes a document the way senpi writes it (two-space indent, trailing newline).
pub fn serialize_document(document: &CredentialPoolStateDocument) -> String {
    let mut serialized = serde_json::to_string_pretty(document).unwrap_or_else(|_| "{}".to_owned());
    serialized.push('\n');
    serialized
}

pub fn document_to_value(document: &CredentialPoolStateDocument) -> Value {
    serde_json::to_value(document).unwrap_or(Value::Null)
}
#[cfg(test)]
mod tests {
    use super::*;

    fn slot_state(blocked_until: Option<u64>, reason: Option<BlockReason>) -> CredentialSlotState {
        CredentialSlotState {
            state_version: 0,
            blocked_until,
            block_reason: reason,
            failure_count: Some(1),
            last_success_at: None,
            credential_revision: None,
            lease: None,
        }
    }

    #[tokio::test]
    async fn a_fresh_repository_writes_a_document_with_an_installation_key() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join(CREDENTIAL_POOL_STATE_FILENAME).to_string_lossy().into_owned();
        let repository = CredentialSlotRepository::new(&path);
        let key = repository.installation_key().await.expect("key");
        assert!(is_hex_256(&key));
        assert!(Path::new(&path).exists());
        assert_eq!(repository.installation_key().await.expect("key"), key);
    }

    #[test]
    fn an_unreadable_or_invalid_document_resets_to_a_fresh_one() {
        assert_eq!(parse_document("not json").schema_version, 1);
        assert_eq!(parse_document("{\"schemaVersion\":2}").schema_version, 1);
        let document = parse_document("{\"schemaVersion\":1,\"installationKey\":\"short\",\"providers\":{}}");
        assert!(is_hex_256(&document.installation_key));
        assert_ne!(document.installation_key, "short");
    }

    #[test]
    fn slot_health_follows_blocks_and_leases() {
        assert_eq!(slot_health(None, 100), SlotHealth::Ready);
        let blocked = slot_state(Some(200), Some(BlockReason::RateLimit));
        assert_eq!(slot_health(Some(&blocked), 100), SlotHealth::Blocked);
        assert_eq!(slot_health(Some(&blocked), 300), SlotHealth::Ready);
        let mut half_open = blocked.clone();
        half_open.lease = Some(SlotLease { id: "l".to_owned(), expires_at: 250 });
        assert_eq!(slot_health(Some(&half_open), 240), SlotHealth::HalfOpen);
        assert_eq!(slot_health(Some(&half_open), 300), SlotHealth::Ready);
        let permanent = CredentialSlotState { block_reason: Some(BlockReason::AuthError), blocked_until: Some(1), ..blocked.clone() };
        assert_eq!(slot_health(Some(&permanent), 10_000), SlotHealth::Blocked);
        let disabled = CredentialSlotState { block_reason: Some(BlockReason::AccountDisabled), ..blocked };
        assert_eq!(slot_health(Some(&disabled), 10_000), SlotHealth::Blocked);
    }

    #[test]
    fn hmac_matches_the_rfc_4231_test_vector() {
        assert_eq!(
            hmac_sha256_hex("0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b", "Hi There"),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    #[tokio::test]
    async fn env_and_stored_revisions_are_installation_local_and_material_bound() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join(CREDENTIAL_POOL_STATE_FILENAME).to_string_lossy().into_owned();
        let repository = CredentialSlotRepository::new(&path);
        let first = repository.env_credential_revision("ANTHROPIC_API_KEY", "secret").await.expect("revision");
        assert!(is_hex_256(&first));
        assert_eq!(repository.env_credential_revision("ANTHROPIC_API_KEY", "secret").await.expect("revision"), first);
        assert_ne!(repository.env_credential_revision("ANTHROPIC_API_KEY", "other").await.expect("revision"), first);
        assert_ne!(repository.env_credential_revision("OTHER_KEY", "secret").await.expect("revision"), first);

        let material = CredentialMaterial { key: Some("k".to_owned()), ..CredentialMaterial::default() };
        let stored = repository.stored_credential_revision("anthropic", "login-1", &material).await.expect("revision");
        assert!(is_hex_256(&stored));
        let rotated = CredentialMaterial { key: Some("k2".to_owned()), ..CredentialMaterial::default() };
        assert_ne!(repository.stored_credential_revision("anthropic", "login-1", &rotated).await.expect("revision"), stored);
    }

    #[tokio::test]
    async fn slot_state_is_read_modify_written_with_incrementing_versions() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join(CREDENTIAL_POOL_STATE_FILENAME).to_string_lossy().into_owned();
        let repository = CredentialSlotRepository::new(&path);
        assert!(repository.list_slots("anthropic", "primary").await.expect("list").is_empty());

        let written = repository
            .mutate_slot_state("anthropic", "primary", "login-1", |current| {
                assert!(current.is_none());
                Some(slot_state(Some(500), Some(BlockReason::RateLimit)))
            })
            .await
            .expect("mutate")
            .expect("state");
        assert_eq!(written.state_version, 1);
        assert_eq!(written.blocked_until, Some(500));

        let updated = repository
            .mutate_slot_state("anthropic", "primary", "login-1", |current| {
                let mut next = current.expect("state").clone();
                next.blocked_until = Some(900);
                Some(next)
            })
            .await
            .expect("mutate")
            .expect("state");
        assert_eq!(updated.state_version, 2);
        assert_eq!(updated.blocked_until, Some(900));

        let removed = repository.mutate_slot_state("anthropic", "primary", "login-1", |_| None).await.expect("mutate");
        assert!(removed.is_none());
        assert!(repository.list_slots("anthropic", "primary").await.expect("list").is_empty());
    }

    #[tokio::test]
    async fn exactly_one_caller_wins_the_half_open_lease() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join(CREDENTIAL_POOL_STATE_FILENAME).to_string_lossy().into_owned();
        let repository = CredentialSlotRepository::new(&path);
        repository
            .mutate_slot_state("anthropic", "primary", "login-1", |_| Some(slot_state(Some(100), Some(BlockReason::RateLimit))))
            .await
            .expect("mutate");

        let first = acquire_half_open_lease(&repository, "anthropic", "primary", "login-1", 200, DEFAULT_HALF_OPEN_LEASE_TTL_MS)
            .await
            .expect("lease");
        assert!(first.is_some());
        let second = acquire_half_open_lease(&repository, "anthropic", "primary", "login-1", 200, DEFAULT_HALF_OPEN_LEASE_TTL_MS)
            .await
            .expect("lease");
        assert!(second.is_none());
        let after_lease = acquire_half_open_lease(&repository, "anthropic", "primary", "login-1", 200_000, DEFAULT_HALF_OPEN_LEASE_TTL_MS)
            .await
            .expect("lease");
        assert!(after_lease.is_some());
    }

    #[tokio::test]
    async fn an_auth_error_block_never_goes_half_open() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join(CREDENTIAL_POOL_STATE_FILENAME).to_string_lossy().into_owned();
        let repository = CredentialSlotRepository::new(&path);
        repository
            .mutate_slot_state("anthropic", "primary", "login-1", |_| Some(slot_state(Some(100), Some(BlockReason::AuthError))))
            .await
            .expect("mutate");
        assert!(acquire_half_open_lease(&repository, "anthropic", "primary", "login-1", 200, 30_000).await.expect("lease").is_none());
    }

    #[test]
    fn the_document_serializes_with_camel_case_keys() {
        let mut document = CredentialPoolStateDocument::fresh();
        let mut blocked = slot_state(Some(5), Some(BlockReason::RateLimit));
        blocked.lease = Some(SlotLease { id: "l".to_owned(), expires_at: 9 });
        document.providers.insert(
            "anthropic".to_owned(),
            CredentialPoolProvider {
                lanes: BTreeMap::from([(
                    "primary".to_owned(),
                    CredentialPoolLane { slots: BTreeMap::from([("login-1".to_owned(), blocked)]) },
                )]),
            },
        );
        let value = document_to_value(&document);
        assert_eq!(value["schemaVersion"], 1);
        assert_eq!(value["providers"]["anthropic"]["lanes"]["primary"]["slots"]["login-1"]["blockedUntil"], 5);
        assert_eq!(value["providers"]["anthropic"]["lanes"]["primary"]["slots"]["login-1"]["blockReason"], "rate_limit");
        assert_eq!(value["providers"]["anthropic"]["lanes"]["primary"]["slots"]["login-1"]["lease"]["expiresAt"], 9);
        assert!(serialize_document(&document).ends_with('\n'));
        assert_eq!(parse_document(&serialize_document(&document)), document);
    }
}
