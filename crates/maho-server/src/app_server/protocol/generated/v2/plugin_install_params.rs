#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PluginInstallParams {
    #[serde(rename = "marketplacePath", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub marketplace_path: Option<Option<Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>>>,
    #[serde(rename = "remoteMarketplaceName", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub remote_marketplace_name: Option<Option<String>>,
    #[serde(rename = "pluginName")]
    pub plugin_name: String,
}
