#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct McpServerToolCallResponse {
    #[serde(rename = "content")]
    pub content: Vec<Box<crate::app_server::protocol::generated::serde_json::json_value::JsonValue>>,
    #[serde(rename = "structuredContent", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize_present")]
    pub structured_content: Option<Box<crate::app_server::protocol::generated::serde_json::json_value::JsonValue>>,
    #[serde(rename = "isError", default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize_present")]
    pub _meta: Option<Box<crate::app_server::protocol::generated::serde_json::json_value::JsonValue>>,
}
