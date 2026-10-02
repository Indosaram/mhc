#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ListMcpServerStatusParams {
    #[serde(rename = "cursor", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub cursor: Option<Option<String>>,
    #[serde(rename = "limit", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub limit: Option<Option<f64>>,
    #[serde(rename = "detail", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub detail: Option<Option<Box<crate::app_server::protocol::generated::v2::mcp_server_status_detail::McpServerStatusDetail>>>,
    #[serde(rename = "threadId", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub thread_id: Option<Option<String>>,
}
