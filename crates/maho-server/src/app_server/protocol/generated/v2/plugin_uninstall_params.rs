#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PluginUninstallParams {
    #[serde(rename = "pluginId")]
    pub plugin_id: String,
}
