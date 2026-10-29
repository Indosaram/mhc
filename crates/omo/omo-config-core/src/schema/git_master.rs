use serde_json::{Map, Value, json};

use crate::internal::validate::{Node, ObjectSpec, boolean, defaulted, optional, string, union};
use crate::issue::{Issues, custom};

pub const GIT_MASTER_HARNESS_SUPPORT: [(&str, &[&str]); 3] = [
    ("git_master.commit_footer", &["native"]),
    ("git_master.include_co_authored_by", &["native"]),
    ("git_master.git_env_prefix", &["native"]),
];

const GIT_ENV_PREFIX_MESSAGE: &str =
    "git_env_prefix must be empty or use shell-safe env assignments like \"GIT_MASTER=1\"";

/// Matches `/^(?:[A-Za-z_][A-Za-z0-9_]*=[A-Za-z0-9_-]*)(?: [A-Za-z_][A-Za-z0-9_]*=[A-Za-z0-9_-]*)*$/`.
pub fn is_valid_git_env_prefix(value: &str) -> bool {
    if value.is_empty() { return true; }
    value.split(' ').all(|assignment| {
        let Some((name, val)) = assignment.split_once('=') else { return false; };
        let mut chars = name.chars();
        let name_ok = matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
        name_ok && val.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    })
}

fn git_master_refine(value: &serde_json::Value, path: &[String], issues: &mut Issues) {
    if let Some(Value::String(prefix)) = value.get("git_env_prefix") && !is_valid_git_env_prefix(prefix) {
        let mut issue_path = path.to_vec();
        issue_path.push("git_env_prefix".to_string());
        issues.push(custom(&issue_path, GIT_ENV_PREFIX_MESSAGE));
    }
}

fn git_master_node(fields: Vec<crate::internal::validate::Field>) -> Node {
    Node::Object(Box::new(ObjectSpec { fields, strict: true, preprocess: None, refine: Some(git_master_refine) }))
}

pub fn git_master_setting_harness_support(setting_path: &str) -> Option<&'static [&'static str]> {
    GIT_MASTER_HARNESS_SUPPORT.iter().find(|(path, _)| *path == setting_path).map(|(_, harnesses)| *harnesses)
}

pub fn omo_git_master_settings_layer_schema() -> Node {
    git_master_node(vec![optional("commit_footer", union(vec![boolean(), string()])), optional("include_co_authored_by", boolean()), optional("git_env_prefix", string())])
}

pub fn omo_git_master_settings_schema() -> Node {
    git_master_node(vec![
        defaulted("commit_footer", union(vec![boolean(), string()]), || json!(false)),
        defaulted("include_co_authored_by", boolean(), || json!(true)),
        defaulted("git_env_prefix", string(), || json!("GIT_MASTER=1")),
    ])
}

fn default_git_master_settings() -> Map<String, Value> {
    match json!({ "commit_footer": false, "include_co_authored_by": true, "git_env_prefix": "GIT_MASTER=1" }) {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

/// Port of TS `resolveOmoGitMasterSettings`: `config.git_master ?? schema-defaults`, always with both keys.
pub fn resolve_omo_git_master_settings(config: &Value) -> Value {
    let mut resolved = default_git_master_settings();
    if let Some(Value::Object(section)) = config.get("git_master") {
        for (key, value) in section { resolved.insert(key.clone(), value.clone()); }
    }
    Value::Object(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn git_env_prefix_accepts_empty_and_safe_assignments() {
        assert!(is_valid_git_env_prefix("")); assert!(is_valid_git_env_prefix("GIT_MASTER=1"));
        assert!(is_valid_git_env_prefix("A=1 B_2=x-y")); assert!(is_valid_git_env_prefix("A="));
    }
    #[test]
    fn git_env_prefix_rejects_unsafe_values() {
        assert!(!is_valid_git_env_prefix("GIT_MASTER")); assert!(!is_valid_git_env_prefix("1A=1"));
        assert!(!is_valid_git_env_prefix("A=1;rm")); assert!(!is_valid_git_env_prefix("A=1  B=2"));
        assert!(!is_valid_git_env_prefix(" A=1")); assert!(!is_valid_git_env_prefix("A=$X"));
    }
    #[test]
    fn resolved_settings_default_git_env_prefix() {
        let resolved = resolve_omo_git_master_settings(&json!({}));
        assert_eq!(resolved["git_env_prefix"], json!("GIT_MASTER=1"));
        assert_eq!(resolved["include_co_authored_by"], json!(true));
        assert_eq!(resolve_omo_git_master_settings(&json!({"git_master":{"include_co_authored_by":false}}))["include_co_authored_by"], json!(false));
    }
}
