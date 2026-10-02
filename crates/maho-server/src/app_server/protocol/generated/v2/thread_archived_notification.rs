#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadArchivedNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
}
