#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WarningNotification {
    #[serde(rename = "threadId", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub thread_id: Option<String>,
    #[serde(rename = "message")]
    pub message: String,
}
