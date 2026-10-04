#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct McpServerInfo {
    #[serde(rename = "name")]
    pub name: String,
    #[serde(rename = "title", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub title: Option<String>,
    #[serde(rename = "version")]
    pub version: String,
    #[serde(rename = "description", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub description: Option<String>,
    #[serde(rename = "icons", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub icons: Option<Vec<Box<crate::app_server::protocol::generated::serde_json::json_value::JsonValue>>>,
    #[serde(rename = "websiteUrl", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub website_url: Option<String>,
}
