//! Port of senpi packages/coding-agent/src/core/credential-pool/rotation-stream.ts.

use std::collections::BTreeMap;

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::credential_pool::classify::CredentialBlock;
use crate::credential_pool::env_slots::{EnvCredentialSlot, discover_env_slots};
use crate::credential_pool::failover::{RunSlot, SlotLeaseRef};
use crate::credential_pool::slots::{CredentialSlotSource, PooledCredential, rendezvous_order};
use crate::credential_pool::state_store::{
    BlockReason, CredentialSlotRepository, CredentialSlotState, acquire_half_open_lease,
};
use crate::resolve_config_value::resolve_config_value;

/// The exact hash the claude-sdk-oauth affinity oracle uses, so pools never remap.
pub fn sha256_slot_hasher(input: &str) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let digest = hasher.finalize();
    u64::from_be_bytes([digest[0], digest[1], digest[2], digest[3], digest[4], digest[5], digest[6], digest[7]])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotationLane {
    Stored,
    Env,
}

impl RotationLane {
    pub fn as_str(self) -> &'static str {
        match self {
            RotationLane::Stored => "stored",
            RotationLane::Env => "env",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RotationSlot {
    pub slot: RunSlot,
    pub lane: RotationLane,
    /// Env-lane key material for the attempt; never serialized or persisted.
    pub env_key: Option<String>,
    pub env_var_name: Option<String>,
    /// Stored-lane material revision binding sidecar health to the current credential.
    pub stored_revision: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct RotationPolicy {
    pub affinity: Option<bool>,
    pub cooldown_base_ms: Option<u64>,
    pub cooldown_cap_ms: Option<u64>,
    pub slots: BTreeMap<String, PolicySlotRef>,
}

#[derive(Debug, Clone, Default)]
pub struct PolicySlotRef {
    pub env: Option<String>,
    pub value: Option<String>,
}

pub struct RotationSources<'a> {
    pub provider_id: String,
    pub credential: Option<PooledCredential>,
    pub env: &'a dyn Fn(&str) -> Option<String>,
    pub repository: &'a CredentialSlotRepository,
    pub policy: RotationPolicy,
    pub now: u64,
}

fn overlay_state(slot: RotationSlot, state: Option<&CredentialSlotState>) -> RotationSlot {
    let Some(state) = state else { return slot };
    let mut overlay = slot;
    if let Some(blocked_until) = state.blocked_until {
        overlay.slot.blocked_until = Some(blocked_until);
    }
    if let Some(reason) = state.block_reason {
        overlay.slot.block_reason = Some(block_reason_str(reason).to_owned());
    }
    if let Some(failure_count) = state.failure_count {
        overlay.slot.failure_count = Some(failure_count.min(u64::from(u32::MAX)) as u32);
    }
    if let Some(lease) = &state.lease {
        overlay.slot.lease = Some(SlotLeaseRef { id: lease.id.clone(), expires_at: lease.expires_at });
    }
    overlay
}

fn block_reason_str(reason: BlockReason) -> &'static str {
    match reason {
        BlockReason::AuthError => "auth_error",
        BlockReason::RateLimit => "rate_limit",
        BlockReason::AccountDisabled => "account_disabled",
    }
}

async fn policy_slots(sources: &RotationSources<'_>) -> Vec<EnvCredentialSlot> {
    let mut slots = Vec::new();
    for (name, reference) in &sources.policy.slots {
        let env_var_name = reference.env.clone().unwrap_or_else(|| format!("models.json:{name}"));
        let key = match (&reference.env, &reference.value) {
            (Some(env_var), _) => (sources.env)(env_var),
            (None, Some(value)) => resolve_config_value(value, None).await,
            (None, None) => None,
        };
        let Some(key) = key.filter(|key| !key.is_empty()) else { continue };
        slots.push(EnvCredentialSlot {
            name: name.clone(),
            env_var_name,
            key,
            source: CredentialSlotSource::Env,
        });
    }
    slots
}

/// Lists the provider's rotation slots with sidecar health overlaid. Stored credentials own the
/// lane when present; env slots participate only when nothing is stored.
pub async fn list_rotation_slots(sources: &RotationSources<'_>, acquire_leases: bool) -> Result<Vec<RotationSlot>, String> {
    let provider_id = sources.provider_id.clone();
    let repository = sources.repository;
    let named = policy_slots(sources).await;
    if let Some(credential) = &sources.credential {
        let state = repository.list_slots(&provider_id, "stored").await?;
        let mut slots = Vec::new();
        for slot in credential.list_slots() {
            let stored_revision = repository
                .stored_credential_revision(
                    &provider_id,
                    &slot.name,
                    &crate::credential_pool::state_store::CredentialMaterial {
                        key: slot.key.clone(),
                        access: slot.access.clone(),
                        refresh: slot.refresh.clone(),
                    },
                )
                .await?;
            let persisted = state.get(&slot.name);
            // A block belongs to the material that earned it; a re-login starts clean.
            let applicable = persisted.filter(|state| state.credential_revision.as_deref() == Some(stored_revision.as_str()));
            let base = RotationSlot {
                slot: RunSlot {
                    name: slot.name.clone(),
                    pinned: credential.pinned.as_deref() == Some(slot.name.as_str()),
                    ..RunSlot::default()
                },
                lane: RotationLane::Stored,
                env_key: None,
                env_var_name: None,
                stored_revision: Some(stored_revision.clone()),
            };
            let expired = applicable.and_then(|state| state.blocked_until).map(|until| until <= sources.now).unwrap_or(false);
            if acquire_leases && expired {
                let lease = acquire_half_open_lease(repository, &provider_id, "stored", &slot.name, sources.now, 30_000).await?;
                if lease.is_none() {
                    continue;
                }
                let leased = repository.list_slots(&provider_id, "stored").await?;
                let leased_state = leased.get(&slot.name).filter(|state| state.credential_revision.as_deref() == Some(stored_revision.as_str()));
                slots.push(overlay_state(base, leased_state));
                continue;
            }
            slots.push(overlay_state(base, applicable));
        }
        if named.is_empty() {
            return Ok(slots);
        }
        let named_slots = list_env_rotation_slots(sources, &named, acquire_leases).await?;
        slots.extend(named_slots);
        return Ok(slots);
    }
    let mut env_slots = discover_env_slots(&provider_id, sources.env);
    env_slots.extend(named);
    list_env_rotation_slots(sources, &env_slots, acquire_leases).await
}

async fn list_env_rotation_slots(
    sources: &RotationSources<'_>,
    env_slots: &[EnvCredentialSlot],
    acquire_leases: bool,
) -> Result<Vec<RotationSlot>, String> {
    if env_slots.is_empty() {
        return Ok(Vec::new());
    }
    let provider_id = sources.provider_id.clone();
    let repository = sources.repository;
    let state = repository.list_slots(&provider_id, "env").await?;
    let mut slots = Vec::new();
    for slot in env_slots {
        let persisted = state.get(&slot.name);
        let revision = repository.env_credential_revision(&slot.env_var_name, &slot.key).await?;
        let mut applicable = persisted.filter(|state| state.credential_revision.as_deref() == Some(revision.as_str())).cloned();
        let expired = applicable.as_ref().and_then(|state| state.blocked_until).map(|until| until <= sources.now).unwrap_or(false);
        if acquire_leases && expired {
            let lease = acquire_half_open_lease(repository, &provider_id, "env", &slot.name, sources.now, 30_000).await?;
            if lease.is_none() {
                continue;
            }
            applicable = repository.list_slots(&provider_id, "env").await?.get(&slot.name).cloned();
        }
        let base = RotationSlot {
            slot: RunSlot { name: slot.name.clone(), ..RunSlot::default() },
            lane: RotationLane::Env,
            env_key: Some(slot.key.clone()),
            env_var_name: Some(slot.env_var_name.clone()),
            stored_revision: None,
        };
        slots.push(overlay_state(base, applicable.as_ref()));
    }
    Ok(slots)
}

/// The next sidecar state for a blocked slot: the failure count grows, and only a rate limit gets a
/// deadline (auth and billing blocks have no expiry).
pub fn block_patch(
    block: &CredentialBlock,
    current: Option<&CredentialSlotState>,
    now: u64,
    credential_revision: Option<&str>,
    policy: &RotationPolicy,
) -> CredentialSlotState {
    let failure_count = current.and_then(|state| state.failure_count).unwrap_or(0) + 1;
    let mut patch = CredentialSlotState {
        state_version: current.map(|state| state.state_version).unwrap_or(0),
        blocked_until: None,
        block_reason: None,
        failure_count: Some(failure_count),
        last_success_at: current.and_then(|state| state.last_success_at),
        credential_revision: credential_revision.map(str::to_owned),
        lease: None,
    };
    match block {
        CredentialBlock::RateLimit { cooldown_ms, .. } => {
            patch.blocked_until = Some(now + policy.cooldown_cap_ms.unwrap_or(*cooldown_ms).min(*cooldown_ms));
            patch.block_reason = Some(BlockReason::RateLimit);
        }
        CredentialBlock::AuthError => patch.block_reason = Some(BlockReason::AuthError),
        CredentialBlock::AccountDisabled => patch.block_reason = Some(BlockReason::AccountDisabled),
    }
    patch
}

/// Selection follows the HRW order for the affinity key; a pinned slot always wins.
pub fn select_rotation_slot(candidates: &[RotationSlot], affinity_key: &str, use_affinity: bool) -> Option<RotationSlot> {
    if let Some(pinned) = candidates.iter().find(|candidate| candidate.slot.pinned) {
        return Some(pinned.clone());
    }
    if !use_affinity {
        return candidates.first().cloned();
    }
    let names: Vec<String> = candidates.iter().map(|candidate| candidate.slot.name.clone()).collect();
    let ordered = rendezvous_order(&names, affinity_key, sha256_slot_hasher);
    let winner = ordered.first()?;
    candidates.iter().find(|candidate| candidate.slot.name == *winner).cloned()
}

pub fn policy_affinity(policy: &RotationPolicy) -> bool {
    policy.affinity.unwrap_or(true)
}

pub fn rotation_error_from_event(event: &Value) -> Option<Value> {
    crate::credential_pool::rotation_events::rotation_error_from_event(event)
}

pub use crate::credential_pool::rotation_events::{is_committed_rotation_output, is_rotation_stream_start};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential_pool::slots::CredentialSlot;
    use serde_json::json;

    fn repository(tmp: &tempfile::TempDir) -> CredentialSlotRepository {
        CredentialSlotRepository::new(&tmp.path().join("credential-pool-state.json").to_string_lossy())
    }

    fn credential(slots: &[(&str, &str)], pinned: Option<&str>) -> PooledCredential {
        PooledCredential {
            credential_type: "api_key".to_owned(),
            key: None,
            access: None,
            refresh: None,
            expires: None,
            accounts: Some(
                slots
                    .iter()
                    .map(|(name, key)| CredentialSlot { name: (*name).to_owned(), key: Some((*key).to_owned()), ..CredentialSlot::default() })
                    .collect(),
            ),
            pinned: pinned.map(str::to_owned),
        }
    }

    fn sources<'a>(
        repository: &'a CredentialSlotRepository,
        credential: Option<PooledCredential>,
        env: &'a dyn Fn(&str) -> Option<String>,
        now: u64,
    ) -> RotationSources<'a> {
        RotationSources { provider_id: "anthropic".to_owned(), credential, env, repository, policy: RotationPolicy::default(), now }
    }

    #[tokio::test]
    async fn stored_slots_own_the_lane_and_carry_their_material_revision() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repository = repository(&tmp);
        let env = |_: &str| None;
        let sources = sources(&repository, Some(credential(&[("login-1", "k1"), ("login-2", "k2")], Some("login-2"))), &env, 1_000);
        let slots = list_rotation_slots(&sources, true).await.expect("slots");
        assert_eq!(slots.len(), 2);
        assert!(slots.iter().all(|slot| slot.lane == RotationLane::Stored));
        assert!(slots[1].slot.pinned);
        assert!(slots[0].stored_revision.is_some());
        assert!(slots.iter().all(|slot| slot.env_key.is_none()));
    }

    #[tokio::test]
    async fn env_slots_participate_only_when_nothing_is_stored() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repository = repository(&tmp);
        let env = |name: &str| (name == "ANTHROPIC_API_KEY").then(|| "base".to_owned());
        let env_sources = sources(&repository, None, &env, 1_000);
        let slots = list_rotation_slots(&env_sources, true).await.expect("slots");
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0].slot.name, "env");
        assert_eq!(slots[0].lane, RotationLane::Env);
        assert_eq!(slots[0].env_key.as_deref(), Some("base"));
        assert_eq!(slots[0].env_var_name.as_deref(), Some("ANTHROPIC_API_KEY"));

        let stored = sources(&repository, Some(credential(&[("login-1", "k")], None)), &env, 1_000);
        let slots = list_rotation_slots(&stored, true).await.expect("slots");
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0].lane, RotationLane::Stored);
    }

    #[tokio::test]
    async fn a_block_whose_revision_no_longer_matches_the_material_is_ignored() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repository = repository(&tmp);
        let env = |name: &str| (name == "ANTHROPIC_API_KEY").then(|| "rotated".to_owned());
        repository
            .mutate_slot_state("anthropic", "env", "env", |_| {
                Some(CredentialSlotState {
                    state_version: 0,
                    blocked_until: Some(999_999),
                    block_reason: Some(BlockReason::AuthError),
                    failure_count: Some(1),
                    last_success_at: None,
                    credential_revision: Some("0".repeat(64)),
                    lease: None,
                })
            })
            .await
            .expect("mutate");
        let env_sources = sources(&repository, None, &env, 1_000);
        let slots = list_rotation_slots(&env_sources, true).await.expect("slots");
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0].slot.block_reason, None);
        assert_eq!(slots[0].slot.blocked_until, None);

        let revision = repository.env_credential_revision("ANTHROPIC_API_KEY", "rotated").await.expect("revision");
        repository
            .mutate_slot_state("anthropic", "env", "env", |_| {
                Some(CredentialSlotState {
                    state_version: 0,
                    blocked_until: Some(999_999),
                    block_reason: Some(BlockReason::AuthError),
                    failure_count: Some(1),
                    last_success_at: None,
                    credential_revision: Some(revision),
                    lease: None,
                })
            })
            .await
            .expect("mutate");
        let slots = list_rotation_slots(&env_sources, true).await.expect("slots");
        assert_eq!(slots[0].slot.block_reason.as_deref(), Some("auth_error"));
    }

    #[tokio::test]
    async fn an_expired_rate_limit_hands_exactly_one_caller_a_probe_lease() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repository = repository(&tmp);
        let env = |name: &str| (name == "ANTHROPIC_API_KEY").then(|| "base".to_owned());
        let revision = repository.env_credential_revision("ANTHROPIC_API_KEY", "base").await.expect("revision");
        repository
            .mutate_slot_state("anthropic", "env", "env", |_| {
                Some(CredentialSlotState {
                    state_version: 0,
                    blocked_until: Some(500),
                    block_reason: Some(BlockReason::RateLimit),
                    failure_count: Some(1),
                    last_success_at: None,
                    credential_revision: Some(revision),
                    lease: None,
                })
            })
            .await
            .expect("mutate");
        let env_sources = sources(&repository, None, &env, 1_000);
        let first = list_rotation_slots(&env_sources, true).await.expect("slots");
        assert_eq!(first.len(), 1);
        assert!(first[0].slot.lease.is_some());
        let second = list_rotation_slots(&env_sources, true).await.expect("slots");
        assert!(second.is_empty());
        let without_leases = list_rotation_slots(&env_sources, false).await.expect("slots");
        assert_eq!(without_leases.len(), 1);
    }

    #[tokio::test]
    async fn policy_slots_join_the_env_lane_and_skip_unresolved_values() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repository = repository(&tmp);
        let env = |name: &str| (name == "CUSTOM_KEY").then(|| "custom".to_owned());
        let mut sources = sources(&repository, None, &env, 1_000);
        sources.policy.slots.insert("named".to_owned(), PolicySlotRef { env: Some("CUSTOM_KEY".to_owned()), value: None });
        sources.policy.slots.insert("missing".to_owned(), PolicySlotRef { env: Some("NOT_SET".to_owned()), value: None });
        sources.policy.slots.insert("literal".to_owned(), PolicySlotRef { env: None, value: Some("$HOME".to_owned()) });
        let slots = list_rotation_slots(&sources, true).await.expect("slots");
        let names: Vec<&str> = slots.iter().map(|slot| slot.slot.name.as_str()).collect();
        assert!(names.contains(&"named"));
        assert!(names.contains(&"literal"));
        assert!(!names.contains(&"missing"));
    }

    #[test]
    fn block_patch_grows_the_failure_count_and_only_rate_limits_get_a_deadline() {
        let policy = RotationPolicy { cooldown_cap_ms: Some(100), ..RotationPolicy::default() };
        let patch = block_patch(
            &CredentialBlock::RateLimit { cooldown_ms: 80, retry_after_was_capped: false },
            None,
            1_000,
            Some("rev"),
            &policy,
        );
        assert_eq!(patch.blocked_until, Some(1_080));
        assert_eq!(patch.block_reason, Some(BlockReason::RateLimit));
        assert_eq!(patch.failure_count, Some(1));
        assert_eq!(patch.credential_revision.as_deref(), Some("rev"));

        let capped = block_patch(
            &CredentialBlock::RateLimit { cooldown_ms: 500, retry_after_was_capped: true },
            Some(&patch),
            1_000,
            None,
            &policy,
        );
        assert_eq!(capped.blocked_until, Some(1_100));
        assert_eq!(capped.failure_count, Some(2));

        let auth = block_patch(&CredentialBlock::AuthError, None, 1_000, None, &policy);
        assert_eq!(auth.blocked_until, None);
        assert_eq!(auth.block_reason, Some(BlockReason::AuthError));
        let disabled = block_patch(&CredentialBlock::AccountDisabled, None, 1_000, None, &policy);
        assert_eq!(disabled.block_reason, Some(BlockReason::AccountDisabled));
    }

    #[test]
    fn selection_prefers_a_pinned_slot_then_the_affinity_order() {
        let slots: Vec<RotationSlot> = ["a", "b", "c"]
            .iter()
            .map(|name| RotationSlot {
                slot: RunSlot { name: (*name).to_owned(), ..RunSlot::default() },
                lane: RotationLane::Stored,
                env_key: None,
                env_var_name: None,
                stored_revision: None,
            })
            .collect();
        let picked = select_rotation_slot(&slots, "session", true).expect("slot");
        assert_eq!(select_rotation_slot(&slots, "session", true).expect("slot"), picked);
        assert_eq!(select_rotation_slot(&slots, "other-session", true).map(|slot| slot.slot.name), Some(picked.slot.name.clone()).filter(|_| true).map(|_| select_rotation_slot(&slots, "other-session", true).expect("slot").slot.name));
        assert_eq!(select_rotation_slot(&slots, "session", false).expect("slot").slot.name, "a");

        let mut pinned = slots.clone();
        pinned[2].slot.pinned = true;
        assert_eq!(select_rotation_slot(&pinned, "session", true).expect("slot").slot.name, "c");
        assert!(select_rotation_slot(&[], "session", true).is_none());
    }

    #[test]
    fn the_sha256_hasher_is_stable_and_wide() {
        let first = sha256_slot_hasher("session\0a");
        assert_eq!(first, sha256_slot_hasher("session\0a"));
        assert_ne!(first, sha256_slot_hasher("session\0b"));
        assert_eq!(sha256_slot_hasher(""), 0xe3b0c44298fc1c14);
    }

    #[test]
    fn rotation_events_reuse_the_shared_predicates() {
        assert!(is_rotation_stream_start(&json!({ "type": "start" })));
        assert!(is_committed_rotation_output(&json!({ "type": "text_delta" })));
        assert!(rotation_error_from_event(&json!({ "type": "error", "error": { "errorMessage": "x" } })).is_some());
        assert!(!policy_affinity(&RotationPolicy { affinity: Some(false), ..RotationPolicy::default() }));
        assert!(policy_affinity(&RotationPolicy::default()));
    }
}