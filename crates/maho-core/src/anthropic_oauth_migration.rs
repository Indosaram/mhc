//! Port of senpi packages/coding-agent/src/core/anthropic-oauth-migration.ts.
//!
//! Moves a Claude subscription login out of the direct `anthropic` provider into the Agent SDK lane
//! (senpi#3259). Two steps, one per auth.json load, so no credential is ever lost: (1) copy every
//! unrecorded OAuth grant into `anthropic-subscription` and record its digest on the source in the same
//! write; (2) a later load that finds every grant recorded removes the OAuth part of `anthropic`,
//! keeping API-key slots. Anything it cannot fully parse is left untouched and explained in `skipped`.

use std::collections::HashSet;

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::auth_storage::AuthStorageData;
use crate::credential_accounts::{SENTINEL_OAUTH_FIELDS_ACCESS, SENTINEL_OAUTH_FIELDS_EXPIRES};

const ANTHROPIC_SUBSCRIPTION_PROVIDER_ID: &str = "anthropic-subscription";

/// Field on the source `anthropic` entry: digests of its grants already copied into the subscription lane.
pub const IMPORTED_GRANTS_FIELD: &str = "importedToSubscription";

#[derive(Debug, Clone, PartialEq)]
pub struct AnthropicOAuthMigrationResult {
    pub data: AuthStorageData,
    pub migrated: bool,
    /// Why an `anthropic` OAuth entry was left exactly as stored; set only when nothing was written.
    pub skipped: Option<String>,
}

type Slot = Map<String, Value>;

/// The anthropic-subscription extension's `authGrantDigest`.
fn auth_grant_digest(refresh: &str) -> String {
    let digest = hex::encode(Sha256::digest(refresh.as_bytes()));
    digest[..32].to_owned()
}

fn is_oauth_grant(slot: &Slot) -> bool {
    slot.get("access").is_some_and(Value::is_string)
        && slot.get("refresh").is_some_and(Value::is_string)
        && slot.get("expires").is_some_and(Value::is_number)
}

fn is_key_slot(slot: &Slot) -> bool {
    slot.get("key").is_some_and(Value::is_string)
        && !slot.contains_key("access")
        && !slot.contains_key("refresh")
}

fn slot_list(value: &Value) -> Option<Vec<Slot>> {
    value
        .as_array()?
        .iter()
        .map(|slot| slot.as_object().cloned())
        .collect()
}

fn refresh(slot: &Slot) -> &str {
    slot.get("refresh")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

struct Source {
    grants: Vec<Slot>,
    key_slots: Vec<Slot>,
    pooled: bool,
    pinned: Option<Value>,
}

fn read_source(entry: &Slot) -> Result<Source, &'static str> {
    let Some(accounts) = entry.get("accounts") else {
        return if is_oauth_grant(entry) {
            Ok(Source {
                grants: vec![entry.clone()],
                key_slots: Vec::new(),
                pooled: false,
                pinned: None,
            })
        } else {
            Err("it holds no complete OAuth grant")
        };
    };
    let Some(accounts) = slot_list(accounts) else {
        return Err("its accounts list is not a list of account objects");
    };
    let slot_grants: Vec<Slot> = accounts
        .iter()
        .filter(|slot| is_oauth_grant(slot))
        .cloned()
        .collect();
    let key_slots: Vec<Slot> = accounts
        .iter()
        .filter(|slot| is_key_slot(slot))
        .cloned()
        .collect();
    if slot_grants.len() + key_slots.len() != accounts.len() {
        return Err("an account in it is neither a complete OAuth grant nor an API key");
    }
    let flat_is_extra = is_oauth_grant(entry)
        && !slot_grants
            .iter()
            .any(|slot| refresh(slot) == refresh(entry));
    let mut grants = slot_grants;
    if flat_is_extra {
        let mut flat = entry.clone();
        flat.remove("name");
        grants.push(flat);
    }
    if grants.is_empty() {
        return Err("it holds no complete OAuth grant");
    }
    Ok(Source {
        grants,
        key_slots,
        pooled: true,
        pinned: entry.get("pinned").cloned(),
    })
}

fn empty_subscription() -> Slot {
    let mut credential = Slot::new();
    credential.insert("type".to_owned(), json!("oauth"));
    credential.insert("access".to_owned(), json!(SENTINEL_OAUTH_FIELDS_ACCESS));
    credential.insert("refresh".to_owned(), json!(SENTINEL_OAUTH_FIELDS_ACCESS));
    credential.insert("expires".to_owned(), json!(SENTINEL_OAUTH_FIELDS_EXPIRES));
    credential.insert("accounts".to_owned(), json!([]));
    credential
}

fn read_subscription(stored: Option<&Value>) -> Result<Slot, &'static str> {
    let Some(stored) = stored else {
        return Ok(empty_subscription());
    };
    let Some(stored) = stored.as_object() else {
        return Err("the existing anthropic-subscription entry is not an object");
    };
    if let Some(accounts) = stored.get("accounts") {
        let valid = slot_list(accounts).is_some_and(|slots| {
            slots
                .iter()
                .all(|slot| slot.get("name").is_some_and(Value::is_string))
        });
        if !valid {
            return Err("the existing anthropic-subscription accounts are not account objects");
        }
    }
    Ok(stored.clone())
}

/// `String.prototype.slice(0, units)`: JS strings count UTF-16 code units.
fn truncate(value: &str, units: usize) -> String {
    String::from_utf16_lossy(&value.encode_utf16().take(units).collect::<Vec<u16>>())
}

fn imported_slot_name(slot: &Slot, pooled: bool, taken: &HashSet<String>) -> String {
    let base = match slot.get("name").and_then(Value::as_str) {
        Some(name) if pooled && !name.is_empty() => format!("imported-anthropic-{name}"),
        _ => "imported-anthropic".to_owned(),
    };
    let mut name = truncate(&base, 64);
    let mut suffix = 2;
    while taken.contains(&name) {
        name = format!("{}-{suffix}", truncate(&base, 58));
        suffix += 1;
    }
    name
}

fn remaining_api_key_entry(key_slots: &[Slot], pinned: Option<&Value>) -> Option<Value> {
    let pinned_slot = pinned.and_then(Value::as_str).and_then(|pinned| {
        key_slots
            .iter()
            .find(|slot| slot.get("name").and_then(Value::as_str) == Some(pinned))
    });
    let key = pinned_slot.or(key_slots.first())?.get("key")?.clone();
    let mut entry = Slot::new();
    entry.insert("type".to_owned(), json!("api_key"));
    entry.insert("key".to_owned(), key);
    entry.insert(
        "accounts".to_owned(),
        Value::Array(key_slots.iter().cloned().map(Value::Object).collect()),
    );
    if let Some(name) = pinned_slot.and_then(|slot| slot.get("name")) {
        entry.insert("pinned".to_owned(), name.clone());
    }
    Some(Value::Object(entry))
}

fn imported_digests(entry: &Slot) -> Vec<String> {
    entry
        .get(IMPORTED_GRANTS_FIELD)
        .and_then(Value::as_array)
        .map(|recorded| {
            recorded
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn copy_grants(
    data: &AuthStorageData,
    entry: &Slot,
    source: &Source,
    mut subscription: Slot,
    unrecorded: &[&Slot],
) -> AuthStorageData {
    let mut accounts: Vec<Slot> = subscription
        .get("accounts")
        .and_then(slot_list)
        .unwrap_or_default();
    let mut taken: HashSet<String> = accounts
        .iter()
        .filter_map(|slot| slot.get("name").and_then(Value::as_str).map(str::to_owned))
        .collect();
    let mut pinned = subscription.get("pinned").cloned();
    for grant in unrecorded {
        let digest = auth_grant_digest(refresh(grant));
        let existing = accounts.iter().position(|slot| {
            refresh(slot) == refresh(grant)
                || slot.get("importedFrom").and_then(Value::as_str) == Some(digest.as_str())
        });
        let account_name = match existing {
            Some(index) => accounts[index].get("name").cloned().unwrap_or(Value::Null),
            None => {
                let name = imported_slot_name(grant, source.pooled, &taken);
                taken.insert(name.clone());
                let mut account = Slot::new();
                account.insert("name".to_owned(), json!(name));
                if let Some(display_name) =
                    grant.get("displayName").filter(|value| value.is_string())
                {
                    account.insert("displayName".to_owned(), display_name.clone());
                }
                for field in ["access", "refresh", "expires"] {
                    account.insert(
                        field.to_owned(),
                        grant.get(field).cloned().unwrap_or(Value::Null),
                    );
                }
                account.insert("source".to_owned(), json!("import"));
                account.insert("importedFrom".to_owned(), json!(digest));
                accounts.push(account);
                json!(name)
            }
        };
        if pinned.is_none()
            && let Some(name) = grant.get("name").and_then(Value::as_str)
            && source.pinned.as_ref().and_then(Value::as_str) == Some(name)
        {
            pinned = Some(account_name);
        }
    }
    let mut recorded = imported_digests(entry);
    for grant in &source.grants {
        let digest = auth_grant_digest(refresh(grant));
        if !recorded.contains(&digest) {
            recorded.push(digest);
        }
    }
    let mut next = data.clone();
    let mut anthropic = entry.clone();
    anthropic.insert(IMPORTED_GRANTS_FIELD.to_owned(), json!(recorded));
    next.insert("anthropic".to_owned(), Value::Object(anthropic));
    subscription.insert(
        "accounts".to_owned(),
        Value::Array(accounts.into_iter().map(Value::Object).collect()),
    );
    if let Some(pinned) = pinned {
        subscription.insert("pinned".to_owned(), pinned);
    }
    next.insert(
        ANTHROPIC_SUBSCRIPTION_PROVIDER_ID.to_owned(),
        Value::Object(subscription),
    );
    next
}

fn unchanged(data: AuthStorageData, skipped: Option<&str>) -> AnthropicOAuthMigrationResult {
    AnthropicOAuthMigrationResult {
        data,
        migrated: false,
        skipped: skipped.map(str::to_owned),
    }
}

/// Never fails: a broken entry can never take other providers' credentials down with it.
pub fn migrate_anthropic_oauth_to_subscription(
    data: AuthStorageData,
) -> AnthropicOAuthMigrationResult {
    let Some(entry) = data.get("anthropic").and_then(Value::as_object).cloned() else {
        return unchanged(data, None);
    };
    if entry.get("type").and_then(Value::as_str) != Some("oauth") {
        return unchanged(data, None);
    }
    let source = match read_source(&entry) {
        Ok(source) => source,
        Err(reason) => return unchanged(data, Some(reason)),
    };
    let subscription = match read_subscription(data.get(ANTHROPIC_SUBSCRIPTION_PROVIDER_ID)) {
        Ok(subscription) => subscription,
        Err(reason) => return unchanged(data, Some(reason)),
    };
    let recorded = imported_digests(&entry);
    let unrecorded: Vec<&Slot> = source
        .grants
        .iter()
        .filter(|grant| !recorded.contains(&auth_grant_digest(refresh(grant))))
        .collect();
    if !unrecorded.is_empty() {
        let next = copy_grants(&data, &entry, &source, subscription, &unrecorded);
        return AnthropicOAuthMigrationResult {
            data: next,
            migrated: true,
            skipped: None,
        };
    }
    let mut next = data;
    match remaining_api_key_entry(&source.key_slots, source.pinned.as_ref()) {
        Some(remaining) => {
            next.insert("anthropic".to_owned(), remaining);
        }
        None => {
            next.shift_remove("anthropic");
        }
    }
    AnthropicOAuthMigrationResult {
        data: next,
        migrated: true,
        skipped: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(value: Value) -> AuthStorageData {
        value.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn copies_then_retires_a_flat_grant() {
        let start = data(
            json!({"anthropic": {"type": "oauth", "access": "a", "refresh": "r1", "expires": 1}}),
        );
        let first = migrate_anthropic_oauth_to_subscription(start);
        assert!(first.migrated);
        let subscription = &first.data[ANTHROPIC_SUBSCRIPTION_PROVIDER_ID];
        assert_eq!(subscription["accounts"][0]["name"], "imported-anthropic");
        assert_eq!(
            subscription["accounts"][0]["importedFrom"],
            json!(auth_grant_digest("r1"))
        );
        assert_eq!(subscription["access"], SENTINEL_OAUTH_FIELDS_ACCESS);
        assert_eq!(
            first.data["anthropic"][IMPORTED_GRANTS_FIELD],
            json!([auth_grant_digest("r1")])
        );
        assert_eq!(first.data["anthropic"]["refresh"], "r1");

        let second = migrate_anthropic_oauth_to_subscription(first.data);
        assert!(second.migrated);
        assert!(!second.data.contains_key("anthropic"));
        assert_eq!(
            second.data[ANTHROPIC_SUBSCRIPTION_PROVIDER_ID]["accounts"]
                .as_array()
                .map(Vec::len),
            Some(1)
        );
    }

    #[test]
    fn keeps_api_key_slots_and_maps_the_pin() {
        let start = data(json!({"anthropic": {
            "type": "oauth", "pinned": "work",
            "accounts": [
                {"name": "work", "access": "a", "refresh": "r1", "expires": 1},
                {"name": "key", "key": "sk"}
            ]
        }}));
        let first = migrate_anthropic_oauth_to_subscription(start);
        assert_eq!(
            first.data[ANTHROPIC_SUBSCRIPTION_PROVIDER_ID]["pinned"],
            "imported-anthropic-work"
        );
        let second = migrate_anthropic_oauth_to_subscription(first.data);
        assert_eq!(
            second.data["anthropic"],
            json!({"type": "api_key", "key": "sk", "accounts": [{"name": "key", "key": "sk"}]})
        );
    }

    #[test]
    fn leaves_unparseable_entries_untouched_with_a_reason() {
        let broken = data(json!({"anthropic": {"type": "oauth", "accounts": [{"name": "x"}]}}));
        let result = migrate_anthropic_oauth_to_subscription(broken.clone());
        assert_eq!(
            result,
            AnthropicOAuthMigrationResult {
                data: broken,
                migrated: false,
                skipped: Some(
                    "an account in it is neither a complete OAuth grant nor an API key".to_owned()
                ),
            }
        );
        let api_key = data(json!({"anthropic": {"type": "api_key", "key": "k"}}));
        assert_eq!(
            migrate_anthropic_oauth_to_subscription(api_key.clone()).data,
            api_key
        );
        let taken = HashSet::from(["imported-anthropic".to_owned()]);
        assert_eq!(
            imported_slot_name(&Slot::new(), false, &taken),
            "imported-anthropic-2"
        );
    }
}
