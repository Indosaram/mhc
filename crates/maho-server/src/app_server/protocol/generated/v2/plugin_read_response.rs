#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PluginReadResponse {
    #[serde(rename = "plugin")]
    pub plugin: Box<crate::app_server::protocol::generated::v2::plugin_detail::PluginDetail>,
}
