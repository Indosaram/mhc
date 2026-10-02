use crate::{types::{Action, PermissionConfig, PermissionPresetName, PermissionValue, Rule, Ruleset}, wildcard};
use std::collections::BTreeSet;

pub const EDIT_TOOLS: [&str; 4] = ["edit", "write", "apply_patch", "multiedit"];
pub const DEFAULT_PERMISSION_PRESET: PermissionPresetName = PermissionPresetName::FullAccess;

pub fn expand(path: &str, home: &str) -> String {
    if path == "~" { return home.into(); }
    if let Some(suffix) = path.strip_prefix('~').filter(|suffix| suffix.starts_with('/')) { return format!("{home}{suffix}"); }
    if let Some(suffix) = path.strip_prefix("$HOME") { return format!("{home}{suffix}"); }
    path.into()
}
pub fn from_config(config: &PermissionConfig, home: &str) -> Result<Ruleset, serde_json::Error> {
    let mut rules = Vec::new();
    for (permission, value) in config {
        match value {
            PermissionValue::Action(action) => rules.push(Rule { permission: permission.clone(), pattern: "*".into(), action: *action }),
            PermissionValue::Patterns(patterns) => for (pattern, action) in patterns {
                rules.push(Rule { permission: permission.clone(), pattern: expand(pattern,home), action: serde_json::from_value(action.clone())? });
            },
        }
    }
    Ok(rules)
}
pub fn rules_for_preset(preset: PermissionPresetName) -> Ruleset {
    let rule = |permission: &str, action| Rule { permission: permission.into(), pattern: "*".into(), action };
    match preset {
        PermissionPresetName::FullAccess => vec![rule("*",Action::Allow)],
        PermissionPresetName::Ask => vec![rule("*",Action::Ask)],
        PermissionPresetName::Workspace | PermissionPresetName::ReadOnly => vec![
            rule("*",Action::Ask), rule("read",Action::Allow),rule("list",Action::Allow),rule("grep",Action::Allow),
            rule("edit",if preset == PermissionPresetName::Workspace { Action::Allow } else { Action::Ask }),
            rule("bash",if preset == PermissionPresetName::Workspace { Action::Allow } else { Action::Ask }),rule("external_directory",Action::Ask),
        ],
    }
}
pub fn merge(rulesets: &[&[Rule]]) -> Ruleset { rulesets.iter().flat_map(|rules| rules.iter().cloned()).collect() }
pub fn disabled(tools: &[String], ruleset: &[Rule]) -> BTreeSet<String> {
    tools.iter().filter(|tool| {
        let permission = if EDIT_TOOLS.contains(&tool.as_str()) { "edit" } else { tool.as_str() };
        ruleset.iter().rev().find(|rule| wildcard::matches(permission,&rule.permission)).is_some_and(|rule| rule.pattern == "*" && rule.action == Action::Deny)
    }).cloned().collect()
}
