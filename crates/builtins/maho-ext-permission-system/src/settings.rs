use crate::{cli::parse_permission_preset_name, config::{DEFAULT_PERMISSION_PRESET, from_config, rules_for_preset}, storage::load_approved, types::{PermissionPresetName, PermissionValue, Ruleset}};
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
        let preset=parse_permission_preset_name(name).ok_or_else(||SettingsError::InvalidPreset(format!("Invalid {scope} permissionPreset \"{name}\". Expected one of: full-access, workspace, read-only, ask.")))?;
        rules.extend(rules_for_preset(preset));
    }
    if let Some(config)=settings.get("permission").and_then(Value::as_object) {
        let config=config.iter().map(|(key,value)|Ok((key.clone(),serde_json::from_value::<PermissionValue>(value.clone())?))).collect::<Result<Vec<_>,serde_json::Error>>()?;
        rules.extend(from_config(&config,home)?);
    }
    Ok(rules)
}
pub fn load_permission_settings(manager:&SettingsManager,cli:&[crate::types::Rule],project:(&Path,&str),preset:Option<PermissionPresetName>)->Result<(Ruleset,Ruleset),SettingsError> {
    let mut rules=rules_for_preset(DEFAULT_PERMISSION_PRESET);
    rules.extend(scope_rules(manager.get_global(),"global",project.1)?);
    rules.extend(scope_rules(manager.get_project(),"project",project.1)?);
    if let Some(preset)=preset {rules.extend(rules_for_preset(preset));}
    rules.extend_from_slice(cli);
    Ok((rules,load_approved(project.0)?))
}
