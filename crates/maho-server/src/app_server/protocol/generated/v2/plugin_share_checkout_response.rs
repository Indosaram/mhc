#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PluginShareCheckoutResponse {
    #[serde(rename = "remotePluginId")]
    pub remote_plugin_id: String,
    #[serde(rename = "pluginId")]
    pub plugin_id: String,
    #[serde(rename = "pluginName")]
    pub plugin_name: String,
    #[serde(rename = "pluginPath")]
    pub plugin_path: Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>,
    #[serde(rename = "marketplaceName")]
    pub marketplace_name: String,
    #[serde(rename = "marketplacePath")]
    pub marketplace_path: Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>,
    #[serde(rename = "remoteVersion", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub remote_version: Option<String>,
}
