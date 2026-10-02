#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RawResponseItemCompletedNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
    #[serde(rename = "item")]
    pub item: Box<crate::app_server::protocol::generated::response_item::ResponseItem>,
}
