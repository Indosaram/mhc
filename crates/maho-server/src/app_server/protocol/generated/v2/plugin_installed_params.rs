#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PluginInstalledParams {
    #[serde(rename = "cwds", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub cwds: Option<Option<Vec<Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>>>>,
    #[serde(rename = "installSuggestionPluginNames", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub install_suggestion_plugin_names: Option<Option<Vec<String>>>,
}
