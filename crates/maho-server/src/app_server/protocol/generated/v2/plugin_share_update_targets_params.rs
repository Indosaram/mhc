#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PluginShareUpdateTargetsParams {
    #[serde(rename = "remotePluginId")]
    pub remote_plugin_id: String,
    #[serde(rename = "discoverability")]
    pub discoverability: Box<crate::app_server::protocol::generated::v2::plugin_share_update_discoverability::PluginShareUpdateDiscoverability>,
    #[serde(rename = "shareTargets")]
    pub share_targets: Vec<Box<crate::app_server::protocol::generated::v2::plugin_share_target::PluginShareTarget>>,
}
