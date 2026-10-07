use serde_json::{Map, Value, json};

use crate::internal::validate::{Node, boolean, defaulted, optional, strict_object, string, union};

pub const GIT_MASTER_HARNESS_SUPPORT: [(&str, &[&str]); 2] = [
    ("git_master.commit_footer", &["native"]),
    ("git_master.include_co_authored_by", &["native"]),
];

pub fn git_master_setting_harness_support(setting_path: &str) -> Option<&'static [&'static str]> {
    GIT_MASTER_HARNESS_SUPPORT
        .iter()
        .find(|(path, _)| *path == setting_path)
        .map(|(_, harnesses)| *harnesses)
}

pub fn omo_git_master_settings_layer_schema() -> Node {
    strict_object(vec![
        optional("commit_footer", union(vec![boolean(), string()])),
        optional("include_co_authored_by", boolean()),
    ])
}

pub fn omo_git_master_settings_schema() -> Node {
    strict_object(vec![
        defaulted("commit_footer", union(vec![boolean(), string()]), || {
            json!(false)
        }),
        defaulted("include_co_authored_by", boolean(), || json!(false)),
    ])
}

fn default_git_master_settings() -> Map<String, Value> {
    match json!({ "commit_footer": false, "include_co_authored_by": false }) {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

/// Port of TS `resolveOmoGitMasterSettings`: `config.git_master ?? schema-defaults`, always with both keys.
pub fn resolve_omo_git_master_settings(config: &Value) -> Value {
    let mut resolved = default_git_master_settings();
    if let Some(Value::Object(section)) = config.get("git_master") {
        for (key, value) in section {
            resolved.insert(key.clone(), value.clone());
        }
    }
    Value::Object(resolved)
}
