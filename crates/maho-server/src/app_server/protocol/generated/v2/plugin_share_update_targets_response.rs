#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PluginShareUpdateTargetsResponse {
    #[serde(rename = "principals")]
    pub principals: Vec<Box<crate::app_server::protocol::generated::v2::plugin_share_principal::PluginSharePrincipal>>,
    #[serde(rename = "discoverability")]
    pub discoverability: Box<crate::app_server::protocol::generated::v2::plugin_share_discoverability::PluginShareDiscoverability>,
}
