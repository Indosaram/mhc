#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GuardianWarningNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "message")]
    pub message: String,
}
