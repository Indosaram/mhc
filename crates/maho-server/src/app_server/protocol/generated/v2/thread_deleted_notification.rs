#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadDeletedNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
}
