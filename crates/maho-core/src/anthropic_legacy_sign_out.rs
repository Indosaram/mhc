//! Auth-store migration for the retired senpi-owned Claude OAuth login (senpi#4436).

use crate::auth_storage::AuthStorageData;
use serde_json::{Map, Value, json};

pub struct LegacySignOutResult {
    pub data: AuthStorageData,
    pub migrated: bool,
    pub skipped: Option<String>,
}

const SUBSCRIPTION: &str = "anthropic-subscription";
const SENTINEL: &str = "claude-sdk-oauth-managed";

fn slot_list(value: Option<&Value>) -> Result<Option<&[Value]>, ()> {
    match value {
        None => Ok(None),
        Some(Value::Array(slots)) if slots.iter().all(Value::is_object) => Ok(Some(slots)),
        Some(_) => Err(()),
    }
}

fn retired(slot: &Value) -> bool {
    slot.get("store").is_none() && matches!(slot["source"].as_str(), Some("login" | "import"))
}

fn signed_out_slot(slot: &Value) -> Value {
    let mut next = json!({
        "name": slot["name"],
        "source": "login",
        "store": "claude-cli",
        "dirId": uuid::Uuid::new_v4().simple().to_string(),
        "signInRequired": "legacy_sign_in_retired"
    });
    if slot["displayName"].is_string() {
        next["displayName"] = slot["displayName"].clone();
    }
    if slot["modelBlocks"].is_object() {
        next["modelBlocks"] = slot["modelBlocks"].clone();
    }
    next
}

fn subscription(entry: &Value) -> Result<Option<Value>, &'static str> {
    let stored = entry
        .as_object()
        .ok_or("the anthropic-subscription entry is not an object")?;
    let slots = slot_list(stored.get("accounts"))
        .map_err(|()| "the anthropic-subscription accounts are not account objects")?
        .unwrap_or_default();
    if slots
        .iter()
        .any(|slot| retired(slot) && !slot["name"].is_string())
    {
        return Err("an anthropic-subscription account has no name");
    }
    if !slots.iter().any(retired) {
        return Ok(None);
    }

    let mut next = stored.clone();
    next.insert("access".into(), json!(SENTINEL));
    next.insert("refresh".into(), json!(SENTINEL));
    next.insert("expires".into(), json!(4_102_444_800_000_i64));
    next.insert(
        "accounts".into(),
        Value::Array(
            slots
                .iter()
                .map(|slot| {
                    if retired(slot) {
                        signed_out_slot(slot)
                    } else {
                        slot.clone()
                    }
                })
                .collect(),
        ),
    );
    Ok(Some(Value::Object(next)))
}

fn is_grant(slot: &Value) -> bool {
    slot["access"].is_string() && slot["refresh"].is_string() && slot["expires"].is_number()
}

fn anthropic_key_slots(entry: &Value) -> Result<Vec<Value>, &'static str> {
    let slots = slot_list(entry.get("accounts"))
        .map_err(|()| "its accounts list is not a list of account objects")?;
    let Some(slots) = slots else {
        return if is_grant(entry) {
            Ok(Vec::new())
        } else {
            Err("it holds no complete OAuth grant")
        };
    };
    let mut keys = Vec::new();
    for slot in slots {
        if slot["key"].is_string() && slot.get("access").is_none() && slot.get("refresh").is_none()
        {
            keys.push(slot.clone());
        } else if !is_grant(slot) {
            return Err("an account in it is neither a complete OAuth grant nor an API key");
        }
    }
    Ok(keys)
}

fn remaining_key_entry(slots: Vec<Value>, pin: Option<&str>) -> Option<Value> {
    let pinned = pin.and_then(|pin| slots.iter().find(|slot| slot["name"].as_str() == Some(pin)));
    let selected = pinned.or_else(|| slots.first())?;
    let mut next = Map::new();
    next.insert("type".into(), json!("api_key"));
    next.insert("key".into(), selected["key"].clone());
    if let Some(pinned) = pinned {
        next.insert("pinned".into(), pinned["name"].clone());
    }
    next.insert("accounts".into(), Value::Array(slots));
    Some(Value::Object(next))
}

/// Sign out retired OAuth grants, never copying tokens to the new subscription slots.
/// Malformed entries are left intact while unrelated valid entries are still migrated.
pub fn sign_out_legacy_claude_logins(data: &AuthStorageData) -> LegacySignOutResult {
    let mut next = data.clone();
    let mut migrated = false;
    let mut skipped = Vec::new();
    if let Some(entry) = data.get(SUBSCRIPTION) {
        match subscription(entry) {
            Ok(Some(entry)) => {
                next.insert(SUBSCRIPTION.into(), entry);
                migrated = true;
            }
            Ok(None) => {}
            Err(reason) => skipped.push(reason.to_owned()),
        }
    }
    if let Some(entry) = data.get("anthropic")
        && entry["type"] == "oauth"
    {
        match anthropic_key_slots(entry) {
            Ok(keys) => {
                if let Some(entry) = remaining_key_entry(keys, entry["pinned"].as_str()) {
                    next.insert("anthropic".into(), entry);
                } else {
                    next.remove("anthropic");
                }
                migrated = true;
            }
            Err(reason) => skipped.push(format!("the anthropic entry: {reason}")),
        }
    }
    LegacySignOutResult {
        data: next,
        migrated,
        skipped: (!skipped.is_empty()).then(|| skipped.join("; ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retires_grants_without_losing_names_order_pins_or_api_keys() {
        let input = json!({
            "anthropic": {"type":"oauth","pinned":"key","accounts":[
                {"name":"old","access":"test-a","refresh":"test-r","expires":1},
                {"name":"key","key":"fixture"}
            ]},
            SUBSCRIPTION: {"type":"oauth","pinned":"work","order":["work","cli"],"accounts":[
                {"name":"work","displayName":"Work","source":"import","access":"test-a",
                 "refresh":"test-r","expires":1,"blockedUntil":100,"modelBlocks":{"opus":{"blockedUntil":100}}},
                {"name":"cli","store":"claude-cli","dirId":"00000000000000000000000000000000"}
            ]}
        });
        let result = sign_out_legacy_claude_logins(input.as_object().expect("map"));
        assert!(result.migrated);
        assert!(result.skipped.is_none());
        assert_eq!(
            result.data["anthropic"],
            json!({
                "type":"api_key","key":"fixture","pinned":"key","accounts":[{"name":"key","key":"fixture"}]
            })
        );
        let subscription = &result.data[SUBSCRIPTION];
        assert_eq!(subscription["access"], SENTINEL);
        assert_eq!(subscription["pinned"], "work");
        assert_eq!(subscription["order"], json!(["work", "cli"]));
        let slot = &subscription["accounts"][0];
        assert_eq!(slot["signInRequired"], "legacy_sign_in_retired");
        assert_eq!(slot["dirId"].as_str().expect("dir id").len(), 32);
        assert_eq!(slot["displayName"], "Work");
        assert_eq!(slot["modelBlocks"]["opus"]["blockedUntil"], 100);
        assert!(slot.get("access").is_none() && slot.get("blockedUntil").is_none());
        assert_eq!(
            subscription["accounts"][1],
            input[SUBSCRIPTION]["accounts"][1]
        );
        let again = sign_out_legacy_claude_logins(&result.data);
        assert!(!again.migrated);
        assert_eq!(again.data, result.data);
    }

    #[test]
    fn leaves_malformed_entry_but_retires_independent_flat_oauth() {
        let input = json!({
            SUBSCRIPTION: {"accounts": [false]},
            "anthropic": {"type":"oauth","access":"test-a","refresh":"test-r","expires":1}
        });
        let result = sign_out_legacy_claude_logins(input.as_object().expect("map"));
        assert!(result.migrated);
        assert_eq!(result.data[SUBSCRIPTION], input[SUBSCRIPTION]);
        assert!(!result.data.contains_key("anthropic"));
        assert!(result.skipped.is_some());
    }
}
