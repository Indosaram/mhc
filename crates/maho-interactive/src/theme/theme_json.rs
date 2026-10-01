//! Port of theme-json.ts: typed document validation.
use std::collections::BTreeMap;
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum ColorValue {
    Index(u8),
    Text(String),
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ThemeJson {
    pub name: String,
    #[serde(default)]
    pub vars: BTreeMap<String, ColorValue>,
    pub colors: BTreeMap<String, ColorValue>,
    #[serde(default, rename = "export")]
    pub export_colors: BTreeMap<String, ColorValue>,
}
#[derive(Debug, thiserror::Error)]
pub enum ThemeError {
    #[error("Invalid theme: {0}")]
    Invalid(String),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
pub fn validate_theme_json(label: &str, value: serde_json::Value) -> Result<ThemeJson, ThemeError> {
    let document: ThemeJson = serde_json::from_value(value)?;
    if document.name.contains('/') {
        return Err(ThemeError::Invalid(format!(
            "theme names cannot contain /: {}",
            document.name
        )));
    }
    let optional = [
        "scrollbarTrack",
        "scrollbarThumb",
        "thinkingMax",
        "searchMatchText",
        "skillMention",
        "searchMatchBg",
    ];
    let missing: Vec<_> = super::theme::ThemeColor::ALL
        .iter()
        .map(|c| c.key())
        .chain(super::theme::ThemeBg::ALL.iter().map(|c| c.key()))
        .filter(|key| !optional.contains(key) && !document.colors.contains_key(*key))
        .collect();
    if !missing.is_empty() {
        return Err(ThemeError::Invalid(format!(
            "{label}: Missing required color tokens: {}",
            missing.join(", ")
        )));
    }
    Ok(document)
}
