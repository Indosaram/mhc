#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CollabAgentState {
    #[serde(rename = "status")]
    pub status: Box<crate::app_server::protocol::generated::v2::collab_agent_status::CollabAgentStatus>,
    #[serde(rename = "message", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub message: Option<String>,
}
