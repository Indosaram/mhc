#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct McpServerElicitationRequestResponse {
    #[serde(rename = "action")]
    pub action: Box<crate::app_server::protocol::generated::v2::mcp_server_elicitation_action::McpServerElicitationAction>,
    #[serde(rename = "content", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub content: Option<Box<crate::app_server::protocol::generated::serde_json::json_value::JsonValue>>,
    #[serde(rename = "_meta", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub _meta: Option<Box<crate::app_server::protocol::generated::serde_json::json_value::JsonValue>>,
}
