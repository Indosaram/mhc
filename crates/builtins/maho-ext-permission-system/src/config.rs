use crate::{types::{Action, PermissionConfig, PermissionPresetName, PermissionValue, Rule, Ruleset}, wildcard};
use std::{collections::BTreeSet, path::PathBuf};

pub const EDIT_TOOLS: [&str; 4] = ["edit", "write", "apply_patch", "multiedit"];
pub const DEFAULT_PERMISSION_PRESET: PermissionPresetName = PermissionPresetName::FullAccess;
pub const ACCEPT_EDITS_PERMISSION_PRESET_CAPABILITY: &str = "permission_preset_accept_edits";
pub const AUTO_PERMISSION_PRESET_CAPABILITY: &str = "permission_preset_auto";

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
/// Locations the `guarded` preset treats as mhc-owned (and credentials as protected).
#[derive(Clone, Debug)]
pub struct PresetContext { pub agent_dir: PathBuf, pub temp_dir: PathBuf }
impl Default for PresetContext {
    fn default() -> Self { Self { agent_dir: PathBuf::from(maho_core::config::get_agent_dir()), temp_dir: std::env::temp_dir() } }
}
pub fn rules_for_preset(preset: PermissionPresetName) -> Ruleset { rules_for_preset_in(preset, &PresetContext::default()) }
pub fn rules_for_preset_in(preset: PermissionPresetName, context: &PresetContext) -> Ruleset {
    let rule = |permission: &str, action| Rule { permission: permission.into(), pattern: "*".into(), action };
    let credential = rule("credential_file", Action::Ask);
    match preset {
        PermissionPresetName::FullAccess => vec![rule("*",Action::Allow)],
        PermissionPresetName::Ask => vec![rule("*",Action::Ask)],
        PermissionPresetName::Auto => vec![rule("*",Action::Ask), credential],
        PermissionPresetName::Guarded => {
            let owned = |dir: String| Rule { permission: "external_directory".into(), pattern: format!("{dir}/*"), action: Action::Allow };
            let agent_dir = context.agent_dir.to_string_lossy().into_owned();
            // Render the memory root from its parent's string form: `Path::join` would emit a
            // backslash on Windows and mix separators with the sibling patterns above.
            let memory_parent = context
                .agent_dir
                .parent()
                .map_or_else(|| agent_dir.clone(), |dir| dir.to_string_lossy().into_owned());
            let memory = format!("{memory_parent}/memory");
            // The shared system temp root (`context.temp_dir`) is deliberately not allowed: it is not mhc-owned.
            vec![
                rule("*",Action::Allow), rule("external_directory",Action::Ask),
                owned(format!("{agent_dir}/sessions")), owned(format!("{agent_dir}/cache")), owned(format!("{agent_dir}/tmp")), owned(memory),
                credential,
            ]
        }
        PermissionPresetName::Workspace | PermissionPresetName::AcceptEdits | PermissionPresetName::ReadOnly => {
            let mut rules = vec![
                rule("*",Action::Ask), rule("read",Action::Allow),rule("list",Action::Allow),rule("grep",Action::Allow),
                rule("edit",if preset != PermissionPresetName::ReadOnly { Action::Allow } else { Action::Ask }),
                rule("bash",if preset == PermissionPresetName::Workspace { Action::Allow } else { Action::Ask }),rule("external_directory",Action::Ask),
            ];
            if preset != PermissionPresetName::ReadOnly { rules.push(credential); }
            rules
        }
    }
}
pub fn merge(rulesets: &[&[Rule]]) -> Ruleset { rulesets.iter().flat_map(|rules| rules.iter().cloned()).collect() }
pub fn disabled(tools: &[String], ruleset: &[Rule]) -> BTreeSet<String> {
    tools.iter().filter(|tool| {
        let permission = if EDIT_TOOLS.contains(&tool.as_str()) { "edit" } else { tool.as_str() };
        ruleset.iter().rev().find(|rule| wildcard::matches(permission,&rule.permission)).is_some_and(|rule| rule.pattern == "*" && rule.action == Action::Deny)
    }).cloned().collect()
}
