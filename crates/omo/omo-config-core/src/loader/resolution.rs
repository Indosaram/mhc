use serde_json::{Map, Value};

use crate::loader::merge::merge_omo_config_records;
use crate::loader::types::{DIAGNOSTIC_PROFILE, OmoConfigDiagnostic, OmoConfigEnv};
use crate::schema::harness::{
    OMO_CONFIG_LEGACY_HARNESS_ALIASES, canonical_harness_name, harness_block_key,
};

pub const HARNESS_KEYS: [&str; 5] = ["[codex]", "[opencode]", "[omo]", "[native]", "[senpi]"];

fn profile_name(value: Option<&String>) -> Option<String> {
    match value {
        Some(value) if !value.is_empty() => Some(value.clone()),
        _ => None,
    }
}

fn profile_name_from_open_code_config_dir(path: Option<&String>) -> Option<String> {
    let path = path?;
    let normalized = path.replace('\\', "/");
    let trimmed = normalized.trim_end_matches('/');
    let marker = trimmed.rfind("/profiles/");
    let (head, tail) = match marker {
        Some(index) => (&trimmed[..index], &trimmed[index + "/profiles/".len()..]),
        None if trimmed.starts_with("profiles/") => ("", &trimmed["profiles/".len()..]),
        None => return None,
    };
    let _ = head;
    if tail.is_empty() || tail.contains('/') {
        return None;
    }
    profile_name(Some(&tail.to_string()))
}

pub fn resolve_omo_profile_name(
    env: Option<&OmoConfigEnv>,
    profile: Option<&String>,
) -> Option<String> {
    let default_env = crate::loader::paths::process_env();
    let env = env.unwrap_or(&default_env);
    profile_name(profile)
        .or_else(|| profile_name(env.get("OMO_PROFILE")))
        .or_else(|| profile_name(env.get("OCX_PROFILE")))
        .or_else(|| profile_name_from_open_code_config_dir(env.get("OPENCODE_CONFIG_DIR")))
}

fn to_record(value: Option<&Value>) -> Option<Map<String, Value>> {
    match value {
        Some(Value::Object(map)) => Some(map.clone()),
        _ => None,
    }
}

fn without_control_keys(config: &Map<String, Value>) -> Map<String, Value> {
    let mut result = Map::new();
    for (key, value) in config {
        if key == "profiles" || HARNESS_KEYS.contains(&key.as_str()) {
            continue;
        }
        result.insert(key.clone(), value.clone());
    }
    result
}

// The legacy block is folded in FIRST so the canonical `[native]` block wins every key it also
// sets, while a config that only ever named `[senpi]` keeps applying in full.
fn harness_layer(config: &Map<String, Value>, harness: Option<&String>) -> Map<String, Value> {
    let Some(harness) = harness else {
        return Map::new();
    };
    let canonical = canonical_harness_name(harness);
    let mut layer = Map::new();
    for (legacy, target) in OMO_CONFIG_LEGACY_HARNESS_ALIASES {
        if target == canonical {
            let block = to_record(config.get(&harness_block_key(legacy))).unwrap_or_default();
            layer = merge_omo_config_records(&layer, &block);
        }
    }
    let block = to_record(config.get(&harness_block_key(&canonical))).unwrap_or_default();
    merge_omo_config_records(&layer, &block)
}

pub struct ResolveOmoConfigViewOptions<'a> {
    pub config: &'a Map<String, Value>,
    pub harness: Option<&'a String>,
    pub profile: Option<&'a String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolveOmoConfigViewResult {
    pub config: Map<String, Value>,
    pub diagnostics: Vec<OmoConfigDiagnostic>,
    pub profile: Option<String>,
}

pub fn resolve_omo_config_view(
    options: ResolveOmoConfigViewOptions<'_>,
) -> ResolveOmoConfigViewResult {
    let profiles = to_record(options.config.get("profiles"));
    let profile = options.profile.and_then(|name| {
        profiles
            .as_ref()
            .and_then(|profiles| to_record(profiles.get(name)))
    });
    let diagnostics: Vec<OmoConfigDiagnostic> = match (options.profile, profile.as_ref()) {
        (Some(name), None) => vec![OmoConfigDiagnostic {
            kind: DIAGNOSTIC_PROFILE,
            message: format!(
                "Activated omo profile \"{name}\" does not exist; using the base configuration"
            ),
            path: format!("profiles.{name}"),
            issue_paths: Vec::new(),
        }],
        _ => Vec::new(),
    };
    let empty = Map::new();
    let profile_layers: &Map<String, Value> = profile.as_ref().unwrap_or(&empty);
    let layers = vec![
        without_control_keys(options.config),
        harness_layer(options.config, options.harness),
        if profile.is_none() {
            Map::new()
        } else {
            without_control_keys(profile_layers)
        },
        if profile.is_none() {
            Map::new()
        } else {
            harness_layer(profile_layers, options.harness)
        },
    ];

    let mut config: Map<String, Value> = Map::new();
    for layer in &layers {
        config = merge_omo_config_records(&config, layer);
    }

    let resolved_profile = match (options.profile, profile.as_ref()) {
        (Some(name), Some(_)) => Some(name.clone()),
        _ => None,
    };
    ResolveOmoConfigViewResult {
        config: without_control_keys(&config),
        diagnostics,
        profile: resolved_profile,
    }
}
