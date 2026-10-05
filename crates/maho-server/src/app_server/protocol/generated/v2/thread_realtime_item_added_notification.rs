#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadRealtimeItemAddedNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "item")]
    pub item: Box<crate::app_server::protocol::generated::serde_json::json_value::JsonValue>,
}
