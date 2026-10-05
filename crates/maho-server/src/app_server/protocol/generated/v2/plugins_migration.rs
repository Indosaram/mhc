#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PluginsMigration {
    #[serde(rename = "marketplaceName")]
    pub marketplace_name: String,
    #[serde(rename = "pluginNames")]
    pub plugin_names: Vec<String>,
}
