#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadClosedNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
}
