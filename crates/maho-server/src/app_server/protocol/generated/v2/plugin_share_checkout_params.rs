#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PluginShareCheckoutParams {
    #[serde(rename = "remotePluginId")]
    pub remote_plugin_id: String,
}
