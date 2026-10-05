#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadRealtimeClosedNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "reason", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub reason: Option<String>,
}
