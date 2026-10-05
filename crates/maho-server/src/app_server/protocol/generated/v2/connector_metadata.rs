#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConnectorMetadata {
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "name")]
    pub name: String,
    #[serde(rename = "description", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub description: Option<String>,
    #[serde(rename = "iconUrl", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub icon_url: Option<String>,
    #[serde(rename = "iconUrlDark", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub icon_url_dark: Option<String>,
    #[serde(rename = "distributionChannel", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub distribution_channel: Option<String>,
    #[serde(rename = "installUrl", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub install_url: Option<String>,
    #[serde(rename = "pluginDisplayNames")]
    pub plugin_display_names: Vec<String>,
    #[serde(rename = "toolSummaries", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub tool_summaries: Option<Vec<Box<crate::app_server::protocol::generated::v2::app_tool_summary::AppToolSummary>>>,
}
