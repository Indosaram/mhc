use std::collections::HashSet;

use serde_json::{Map, Value};

use crate::loader::types::OmoConfigRawLayer;
use crate::schema::harness::{
    OMO_CONFIG_LEGACY_HARNESS_ALIASES, canonical_harness_name, harness_block_key,
};

pub struct CollectDisabledSkillsOptions<'a> {
    pub harness: Option<&'a str>,
    pub layers: &'a [OmoConfigRawLayer],
    pub profile: Option<&'a str>,
}

fn to_record(value: Option<&Value>) -> Option<&Map<String, Value>> {
    match value {
        Some(Value::Object(map)) => Some(map),
        _ => None,
    }
}

fn names_at(record: Option<&Map<String, Value>>) -> Vec<String> {
    let Some(record) = record else {
        return Vec::new();
    };
    let Some(Value::Array(entries)) = record.get("disabled_skills") else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect()
}

// Both spellings of a renamed harness are read. The denylist is a union, so there is no precedence
// to decide, and a raw layer handed in by a caller that did not canonicalize it still contributes.
fn harness_block_keys(harness: &str) -> Vec<String> {
    let canonical = canonical_harness_name(harness);
    let mut keys = vec![harness_block_key(&canonical)];
    for (legacy, target) in OMO_CONFIG_LEGACY_HARNESS_ALIASES {
        if target == canonical {
            keys.push(harness_block_key(legacy));
        }
    }
    keys
}

fn scopes_of<'a>(
    config: &'a Map<String, Value>,
    harness: Option<&str>,
) -> Vec<Option<&'a Map<String, Value>>> {
    let Some(harness) = harness else {
        return vec![Some(config)];
    };
    let mut scopes = vec![Some(config)];
    for key in harness_block_keys(harness) {
        scopes.push(to_record(config.get(&key)));
    }
    scopes
}

/// The canonical skill denylist across every loaded layer.
///
/// Unlike the generic view merge, where a later array replaces an earlier one, `disabled_skills`
/// is a UNION: a project layer cannot re-enable a skill the user layer turned off by omitting it.
/// The shared base, the `[harness]` block, and the selected profile's copies of both all count.
pub fn collect_disabled_skills(options: &CollectDisabledSkillsOptions<'_>) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for layer in options.layers {
        let Some(config) = to_record(Some(&layer.config)) else {
            continue;
        };
        let profile = options.profile.and_then(|name| {
            to_record(config.get("profiles")).and_then(|profiles| to_record(profiles.get(name)))
        });
        let mut scopes = scopes_of(config, options.harness);
        if let Some(profile) = profile {
            scopes.extend(scopes_of(profile, options.harness));
        }
        for scope in scopes {
            for name in names_at(scope) {
                if seen.insert(name.clone()) {
                    names.push(name);
                }
            }
        }
    }
    names
}
