#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolsV2 {
    #[serde(rename = "web_search", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub web_search: Option<Box<crate::app_server::protocol::generated::web_search_tool_config::WebSearchToolConfig>>,
}
