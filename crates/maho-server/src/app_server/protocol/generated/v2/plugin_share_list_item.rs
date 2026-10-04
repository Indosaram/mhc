#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PluginShareListItem {
    #[serde(rename = "plugin")]
    pub plugin: Box<crate::app_server::protocol::generated::v2::plugin_summary::PluginSummary>,
    #[serde(rename = "localPluginPath", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub local_plugin_path: Option<Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>>,
}
