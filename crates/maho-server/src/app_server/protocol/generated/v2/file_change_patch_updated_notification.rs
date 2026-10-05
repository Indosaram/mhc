#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FileChangePatchUpdatedNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
    #[serde(rename = "itemId")]
    pub item_id: String,
    #[serde(rename = "changes")]
    pub changes: Vec<Box<crate::app_server::protocol::generated::v2::file_update_change::FileUpdateChange>>,
}
