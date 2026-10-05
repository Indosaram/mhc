#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PluginShareSaveParams {
    #[serde(rename = "pluginPath")]
    pub plugin_path: Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>,
    #[serde(rename = "remotePluginId", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub remote_plugin_id: Option<Option<String>>,
    #[serde(rename = "discoverability", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub discoverability: Option<Option<Box<crate::app_server::protocol::generated::v2::plugin_share_discoverability::PluginShareDiscoverability>>>,
    #[serde(rename = "shareTargets", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub share_targets: Option<Option<Vec<Box<crate::app_server::protocol::generated::v2::plugin_share_target::PluginShareTarget>>>>,
}
