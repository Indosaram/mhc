use crate::{evaluate::evaluate, cli::parse_permission_preset_name, config::{DEFAULT_PERMISSION_PRESET, from_config, rules_for_preset}, storage::load_approved, types::{Action, PermissionPresetName, PermissionValue, Ruleset}};
use maho_core::settings_manager::SettingsManager;
use serde_json::{Map,Value};
use std::path::Path;

#[derive(Debug,thiserror::Error)]
pub enum SettingsError {
    #[error("{0}")]
    InvalidPreset(String),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
fn scope_rules(settings:&Map<String,Value>,scope:&str,home:&str)->Result<Ruleset,SettingsError> {
    let mut rules=Vec::new();
    if let Some(value)=settings.get("permissionPreset") {
        let Some(name)=value.as_str() else {return Err(SettingsError::InvalidPreset(format!("Invalid {scope} permissionPreset {value}. Expected a string.")))};
        let preset=parse_permission_preset_name(name).ok_or_else(||SettingsError::InvalidPreset(format!("Invalid {scope} permissionPreset \"{name}\". Expected one of: guarded, full-access, workspace, accept-edits, auto, read-only, ask.")))?;
        rules.extend(rules_for_preset(preset));
    }
    if let Some(config)=settings.get("permission").and_then(Value::as_object) {
        let config=config.iter().map(|(key,value)|Ok((key.clone(),serde_json::from_value::<PermissionValue>(value.clone())?))).collect::<Result<Vec<_>,serde_json::Error>>()?;
        rules.extend(from_config(&config,home)?);
    }
    Ok(rules)
}
/// The session's effective permission configuration: the merged rules (default preset, global, project,
/// CLI preset, CLI rules, in that order) and the preset that is in force (CLI, else project, else global, else default).
#[derive(Clone,Debug)]
pub struct ResolvedPermissionConfig { pub rules:Ruleset, pub preset:PermissionPresetName }
/// The session's effective approval behavior.
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum ApprovalSummary { Never, OnRequest, Untrusted }
impl ApprovalSummary {
    pub fn as_str(self)->&'static str { match self { Self::Never=>"never", Self::OnRequest=>"on-request", Self::Untrusted=>"untrusted" } }
}
pub fn resolve_permission_config(manager:&SettingsManager,cli:&[crate::types::Rule],home:&str,preset:Option<PermissionPresetName>)->Result<ResolvedPermissionConfig,SettingsError> {
    let mut rules=rules_for_preset(DEFAULT_PERMISSION_PRESET);
    rules.extend(scope_rules(manager.get_global(),"global",home)?);
    rules.extend(scope_rules(manager.get_project(),"project",home)?);
    if let Some(preset)=preset {rules.extend(rules_for_preset(preset));}
    rules.extend_from_slice(cli);
    let from=|settings:&Map<String,Value>|settings.get("permissionPreset").and_then(Value::as_str).and_then(parse_permission_preset_name);
    let effective=preset.or_else(||from(manager.get_project())).or_else(||from(manager.get_global())).unwrap_or(DEFAULT_PERMISSION_PRESET);
    Ok(ResolvedPermissionConfig{rules,preset:effective})
}
/// `untrusted` for the read-only and ask presets. Otherwise the rules are evaluated last-match-wins (as the runtime
/// does, so the always-prepended default base is overridden by later presets) for representative permissions:
/// any `ask` -> `on-request`, none -> `never`.
pub fn effective_approval_summary(config:&ResolvedPermissionConfig)->ApprovalSummary {
    if matches!(config.preset,PermissionPresetName::ReadOnly|PermissionPresetName::Ask) {return ApprovalSummary::Untrusted;}
    let asks=["bash","edit","read","external_directory","credential_file"].iter().any(|permission|evaluate(permission,"/probe/path",&[&config.rules]).action==Action::Ask);
    if asks {ApprovalSummary::OnRequest} else {ApprovalSummary::Never}
}
pub fn load_permission_settings(manager:&SettingsManager,cli:&[crate::types::Rule],project:(&Path,&str),preset:Option<PermissionPresetName>)->Result<(Ruleset,Ruleset),SettingsError> {
    let resolved=resolve_permission_config(manager,cli,project.1,preset)?;
    Ok((resolved.rules,load_approved(project.0)?))
}

/// The preset a session runs under when it is not the default: `--permission-preset`, else project, else global `permissionPreset`.
/// `None` means the default (full-access) applies, so children must not be handed a preset.
pub fn explicit_non_default_preset(manager:&SettingsManager,cli:Option<PermissionPresetName>)->Option<PermissionPresetName> {
    let from=|settings:&Map<String,Value>|settings.get("permissionPreset").and_then(Value::as_str).and_then(parse_permission_preset_name);
    cli.or_else(||from(manager.get_project())).or_else(||from(manager.get_global())).filter(|preset|*preset!=DEFAULT_PERMISSION_PRESET)
}
