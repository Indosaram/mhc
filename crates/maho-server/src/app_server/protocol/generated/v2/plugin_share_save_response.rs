#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PluginShareSaveResponse {
    #[serde(rename = "remotePluginId")]
    pub remote_plugin_id: String,
    #[serde(rename = "shareUrl")]
    pub share_url: String,
}
