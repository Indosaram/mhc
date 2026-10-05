#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct McpToolCallResult {
    #[serde(rename = "content")]
    pub content: Vec<Box<crate::app_server::protocol::generated::serde_json::json_value::JsonValue>>,
    #[serde(rename = "structuredContent", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub structured_content: Option<Box<crate::app_server::protocol::generated::serde_json::json_value::JsonValue>>,
    #[serde(rename = "_meta", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub _meta: Option<Box<crate::app_server::protocol::generated::serde_json::json_value::JsonValue>>,
}
