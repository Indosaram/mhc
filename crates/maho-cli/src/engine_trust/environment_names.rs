//! Port of `packages/coding-agent/src/engine-trust/environment-names.ts`.

use std::collections::{BTreeMap, BTreeSet};

pub type EnvironmentEntries = BTreeMap<String, Option<String>>;

fn case_forms_of(env: &EnvironmentEntries, name: &str, platform: &str) -> Vec<String> {
    if platform != "win32" {
        return if env.contains_key(name) {
            vec![name.to_owned()]
        } else {
            Vec::new()
        };
    }
    env.keys()
        .filter(|key| key.eq_ignore_ascii_case(name))
        .cloned()
        .collect()
}

pub fn has_name(env: &EnvironmentEntries, name: &str, platform: &str) -> bool {
    case_forms_of(env, name, platform)
        .into_iter()
        .any(|key| env.get(&key).and_then(|v| v.as_ref()).is_some())
}

/// Every distinct defined value the name carries; more than one means the CRT, not us, picks.
pub fn values_of(env: &EnvironmentEntries, name: &str, platform: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for key in case_forms_of(env, name, platform) {
        if let Some(Some(value)) = env.get(&key)
            && seen.insert(value.clone())
        {
            out.push(value.clone());
        }
    }
    out
}

pub fn delete_name(env: &mut EnvironmentEntries, name: &str, platform: &str) {
    for key in case_forms_of(env, name, platform) {
        env.remove(&key);
    }
}

pub fn set_name(env: &mut EnvironmentEntries, name: &str, value: &str, platform: &str) {
    delete_name(env, name, platform);
    env.insert(name.to_owned(), Some(value.to_owned()));
}
