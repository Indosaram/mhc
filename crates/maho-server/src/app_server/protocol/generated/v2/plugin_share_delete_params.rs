#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PluginShareDeleteParams {
    #[serde(rename = "remotePluginId")]
    pub remote_plugin_id: String,
}
