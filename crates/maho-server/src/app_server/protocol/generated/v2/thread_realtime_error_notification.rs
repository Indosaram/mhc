#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadRealtimeErrorNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "message")]
    pub message: String,
}
