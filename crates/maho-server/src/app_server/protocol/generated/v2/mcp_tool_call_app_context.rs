#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct McpToolCallAppContext {
    #[serde(rename = "connectorId")]
    pub connector_id: String,
    #[serde(rename = "linkId", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub link_id: Option<String>,
    #[serde(rename = "resourceUri", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub resource_uri: Option<String>,
    #[serde(rename = "appName", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub app_name: Option<String>,
    #[serde(rename = "actionName", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub action_name: Option<String>,
}
