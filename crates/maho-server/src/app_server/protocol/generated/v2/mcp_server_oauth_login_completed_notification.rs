#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct McpServerOauthLoginCompletedNotification {
    #[serde(rename = "name")]
    pub name: String,
    #[serde(rename = "threadId", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub thread_id: Option<String>,
    #[serde(rename = "success")]
    pub success: bool,
    #[serde(rename = "error", default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
