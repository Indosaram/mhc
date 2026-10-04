#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ItemStartedNotification {
    #[serde(rename = "item")]
    pub item: Box<crate::app_server::protocol::generated::v2::thread_item::ThreadItem>,
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
    #[serde(rename = "startedAtMs")]
    pub started_at_ms: f64,
}
