//! Port of senpi packages/ai/src/auth/pool/kinds.ts.

use super::slots::{CredentialSlot, PooledCredential, PooledCredentialType, list_slots};

/// `Credential["type"]`.
pub type CredentialKind = PooledCredentialType;

/// A slot's kind from its own material: OAuth tokens make an OAuth login, a key alone makes an API
/// key. A slot with neither is not classified, so a caller never drops or reports it by guess.
pub fn slot_kind(slot: &CredentialSlot) -> Option<CredentialKind> {
    if slot.access.is_some() || slot.refresh.is_some() {
        return Some(CredentialKind::OAuth);
    }
    slot.key.as_ref().map(|_| CredentialKind::ApiKey)
}

/// Kinds held by a credential's slots that its top-level type cannot use: slots are projected
/// through that type, so a slot of the other kind is never used (senpi#3448).
pub fn unused_slot_kinds(credential: Option<&PooledCredential>) -> Vec<CredentialKind> {
    let Some(credential) = credential else {
        return Vec::new();
    };
    if credential.accounts.is_none() {
        return Vec::new();
    }
    let mut kinds = Vec::new();
    for slot in list_slots(Some(credential)) {
        if let Some(kind) = slot_kind(&slot)
            && kind != credential.credential_type
            && !kinds.contains(&kind)
        {
            kinds.push(kind);
        }
    }
    kinds
}

fn other_kind(kind: CredentialKind) -> CredentialKind {
    match kind {
        CredentialKind::OAuth => CredentialKind::ApiKey,
        CredentialKind::ApiKey => CredentialKind::OAuth,
    }
}

/// The slots a credential can actually use: a slot of the other kind never projects (senpi#3486).
pub fn slots_of_own_kind(credential: &PooledCredential) -> Vec<CredentialSlot> {
    list_slots(Some(credential))
        .into_iter()
        .filter(|slot| slot_kind(slot) != Some(other_kind(credential.credential_type)))
        .collect()
}
