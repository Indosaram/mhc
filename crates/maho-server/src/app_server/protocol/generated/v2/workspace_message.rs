#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WorkspaceMessage {
    #[serde(rename = "messageId")]
    pub message_id: String,
    #[serde(rename = "messageType")]
    pub message_type: Box<crate::app_server::protocol::generated::v2::workspace_message_type::WorkspaceMessageType>,
    #[serde(rename = "messageBody")]
    pub message_body: String,
    #[serde(rename = "createdAt", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub created_at: Option<f64>,
    #[serde(rename = "archivedAt", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub archived_at: Option<f64>,
}
