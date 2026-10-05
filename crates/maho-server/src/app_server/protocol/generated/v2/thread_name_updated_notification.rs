#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadNameUpdatedNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "threadName", default, skip_serializing_if = "Option::is_none")]
    pub thread_name: Option<String>,
}
