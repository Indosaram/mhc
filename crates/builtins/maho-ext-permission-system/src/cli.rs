use crate::types::{Action, PermissionPresetName, Rule, Ruleset};

pub fn parse_permission_flag(value: &str) -> Ruleset {
    value.split(',').filter_map(|entry| {
        let (left, action) = entry.trim().split_once('=')?;
        if left.is_empty() || action.is_empty() { return None; }
        let (permission, pattern) = left.split_once(':').unwrap_or((left,"*"));
        if permission.is_empty() || pattern.is_empty() { return None; }
        let action = match action.trim() { "allow" => Action::Allow, "deny" => Action::Deny, "ask" => Action::Ask, _ => return None };
        Some(Rule { permission: permission.trim().into(), pattern: pattern.trim().into(), action })
    }).collect()
}
pub fn parse_permission_preset_name(value: &str) -> Option<PermissionPresetName> {
    match value {
        "full-access" => Some(PermissionPresetName::FullAccess), "workspace" => Some(PermissionPresetName::Workspace),
        "read-only" => Some(PermissionPresetName::ReadOnly), "ask" => Some(PermissionPresetName::Ask), _ => None,
    }
}
pub fn parse_permission_preset_flag(value: &str) -> Option<PermissionPresetName> { parse_permission_preset_name(value.trim()) }
