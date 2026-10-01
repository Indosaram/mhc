//! Local mirror of the parts of senpi packages/ai/src/auth/pool/slots.ts this crate consumes
//! (deviation: maho-ai's auth pool is a stub owned by todos 13/17; these shapes are re-pointed there).

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const DEFAULT_SLOT_NAME: &str = "default";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CredentialSlotSource {
    Login,
    Import,
    Env,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CredentialSlot {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<CredentialSlotSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PooledCredential {
    #[serde(rename = "type")]
    pub credential_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accounts: Option<Vec<CredentialSlot>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned: Option<String>,
}

impl PooledCredential {
    pub fn accounts(&self) -> &[CredentialSlot] {
        self.accounts.as_deref().unwrap_or(&[])
    }

    /// The flat fields remain a valid credential, so they are the fallback slot.
    pub fn list_slots(&self) -> Vec<CredentialSlot> {
        let mut slots: Vec<CredentialSlot> = self.accounts().iter().filter(|slot| !slot.name.is_empty()).cloned().collect();
        if slots.is_empty() && (self.key.is_some() || self.access.is_some()) {
            slots.push(CredentialSlot {
                name: DEFAULT_SLOT_NAME.to_owned(),
                key: self.key.clone(),
                access: self.access.clone(),
                refresh: self.refresh.clone(),
                expires: self.expires,
                ..CredentialSlot::default()
            });
        }
        slots
    }
}

/// rendezvousOrder (packages/ai/src/auth/pool/select.ts): a stable hash-based ordering.
pub fn rendezvous_order(slot_ids: &[String], key: &str, hash: impl Fn(&str) -> u64) -> Vec<String> {
    let mut scored: Vec<(u64, String)> = slot_ids.iter().map(|id| (hash(&format!("{key}\u{0}{id}")), id.clone())).collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, id)| id).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_slots_prefers_accounts_and_falls_back_to_the_flat_fields() {
        let pooled = PooledCredential {
            credential_type: "api_key".to_owned(),
            key: Some("flat".to_owned()),
            access: None,
            refresh: None,
            expires: None,
            accounts: Some(vec![CredentialSlot { name: "login-1".to_owned(), key: Some("a".to_owned()), ..CredentialSlot::default() }]),
            pinned: None,
        };
        assert_eq!(pooled.list_slots().len(), 1);
        assert_eq!(pooled.list_slots()[0].name, "login-1");

        let flat = PooledCredential {
            credential_type: "api_key".to_owned(),
            key: Some("flat".to_owned()),
            access: None,
            refresh: None,
            expires: None,
            accounts: None,
            pinned: None,
        };
        assert_eq!(flat.list_slots()[0].name, DEFAULT_SLOT_NAME);
        assert_eq!(flat.list_slots()[0].key.as_deref(), Some("flat"));
        let empty = PooledCredential { credential_type: "oauth".to_owned(), key: None, access: None, refresh: None, expires: None, accounts: None, pinned: None };
        assert!(empty.list_slots().is_empty());
    }

    #[test]
    fn rendezvous_order_is_stable_and_key_dependent() {
        let ids = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];
        let hash = |text: &str| -> u64 {
            let mut value = 1469598103934665603u64;
            for byte in text.as_bytes() {
                value ^= u64::from(*byte);
                value = value.wrapping_mul(1099511628211);
            }
            value
        };
        let first = rendezvous_order(&ids, "k1", hash);
        assert_eq!(first, rendezvous_order(&ids, "k1", hash));
        let mut sorted = first.clone();
        sorted.sort();
        assert_eq!(sorted, ids);
        assert_ne!(first, rendezvous_order(&ids, "k2", hash));
    }
}
