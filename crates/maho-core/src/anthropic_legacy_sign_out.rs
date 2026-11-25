//! Port of senpi `packages/coding-agent/src/core/anthropic-legacy-sign-out.ts`.

use serde_json::{Map, Value};

use crate::auth_storage::AuthStorageData;
use crate::credential_accounts::{SENTINEL_OAUTH_FIELDS_ACCESS, SENTINEL_OAUTH_FIELDS_EXPIRES};

const ANTHROPIC_SUBSCRIPTION_PROVIDER_ID: &str = "anthropic-subscription";

type Slot = Map<String, Value>;

#[derive(Debug, Clone, PartialEq)]
pub struct LegacySignOutResult {
    pub data: AuthStorageData,
    pub migrated: bool,
    /// Why an entry was left exactly as stored; the rest of the store is still signed out.
    pub skipped: Option<String>,
}

fn new_account_dir_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

fn is_oauth_grant(value: &Slot) -> bool {
    value.get("access").is_some_and(Value::is_string)
        && value.get("refresh").is_some_and(Value::is_string)
        && value.get("expires").is_some_and(Value::is_number)
}

fn is_key_slot(value: &Slot) -> bool {
    value.get("key").is_some_and(Value::is_string)
        && !value.contains_key("access")
        && !value.contains_key("refresh")
}

fn slot_list(value: &Value) -> Option<Vec<Slot>> {
    value
        .as_array()?
        .iter()
        .map(|slot| slot.as_object().cloned())
        .collect()
}

fn anthropic_key_slots(entry: &Slot) -> Result<Vec<Slot>, &'static str> {
    let Some(accounts_val) = entry.get("accounts") else {
        return if is_oauth_grant(entry) {
            Ok(Vec::new())
        } else {
            Err("it holds no complete OAuth grant")
        };
    };
    let Some(accounts) = slot_list(accounts_val) else {
        return Err("its accounts list is not a list of account objects");
    };
    let key_slots: Vec<Slot> = accounts
        .iter()
        .filter(|slot| is_key_slot(slot))
        .cloned()
        .collect();
    let oauth_count = accounts.iter().filter(|slot| is_oauth_grant(slot)).count();
    if key_slots.len() + oauth_count != accounts.len() {
        return Err("an account in it is neither a complete OAuth grant nor an API key");
    }
    Ok(key_slots)
}

fn remaining_api_key_entry(key_slots: Vec<Slot>, pinned: Option<&Value>) -> Option<Value> {
    let pinned_name = pinned.and_then(Value::as_str);
    let pinned_slot = pinned_name.and_then(|name| {
        key_slots
            .iter()
            .find(|slot| slot.get("name").and_then(Value::as_str) == Some(name))
    });
    let key = pinned_slot
        .or_else(|| key_slots.first())
        .and_then(|slot| slot.get("key"))
        .cloned()?;
    let pinned_name_to_insert = pinned_slot.and_then(|slot| slot.get("name").cloned());
    let mut map = Map::new();
    map.insert("type".to_owned(), Value::String("api_key".to_owned()));
    map.insert("key".to_owned(), key);
    map.insert(
        "accounts".to_owned(),
        Value::Array(key_slots.into_iter().map(Value::Object).collect()),
    );
    if let Some(name) = pinned_name_to_insert {
        map.insert("pinned".to_owned(), name);
    }
    Some(Value::Object(map))
}

fn signed_out_slot(slot: &Slot) -> Slot {
    let mut out = Map::new();
    if let Some(name) = slot.get("name").cloned() {
        out.insert("name".to_owned(), name);
    }
    if let Some(display_name) = slot.get("displayName").filter(|v| v.is_string()).cloned() {
        out.insert("displayName".to_owned(), display_name);
    }
    out.insert("source".to_owned(), Value::String("login".to_owned()));
    out.insert("store".to_owned(), Value::String("claude-cli".to_owned()));
    out.insert("dirId".to_owned(), Value::String(new_account_dir_id()));
    out.insert(
        "signInRequired".to_owned(),
        Value::String("legacy_sign_in_retired".to_owned()),
    );
    if let Some(blocks) = slot.get("modelBlocks").filter(|v| v.is_object()).cloned() {
        out.insert("modelBlocks".to_owned(), blocks);
    }
    out
}

fn is_retired_login_slot(slot: &Slot) -> bool {
    !slot.contains_key("store")
        && matches!(
            slot.get("source").and_then(Value::as_str),
            Some("login" | "import")
        )
}

fn sign_out_subscription(stored: Option<&Value>) -> Result<Option<Value>, &'static str> {
    let Some(stored_val) = stored else {
        return Ok(None);
    };
    let Some(obj) = stored_val.as_object() else {
        return Err("the anthropic-subscription entry is not an object");
    };
    let accounts = match obj.get("accounts") {
        None => Vec::new(),
        Some(raw) => {
            slot_list(raw).ok_or("the anthropic-subscription accounts are not account objects")?
        }
    };
    if accounts
        .iter()
        .any(|slot| is_retired_login_slot(slot) && !slot.get("name").is_some_and(Value::is_string))
    {
        return Err("an anthropic-subscription account has no name");
    }
    if !accounts.iter().any(is_retired_login_slot) {
        return Ok(None);
    }
    let signed_out: Vec<Value> = accounts
        .iter()
        .map(|slot| {
            Value::Object(if is_retired_login_slot(slot) {
                signed_out_slot(slot)
            } else {
                slot.clone()
            })
        })
        .collect();
    let mut next = obj.clone();
    next.insert("type".to_owned(), Value::String("oauth".to_owned()));
    next.insert(
        "access".to_owned(),
        Value::String(SENTINEL_OAUTH_FIELDS_ACCESS.to_owned()),
    );
    next.insert("refresh".to_owned(), Value::String(String::new()));
    next.insert(
        "expires".to_owned(),
        Value::Number(SENTINEL_OAUTH_FIELDS_EXPIRES.into()),
    );
    next.insert("accounts".to_owned(), Value::Array(signed_out));
    Ok(Some(Value::Object(next)))
}

enum AnthropicOutcome {
    Unchanged,
    Delete,
    Replace(Value),
}

fn sign_out_anthropic(stored: Option<&Value>) -> Result<AnthropicOutcome, &'static str> {
    let Some(obj) = stored.and_then(Value::as_object) else {
        return Ok(AnthropicOutcome::Unchanged);
    };
    if obj.get("type").and_then(Value::as_str) != Some("oauth") {
        return Ok(AnthropicOutcome::Unchanged);
    }
    let key_slots = anthropic_key_slots(obj)?;
    match remaining_api_key_entry(key_slots, obj.get("pinned")) {
        Some(entry) => Ok(AnthropicOutcome::Replace(entry)),
        None => Ok(AnthropicOutcome::Delete),
    }
}

/// Signs out every Claude subscription login from the retired senpi-owned sign-in (senpi#4436), once, at the
/// auth.json load seam. No token is copied or kept anywhere.
pub fn sign_out_legacy_claude_logins(data: &AuthStorageData) -> LegacySignOutResult {
    let mut skipped = Vec::new();
    let mut next = data.clone();
    let mut migrated = false;

    match sign_out_subscription(data.get(ANTHROPIC_SUBSCRIPTION_PROVIDER_ID)) {
        Ok(Some(entry)) => {
            next.insert(ANTHROPIC_SUBSCRIPTION_PROVIDER_ID.to_owned(), entry);
            migrated = true;
        }
        Ok(None) => {}
        Err(reason) => skipped.push(reason.to_owned()),
    }

    match sign_out_anthropic(data.get("anthropic")) {
        Ok(AnthropicOutcome::Unchanged) => {}
        Ok(AnthropicOutcome::Delete) => {
            next.remove("anthropic");
            migrated = true;
        }
        Ok(AnthropicOutcome::Replace(entry)) => {
            next.insert("anthropic".to_owned(), entry);
            migrated = true;
        }
        Err(reason) => skipped.push(format!("the anthropic entry: {reason}")),
    }

    LegacySignOutResult {
        data: if migrated { next } else { data.clone() },
        migrated,
        skipped: if skipped.is_empty() {
            None
        } else {
            Some(skipped.join("; "))
        },
    }
}
