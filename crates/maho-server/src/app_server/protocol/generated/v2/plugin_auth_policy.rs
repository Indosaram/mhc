#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum PluginAuthPolicy {
    #[serde(rename = "ON_INSTALL")]
    ONINSTALL,
    #[serde(rename = "ON_USE")]
    ONUSE,
}
