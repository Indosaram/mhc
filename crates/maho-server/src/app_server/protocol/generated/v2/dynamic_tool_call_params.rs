#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DynamicToolCallParams {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
    #[serde(rename = "callId")]
    pub call_id: String,
    #[serde(rename = "namespace", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub namespace: Option<String>,
    #[serde(rename = "tool")]
    pub tool: String,
    #[serde(rename = "arguments")]
    pub arguments: Box<crate::app_server::protocol::generated::serde_json::json_value::JsonValue>,
}
