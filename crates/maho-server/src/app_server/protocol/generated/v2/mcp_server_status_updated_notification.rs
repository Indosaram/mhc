#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct McpServerStatusUpdatedNotification {
    #[serde(rename = "threadId", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub thread_id: Option<String>,
    #[serde(rename = "name")]
    pub name: String,
    #[serde(rename = "status")]
    pub status: Box<crate::app_server::protocol::generated::v2::mcp_server_startup_state::McpServerStartupState>,
    #[serde(rename = "error", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub error: Option<String>,
    #[serde(rename = "failureReason", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub failure_reason: Option<Box<crate::app_server::protocol::generated::v2::mcp_server_startup_failure_reason::McpServerStartupFailureReason>>,
}
