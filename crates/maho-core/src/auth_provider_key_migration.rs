//! Port of senpi packages/coding-agent/src/core/auth-provider-key-migration.ts.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

/// One-shot auth.json provider-key migration for the provider rename: a credential stored under a
/// legacy provider id is rewritten under its canonical id. The credential object moves verbatim;
/// when both keys hold an entry the canonical entry wins and the legacy entry is removed.
pub fn migrate_legacy_provider_keys(
    data: &Map<String, Value>,
    legacy_provider_ids: &BTreeMap<String, String>,
) -> (Map<String, Value>, bool) {
    let mut next: Option<Map<String, Value>> = None;
    for (legacy_id, canonical_id) in legacy_provider_ids {
        let legacy = data.get(legacy_id);
        let Some(Value::Object(legacy_object)) = legacy else { continue };
        let working = next.get_or_insert_with(|| data.clone());
        working.remove(legacy_id);
        if !working.contains_key(canonical_id) {
            working.insert(canonical_id.clone(), Value::Object(legacy_object.clone()));
        }
    }
    match next {
        Some(next) => (next, true),
        None => (data.clone(), false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn legacy() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("openai-codex".to_owned(), "chatgpt-subscription".to_owned()),
            ("claude-sdk-oauth".to_owned(), "anthropic-subscription".to_owned()),
        ])
    }

    fn data(value: Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn a_legacy_key_is_moved_verbatim() {
        let input = data(json!({ "openai-codex": { "type": "oauth", "accounts": ["a"] } }));
        let (next, migrated) = migrate_legacy_provider_keys(&input, &legacy());
        assert!(migrated);
        assert!(!next.contains_key("openai-codex"));
        assert_eq!(next["chatgpt-subscription"], json!({ "type": "oauth", "accounts": ["a"] }));
    }

    #[test]
    fn the_canonical_entry_wins_on_conflict() {
        let input = data(json!({
            "openai-codex": { "type": "oauth", "accounts": ["legacy"] },
            "chatgpt-subscription": { "type": "oauth", "accounts": ["canonical"] }
        }));
        let (next, migrated) = migrate_legacy_provider_keys(&input, &legacy());
        assert!(migrated);
        assert!(!next.contains_key("openai-codex"));
        assert_eq!(next["chatgpt-subscription"], json!({ "type": "oauth", "accounts": ["canonical"] }));
    }

    #[test]
    fn no_legacy_key_is_a_no_op() {
        let input = data(json!({ "anthropic": { "type": "api_key", "key": "x" } }));
        let (next, migrated) = migrate_legacy_provider_keys(&input, &legacy());
        assert!(!migrated);
        assert_eq!(next, input);
    }

    #[test]
    fn a_non_object_legacy_value_stays_put() {
        let input = data(json!({ "openai-codex": "not-an-object" }));
        let (next, migrated) = migrate_legacy_provider_keys(&input, &legacy());
        assert!(!migrated);
        assert_eq!(next["openai-codex"], json!("not-an-object"));
    }
}
