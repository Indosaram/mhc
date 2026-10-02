#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum PluginInstallPolicy {
    #[serde(rename = "NOT_AVAILABLE")]
    NOTAVAILABLE,
    #[serde(rename = "AVAILABLE")]
    AVAILABLE,
    #[serde(rename = "INSTALLED_BY_DEFAULT")]
    INSTALLEDBYDEFAULT,
}
