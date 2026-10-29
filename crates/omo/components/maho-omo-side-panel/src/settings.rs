use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct OmoSidePanelSections { pub session: bool, pub goal: bool, pub context: bool, pub usage: bool, pub agents: bool, pub tools: bool, pub files: bool, pub memory: bool }
impl Default for OmoSidePanelSections { fn default() -> Self { Self { session:true, goal:true, context:true, usage:false, agents:true, tools:true, files:true, memory:true } } }
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)] pub enum PanelWidth { Columns(usize), Percent(String) }
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct OmoSidePanelSettings { pub enabled: bool, pub width: PanelWidth, pub min_columns: usize, pub clickable: bool, pub usage_poll_seconds: u64, pub sections: OmoSidePanelSections }
impl Default for OmoSidePanelSettings { fn default() -> Self { Self { enabled:false, width:PanelWidth::Percent("26%".into()), min_columns:120, clickable:true, usage_poll_seconds:150, sections:OmoSidePanelSections::default() } } }
#[derive(Debug)] pub enum SettingsError { Json(serde_json::Error), Range(&'static str) }
impl std::fmt::Display for SettingsError { fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result { match self { Self::Json(e)=>e.fmt(f),Self::Range(field)=>write!(f,"invalid side_panel.{field}") } } }
impl std::error::Error for SettingsError {}
pub fn resolve_omo_side_panel_settings(config: &serde_json::Value) -> Result<OmoSidePanelSettings, SettingsError> {
    let settings: OmoSidePanelSettings = serde_json::from_value(config.get("side_panel").cloned().unwrap_or_else(||serde_json::json!({}))).map_err(SettingsError::Json)?;
    let valid_width = match &settings.width { PanelWidth::Columns(n) => (24..=160).contains(n), PanelWidth::Percent(s) => s.strip_suffix('%').is_some_and(|n| n.len() == 2 && n.bytes().all(|b| b.is_ascii_digit()) && n.parse::<usize>().is_ok_and(|p| (10..=50).contains(&p))) };
    if !valid_width { return Err(SettingsError::Range("width")); }
    if !(60..=400).contains(&settings.min_columns) { return Err(SettingsError::Range("min_columns")); }
    if !(60..=3600).contains(&settings.usage_poll_seconds) { return Err(SettingsError::Range("usage_poll_seconds")); }
    Ok(settings)
}
