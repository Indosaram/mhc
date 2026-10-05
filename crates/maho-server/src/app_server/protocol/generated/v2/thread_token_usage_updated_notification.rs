#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadTokenUsageUpdatedNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
    #[serde(rename = "tokenUsage")]
    pub token_usage: Box<crate::app_server::protocol::generated::v2::thread_token_usage::ThreadTokenUsage>,
}
