#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadUnarchivedNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
}
