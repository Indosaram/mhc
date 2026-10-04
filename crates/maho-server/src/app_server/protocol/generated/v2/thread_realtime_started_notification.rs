#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadRealtimeStartedNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "realtimeSessionId", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub realtime_session_id: Option<String>,
    #[serde(rename = "version")]
    pub version: Box<crate::app_server::protocol::generated::realtime_conversation_version::RealtimeConversationVersion>,
}
