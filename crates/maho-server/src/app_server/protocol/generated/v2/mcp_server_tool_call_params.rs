#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct McpServerToolCallParams {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "server")]
    pub server: String,
    #[serde(rename = "tool")]
    pub tool: String,
    #[serde(rename = "arguments", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize_present")]
    pub arguments: Option<Box<crate::app_server::protocol::generated::serde_json::json_value::JsonValue>>,
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize_present")]
    pub _meta: Option<Box<crate::app_server::protocol::generated::serde_json::json_value::JsonValue>>,
}
