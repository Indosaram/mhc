#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ListMcpServerStatusResponse {
    #[serde(rename = "data")]
    pub data: Vec<Box<crate::app_server::protocol::generated::v2::mcp_server_status::McpServerStatus>>,
    #[serde(rename = "nextCursor", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub next_cursor: Option<String>,
}
