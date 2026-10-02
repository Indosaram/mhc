#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum PluginAvailability {
    #[serde(rename = "AVAILABLE")]
    AVAILABLE,
    #[serde(rename = "DISABLED_BY_ADMIN")]
    DISABLEDBYADMIN,
}
