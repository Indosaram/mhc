#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadStatusChangedNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "status")]
    pub status: Box<crate::app_server::protocol::generated::v2::thread_status::ThreadStatus>,
}
