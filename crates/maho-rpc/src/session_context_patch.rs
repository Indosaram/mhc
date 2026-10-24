//! `set_session_context`: a patch layer over the context a session was opened with (senpi
//! `modes/rpc/session-context-patch.ts`).
//!
//! The open-time context stays frozen (it is what the session's extensions read as `pi.sessionContext`);
//! patches live in a separate overlay the host republishes on `list_sessions { include_workers: true }` and
//! on `session_status` for subscribers that asked for context. A `null` value removes the key from the
//! published view, including a key the open supplied. The host's own identity keys can never be patched.
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// `SessionContextValue`: a string, number or boolean (a JSON scalar).
pub type SessionContextValue = Value;
/// `SessionContextOverlay`: key to scalar, or `null` to remove the key from the published view.
pub type SessionContextOverlay = Map<String, Value>;
/// `PublishedSessionContext` (session-status.ts): the open-time context overlaid with every patch.
pub type PublishedSessionContext = Map<String, Value>;

/// `SESSION_CONTEXT_PATCH_LIMITS`.
pub struct SessionContextPatchLimits {
    /// `keyPattern`: `^[a-z][a-z0-9_]{0,63}$` (see [`key_matches_pattern`]).
    pub key_pattern: &'static str,
    pub value_chars: usize,
    pub overlay_bytes: usize,
}

pub const SESSION_CONTEXT_PATCH_LIMITS: SessionContextPatchLimits = SessionContextPatchLimits {
    key_pattern: "^[a-z][a-z0-9_]{0,63}$",
    value_chars: 256,
    overlay_bytes: 4_096,
};

/// Keys a client may never patch: the host's own stamps, and the lineage labels a
/// `childrenOf` subscription matches on. Lineage belongs to the frozen open-time context;
/// letting a patch rewrite it would let any attached client re-parent a session in every
/// supervisor's `childrenOf` view without the subscribers ever being told.
pub const RESERVED_SESSION_CONTEXT_KEYS: [&str; 6] = [
    "host_socket",
    "host_instance",
    "shard_key",
    "tree_key",
    "parent_session_id",
    "root_session_id",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextPatchRefusal {
    pub reason: String,
    pub action: String,
}

/// `SESSION_CONTEXT_PATCH_LIMITS.keyPattern.test(key)`.
pub fn key_matches_pattern(key: &str) -> bool {
    let mut chars = key.chars();
    chars.next().is_some_and(|first| first.is_ascii_lowercase())
        && key.len() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn refuse(reason: impl Into<String>, action: impl Into<String>) -> ContextPatchRefusal {
    ContextPatchRefusal {
        reason: reason.into(),
        action: action.into(),
    }
}

/// `applyContextPatch`: the merged overlay, or the refusal (`{ refusal }`) naming the reason and the fix.
pub fn apply_context_patch(
    overlay: Option<&SessionContextOverlay>,
    patch: &Value,
) -> Result<SessionContextOverlay, ContextPatchRefusal> {
    let shape = "send patch as an object of lowercase keys to string, number, boolean or null values, e.g. { \"task_status\": \"running\" }";
    let Some(entries) = patch.as_object() else {
        return Err(refuse("patch must be an object", shape));
    };
    if entries.is_empty() {
        return Err(refuse("patch has no keys", shape));
    }
    let mut merged: SessionContextOverlay = overlay.cloned().unwrap_or_default();
    for (key, value) in entries {
        if !key_matches_pattern(key) {
            return Err(refuse(
                format!(
                    "key \"{key}\" must match {}",
                    SESSION_CONTEXT_PATCH_LIMITS.key_pattern
                ),
                "rename the key to lowercase letters, digits and underscores",
            ));
        }
        if RESERVED_SESSION_CONTEXT_KEYS.contains(&key.as_str()) {
            return Err(refuse(
                format!("key \"{key}\" is set by the host and cannot be patched"),
                "drop that key from the patch; the host keeps it current",
            ));
        }
        if let Some(text) = value.as_str() {
            // JS `string.length` counts UTF-16 code units.
            let length = text.encode_utf16().count();
            if length > SESSION_CONTEXT_PATCH_LIMITS.value_chars {
                return Err(refuse(
                    format!(
                        "value for \"{key}\" is {length} characters, at most {} are accepted",
                        SESSION_CONTEXT_PATCH_LIMITS.value_chars
                    ),
                    "shorten the value or store it elsewhere and publish a reference",
                ));
            }
        }
        if value.is_number() && value.as_f64().is_none_or(|number| !number.is_finite()) {
            return Err(refuse(
                format!("value for \"{key}\" must be a finite number"),
                shape,
            ));
        }
        if !(value.is_null() || value.is_string() || value.is_number() || value.is_boolean()) {
            return Err(refuse(
                format!("value for \"{key}\" must be a string, number, boolean or null"),
                shape,
            ));
        }
        merged.insert(key.clone(), value.clone());
    }
    let bytes = Value::Object(merged.clone()).to_string().len();
    if bytes > SESSION_CONTEXT_PATCH_LIMITS.overlay_bytes {
        return Err(refuse(
            format!(
                "patched context would be {bytes} bytes, at most {} are accepted",
                SESSION_CONTEXT_PATCH_LIMITS.overlay_bytes
            ),
            "remove keys with null or shorten values before adding more",
        ));
    }
    Ok(merged)
}

/// `publishedSessionContext`: the frozen open-time `base` with every overlay key applied; `null` removes.
pub fn published_session_context(
    base: &BTreeMap<String, String>,
    overlay: Option<&SessionContextOverlay>,
) -> PublishedSessionContext {
    let mut published: PublishedSessionContext = base
        .iter()
        .map(|(key, value)| (key.clone(), Value::String(value.clone())))
        .collect();
    let Some(overlay) = overlay else {
        return published;
    };
    for (key, value) in overlay {
        if value.is_null() {
            published.remove(key);
        } else {
            published.insert(key.clone(), value.clone());
        }
    }
    published
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn refusal(patch: Value) -> ContextPatchRefusal {
        match apply_context_patch(None, &patch) {
            Err(refusal) => refusal,
            Ok(overlay) => panic!("expected a refusal, got {overlay:?}"),
        }
    }

    #[test]
    fn merges_scalars_over_the_previous_overlay_and_keeps_null() {
        let first = apply_context_patch(None, &json!({"task_status":"running","n":1})).unwrap();
        let second =
            apply_context_patch(Some(&first), &json!({"task_status":null,"ok":true})).unwrap();
        assert_eq!(
            Value::Object(second),
            json!({"task_status":null,"n":1,"ok":true})
        );
    }

    #[test]
    fn refuses_non_objects_and_empty_patches_with_the_shape_hint() {
        for patch in [json!(null), json!([]), json!("x"), json!(3)] {
            assert_eq!(refusal(patch).reason, "patch must be an object");
        }
        let empty = refusal(json!({}));
        assert_eq!(empty.reason, "patch has no keys");
        assert!(empty.action.starts_with("send patch as an object"));
    }

    #[test]
    fn refuses_bad_and_reserved_keys() {
        assert_eq!(
            refusal(json!({"Bad":1})).reason,
            "key \"Bad\" must match ^[a-z][a-z0-9_]{0,63}$"
        );
        assert_eq!(
            refusal(json!({"host_socket":"x"})).reason,
            "key \"host_socket\" is set by the host and cannot be patched"
        );
        for key in RESERVED_SESSION_CONTEXT_KEYS {
            let patch = Value::Object(Map::from_iter([(key.to_owned(), json!(1))]));
            assert!(apply_context_patch(None, &patch).is_err());
        }
        assert!(key_matches_pattern(&format!("a{}", "b".repeat(63))));
        assert!(!key_matches_pattern(&format!("a{}", "b".repeat(64))));
    }

    #[test]
    fn refuses_long_values_and_non_scalars() {
        let long = refusal(json!({"k":"x".repeat(257)}));
        assert_eq!(
            long.reason,
            "value for \"k\" is 257 characters, at most 256 are accepted"
        );
        assert!(apply_context_patch(None, &json!({"k":"x".repeat(256)})).is_ok());
        assert_eq!(
            refusal(json!({"k":{"a":1}})).reason,
            "value for \"k\" must be a string, number, boolean or null"
        );
    }

    #[test]
    fn refuses_an_overlay_over_the_byte_limit() {
        let mut patch = Map::new();
        for index in 0..20 {
            patch.insert(format!("key_{index}"), Value::String("v".repeat(256)));
        }
        let refused = refusal(Value::Object(patch));
        assert!(refused.reason.starts_with("patched context would be "));
        assert_eq!(
            refused.action,
            "remove keys with null or shorten values before adding more"
        );
    }

    #[test]
    fn published_context_overlays_and_removes() {
        let base = BTreeMap::from([
            ("a".to_owned(), "1".to_owned()),
            ("b".to_owned(), "2".to_owned()),
        ]);
        assert_eq!(
            Value::Object(published_session_context(&base, None)),
            json!({"a":"1","b":"2"})
        );
        let overlay = apply_context_patch(None, &json!({"a":null,"c":3})).unwrap();
        assert_eq!(
            Value::Object(published_session_context(&base, Some(&overlay))),
            json!({"b":"2","c":3})
        );
    }
}
